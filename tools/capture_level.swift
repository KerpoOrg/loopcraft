// Records from the LoopCraft device's input for a few seconds (no output is
// written) and reports peak/RMS per channel, to confirm what apps play into
// the device comes back out.
//   swiftc -O -o build/capture_level tools/capture_level.swift && build/capture_level [seconds]
import CoreAudio
import Foundation

let uid = "org.kerpo.loopcraft:8E0F3C52-6B7A-4C1E-9D2F-3A5B7C9E1F20"
let seconds = Double(CommandLine.arguments.dropFirst().first ?? "3") ?? 3

var device = AudioObjectID(kAudioObjectUnknown)
var a = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyTranslateUIDToDevice, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
var cfUID = uid as CFString
var size = UInt32(MemoryLayout<AudioObjectID>.size)
_ = withUnsafeMutablePointer(to: &cfUID) { q in
    AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &a, UInt32(MemoryLayout<CFString>.size), q, &size, &device)
}
guard device != kAudioObjectUnknown else { print("LoopCraft device not found"); exit(1) }

var peak: [Float] = [0, 0]
var sumSq: [Double] = [0, 0]
var frames = 0
var nonSilentBlocks = 0, blocks = 0

var procID: AudioDeviceIOProcID?
AudioDeviceCreateIOProcIDWithBlock(&procID, device, DispatchQueue(label: "capture")) { _, inInput, _, _, _ in
    let list = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: inInput))
    guard let buf = list.first, let data = buf.mData?.assumingMemoryBound(to: Float.self) else { return }
    let n = Int(buf.mDataByteSize) / (MemoryLayout<Float>.size * 2)
    var blockPeak: Float = 0
    for i in 0..<n {
        for c in 0..<2 {
            let v = data[2 * i + c]
            peak[c] = max(peak[c], abs(v))
            sumSq[c] += Double(v * v)
            blockPeak = max(blockPeak, abs(v))
        }
    }
    frames += n
    blocks += 1
    if blockPeak > 1e-4 { nonSilentBlocks += 1 }
}
AudioDeviceStart(device, procID!)
Thread.sleep(forTimeInterval: seconds)
AudioDeviceStop(device, procID!)
AudioDeviceDestroyIOProcID(device, procID!)

func db(_ x: Double) -> String { x > 0 ? String(format: "%.1f dBFS", 20 * log10(x)) : "-inf" }
for c in 0..<2 {
    let rms = frames > 0 ? (sumSq[c] / Double(frames)).squareRoot() : 0
    print("channel \(c + 1): peak \(db(Double(peak[c])))  rms \(db(rms))")
}
print("\(frames) frames captured, \(nonSilentBlocks)/\(blocks) buffers contained audio")
