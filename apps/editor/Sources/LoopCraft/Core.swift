// The editor's view of the LoopCraft core (crates/lc-ffi): the config model,
// loading and validated saving, the audio devices present, device volume.

import CLoopCraft
import Foundation

struct EConfig: Codable, Equatable {
    var devices: [EDevice]
}

struct EDevice: Codable, Equatable {
    var name: String
    var id: String
    /// Bus channels: what the busses add up to.
    var busses: UInt32
    /// In channel order, two channels each or one when mono (wires here are
    /// channel numbers; the file names busses by id).
    var bus_list: [EBus]
    /// Bus groups, each shown as a unit of its own.
    var groups: [EGroup]
    var virtual_inputs: UInt32
    var sample_rate: UInt32
    var buffer_frames: UInt32?
    var inputs: [EPort]
    var outputs: [EPort]

    var uid: String { "org.kerpo.loopcraft:" + id }
}

/// An input or output. Inputs: "virtual", "virtual_device", "device", "app";
/// outputs: "device", "virtual_device".
struct EBus: Codable, Equatable {
    var id: String
    /// Empty: shown by its channel numbers.
    var name: String
    var mono: Bool
    /// The group's id; nil: shown in the mixer itself.
    var group: String?
}

struct EGroup: Codable, Equatable {
    var id: String
    var name: String
}

struct EPort: Codable, Equatable {
    var kind: String
    var name: String?
    var id: String?
    var channels: UInt32?
    var icon: String?
    var mute_original: Bool = false
    var tap: Bool = false
    /// [from, to] channels, numbered from 1.
    var wires: [[UInt32]]
    var enabled: Bool
    /// nil = -inf.
    var gain_db: Double?
    var muted: Bool
    var follow_volume: Bool = false
    /// Channels whose jacks and meters are one per channel, not a stereo pair.
    var mono: [UInt32] = []
    /// A half-width unit (stereo and mono units only).
    var half: Bool = false
}

struct HardwareDevice: Codable, Equatable {
    var uid: String
    var name: String
    var manufacturer: String
    var inputs: UInt32
    var outputs: UInt32
    var rate: Double
    var loopcraft: Bool
    /// What the channels carry: from the device's profile, else its own names.
    var input_groups: [ChannelGroup]
    var output_groups: [ChannelGroup]
    /// Channels patched when the device is added (from its profile).
    var default_input: [UInt32]
    var default_output: [UInt32]
    /// "built-in:<name>" or a file in ~/.config/loopcraft/devices; nil without one.
    var profile: String?
}

struct ChannelGroup: Codable, Equatable {
    var channels: [UInt32]
    var name: String
    var note: String?
}

struct CoreError: Error {
    var message: String
}

enum Core {
    private static func call(_ f: () -> UnsafeMutablePointer<CChar>?) -> Data {
        guard let p = f() else { return Data() }
        defer { lc_free(p) }
        return Data(String(cString: p).utf8)
    }

    static var defaultPath: String {
        (try? JSONDecoder().decode(String.self, from: call { lc_config_default_path() })) ?? (NSHomeDirectory() + "/.config/loopcraft/config.yaml")
    }

    private struct LoadResult: Decodable {
        var ok: Bool
        var config: EConfig?
        var error: String?
    }

    private struct SaveResult: Decodable {
        var ok: Bool
        var error: String?
    }

    static func load(_ path: String) -> Result<EConfig, CoreError> {
        let data = path.withCString { p in call { lc_config_load(p) } }
        guard let r = try? JSONDecoder().decode(LoadResult.self, from: data) else { return .failure(CoreError(message: "the core returned no answer")) }
        if r.ok, let c = r.config { return .success(c) }
        return .failure(CoreError(message: r.error ?? "unknown error"))
    }

    /// Validates and writes; nil on success, else why not.
    static func save(_ config: EConfig, to path: String) -> String? {
        guard let json = try? String(data: JSONEncoder().encode(config), encoding: .utf8) else { return "cannot encode the configuration" }
        let data = path.withCString { p in json.withCString { j in call { lc_config_save(p, j) } } }
        guard let r = try? JSONDecoder().decode(SaveResult.self, from: data) else { return "the core returned no answer" }
        return r.ok ? nil : (r.error ?? "unknown error")
    }

    static func devices() -> [HardwareDevice] {
        (try? JSONDecoder().decode([HardwareDevice].self, from: call { lc_devices() })) ?? []
    }

    private struct Ok: Decodable { var ok: Bool; var error: String? }

    static func presets() -> [String] {
        struct R: Decodable { var names: [String] }
        return (try? JSONDecoder().decode(R.self, from: call { lc_presets() }))?.names ?? []
    }

    /// nil when stored, else why not.
    static func storePreset(name: String, device: EDevice) -> String? {
        guard let json = try? String(data: JSONEncoder().encode(device), encoding: .utf8) else { return "cannot encode the mixer" }
        let data = name.withCString { n in json.withCString { j in call { lc_preset_store(n, j) } } }
        guard let r = try? JSONDecoder().decode(Ok.self, from: data) else { return "the core returned no answer" }
        return r.ok ? nil : (r.error ?? "unknown error")
    }

    static func loadPreset(name: String) -> Result<EDevice, CoreError> {
        struct R: Decodable { var ok: Bool; var device: EDevice?; var error: String? }
        let data = name.withCString { n in call { lc_preset_load(n) } }
        guard let r = try? JSONDecoder().decode(R.self, from: data) else { return .failure(CoreError(message: "the core returned no answer")) }
        if r.ok, let d = r.device { return .success(d) }
        return .failure(CoreError(message: r.error ?? "unknown error"))
    }

    static func deletePreset(name: String) -> String? {
        let data = name.withCString { n in call { lc_preset_delete(n) } }
        guard let r = try? JSONDecoder().decode(Ok.self, from: data) else { return "the core returned no answer" }
        return r.ok ? nil : (r.error ?? "unknown error")
    }

    /// Writes a profile for the device to edit; its path, or why not.
    static func saveProfile(uid: String) -> Result<String, CoreError> {
        struct R: Decodable { var ok: Bool; var path: String?; var error: String? }
        let data = uid.withCString { u in call { lc_save_profile(u) } }
        guard let r = try? JSONDecoder().decode(R.self, from: data) else { return .failure(CoreError(message: "the core returned no answer")) }
        if r.ok, let p = r.path { return .success(p) }
        return .failure(CoreError(message: r.error ?? "unknown error"))
    }

    /// Profiles that were skipped, with why.
    static func profileErrors() -> [String] {
        struct R: Decodable { var errors: [String] }
        return (try? JSONDecoder().decode(R.self, from: call { lc_profiles_status() }))?.errors ?? []
    }

    private struct Volume: Decodable {
        var db: Double?
        var muted: Bool?
        var error: String?
    }

    /// The device's own volume (macOS slider) in dB, -inf when at the bottom;
    /// nil when it has none.
    static func deviceVolume(uid: String) -> Double? {
        let data = uid.withCString { u in call { lc_device_volume(u) } }
        guard let v = try? JSONDecoder().decode(Volume.self, from: data), v.error == nil else { return nil }
        return v.db ?? -.infinity
    }

    struct Levels: Decodable {
        var generation: UInt32
        var inputs: [[Float]]
        var busses: [Float]
        var outputs: [[Float]]
    }

    /// Peaks since the last call (linear), or nil when the daemon isn't routing the device.
    static func levels(deviceID: String) -> Levels? {
        let data = deviceID.withCString { d in call { lc_levels(d) } }
        return try? JSONDecoder().decode(Levels.self, from: data)
    }

    /// nil on success, else why not.
    static func setDeviceRate(uid: String, rate: Double) -> String? {
        switch uid.withCString({ lc_set_device_rate($0, rate) }) {
        case 0: return nil
        case -2: return "the device doesn’t offer \(Int(rate)) Hz"
        default: return "Core Audio refused the change"
        }
    }

    /// Asks the daemon to fade out and in around the next change.
    static func requestTransition(deviceID: String) {
        _ = deviceID.withCString { lc_request_transition($0) }
    }

    static func setDeviceVolume(uid: String, db: Double) {
        _ = uid.withCString { lc_set_device_volume($0, db == -.infinity ? -1000 : db) }
    }
}

/// The console fader law shared with the plug-in: unity at 75 % of travel,
/// +12 dB at the top, 40·log10 below unity, silence at the bottom.
enum FaderLaw {
    static func scalar(_ db: Double) -> Double {
        if db == -.infinity || db <= -96 { return 0 }
        if db >= 0 { return 0.75 + min(db, 12) / 12 * 0.25 }
        return 0.75 * pow(10, db / 40)
    }

    static func db(_ s: Double) -> Double {
        if s <= 0.003 { return -.infinity }
        if s >= 0.75 { return (s - 0.75) / 0.25 * 12 }
        return 40 * log10(s / 0.75)
    }

    static func label(_ db: Double?) -> String {
        guard let db, db != -.infinity else { return "−∞" }
        let v = (db * 10).rounded() / 10
        return (v > 0 ? "+" : v < 0 ? "−" : "") + String(format: "%.1f", abs(v))
    }
}
