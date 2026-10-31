// The front of the rack: one unit per input, the mixer and its bus groups,
// one unit per output. Knobs and buttons edit the config; meters stay dark
// until the daemon reports levels.

import SwiftUI

/// The rack's frame: a rail under each side's ears, with a hole behind every
/// screw, and the same pattern on down where no unit is mounted yet.
struct RackFrame: View {
    var layout: RackLayout
    var body: some View {
        let height = layout.height
        ZStack(alignment: .topLeading) {
            Rectangle().fill(LinearGradient(colors: [Color(hex: 0x111315), Color(hex: 0x0b0c0e)], startPoint: .top, endPoint: .bottom))
            Rail(left: true, height: height, holes: layout.screwRows).offset(x: Rack.unitX - Rack.railOverhang)
            Rail(left: false, height: height, holes: layout.screwRows).offset(x: Rack.unitX + Rack.unitWidth - Rack.earWidth)
        }
        .frame(width: Rack.width, height: height)
    }
}

struct Rail: View {
    var left: Bool
    var height: CGFloat
    var holes: [CGFloat]
    var body: some View {
        let w = Rack.earWidth + Rack.railOverhang
        Canvas { ctx, size in
            ctx.fill(Path(CGRect(origin: .zero, size: size)), with: .linearGradient(
                Gradient(colors: [Color(hex: 0x44484e), Color(hex: 0x2f3237), Color(hex: 0x3d4146)]),
                startPoint: .zero, endPoint: CGPoint(x: size.width, y: 0)))
            ctx.fill(Path(CGRect(x: 0, y: 0, width: 1, height: size.height)), with: .color(.white.opacity(0.12)))
            ctx.fill(Path(CGRect(x: size.width - 1, y: 0, width: 1, height: size.height)), with: .color(.black.opacity(0.6)))
            // Hole centres line up with the screws, which sit 14 pt in from the unit's edge.
            let cx = left ? Rack.railOverhang + Rack.screwInset : Rack.earWidth - Rack.screwInset
            for y in holes {
                let r = CGRect(x: cx - 5, y: y - 4.5, width: 10, height: 9)
                ctx.fill(Path(roundedRect: r, cornerRadius: 1.5), with: .color(.black.opacity(0.9)))
                ctx.fill(Path(CGRect(x: r.minX, y: r.maxY, width: r.width, height: 1)), with: .color(.white.opacity(0.08)))
            }
        }
        .frame(width: w, height: height)
    }
}

/// A rack unit with its ears and screws; content goes on the face. A
/// half-width unit has an ear on its outer side only.
struct Unit<Content: View>: View {
    var height: CGFloat = Rack.unitHeight
    var half: Half? = nil
    var dark = false
    var back = false
    var stripe: Color?
    @ViewBuilder var content: () -> Content
    var body: some View {
        let width = half == nil ? Rack.unitWidth : Rack.halfWidth
        let leftEar = half != .right, rightEar = half != .left
        ZStack(alignment: .topLeading) {
            BrushedPanel(dark: dark, back: back).frame(width: width, height: height)
            // The seam between two halves of a row: a dark edge, then a lit one.
            if half == .left {
                Rectangle().fill(Color.black.opacity(0.75)).frame(width: 2, height: height).offset(x: width - 2)
            } else if half == .right {
                Rectangle().fill(Color.white.opacity(0.07)).frame(width: 1, height: height)
            }
            if let stripe {
                Rectangle().fill(stripe).frame(width: 4).padding(.vertical, 6).offset(x: leftEar ? 26 : 8).shadow(color: stripe.opacity(0.6), radius: 3)
            }
            if leftEar {
                VStack { Screw(); Spacer(); Screw() }.padding(.vertical, Rack.screwInset - 6).offset(x: Rack.screwInset - 6)
            }
            if rightEar {
                VStack { Screw(); Spacer(); Screw() }.padding(.vertical, Rack.screwInset - 6).offset(x: width - Rack.screwInset - 6)
            }
            content().padding(.leading, leftEar ? 40 : 20).padding(.trailing, rightEar ? 36 : 12)
        }
        // Pinned to the left edge: a face too full spills right, never over the rail.
        .frame(width: width, height: height, alignment: .topLeading)
    }
}

/// Closes the empty half of a row.
struct BlankPlate: View {
    var half: Half
    var body: some View {
        Unit(half: half, dark: true) {
            HStack(spacing: 6) {
                ForEach(0..<14, id: \.self) { _ in Capsule().fill(Color.black.opacity(0.35)).frame(width: 4, height: 34) }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

extension Spot {
    /// The unit's left edge in the rack.
    var x: CGFloat { Rack.unitX + (half == .right ? Rack.halfWidth : 0) }
}

struct FrontView: View {
    @EnvironmentObject var model: RackModel
    var device: EDevice

    var body: some View {
        let l = model.layout(device)
        ZStack(alignment: .topLeading) {
            RackFrame(layout: l)
            PresetUnit(hidden: l.hidden).offset(x: Rack.unitX, y: l.presetTop)
            ForEach(l.inputs, id: \.self) { i in
                let spot = l.inputSpots[i] ?? Spot(top: 0)
                InputUnit(index: i, port: device.inputs[i], device: device, half: spot.half)
                    .offset(x: spot.x, y: spot.top)
            }
            ForEach(Array(l.busUnits.enumerated()), id: \.offset) { _, u in
                BusUnit(group: u.group, device: device).offset(x: Rack.unitX, y: u.top)
            }
            ForEach(l.outputs, id: \.self) { i in
                let spot = l.outputSpots[i] ?? Spot(top: 0)
                OutputUnit(index: i, port: device.outputs[i], device: device, half: spot.half)
                    .offset(x: spot.x, y: spot.top)
            }
            ForEach(Array(l.blanks.enumerated()), id: \.offset) { _, b in
                BlankPlate(half: b.half ?? .right).offset(x: b.x, y: b.top)
            }
        }
        .frame(width: Rack.width, height: l.height, alignment: .topLeading)
    }
}

// MARK: - Shared pieces

/// "MAIN → BUS 9/10" for a device input, "BUS 3/4 → SAMPLER" for a device
/// output: the device side by what it carries, when its profile says.
@MainActor func deviceRoute(_ model: RackModel, _ p: EPort, input: Bool, spans: [(ch: UInt32, width: UInt32)], in d: EDevice) -> String? {
    guard !p.wires.isEmpty else { return nil }
    let wired = Set(p.wires.map { input ? $0[0] : $0[1] })
    let used = spans.filter { s in (s.ch..<s.ch + s.width).contains { wired.contains($0) } }
    let names = used.compactMap { model.spanName(p, input: input, ch: $0.ch, width: $0.width) }
    guard names.count == used.count else { return nil }
    let side = names.prefix(2).joined(separator: ", ").uppercased() + (names.count > 2 ? " +\(names.count - 2)" : "")
    let bus = busList(p.wires.map { input ? $0[1] : $0[0] }, in: d, shown: 2)
    return input ? "\(side) → \(bus)" : "\(bus) → \(side)"
}

/// "BUS 1/2 3/4", or the busses' names where they have them: "MUSIC, 5/6".
func busList(_ chans: [UInt32], in d: EDevice, shown: Int) -> String {
    let ks = Array(Set(chans.compactMap { d.bus(ofChannel: $0) })).sorted()
    let more = ks.count > shown ? " +\(ks.count - shown)" : ""
    let spans = d.busSpans
    let place = { (k: Int) in spans[k].width == 1 ? "\(spans[k].first)" : "\(spans[k].first)/\(spans[k].first + 1)" }
    if ks.contains(where: { !d.bus_list[$0].name.isEmpty }) {
        return ks.prefix(shown).map { d.bus_list[$0].name.isEmpty ? place($0) : d.bus_list[$0].name.uppercased() }.joined(separator: ", ") + more
    }
    return "BUS " + ks.prefix(shown).map(place).joined(separator: " ") + more
}

func busRoute(_ chans: [UInt32], arrow: String, in d: EDevice) -> String {
    chans.isEmpty ? "NOT PATCHED" : "\(arrow) " + busList(chans, in: d, shown: d.bus_list.contains { !$0.name.isEmpty } ? 2 : 3)
}

/// The route display, or why the route isn't running: a device at another
/// sample rate is left out, with a button to match it.
struct RouteLCD: View {
    @EnvironmentObject var model: RackModel
    var port: EPort
    var input: Bool
    var text: String

    var body: some View {
        if let rate = model.rateMismatch(port), let d = model.device {
            HStack(spacing: 6) {
                LCD(text: "\(kHz(rate)) · SKIPPED", width: 136, error: true, size: 11)
                    .help("\(port.name ?? "This device") runs at \(Int(rate)) Hz and the mixer at \(d.sample_rate) Hz, so it is left out. LoopCraft never changes a device’s rate by itself.")
                LitButton(title: "MATCH", on: false, color: .orange, width: 56, height: 22) { model.matchRate(port) }
                    .help("Set \(port.name ?? "the device") to \(d.sample_rate) Hz")
            }
            .frame(width: 200)
        } else {
            LCD(text: text, width: 200)
        }
    }
}

func kHz(_ r: Double) -> String {
    let k = r / 1000
    return k == k.rounded() ? "\(Int(k)) kHz" : String(format: "%.1f kHz", k)
}

struct Nameplate: View {
    var identity: Identity
    var name: String
    var kind: String
    var width: CGFloat = 300
    /// Set for units that can be renamed: which one, for `RackModel.renaming`.
    var renameKey: String? = nil
    /// Shown in place of the kind, as on a display (half-width units).
    var route: String? = nil
    var body: some View {
        HStack(spacing: 12) {
            Tile(identity: identity)
            VStack(alignment: .leading, spacing: 3) {
                UnitName(name: name, renameKey: renameKey)
                if let route {
                    Text(route).font(.system(size: 10, weight: .medium, design: .monospaced)).foregroundStyle(Theme.lcdText).lineLimit(1)
                        .help(kind)
                } else {
                    HStack(spacing: 6) {
                        if let brand = identity.brand {
                            Text(brand.uppercased()).font(.system(size: 11, weight: .heavy).width(.condensed)).tracking(1.2).foregroundStyle(identity.brandColor)
                        }
                        Text(kind.uppercased()).font(Theme.kind).tracking(1.2).foregroundStyle(Theme.dim).lineLimit(1)
                    }
                }
            }
        }
        .frame(width: width, alignment: .leading)
    }
}

/// A unit's name, edited in place when it can be renamed: double-click it,
/// or Rename… in the unit's menu; Enter keeps the new name, Esc doesn't.
struct UnitName: View {
    @EnvironmentObject var model: RackModel
    var name: String
    var renameKey: String?
    var font: Font = Theme.name
    @State private var draft = ""
    @FocusState private var focused: Bool

    var body: some View {
        if let key = renameKey, model.renaming == key {
            TextField("", text: $draft)
                .textFieldStyle(.plain)
                .font(font)
                .foregroundStyle(Theme.lcdText)
                .padding(.horizontal, 4)
                .background(RoundedRectangle(cornerRadius: 3).fill(Color(hex: 0x0a1510)))
                .focused($focused)
                .onAppear { draft = name; focused = true }
                .onSubmit { model.commitRename(key, draft) }
                .onExitCommand { model.renaming = nil }
                .onChange(of: focused) { _, f in if !f && model.renaming == key { model.commitRename(key, draft) } }
        } else {
            Text(name).font(font).foregroundStyle(Theme.text).lineLimit(1).minimumScaleFactor(0.75)
                .shadow(color: .black.opacity(0.6), radius: 0, y: 1)
                .onTapGesture(count: 2) { if let key = renameKey { model.renaming = key } }
                .help(renameKey == nil ? "" : "Double-click to rename")
        }
    }
}

/// Up to two channels: a horizontal ladder each, marked L/R for a stereo
/// pair or by channel when the pair is set to mono. More channels: a
/// vertical bar each, side by side in stereo pairs.
struct Meters: View {
    /// The unit's jacks: stereo pairs and mono channels.
    var spans: [(ch: UInt32, width: UInt32)]
    var key: MeterKey
    /// What a jack carries, for the labels (nil = its channel numbers).
    var names: (UInt32, UInt32) -> String? = { _, _ in nil }
    /// Smaller LEDs, for half-width units.
    var compact = false

    private var channels: UInt32 { spans.last.map { $0.ch + $0.width - 1 } ?? 0 }

    var body: some View {
        if channels <= 2 { ladders } else { bars }
    }

    private var ladders: some View {
        let length = MeterNSView.length(segments: MeterStyle.ladder, vertical: false, compact: compact)
        let stereo = spans.count == 1 && channels == 2
        // The LEDs sit on the unit's centre line, level with the display;
        // their scale hangs below without moving them.
        return VStack(alignment: .leading, spacing: 4) {
            ForEach(0..<Int(max(1, channels)), id: \.self) { c in
                HStack(spacing: 4) {
                    Text(channels == 1 ? "" : stereo ? (c == 0 ? "L" : "R") : "\(c + 1)")
                        .font(.system(size: 8, weight: .bold).width(.condensed)).foregroundStyle(Theme.dim)
                        .frame(width: 8, alignment: .trailing)
                    LiveMeter(key: key, channel: c, segments: MeterStyle.ladder, compact: compact)
                }
                .help(stereo ? "" : names(UInt32(c + 1), 1) ?? "")
            }
        }
        .overlay(alignment: .topLeading) {
            ZStack(alignment: .topLeading) {
                ForEach(Array(MeterStyle.marks.enumerated()), id: \.offset) { k, db in
                    Text(db == 0 ? "0" : "−\(Int(-db))").font(.system(size: 8, weight: .medium).width(.condensed)).foregroundStyle(Theme.dim)
                        .fixedSize()
                        .position(x: length * CGFloat(k) / CGFloat(MeterStyle.marks.count - 1), y: 5)
                }
            }
            .frame(width: length, height: 10)
            .offset(x: 12, y: channels <= 1 ? 9 : 19)
        }
        .frame(width: compact ? 124 : 216, alignment: .leading)
    }

    private var bars: some View {
        HStack(spacing: spans.count > 8 ? 4 : 7) {
            ForEach(Array(spans.enumerated()), id: \.offset) { _, s in
                HStack(spacing: 2) {
                    ForEach(0..<Int(s.width), id: \.self) { c in
                        LiveMeter(key: key, channel: Int(s.ch) - 1 + c, segments: MeterStyle.bar, vertical: true)
                    }
                }
                .overlay(alignment: .bottom) {
                    Text(s.width == 2 ? "\(s.ch)/\(s.ch + 1)" : "\(s.ch)").font(.system(size: 8).width(.condensed)).foregroundStyle(Theme.dim)
                        .fixedSize().offset(y: 12)
                }
                .help(names(s.ch, s.width) ?? "")
            }
        }
        .frame(width: 216, alignment: .leading)
    }
}

struct GainControl: View {
    var db: Double?
    var label: String
    var size: CGFloat = 46
    var onChange: (Double?) -> Void
    var body: some View {
        VStack(spacing: 4) {
            Knob(db: db, label: label, size: size, onChange: onChange)
            Text(FaderLaw.label(db) + " dB").font(.system(size: 10, weight: .semibold, design: .monospaced)).foregroundStyle(Theme.lcdText)
        }
        .frame(width: size + 24)
        // The tick marks reach above the knob; this centres knob and ticks on the unit.
        .offset(y: 5)
    }
}

// MARK: - Units

struct InputUnit: View {
    @EnvironmentObject var model: RackModel
    var index: Int
    var port: EPort
    var device: EDevice
    var half: Half?

    var identity: Identity {
        switch port.kind {
        case "app": return Library.app(port.name ?? "")
        case "virtual_device": return Library.virtualDevice(icon: port.icon)
        case "virtual": return Library.system
        default: return Library.hardware(model.hardware(named: port.name), name: port.name ?? "")
        }
    }

    var title: String {
        switch port.kind {
        case "virtual": return "System"
        default: return port.name ?? "?"
        }
    }

    var kind: String {
        let ch = model.channels(ofInput: port, in: device)
        switch port.kind {
        case "virtual": return "virtual input · \(ch) ch"
        case "virtual_device": return "virtual input device · \(ch) ch"
        case "app": return port.tap ? "app · tap" : "app"
        default: return "input device · \(ch) ch" + (model.hardware(named: port.name)?.profile != nil ? " · profile" : "")
        }
    }

    var body: some View {
        let spans = model.spans(ofInput: index, in: device)
        let route = deviceRoute(model, port, input: true, spans: spans, in: device) ?? busRoute(port.wires.map { $0[1] }, arrow: "→", in: device)
        let rename = port.kind == "virtual_device" ? "in:\(index)" : nil
        // Signal flow: the gain, the level after it, where it goes.
        Unit(half: half, stripe: Color(hex: Library.palette[index % Library.palette.count])) {
            if half == nil {
                HStack(spacing: 14) {
                    Nameplate(identity: identity, name: title, kind: kind, renameKey: rename)
                    gain(size: 46)
                    Meters(spans: spans, key: .input(index)) { model.spanName(port, input: true, ch: $0, width: $1) }
                    RouteLCD(port: port, input: true, text: route)
                    Spacer(minLength: 0)
                    if port.kind == "app" { AppSourceButton(index: index, port: port, device: device) }
                    LitButton(title: "MUTE", on: port.muted, color: Color(hex: 0xe5483b)) { model.edit { $0.inputs[index].muted.toggle() } }
                    PowerLED(on: port.enabled) { model.edit { $0.inputs[index].enabled.toggle() } }
                }
                .frame(maxHeight: .infinity)
            } else {
                HStack(spacing: 8) {
                    Nameplate(identity: identity, name: title, kind: kind, width: 156, renameKey: rename, route: model.rateMismatch(port) != nil ? "RATE · SKIPPED" : route)
                    gain(size: 38)
                    Meters(spans: spans, key: .input(index), names: { model.spanName(port, input: true, ch: $0, width: $1) }, compact: true)
                    Spacer(minLength: 0)
                    if port.kind == "app" { AppSourceButton(index: index, port: port, device: device, compact: true) }
                    LitButton(title: "MUTE", on: port.muted, color: Color(hex: 0xe5483b), width: 44) { model.edit { $0.inputs[index].muted.toggle() } }
                    PowerLED(on: port.enabled) { model.edit { $0.inputs[index].enabled.toggle() } }
                }
                .frame(maxHeight: .infinity)
            }
        }
        .contextMenu { UnitMenu(isInput: true, index: index, enabled: port.enabled, port: port, canHalve: model.channels(ofInput: port, in: device) <= 2) }
    }

    func gain(size: CGFloat) -> some View {
        GainControl(db: port.gain_db, label: "\(title) gain", size: size) { v in model.edit(continuous: true) { $0.inputs[index].gain_db = v } }
    }
}

struct OutputUnit: View {
    @EnvironmentObject var model: RackModel
    var index: Int
    var port: EPort
    var device: EDevice
    var half: Half?

    var identity: Identity {
        port.kind == "virtual_device" ? Library.virtualDevice(icon: port.icon) : Library.hardware(model.hardware(named: port.name), name: port.name ?? "")
    }

    var kind: String {
        let ch = model.channels(ofOutput: port)
        if port.kind == "virtual_device" { return "virtual output device · \(ch) ch" }
        guard let hw = model.hardware(named: port.name) else { return "output device · not connected" }
        return "output device · \(ch) ch" + (hw.profile != nil ? " · profile" : "")
    }

    var body: some View {
        let spans = model.spans(ofOutput: index, in: device)
        let route = deviceRoute(model, port, input: false, spans: spans, in: device) ?? busRoute(port.wires.map { $0[0] }, arrow: "←", in: device)
        let rename = port.kind == "virtual_device" ? "out:\(index)" : nil
        // Same columns as the inputs: the gain, the level after it, what feeds it.
        Unit(half: half, dark: true) {
            if half == nil {
                HStack(spacing: 14) {
                    Nameplate(identity: identity, name: port.name ?? "?", kind: kind, renameKey: rename)
                    gain(size: 46)
                    Meters(spans: spans, key: .output(index)) { model.spanName(port, input: false, ch: $0, width: $1) }
                    RouteLCD(port: port, input: false, text: route)
                    Spacer(minLength: 0)
                    if port.kind == "device" { master(width: 64) }
                    LitButton(title: "MUTE", on: port.muted, color: Color(hex: 0xe5483b)) { model.edit { $0.outputs[index].muted.toggle() } }
                    PowerLED(on: port.enabled) { model.edit { $0.outputs[index].enabled.toggle() } }
                }
                .frame(maxHeight: .infinity)
            } else {
                HStack(spacing: 8) {
                    Nameplate(identity: identity, name: port.name ?? "?", kind: kind, width: 156, renameKey: rename, route: model.rateMismatch(port) != nil ? "RATE · SKIPPED" : route)
                    gain(size: 38)
                    Meters(spans: spans, key: .output(index), names: { model.spanName(port, input: false, ch: $0, width: $1) }, compact: true)
                    Spacer(minLength: 0)
                    if port.kind == "device" { master(width: 52) }
                    LitButton(title: "MUTE", on: port.muted, color: Color(hex: 0xe5483b), width: 44) { model.edit { $0.outputs[index].muted.toggle() } }
                    PowerLED(on: port.enabled) { model.edit { $0.outputs[index].enabled.toggle() } }
                }
                .frame(maxHeight: .infinity)
            }
        }
        .contextMenu { UnitMenu(isInput: false, index: index, enabled: port.enabled, port: port, canHalve: model.channels(ofOutput: port) <= 2) }
    }

    func gain(size: CGFloat) -> some View {
        GainControl(db: port.gain_db, label: "\(port.name ?? "output") gain", size: size) { v in model.edit(continuous: true) { $0.outputs[index].gain_db = v } }
    }

    /// Linked to LINK MASTER on the preset unit (the Mac's volume).
    func master(width: CGFloat) -> some View {
        LitButton(title: "MASTER", on: port.follow_volume, color: Color(hex: 0xf2a93b), width: width, height: 22) {
            model.edit { $0.outputs[index].follow_volume.toggle() }
        }
        .help(port.follow_volume
            ? "Linked to LINK MASTER (the Mac’s volume keys and menu bar). Click to unlink."
            : "Link to LINK MASTER (the Mac’s volume keys and menu bar); for devices without a volume knob of their own")
    }
}

/// What `mute_original` means for an app input. Captured in the plug-in (the
/// usual case) the app plays into this mixer, so the choice is whether it is
/// also heard in the System input: IN SYSTEM, lit when it is. Captured with a
/// tap the app plays elsewhere, and MUTE ORIG silences it there.
struct AppSourceButton: View {
    @EnvironmentObject var model: RackModel
    var index: Int
    var port: EPort
    var device: EDevice
    var compact = false

    var body: some View {
        if port.tap {
            LitButton(title: compact ? "M.ORIG" : "MUTE ORIG", on: port.mute_original, color: Color(hex: 0xff9a3c), width: compact ? 46 : 74, height: 22) {
                model.edit { $0.inputs[index].mute_original.toggle() }
            }
            .help("Silence \(port.name ?? "the app") on the device it plays to, while LoopCraft taps it")
        } else {
            let system = device.inputs.first { $0.kind == "virtual" }
            let systemOn = system?.enabled == true && system?.muted == false && !(system?.wires.isEmpty ?? true)
            LitButton(title: compact ? "SYS" : "IN SYSTEM", on: !port.mute_original && systemOn, color: Color(hex: 0x62a8ff), width: compact ? 36 : 74, height: 22) {
                model.edit { $0.inputs[index].mute_original.toggle() }
            }
            .opacity(systemOn ? 1 : 0.45)
            .help(systemOn
                ? (port.mute_original
                    ? "\(port.name ?? "The app") is heard only here. Click to hear it in the System input too."
                    : "\(port.name ?? "The app") is also heard in the System input. Click to hear it only here.")
                : "No effect now: the System input is off, so \(port.name ?? "the app") is heard only here.")
        }
    }
}

struct PowerLED: View {
    var on: Bool
    var action: () -> Void
    var body: some View {
        Button(action: action) {
            VStack(spacing: 5) {
                StatusLED(color: Color(hex: 0x3ddc84), on: on)
                Text(on ? "ON" : "OFF").font(.system(size: 9, weight: .bold).width(.condensed)).tracking(1.2).foregroundStyle(Theme.dim)
            }
            .frame(width: 30, height: 40)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(on ? "Enabled — click to disable" : "Disabled — click to enable")
    }
}

struct UnitMenu: View {
    @EnvironmentObject var model: RackModel
    var isInput: Bool
    var index: Int
    var enabled: Bool
    var port: EPort
    /// Stereo or mono: it can be shown half-width.
    var canHalve: Bool
    var body: some View {
        if port.kind == "virtual_device" {
            Button("Rename…") { model.renaming = (isInput ? "in:" : "out:") + "\(index)" }
        }
        Button(enabled ? "Disable" : "Enable") {
            model.edit { d in if isInput { d.inputs[index].enabled.toggle() } else { d.outputs[index].enabled.toggle() } }
        }
        if canHalve || port.half {
            Button(port.half ? "Full Width" : "Half Width") {
                model.edit { d in if isInput { d.inputs[index].half.toggle() } else { d.outputs[index].half.toggle() } }
            }
        }
        Button("Move Up") { model.move(isInput: isInput, index, by: -1) }
        Button("Move Down") { model.move(isInput: isInput, index, by: 1) }
        Divider()
        Button("Unplug All") {
            model.edit { d in if isInput { d.inputs[index].wires = [] } else { d.outputs[index].wires = [] } }
        }
        if port.kind == "device" {
            Divider()
            Button(model.hardware(named: port.name)?.profile?.hasPrefix("built-in:") == false ? "Edit Device Profile…" : "Save Device Profile…") {
                model.editProfile(of: port)
            }
        }
        Divider()
        Button("Remove", role: .destructive) { model.remove(isInput: isInput, index) }
    }
}

// MARK: - Mixer and bus groups

/// The mixer (`group` nil: its name, rate and buffer, and the busses in no
/// group) or one bus group, with a strip per bus.
struct BusUnit: View {
    @EnvironmentObject var model: RackModel
    var group: String?
    var device: EDevice

    private var info: EGroup? { device.groups.first { $0.id == group } }

    var body: some View {
        let busses = device.busses(in: group)
        Unit(height: Rack.mixerHeight, dark: true) {
            HStack(alignment: .top, spacing: 22) {
                Group {
                    if let g = info { groupHeader(g, count: busses.count) } else { mixerHeader }
                }
                .padding(.top, 14)

                HStack(alignment: .top, spacing: 10) {
                    ForEach(busses, id: \.self) { k in BusStrip(bus: k, device: device) }
                    if busses.isEmpty {
                        Text(group == nil ? "Every bus is in a group." : "No busses yet: right-click to add one.")
                            .font(.system(size: 11)).foregroundStyle(Theme.dim).padding(.top, 70)
                    }
                }
                .padding(.top, 12)

                Spacer(minLength: 0)
            }
        }
        .contextMenu {
            Button(info == nil ? "Add Bus" : "Add Bus to \(info!.name)") { model.addBus(to: group) }
            Button("Add Bus Group") { model.addGroup() }
            if let g = info {
                Divider()
                Button("Rename Group…") { model.renaming = "group:" + g.id }
                Button("Move Group Up") { model.moveGroup(g.id, by: -1) }
                Button("Move Group Down") { model.moveGroup(g.id, by: 1) }
                Divider()
                Button("Remove Group (busses go to the mixer)", role: .destructive) { model.removeGroup(g.id) }
            }
        }
    }

    private var mixerHeader: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Image(systemName: "point.3.connected.trianglepath.dotted").font(.system(size: 22)).foregroundStyle(Color(hex: 0x62a8ff))
                VStack(alignment: .leading, spacing: 2) {
                    UnitName(name: device.name, renameKey: "mixer")
                    Text("LOOPCRAFT MIXER").font(.system(size: 11, weight: .heavy).width(.condensed)).tracking(1.6).foregroundStyle(Color(hex: 0x62a8ff))
                }
            }
            HStack(spacing: 8) {
                LCD(text: "\(device.sample_rate / 1000).\(device.sample_rate % 1000 / 100) kHz")
                LCD(text: "\(device.buffer_frames.map(String.init) ?? "auto") fr")
                LCD(text: "\(device.busses) CH")
                    .help("\(device.bus_list.count) busses on \(device.busses) channels")
            }
            if let f = device.buffer_frames {
                Text(String(format: "buffer %.2f ms", Double(f) / Double(device.sample_rate) * 1000))
                    .font(.system(size: 10, design: .monospaced)).foregroundStyle(Theme.dim)
            }
        }
        .frame(width: 300, alignment: .leading)
    }

    private func groupHeader(_ g: EGroup, count: Int) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Image(systemName: "rectangle.3.group").font(.system(size: 20)).foregroundStyle(Color(hex: 0x62a8ff))
                VStack(alignment: .leading, spacing: 2) {
                    UnitName(name: g.name, renameKey: "group:" + g.id)
                    Text("BUS GROUP").font(.system(size: 11, weight: .heavy).width(.condensed)).tracking(1.6).foregroundStyle(Color(hex: 0x62a8ff))
                }
            }
            LCD(text: "\(count) BUS" + (count == 1 ? "" : "SES"))
            Text("right-click for busses and groups").font(.system(size: 9).width(.condensed)).foregroundStyle(Theme.dim)
        }
        .frame(width: 300, alignment: .leading)
    }
}

struct BusStrip: View {
    @EnvironmentObject var model: RackModel
    var bus: Int
    var device: EDevice
    @State private var editing = false
    @State private var draft = ""
    @FocusState private var focused: Bool

    private var mono: Bool { device.bus_list[bus].mono }

    var body: some View {
        let first = device.busSpans[bus].first
        VStack(spacing: 0) {
            VStack(spacing: 1) {
                if editing {
                    TextField("", text: $draft)
                        .textFieldStyle(.plain)
                        .font(.system(size: 11, weight: .bold).width(.condensed))
                        .multilineTextAlignment(.center)
                        .foregroundStyle(Theme.lcdText)
                        .background(RoundedRectangle(cornerRadius: 2).fill(Color(hex: 0x0a1510)))
                        .focused($focused)
                        .onSubmit { commit() }
                        .onExitCommand { editing = false }
                        .onChange(of: focused) { _, f in if !f && editing { commit() } }
                } else {
                    Text(model.busName(bus, in: device) ?? model.busChannels(bus, in: device))
                        .font(model.busName(bus, in: device) == nil ? .system(size: 12, weight: .bold, design: .monospaced) : .system(size: 11, weight: .bold).width(.condensed))
                        .foregroundStyle(Theme.text).lineLimit(1).minimumScaleFactor(0.6)
                        .onTapGesture(count: 2) { startEditing() }
                }
                if model.busName(bus, in: device) != nil {
                    Text(model.busChannels(bus, in: device)).font(.system(size: 8, design: .monospaced)).foregroundStyle(Theme.dim)
                }
            }
            .frame(height: 26)
            Spacer(minLength: 8)
            HStack(spacing: 2) {
                LiveMeter(key: .bus, channel: Int(first) - 1, segments: MeterStyle.strip, vertical: true)
                if !mono { LiveMeter(key: .bus, channel: Int(first), segments: MeterStyle.strip, vertical: true) }
            }
            Spacer(minLength: 8)
        }
        .frame(width: mono ? 40 : 56, height: 158)
        .padding(.vertical, 8)
        .contentShape(Rectangle())
        .help(routing)
        .background(RoundedRectangle(cornerRadius: 3).fill(Color.black.opacity(0.22)))
        .overlay(RoundedRectangle(cornerRadius: 3).stroke(Color.white.opacity(0.05), lineWidth: 1))
        .contextMenu { menu }
    }

    @ViewBuilder private var menu: some View {
        let mine = device.busses(in: device.groupOf(bus))
        Button("Rename Bus…") { startEditing() }
        Divider()
        Button("Move Left") { model.moveBus(bus, by: -1) }.disabled(mine.first == bus)
        Button("Move Right") { model.moveBus(bus, by: 1) }.disabled(mine.last == bus)
        Menu("Show In") {
            Button("Mixer") { model.moveBus(bus, toGroup: nil) }.disabled(device.groupOf(bus) == nil)
            ForEach(device.groups, id: \.id) { g in
                Button(g.name) { model.moveBus(bus, toGroup: g.id) }.disabled(device.groupOf(bus) == g.id)
            }
            Divider()
            Button("New Group…") { model.addGroup(moving: bus) }
        }
        Divider()
        Button(mono ? "Make Stereo" : "Split into Two Mono Busses") { model.flip(.bus(first: bus)) }
        Button("Add Bus") { model.addBus(to: device.groupOf(bus)) }
        Button("Remove Bus", role: .destructive) { model.removeBus(bus) }.disabled(device.bus_list.count <= 1)
    }

    func startEditing() {
        draft = model.busName(bus, in: device) ?? ""
        editing = true
        focused = true
    }

    func commit() {
        editing = false
        if draft != (model.busName(bus, in: device) ?? "") { model.renameBus(bus, to: draft) }
    }

    func shortName(_ p: EPort) -> String {
        p.kind == "virtual" ? "System" : (p.name ?? "?")
    }

    /// What is patched onto this bus and where it goes, for the tooltip.
    var routing: String {
        let s = device.busSpans[bus]
        let mine = { (c: UInt32) in c >= s.first && c < s.first + s.width }
        let from = model.busSources(bus, in: device).map { shortName(device.inputs[$0]) }
        let to = device.outputs.filter { $0.wires.contains { mine($0[0]) } }.map { $0.name ?? "?" }
        let head = "\(model.busLabel(bus, in: device)) (\(model.busChannels(bus, in: device))\(mono ? ", mono" : ""))"
        return [head,
                "From: " + (from.isEmpty ? "nothing" : from.joined(separator: ", ")),
                "To: " + (to.isEmpty ? "recording apps only" : to.joined(separator: ", ") + ", and recording apps"),
                "Double-click the name to rename; right-click for more"].joined(separator: "\n")
    }
}
