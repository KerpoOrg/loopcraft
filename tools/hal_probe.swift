// Read-only Core Audio HAL probe: dumps the public properties of the devices of
// plug-ins whose bundle ID contains the given text (default "loopcraft").
//   swiftc -O -o build/hal_probe tools/hal_probe.swift && build/hal_probe [bundle-id-substring]
// Only AudioObjectGetPropertyData / HasProperty / IsPropertySettable are used; nothing is set.
import CoreAudio
import Foundation

// From AudioServerPlugIn.h (not imported into Swift)
let kAudioObjectPropertyCustomPropertyInfoList: AudioObjectPropertySelector = 0x63757374 // 'cust'
let kAudioDevicePropertyZeroTimeStampPeriod: AudioObjectPropertySelector = 0x72696e67    // 'ring'
let kAudioServerPlugInCustomPropertyDataTypeCFPropertyList: UInt32 = 0x706c7374          // 'plst'
struct AudioServerPlugInCustomPropertyInfo { var mSelector: UInt32; var mPropertyDataType: UInt32; var mQualifierDataType: UInt32 }

func fourcc(_ v: UInt32) -> String {
    let b = [24, 16, 8, 0].map { UInt8((v >> UInt32($0)) & 0xff) }
    let s = String(bytes: b, encoding: .ascii) ?? "?"
    return b.allSatisfy({ $0 >= 32 && $0 < 127 }) ? "'\(s)'" : String(format: "0x%08x", v)
}

func addr(_ sel: AudioObjectPropertySelector, _ scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal,
          _ el: AudioObjectPropertyElement = kAudioObjectPropertyElementMain) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress(mSelector: sel, mScope: scope, mElement: el)
}

func has(_ obj: AudioObjectID, _ a: AudioObjectPropertyAddress) -> Bool {
    var a = a; return AudioObjectHasProperty(obj, &a)
}

func settable(_ obj: AudioObjectID, _ a: AudioObjectPropertyAddress) -> Bool {
    var a = a; var s: DarwinBoolean = false
    return AudioObjectIsPropertySettable(obj, &a, &s) == noErr && s.boolValue
}

func data(_ obj: AudioObjectID, _ a: AudioObjectPropertyAddress) -> Data? {
    var a = a; var size: UInt32 = 0
    guard AudioObjectGetPropertyDataSize(obj, &a, 0, nil, &size) == noErr else { return nil }
    var d = Data(count: Int(size))
    let st = d.withUnsafeMutableBytes { AudioObjectGetPropertyData(obj, &a, 0, nil, &size, $0.baseAddress!) }
    return st == noErr ? d.prefix(Int(size)) : nil
}

func u32(_ o: AudioObjectID, _ a: AudioObjectPropertyAddress) -> UInt32? {
    data(o, a).flatMap { $0.count >= 4 ? $0.withUnsafeBytes { $0.load(as: UInt32.self) } : nil }
}
func f64(_ o: AudioObjectID, _ a: AudioObjectPropertyAddress) -> Double? {
    data(o, a).flatMap { $0.count >= 8 ? $0.withUnsafeBytes { $0.load(as: Double.self) } : nil }
}
func f32(_ o: AudioObjectID, _ a: AudioObjectPropertyAddress) -> Float32? {
    data(o, a).flatMap { $0.count >= 4 ? $0.withUnsafeBytes { $0.load(as: Float32.self) } : nil }
}
func ids(_ o: AudioObjectID, _ a: AudioObjectPropertyAddress) -> [AudioObjectID] {
    guard let d = data(o, a) else { return [] }
    return d.withUnsafeBytes { Array($0.bindMemory(to: AudioObjectID.self)) }
}
func str(_ o: AudioObjectID, _ a: AudioObjectPropertyAddress) -> String? {
    var a = a; var size = UInt32(MemoryLayout<CFString?>.size); var s: Unmanaged<CFString>?
    guard has(o, a), AudioObjectGetPropertyData(o, &a, 0, nil, &size, &s) == noErr, let v = s else { return nil }
    return v.takeRetainedValue() as String
}
func plist(_ o: AudioObjectID, _ a: AudioObjectPropertyAddress) -> Any? {
    var a = a; var size = UInt32(MemoryLayout<CFPropertyList?>.size); var p: Unmanaged<CFPropertyList>?
    guard AudioObjectGetPropertyData(o, &a, 0, nil, &size, &p) == noErr, let v = p else { return nil }
    return v.takeRetainedValue()
}

let gIn = kAudioObjectPropertyScopeInput, gOut = kAudioObjectPropertyScopeOutput, gGlob = kAudioObjectPropertyScopeGlobal

func line(_ k: String, _ v: Any?) { if let v = v { print("  \(k): \(v)") } }

func dumpControl(_ c: AudioObjectID) {
    let cls = u32(c, addr(kAudioObjectPropertyClass)) ?? 0
    let scope = u32(c, addr(kAudioControlPropertyScope)) ?? 0
    let el = u32(c, addr(kAudioControlPropertyElement)) ?? 0
    var extra = ""
    if let v = f32(c, addr(kAudioLevelControlPropertyScalarValue)) { extra += " scalar=\(v)" }
    if let v = f32(c, addr(kAudioLevelControlPropertyDecibelValue)) { extra += " dB=\(v)" }
    if let d = data(c, addr(kAudioLevelControlPropertyDecibelRange)), d.count == 16 {
        let r = d.withUnsafeBytes { $0.load(as: AudioValueRange.self) }; extra += " dBrange=[\(r.mMinimum),\(r.mMaximum)]"
    }
    if let v = u32(c, addr(kAudioBooleanControlPropertyValue)) { extra += " bool=\(v)" }
    if let v = u32(c, addr(kAudioSelectorControlPropertyCurrentItem)) { extra += " current=\(v)" }
    let items = ids(c, addr(kAudioSelectorControlPropertyAvailableItems)); if !items.isEmpty { extra += " items=\(items)" }
    print("    control id=\(c) class=\(fourcc(cls)) scope=\(fourcc(scope)) element=\(el) settable=\(settable(c, addr(kAudioLevelControlPropertyScalarValue)) || settable(c, addr(kAudioBooleanControlPropertyValue)))\(extra)")
}

func dumpStream(_ s: AudioObjectID) {
    let dir = u32(s, addr(kAudioStreamPropertyDirection)) ?? 9
    var fmt = ""
    if let d = data(s, addr(kAudioStreamPropertyVirtualFormat)), d.count >= MemoryLayout<AudioStreamBasicDescription>.size {
        let f = d.withUnsafeBytes { $0.load(as: AudioStreamBasicDescription.self) }
        fmt = "\(f.mSampleRate)Hz \(fourcc(f.mFormatID)) flags=0x\(String(f.mFormatFlags, radix: 16)) ch=\(f.mChannelsPerFrame) bits=\(f.mBitsPerChannel) bpf=\(f.mBytesPerFrame)"
    }
    var phys = ""
    if let d = data(s, addr(kAudioStreamPropertyPhysicalFormat)), d.count >= MemoryLayout<AudioStreamBasicDescription>.size {
        let f = d.withUnsafeBytes { $0.load(as: AudioStreamBasicDescription.self) }
        phys = "\(fourcc(f.mFormatID)) flags=0x\(String(f.mFormatFlags, radix: 16)) bits=\(f.mBitsPerChannel)"
    }
    let nAvail = (data(s, addr(kAudioStreamPropertyAvailableVirtualFormats))?.count ?? 0) / MemoryLayout<AudioStreamRangedDescription>.size
    print("    stream id=\(s) dir=\(dir == 1 ? "input" : "output") startCh=\(u32(s, addr(kAudioStreamPropertyStartingChannel)) ?? 0) latency=\(u32(s, addr(kAudioStreamPropertyLatency)) ?? 0) terminal=\(fourcc(u32(s, addr(kAudioStreamPropertyTerminalType)) ?? 0)) active=\(u32(s, addr(kAudioStreamPropertyIsActive)) ?? 0)")
    print("      virtual: \(fmt) | physical: \(phys) | availableVirtualFormats=\(nAvail)")
}

func dumpDevice(_ d: AudioObjectID) {
    let cls = u32(d, addr(kAudioObjectPropertyClass)) ?? 0
    print("== device id=\(d) class=\(fourcc(cls)) name=\(str(d, addr(kAudioObjectPropertyName)) ?? "-")")
    line("uid", str(d, addr(kAudioDevicePropertyDeviceUID)))
    line("modelUID", str(d, addr(kAudioDevicePropertyModelUID)))
    line("manufacturer", str(d, addr(kAudioObjectPropertyManufacturer)))
    line("transport", u32(d, addr(kAudioDevicePropertyTransportType)).map(fourcc))
    line("clockDomain", u32(d, addr(kAudioDevicePropertyClockDomain)))
    line("isHidden", u32(d, addr(kAudioDevicePropertyIsHidden)))
    line("isAlive", u32(d, addr(kAudioDevicePropertyDeviceIsAlive)))
    line("isRunningSomewhere", u32(d, addr(kAudioDevicePropertyDeviceIsRunningSomewhere)))
    line("canBeDefault in/out", "\(u32(d, addr(kAudioDevicePropertyDeviceCanBeDefaultDevice, gIn)).map(String.init) ?? "-")/\(u32(d, addr(kAudioDevicePropertyDeviceCanBeDefaultDevice, gOut)).map(String.init) ?? "-")")
    line("canBeSystemDefault in/out", "\(u32(d, addr(kAudioDevicePropertyDeviceCanBeDefaultSystemDevice, gIn)).map(String.init) ?? "-")/\(u32(d, addr(kAudioDevicePropertyDeviceCanBeDefaultSystemDevice, gOut)).map(String.init) ?? "-")")
    line("nominalRate", f64(d, addr(kAudioDevicePropertyNominalSampleRate)))
    line("nominalRate settable", settable(d, addr(kAudioDevicePropertyNominalSampleRate)))
    if let r = data(d, addr(kAudioDevicePropertyAvailableNominalSampleRates)) {
        let rs = r.withUnsafeBytes { Array($0.bindMemory(to: AudioValueRange.self)) }.map { $0.mMinimum == $0.mMaximum ? "\(Int($0.mMinimum))" : "\($0.mMinimum)-\($0.mMaximum)" }
        line("availableRates", rs.joined(separator: ","))
    }
    line("zeroTimeStampPeriod", u32(d, addr(kAudioDevicePropertyZeroTimeStampPeriod)))
    line("actualRate", f64(d, addr(kAudioDevicePropertyActualSampleRate)))
    line("latency in/out", "\(u32(d, addr(kAudioDevicePropertyLatency, gIn)).map(String.init) ?? "-")/\(u32(d, addr(kAudioDevicePropertyLatency, gOut)).map(String.init) ?? "-")")
    line("safetyOffset in/out", "\(u32(d, addr(kAudioDevicePropertySafetyOffset, gIn)).map(String.init) ?? "-")/\(u32(d, addr(kAudioDevicePropertySafetyOffset, gOut)).map(String.init) ?? "-")")
    line("bufferFrameSize", u32(d, addr(kAudioDevicePropertyBufferFrameSize)))
    if let r = data(d, addr(kAudioDevicePropertyBufferFrameSizeRange)), r.count == 16 {
        let v = r.withUnsafeBytes { $0.load(as: AudioValueRange.self) }; line("bufferFrameSizeRange", "\(v.mMinimum)-\(v.mMaximum)")
    }
    line("icon", has(d, addr(kAudioDevicePropertyIcon)) ? "present" : nil)
    line("relatedDevices", ids(d, addr(kAudioDevicePropertyRelatedDevices)))
    for (label, sc) in [("input", gIn), ("output", gOut)] {
        if let s = data(d, addr(kAudioDevicePropertyPreferredChannelsForStereo, sc)), s.count == 8 {
            let v = s.withUnsafeBytes { Array($0.bindMemory(to: UInt32.self)) }; line("stereoPair \(label)", v)
        }
        var names: [String] = []
        for el in 1...64 {
            let a = addr(kAudioObjectPropertyElementName, sc, UInt32(el))
            guard has(d, a) else { break }
            names.append(str(d, a) ?? "")
        }
        if !names.isEmpty { line("channelNames \(label)", names) }
    }
    // Custom properties: list and read plist ones
    if let cpi = data(d, addr(kAudioObjectPropertyCustomPropertyInfoList)) {
        let infos = cpi.withUnsafeBytes { Array($0.bindMemory(to: AudioServerPlugInCustomPropertyInfo.self)) }
        for i in infos {
            print("  customProperty \(fourcc(i.mSelector)) dataType=\(fourcc(i.mPropertyDataType)) qualifier=\(fourcc(i.mQualifierDataType))")
            if i.mPropertyDataType == kAudioServerPlugInCustomPropertyDataTypeCFPropertyList, let p = plist(d, addr(i.mSelector)) {
                if let pd = try? PropertyListSerialization.data(fromPropertyList: p, format: .xml, options: 0), let s = String(data: pd, encoding: .utf8) {
                    print("    plist value:\n" + s.split(separator: "\n").map { "      " + $0 }.joined(separator: "\n"))
                }
            }
        }
    }
    let streams = ids(d, addr(kAudioDevicePropertyStreams, gIn)) + ids(d, addr(kAudioDevicePropertyStreams, gOut))
    print("  streams: \(streams.count)"); streams.forEach(dumpStream)
    let controls = ids(d, addr(kAudioObjectPropertyControlList))
    print("  controls: \(controls.count)"); controls.forEach(dumpControl)
    let owned = ids(d, addr(kAudioObjectPropertyOwnedObjects))
    print("  ownedObjects: \(owned.map { "\($0):\(fourcc(u32($0, addr(kAudioObjectPropertyClass)) ?? 0))" })")
}

let sys = AudioObjectID(kAudioObjectSystemObject)
let arg = CommandLine.arguments.dropFirst().first ?? "loopcraft"
// A device UID (e.g. a hidden engine twin, which no device list shows): dump just that device.
if arg.contains(":") {
    var uid = arg as CFString
    var dev = AudioObjectID(kAudioObjectUnknown)
    var size = UInt32(MemoryLayout<AudioObjectID>.size)
    var a = addr(kAudioHardwarePropertyTranslateUIDToDevice)
    let st = withUnsafeMutablePointer(to: &uid) { q in
        AudioObjectGetPropertyData(sys, &a, UInt32(MemoryLayout<CFString>.size), q, &size, &dev)
    }
    guard st == noErr, dev != kAudioObjectUnknown else { print("no device with UID \(arg) (status \(st))"); exit(1) }
    dumpDevice(dev)
    exit(0)
}
let filter = arg.lowercased()
var foundPlugIn = false
// All plug-ins, with their owned objects (incl. hidden/clock devices)
for p in ids(sys, addr(kAudioHardwarePropertyPlugInList)) {
    guard let bid = str(p, addr(kAudioPlugInPropertyBundleID)), bid.lowercased().contains(filter) else { continue }
    foundPlugIn = true
    print("## plug-in id=\(p) bundle=\(bid) name=\(str(p, addr(kAudioObjectPropertyName)) ?? "-")")
    let owned = ids(p, addr(kAudioObjectPropertyOwnedObjects))
    print("  ownedObjects: \(owned.map { "\($0):\(fourcc(u32($0, addr(kAudioObjectPropertyClass)) ?? 0))" })")
    print("  deviceList: \(ids(p, addr(kAudioPlugInPropertyDeviceList)))")
    print("  clockDeviceList: \(ids(p, addr(kAudioPlugInPropertyClockDeviceList)))")
    if let cpi = data(p, addr(kAudioObjectPropertyCustomPropertyInfoList)) {
        let infos = cpi.withUnsafeBytes { Array($0.bindMemory(to: AudioServerPlugInCustomPropertyInfo.self)) }
        for i in infos { print("  plug-in customProperty \(fourcc(i.mSelector)) dataType=\(fourcc(i.mPropertyDataType))") }
    }
    for o in ids(p, addr(kAudioPlugInPropertyDeviceList)) + ids(p, addr(kAudioPlugInPropertyClockDeviceList)) { dumpDevice(o) }
}

if !foundPlugIn { print("no plug-in with bundle ID containing \"\(filter)\" is loaded"); exit(1) }
