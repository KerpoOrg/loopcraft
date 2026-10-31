// What a unit is, at a glance: the app's own icon from macOS, or for
// hardware the manufacturer in its brand colour and a symbol for the kind of
// device. No other company's artwork is shipped; app icons come from the apps.

import AppKit
import SwiftUI

struct Identity {
    var icon: NSImage?
    var symbol: String
    var tile: Color
    var brand: String?
    var brandColor: Color
}

extension Color {
    init(hex: UInt32) {
        self.init(red: Double((hex >> 16) & 0xff) / 255, green: Double((hex >> 8) & 0xff) / 255, blue: Double(hex & 0xff) / 255)
    }
}

enum Library {
    /// Manufacturer (as Core Audio reports it, lower case) → brand name, colour, symbol.
    private static let brands: [(match: String, name: String, color: UInt32, symbol: String)] = [
        ("rane", "Rane", 0xff5a4e, "slider.horizontal.3"),
        ("logitech", "Logitech G", 0x3cc8ff, "headphones"),
        ("apple", "Apple", 0xc9cfd6, "laptopcomputer"),
        ("focusrite", "Focusrite", 0xff6b5c, "hifispeaker"),
        ("universal audio", "Universal Audio", 0xe9d6a8, "hifispeaker"),
        ("rme", "RME", 0x7fb8ff, "hifispeaker"),
        ("motu", "MOTU", 0x9fd3ff, "hifispeaker"),
        ("native instruments", "Native Instruments", 0xe8ebee, "slider.horizontal.3"),
        ("pioneer", "Pioneer DJ", 0x7cc4ff, "slider.horizontal.3"),
        ("allen", "Allen & Heath", 0xff8f6b, "slider.horizontal.3"),
        ("behringer", "Behringer", 0xffd36b, "slider.horizontal.3"),
        ("microsoft", "Microsoft", 0x8fd0ff, "person.2.wave.2"),
        ("samsung", "Samsung", 0x8fb4ff, "display"),
        ("sony", "Sony", 0xe8ebee, "headphones"),
        ("bose", "Bose", 0xe8ebee, "headphones"),
    ]

    /// A symbol from the device's name, when the brand doesn't say.
    private static func symbol(forName name: String) -> String {
        let n = name.lowercased()
        if ["headset", "headphone", "airpods", "buds"].contains(where: n.contains) { return "headphones" }
        if ["microphone", " mic"].contains(where: n.contains) { return "mic" }
        if ["dj", "rane", "ddj", "traktor", "mixer"].contains(where: n.contains) { return "slider.horizontal.3" }
        if ["display", "monitor", "odyssey", "g9", "tv"].contains(where: n.contains) { return "display" }
        if n.contains("speaker") { return "speaker.wave.2" }
        return "hifispeaker"
    }

    private static var iconCache: [String: NSImage?] = [:]

    /// The app's own icon, by bundle ID or by name.
    static func appIcon(_ query: String) -> NSImage? {
        if let cached = iconCache[query] { return cached }
        var url: URL?
        if query.contains("."), !query.contains(" ") {
            url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: query)
        }
        if url == nil {
            let home = FileManager.default.homeDirectoryForCurrentUser.path
            for dir in ["/Applications", "/Applications/Utilities", "/System/Applications", home + "/Applications"] {
                guard let names = try? FileManager.default.contentsOfDirectory(atPath: dir) else { continue }
                if let hit = names.first(where: { $0.lowercased() == query.lowercased() + ".app" }) {
                    url = URL(fileURLWithPath: dir).appendingPathComponent(hit)
                    break
                }
            }
        }
        let icon = url.map { NSWorkspace.shared.icon(forFile: $0.path) }
        iconCache[query] = icon
        return icon
    }

    static func hardware(_ hw: HardwareDevice?, name: String) -> Identity {
        let maker = hw?.manufacturer.lowercased() ?? ""
        if let b = brands.first(where: { !maker.isEmpty && maker.contains($0.match) }) {
            return Identity(icon: nil, symbol: b.symbol == "hifispeaker" ? symbol(forName: name) : b.symbol, tile: Color(hex: b.color).opacity(0.22),
                            brand: b.name, brandColor: Color(hex: b.color))
        }
        let display = hw?.manufacturer.isEmpty == false ? hw!.manufacturer : nil
        return Identity(icon: nil, symbol: symbol(forName: name), tile: Color(hex: 0x2e3238), brand: display, brandColor: Color(hex: 0xc9cfd6))
    }

    static func app(_ name: String) -> Identity {
        Identity(icon: appIcon(name), symbol: "app", tile: Color(hex: 0x2e3238), brand: nil, brandColor: .white)
    }

    static func virtualDevice(icon: String?) -> Identity {
        if let icon, let img = appIcon(icon) {
            return Identity(icon: img, symbol: "waveform", tile: Color(hex: 0x2e3238), brand: icon, brandColor: Color(hex: 0xffc56b))
        }
        return Identity(icon: nil, symbol: "waveform", tile: Color(hex: 0x23323a), brand: icon, brandColor: Color(hex: 0xffc56b))
    }

    static let system = Identity(icon: nil, symbol: "speaker.wave.2", tile: Color(hex: 0x2e3238), brand: nil, brandColor: .white)

    /// Cable and stripe colours for inputs, in order.
    static let palette: [UInt32] = [0xe9c46a, 0x4cc983, 0x62a8ff, 0xb19cff, 0xf39256, 0x5ed6d0, 0xff7aa8, 0xc7e86b]
}
