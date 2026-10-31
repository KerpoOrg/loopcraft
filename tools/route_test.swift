// Routing test through coreaudiod: plays pseudo-noise into one device (a
// different sequence on left and right) and records another (or the same),
// then reports for every recorded channel which played channel arrived
// there, with what delay and gain, and whether it is bit-exact.
// Only the two named devices are touched; default devices are unchanged.
//   swiftc -O -o build/route_test tools/route_test.swift -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist -Xlinker tools/route_test-Info.plist
//   build/route_test <play-UID> <record-UID> [seconds]
import CoreAudio
import Foundation

func fail(_ msg: String) -> Never { print("FAIL: \(msg)"); exit(1) }

let args = CommandLine.arguments
guard args.count >= 3 else { fail("usage: route_test <play-UID> <record-UID> [seconds]") }
let seconds = args.count > 3 ? Double(args[3]) ?? 5 : 5

func addr(_ s: AudioObjectPropertySelector, _ scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress(mSelector: s, mScope: scope, mElement: kAudioObjectPropertyElementMain)
}

func device(_ uid: String) -> AudioObjectID {
    var dev = AudioObjectID(kAudioObjectUnknown)
    var a = addr(kAudioHardwarePropertyTranslateUIDToDevice)
    var cfUID = uid as CFString
    var size = UInt32(MemoryLayout<AudioObjectID>.size)
    let st = withUnsafeMutablePointer(to: &cfUID) { q in
        AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &a, UInt32(MemoryLayout<CFString>.size), q, &size, &dev)
    }
    guard st == noErr, dev != kAudioObjectUnknown else { fail("no device with UID \(uid)") }
    return dev
}

func channels(_ dev: AudioObjectID, _ scope: AudioObjectPropertyScope) -> Int {
    var a = addr(kAudioDevicePropertyStreamConfiguration, scope)
    var size: UInt32 = 0
    guard AudioObjectGetPropertyDataSize(dev, &a, 0, nil, &size) == noErr else { return 0 }
    let raw = UnsafeMutableRawPointer.allocate(byteCount: Int(size), alignment: 16)
    defer { raw.deallocate() }
    guard AudioObjectGetPropertyData(dev, &a, 0, nil, &size, raw) == noErr else { return 0 }
    return UnsafeMutableAudioBufferListPointer(raw.assumingMemoryBound(to: AudioBufferList.self)).reduce(0) { $0 + Int($1.mNumberChannels) }
}

let play = device(args[1]), record = device(args[2])
var rate: Float64 = 0
var ra = addr(kAudioDevicePropertyNominalSampleRate)
var size = UInt32(MemoryLayout<Float64>.size)
AudioObjectGetPropertyData(play, &ra, 0, nil, &size, &rate)
let playCh = channels(play, kAudioObjectPropertyScopeOutput)
let recCh = channels(record, kAudioObjectPropertyScopeInput)
guard playCh >= 1, recCh >= 1 else { fail("play device has \(playCh) output channels, record device \(recCh) input channels") }
print("playing into \(args[1]) (\(playCh) ch), recording \(args[2]) (\(recCh) ch) at \(Int(rate)) Hz for \(seconds) s")

// Stateless pseudo-noise in ±0.25, different per played channel (0 = left, 1 = right).
func noise(_ ch: Int, _ n: Int) -> Float {
    var x = UInt64(n) &* 0x9E37_79B9_7F4A_7C15 &+ UInt64(ch + 1) &* 0xBF58_476D_1CE4_E5B9
    x ^= x >> 31; x = x &* 0x94D0_49BB_1331_11EB; x ^= x >> 29
    return Float(Int64(x >> 40) - (1 << 23)) / Float(1 << 25)
}

let totalFrames = Int(rate * seconds)
let capacity = totalFrames + Int(rate)
let captured = UnsafeMutablePointer<Float>.allocate(capacity: capacity * recCh)
captured.initialize(repeating: 0, count: capacity * recCh)
var played = 0, recorded = 0

func playInto(_ out: UnsafeMutablePointer<AudioBufferList>) {
    let list = UnsafeMutableAudioBufferListPointer(out)
    guard let buf = list.first, let data = buf.mData?.assumingMemoryBound(to: Float.self) else { return }
    let ch = Int(buf.mNumberChannels)
    let frames = Int(buf.mDataByteSize) / (4 * ch)
    for i in 0..<frames {
        for c in 0..<ch { data[i * ch + c] = (played + i < totalFrames && c < 2) ? noise(c, played + i) : 0 }
    }
    played += frames
}

func recordFrom(_ inp: UnsafePointer<AudioBufferList>) {
    let list = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: inp))
    var offset = 0
    var frames = 0
    for buf in list {
        guard let data = buf.mData?.assumingMemoryBound(to: Float.self) else { continue }
        let ch = Int(buf.mNumberChannels)
        frames = Int(buf.mDataByteSize) / (4 * ch)
        let n = min(frames, capacity - recorded)
        for i in 0..<n { for c in 0..<ch where offset + c < recCh { captured[(recorded + i) * recCh + offset + c] = data[i * ch + c] } }
        offset += ch
    }
    recorded += min(frames, capacity - recorded)
}

let queue = DispatchQueue(label: "route-test")
var procs: [(AudioObjectID, AudioDeviceIOProcID)] = []
func start(_ dev: AudioObjectID, _ block: @escaping AudioDeviceIOBlock) {
    var id: AudioDeviceIOProcID?
    guard AudioDeviceCreateIOProcIDWithBlock(&id, dev, queue, block) == noErr, let pid = id else { fail("cannot create IOProc on \(dev)") }
    guard AudioDeviceStart(dev, pid) == noErr else { fail("cannot start \(dev)") }
    procs.append((dev, pid))
}
if play == record {
    start(play) { _, inp, _, out, _ in playInto(out); recordFrom(inp) }
} else {
    start(record) { _, inp, _, _, _ in recordFrom(inp) }
    start(play) { _, _, _, out, _ in playInto(out) }
}
Thread.sleep(forTimeInterval: seconds + 0.5)
for (dev, pid) in procs { AudioDeviceStop(dev, pid); AudioDeviceDestroyIOProcID(dev, pid) }

// Analysis: for each recorded channel, the played channel and delay with the
// strongest correlation over a window one second into the recording.
let window = 4096, start0 = Int(rate), maxLag = Int(rate * 0.5)
guard recorded > start0 + window else { fail("recorded only \(recorded) frames") }
let sig = (0..<2).map { ch in (0..<totalFrames).map { noise(ch, $0) } }
func rec(_ c: Int, _ n: Int) -> Float { captured[n * recCh + c] }
var allSilent = true
for c in 0..<recCh {
    var energy: Float = 0
    for k in 0..<window { energy += rec(c, start0 + k) * rec(c, start0 + k) }
    let rms = (energy / Float(window)).squareRoot()
    if rms < 1e-6 { print(String(format: "  ch %2d: silent", c + 1)); continue }
    allSilent = false
    var best = (corr: Float(-1), src: 0, lag: 0, gain: Float(0))
    for src in 0..<min(playCh, 2) {
        for lag in 0..<maxLag {
            var xy: Float = 0, yy: Float = 0
            for k in 0..<window {
                let y = sig[src][start0 + k - lag]
                xy += rec(c, start0 + k) * y
                yy += y * y
            }
            let corr = xy / (energy * yy).squareRoot()
            if corr > best.corr { best = (corr, src, lag, xy / yy) }
        }
    }
    // Bit-exact check over everything after the delay.
    let g: Float = abs(best.gain - 1) < 1e-3 ? 1 : best.gain
    var exact = 0, compared = 0
    for n in best.lag..<min(recorded, totalFrames + best.lag) {
        compared += 1
        if rec(c, n) == sig[best.src][n - best.lag] * g { exact += 1 }
    }
    print(String(format: "  ch %2d: %@ in, delay %d frames (%.1f ms), gain %.4f, correlation %.4f, bit-exact %d/%d",
                 c + 1, best.src == 0 ? "left" : "right", best.lag, Double(best.lag) / rate * 1000, best.gain, best.corr, exact, compared))
}
print(allSilent ? "recorded only silence" : "done")
