// End-to-end pass-through test through coreaudiod: plays a known signal into the
// LoopCraft device and records its input in the same IOProc, then checks that
// the recording equals the signal delayed by a constant number of frames.
// Only the LoopCraft device is touched; system default devices are unchanged.
//   swiftc -O -o build/passthru_test tools/passthru_test.swift
//   build/passthru_test [seconds] [sample-rate]
import CoreAudio
import Foundation

let uid = "org.kerpo.loopcraft:8E0F3C52-6B7A-4C1E-9D2F-3A5B7C9E1F20"
let seconds = Double(CommandLine.arguments.dropFirst().first ?? "10") ?? 10
let wantedRate = CommandLine.arguments.count > 2 ? Double(CommandLine.arguments[2]) : nil

func fail(_ msg: String) -> Never { print("FAIL: \(msg)"); exit(1) }

func addr(_ s: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress(mSelector: s, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
}

// Find the device by UID.
var device = AudioObjectID(kAudioObjectUnknown)
var a = addr(kAudioHardwarePropertyTranslateUIDToDevice)
var cfUID = uid as CFString
var size = UInt32(MemoryLayout<AudioObjectID>.size)
let st = withUnsafeMutablePointer(to: &cfUID) { q in
    AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &a, UInt32(MemoryLayout<CFString>.size), q, &size, &device)
}
guard st == noErr, device != kAudioObjectUnknown else { fail("LoopCraft device not found (status \(st))") }

if let r = wantedRate {
    var rate = r
    var ra = addr(kAudioDevicePropertyNominalSampleRate)
    guard AudioObjectSetPropertyData(device, &ra, 0, nil, UInt32(MemoryLayout<Float64>.size), &rate) == noErr else { fail("cannot set rate \(r)") }
    Thread.sleep(forTimeInterval: 0.5)
}
var rate: Float64 = 0
var ra = addr(kAudioDevicePropertyNominalSampleRate)
size = UInt32(MemoryLayout<Float64>.size)
AudioObjectGetPropertyData(device, &ra, 0, nil, &size, &rate)
print("device \(device) at \(Int(rate)) Hz, running \(seconds) s")

// Test signal: 997 Hz sine with a DC offset so no sample is zero; channel 2 is
// the negated channel 1 to catch channel swaps.
let totalFrames = Int(rate * seconds)
func signal(_ n: Int) -> Float { 0.25 + 0.5 * Float(sin(2.0 * Double.pi * 997.0 * Double(n) / rate)) }

let capacity = totalFrames + Int(rate) // room for latency
let captured = UnsafeMutablePointer<Float>.allocate(capacity: capacity * 2)
captured.initialize(repeating: 0, count: capacity * 2)
var played = 0
var recorded = 0
var cycles = 0
var ioFrameSizes = Set<Int>()

let queue = DispatchQueue(label: "passthru-test")
var procID: AudioDeviceIOProcID?
let rc = AudioDeviceCreateIOProcIDWithBlock(&procID, device, queue) { _, inInput, _, outOutput, _ in
    cycles += 1
    let outList = UnsafeMutableAudioBufferListPointer(outOutput)
    if let buf = outList.first, let data = buf.mData?.assumingMemoryBound(to: Float.self) {
        let frames = Int(buf.mDataByteSize) / (MemoryLayout<Float>.size * 2)
        ioFrameSizes.insert(frames)
        for i in 0..<frames {
            let n = played + i
            let v: Float = n < totalFrames ? signal(n) : 0
            data[2 * i] = v
            data[2 * i + 1] = -v
        }
        played += frames
    }
    let inList = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: inInput))
    if let buf = inList.first, let data = buf.mData?.assumingMemoryBound(to: Float.self) {
        let frames = Int(buf.mDataByteSize) / (MemoryLayout<Float>.size * 2)
        let n = min(frames, capacity - recorded)
        if n > 0 {
            (captured + recorded * 2).update(from: data, count: n * 2)
            recorded += n
        }
    }
}
guard rc == noErr, let pid = procID else { fail("AudioDeviceCreateIOProcIDWithBlock: \(rc)") }
guard AudioDeviceStart(device, pid) == noErr else { fail("AudioDeviceStart") }
Thread.sleep(forTimeInterval: seconds + 0.5)
AudioDeviceStop(device, pid)
AudioDeviceDestroyIOProcID(device, pid)

// Analysis (after IO has stopped).
let n = recorded
guard let start = (0..<n).first(where: { captured[2 * $0] != 0 }) else { fail("recorded only silence (\(n) frames, \(cycles) cycles)") }
let latencyFrames = start
let compareFrames = min(totalFrames, n - start)
var mismatches = 0
var firstMismatch = -1
for k in 0..<compareFrames {
    let l = captured[2 * (start + k)], r = captured[2 * (start + k) + 1]
    if l != signal(k) || r != -signal(k) {
        mismatches += 1
        if firstMismatch < 0 { firstMismatch = k }
    }
}
print("cycles \(cycles), IO buffer sizes \(ioFrameSizes.sorted()), played \(played) frames, recorded \(n) frames")
print(String(format: "pass-through latency %d frames (%.2f ms)", latencyFrames, Double(latencyFrames) / rate * 1000))
print("compared \(compareFrames) frames (\(String(format: "%.1f", Double(compareFrames) / rate)) s): \(mismatches) mismatches" + (firstMismatch >= 0 ? ", first at frame \(firstMismatch)" : ""))
if mismatches > 0 {
    print("first captured frames:", (0..<6).map { String(format: "%.4f/%.4f", captured[2 * $0], captured[2 * $0 + 1]) }.joined(separator: " "))
    // Locate the signal by searching for its first 4 samples.
    let probe = (0..<4).map { signal($0) }
    if let lag = (0..<(n - 4)).first(where: { i in (0..<4).allSatisfy { captured[2 * (i + $0)] == probe[$0] } }) {
        print("signal found at frame \(lag)")
    } else {
        print("signal start not found in recording")
    }
}
let ok = mismatches == 0 && compareFrames >= Int(rate * (seconds - 1))
print(ok ? "PASS: bit-exact pass-through, constant latency, no dropouts" : "FAIL")
exit(ok ? 0 : 1)
