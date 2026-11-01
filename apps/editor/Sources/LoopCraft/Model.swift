// The editor's state: the configuration being edited, the devices present,
// saving (debounced, validated by the core) and reloading hand edits.

import AppKit
import Combine
import Foundation

@MainActor
final class RackModel: ObservableObject {
    @Published var config = EConfig(devices: [])
    @Published var hardware: [HardwareDevice] = []
    @Published var deviceIndex = 0 {
        didSet {
            meters.deviceID = device?.id
            if deviceIndex != oldValue { restorePreset() }
        }
    }
    @Published var status = "loading…"
    @Published var statusIsError = false
    /// The whole error when saving failed (the status line shows its start).
    @Published var statusDetail: String?
    @Published var warning: String?
    @Published var showBack = false { didSet { meters.paused = showBack } }
    @Published var deviceVolumeDb: Double?
    /// Units nothing is patched to are shown (a view setting, not config).
    @Published var showUnpatched = UserDefaults.standard.object(forKey: "showUnpatched") as? Bool ?? true {
        didSet { UserDefaults.standard.set(showUnpatched, forKey: "showUnpatched") }
    }
    @Published var presetNames: [String] = []
    /// The preset last loaded or stored, and what it holds.
    @Published var currentPreset: String?
    @Published var presetSnapshot: EDevice?
    /// The preset the display shows while browsing.
    @Published var presetCursor: String?
    /// The unit whose name is being edited: "in:<i>", "out:<i>" or "mixer".
    @Published var renaming: String?
    /// Observed only by the meters, so level updates redraw nothing else.
    let meters = MeterModel()

    /// LOOPCRAFT_CONFIG=<path> edits another file (for trying things out).
    let path = ProcessInfo.processInfo.environment["LOOPCRAFT_CONFIG"] ?? Core.defaultPath
    private var lastWritten: Date?
    private var saveTask: Task<Void, Never>?
    private var timer: Timer?

    init() {
        showBack = CommandLine.arguments.contains("--back")
        reload()
        hardware = Core.devices()
        presetNames = Core.presets()
        restorePreset()
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tick() }
        }
    }

    var device: EDevice? {
        config.devices.indices.contains(deviceIndex) ? config.devices[deviceIndex] : nil
    }

    func reload() {
        switch Core.load(path) {
        case .success(let c):
            config = c
            deviceIndex = min(deviceIndex, max(0, c.devices.count - 1))
            meters.deviceID = device?.id
            status = "config.yaml · loaded"
            statusIsError = false
            lastWritten = modified()
        case .failure(let e):
            status = "config.yaml · " + e.message.components(separatedBy: "\n").first!
            statusIsError = true
        }
    }

    private func modified() -> Date? {
        (try? FileManager.default.attributesOfItem(atPath: path))?[.modificationDate] as? Date
    }

    /// Once a second: pick up hand edits, refresh devices and the device volume.
    private func tick() {
        if saveTask == nil, let m = modified(), m != lastWritten {
            reload()
            status = "config.yaml · reloaded (edited elsewhere)"
        }
        hardware = Core.devices()
        presetNames = Core.presets()
        if let d = device { deviceVolumeDb = Core.deviceVolume(uid: d.uid) }
        if let e = Core.profileErrors().first, warning == nil { warning = "Device profile skipped: " + e }
    }

    /// Applies an edit to the current device and saves it: at once for
    /// buttons and patching, at most every 80 ms while a knob turns.
    func edit(continuous: Bool = false, _ change: (inout EDevice) -> Void) {
        guard config.devices.indices.contains(deviceIndex) else { return }
        change(&config.devices[deviceIndex])
        status = "config.yaml · saving…"
        statusIsError = false
        if !continuous {
            saveTask?.cancel()
            saveNow()
            return
        }
        guard saveTask == nil else { return }
        saveTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 80_000_000)
            guard let self, !Task.isCancelled else { return }
            self.saveNow()
        }
    }

    func saveNow() {
        if let error = Core.save(config, to: path) {
            status = "not saved · " + error.components(separatedBy: "\n").dropFirst().joined(separator: " ").trimmingCharacters(in: CharacterSet(charactersIn: " -"))
            statusDetail = error
            statusIsError = true
        } else {
            status = "config.yaml · saved"
            statusDetail = nil
            statusIsError = false
            lastWritten = modified()
            // Take the ids the core gave units that had none, so they keep them.
            let unnamed = config.devices.contains { d in (d.inputs + d.outputs).contains { ($0.id ?? "").isEmpty } }
            if unnamed, case .success(let c) = Core.load(path) { config = c }
        }
        saveTask = nil
    }

    func setDeviceVolume(_ db: Double) {
        guard let d = device else { return }
        deviceVolumeDb = db
        Core.setDeviceVolume(uid: d.uid, db: db)
    }

    func hardware(named query: String?) -> HardwareDevice? {
        guard let q = query else { return nil }
        if let exact = hardware.first(where: { $0.uid == q || $0.name == q }) { return exact }
        let hits = hardware.filter { $0.name.lowercased().contains(q.lowercased()) }
        return hits.count == 1 ? hits[0] : nil
    }
}

// MARK: - The rack, derived from the device

/// `mixOut`: a MIX, its `unit` 0 for the mixer's, 1 + the group's index.
enum Side { case source, busIn, busOut, mixOut, output }

/// A jack: a stereo pair or a single channel of a unit, or of a bus.
struct JackID: Hashable {
    var side: Side
    /// Input or output index; for busses, the bus's index in `bus_list`.
    var unit: Int
    /// First channel (1-based): the unit's own, or for busses the mix channel.
    var ch: UInt32
    /// 2: stereo, 1: mono.
    var width: UInt32
}

/// The stereo/mono switch beside a pair of jacks on the back.
enum PairSwitch: Hashable {
    /// A unit's pair starting at `ch`.
    case unit(side: Side, unit: Int, ch: UInt32)
    /// A bus slot: a stereo bus, two mono busses side by side, or a lone mono bus.
    case bus(first: Int)
    /// A MIX: the mixer's (nil) or a group's.
    case mix(group: String?)
}

enum Half { case left, right }

/// Where a unit sits: its top, and the half of the row a half-width unit takes.
struct Spot {
    var top: CGFloat
    var half: Half?
}

/// Where the units sit (see Placement.swift): the preset unit on top, the
/// rest each in its rack unit.
struct RackLayout {
    /// Config indices of the units shown, top to bottom.
    var inputs: [Int]
    var outputs: [Int]
    /// Units left out because nothing is patched to them.
    var hidden: Int
    var spots: [UnitRef: Placed] = [:]
    var inputSpots: [Int: Spot] = [:]
    var outputSpots: [Int: Spot] = [:]
    /// The mixer (group nil), then each bus group.
    var busUnits: [(group: String?, top: CGFloat)] = []
    /// Half rows with nothing in them, closed with a blank plate.
    var blanks: [Spot] = []
    var height: CGFloat = 0

    var presetTop: CGFloat { Rack.topPad }
    func inputRow(_ i: Int) -> Int { inputs.firstIndex(of: i) ?? 0 }
    func outputRow(_ i: Int) -> Int { outputs.firstIndex(of: i) ?? 0 }
    func sourceTop(_ i: Int) -> CGFloat { inputSpots[i]?.top ?? 0 }
    func outputTop(_ i: Int) -> CGFloat { outputSpots[i]?.top ?? 0 }
    func busUnitTop(_ group: String?) -> CGFloat { busUnits.first { $0.group == group }?.top ?? busUnits.first?.top ?? 0 }

    /// Heights of the rail holes: two in every rack unit, behind a unit's
    /// screws where one is mounted.
    var screwRows: [CGFloat] {
        var holes: [CGFloat] = []
        var u = 0
        while Rack.top(u) < height {
            holes += [Rack.top(u) + Rack.screwInset, Rack.top(u) + Rack.unitHeight - Rack.screwInset].filter { $0 < height - 6 }
            u += 1
        }
        return holes
    }
}

struct Cable: Identifiable {
    var from: JackID   // source or busOut
    var to: JackID     // busIn or output
    var id: String { "\(from)-\(to)" }
}

enum Rack {
    static let width: CGFloat = 1440
    static let unitX: CGFloat = 140
    static let unitWidth: CGFloat = 1160
    static let halfWidth: CGFloat = 580
    static let unitHeight: CGFloat = 84
    static let pitch: CGFloat = 86
    static let topPad: CGFloat = 24
    /// The mixer and the bus groups take two rack units.
    static let mixerUnits = 2
    static let mixerHeight: CGFloat = unitHeight + pitch * CGFloat(mixerUnits - 1)

    /// The top of rack unit `u` (0: the preset unit's).
    static func top(_ u: Int) -> CGFloat { topPad + CGFloat(u) * pitch }
    /// How tall a unit `units` rack units high is.
    static func height(_ units: Int) -> CGFloat { unitHeight + pitch * CGFloat(units - 1) }

    /// The ears: screws sit `screwInset` in from a unit's edges, over the
    /// rails, which reach `railOverhang` past the units.
    static let earWidth: CGFloat = 28
    static let railOverhang: CGFloat = 8
    static let screwInset: CGFloat = 14
    /// The window shows the rack less this much dark floor on each side:
    /// what is left is the rails and 20 pt beside them.
    static let trim: CGFloat = unitX - railOverhang - 20
    static var shownWidth: CGFloat { width - 2 * trim }

    static let sourceX: CGFloat = 340
    static let busInX: CGFloat = 440
    static let busOutX: CGFloat = 860
    static let mixX: CGFloat = 1226
}

/// The jacks of a unit with `n` channels: stereo pairs, or one per channel
/// where a pair is set to mono, and for a last odd channel.
func unitSpans(_ n: UInt32, mono: [UInt32]) -> [(ch: UInt32, width: UInt32)] {
    var out: [(ch: UInt32, width: UInt32)] = []
    var l: UInt32 = 1
    while l <= n {
        if l == n {
            out.append((l, 1))
        } else if mono.contains(l) || mono.contains(l + 1) {
            out += [(l, 1), (l + 1, 1)]
        } else {
            out.append((l, 2))
        }
        l += 2
    }
    return out
}

/// The wires a cable between two jacks stands for, as [from, to] channels:
/// stereo to stereo side by side, mono onto both sides, stereo into mono.
func cableWires(_ from: (UInt32, UInt32), _ to: (UInt32, UInt32)) -> [[UInt32]] {
    let (f, fw) = from, (t, tw) = to
    switch (fw, tw) {
    case (2, 2): return [[f, t], [f + 1, t + 1]]
    case (1, 2): return [[f, t], [f, t + 1]]
    case (2, 1): return [[f, t], [f + 1, t]]
    default: return [[f, t]]
    }
}

extension EDevice {
    /// Each bus's first channel and how many it has, in `bus_list` order.
    var busSpans: [(first: UInt32, width: UInt32)] {
        var next: UInt32 = 1
        return bus_list.map { b in
            let w: UInt32 = b.mono ? 1 : 2
            defer { next += w }
            return (next, w)
        }
    }

    /// The bus a mix channel belongs to.
    func bus(ofChannel c: UInt32) -> Int? {
        busSpans.firstIndex { c >= $0.first && c < $0.first + $0.width }
    }

    /// The group a bus is shown in (nil: the mixer, also for a group that's gone).
    func groupOf(_ k: Int) -> String? {
        guard let g = bus_list[k].group, groups.contains(where: { $0.id == g }) else { return nil }
        return g
    }

    /// The busses a unit shows: the mixer's (nil) or a group's, in channel order.
    func busses(in group: String?) -> [Int] {
        bus_list.indices.filter { groupOf($0) == group }
    }

    /// A unit's busses as they sit on the back: a stereo bus, or two mono
    /// busses next to each other (one above the other), or a lone mono bus.
    func busSlots(in group: String?) -> [[Int]] {
        var slots: [[Int]] = []
        for k in busses(in: group) {
            if bus_list[k].mono, let last = slots.last, last.count == 1, last[0] == k - 1, bus_list[k - 1].mono {
                slots[slots.count - 1].append(k)
            } else {
                slots.append([k])
            }
        }
        return slots
    }

    func busJack(_ k: Int, _ side: Side) -> JackID {
        let s = busSpans[k]
        return JackID(side: side, unit: k, ch: s.first, width: s.width)
    }

    /// The MIX a mix jack's `unit` stands for: nil for the mixer's.
    func mixGroup(_ unit: Int) -> String? {
        unit > 0 && groups.indices.contains(unit - 1) ? groups[unit - 1].id : nil
    }

    func mixUnit(_ group: String?) -> Int {
        group.flatMap { g in groups.firstIndex { $0.id == g } }.map { $0 + 1 } ?? 0
    }

    func mixIsMono(_ group: String?) -> Bool {
        group.map { g in groups.first { $0.id == g }?.mix_mono ?? false } ?? mix_mono
    }

    func mixJack(_ group: String?) -> JackID {
        JackID(side: .mixOut, unit: mixUnit(group), ch: 1, width: mixIsMono(group) ? 1 : 2)
    }

    /// "MIX", "DAW MIX".
    func mixName(_ group: String?) -> String {
        group.flatMap { g in groups.first { $0.id == g } }.map { $0.name.uppercased() + " MIX" } ?? "MIX"
    }
}

extension RackModel {
    /// Channels of an input: what can be wired from it.
    func channels(ofInput p: EPort, in d: EDevice) -> UInt32 {
        switch p.kind {
        case "virtual": return d.virtual_inputs
        case "app": return 2
        case "virtual_device": return p.channels ?? 2
        default:
            if let hw = hardware(named: p.name), hw.inputs > 0 { return hw.inputs }
            return max(2, p.wires.map { $0[0] }.max() ?? 2)
        }
    }

    func channels(ofOutput p: EPort) -> UInt32 {
        switch p.kind {
        case "virtual_device": return p.channels ?? 2
        default:
            if let hw = hardware(named: p.name), hw.outputs > 0 { return hw.outputs }
            return max(2, (p.wires.map { $0[1] } + p.mix_wires.map(\.to)).max() ?? 2)
        }
    }

    func spans(ofInput i: Int, in d: EDevice) -> [(ch: UInt32, width: UInt32)] {
        unitSpans(channels(ofInput: d.inputs[i], in: d), mono: d.inputs[i].mono)
    }

    func spans(ofOutput i: Int, in d: EDevice) -> [(ch: UInt32, width: UInt32)] {
        unitSpans(channels(ofOutput: d.outputs[i]), mono: d.outputs[i].mono)
    }

    /// Shown half-width: asked for, and no more than two channels.
    func isHalf(input i: Int, in d: EDevice) -> Bool { d.inputs[i].half && channels(ofInput: d.inputs[i], in: d) <= 2 }
    func isHalf(output i: Int, in d: EDevice) -> Bool { d.outputs[i].half && channels(ofOutput: d.outputs[i]) <= 2 }

    /// Units switched on with nothing patched to them. Hardware ones still
    /// run in the routing (clocked and kept in step for nothing).
    func idleUnits(_ d: EDevice) -> (inputs: [Int], outputs: [Int]) {
        (d.inputs.indices.filter { d.inputs[$0].enabled && !d.inputs[$0].patched },
         d.outputs.indices.filter { d.outputs[$0].enabled && !d.outputs[$0].patched })
    }

    /// Switches those off.
    func turnOffIdle() {
        guard let d = device else { return }
        let idle = idleUnits(d)
        guard !(idle.inputs.isEmpty && idle.outputs.isEmpty) else { return }
        edit { d in
            for i in idle.inputs { d.inputs[i].enabled = false }
            for i in idle.outputs { d.outputs[i].enabled = false }
        }
    }

    /// Where a jack sits on the back panel: inputs above the bus inputs,
    /// outputs below the bus outputs, each unit a step further along so a
    /// cable doesn't run over the jacks of the units it passes. The two
    /// channels of a pair set to mono sit one above the other.
    func position(_ j: JackID, in d: EDevice) -> CGPoint {
        let l = layout(d)
        func x(_ x0: CGFloat, _ span: CGFloat, _ maxStep: CGFloat, _ n: Int, _ k: Int) -> CGFloat {
            x0 + CGFloat(k) * (n > 1 ? min(maxStep, span / CGFloat(n - 1)) : 0)
        }
        func unitJack(_ spot: Spot?, row: Int, channels n: UInt32, stagger: CGFloat, x0 full: CGFloat) -> CGPoint {
            let slots = Int((n + 1) / 2), slot = Int((j.ch - 1) / 2)
            let top = spot?.top ?? 0
            let pairOfTwo = j.width == 1 && n >= (j.ch % 2 == 1 ? j.ch + 1 : j.ch)
            let y = pairOfTwo ? top + (j.ch % 2 == 1 ? 26 : 62) : top + 46
            if let half = spot?.half {
                return CGPoint(x: Rack.unitX + (half == .right ? Rack.halfWidth : 0) + 230 + CGFloat(row % 4) * 50 + CGFloat(slot) * 72, y: y)
            }
            let x0 = min(full + CGFloat(row % 6) * stagger, 1240 - CGFloat(max(0, slots - 1)) * 60)
            return CGPoint(x: x(x0, 1240 - x0, 72, slots, slot), y: y)
        }
        switch j.side {
        case .source:
            return unitJack(l.inputSpots[j.unit], row: l.inputRow(j.unit), channels: channels(ofInput: d.inputs[j.unit], in: d), stagger: 96, x0: Rack.sourceX)
        case .output:
            return unitJack(l.outputSpots[j.unit], row: l.outputRow(j.unit), channels: channels(ofOutput: d.outputs[j.unit]), stagger: 50, x0: Rack.busOutX - 100)
        case .busIn, .busOut:
            guard d.bus_list.indices.contains(j.unit) else { return .zero }
            let group = d.groupOf(j.unit)
            let slots = d.busSlots(in: group)
            let s = slots.firstIndex { $0.contains(j.unit) } ?? 0
            let top = l.busUnitTop(group)
            let y = slots[s].count == 2 ? top + (slots[s][0] == j.unit ? 102 : 144) : top + 122
            let px = j.side == .busIn ? x(Rack.busInX, 340, 100, slots.count, s) : x(Rack.busOutX, 280, 100, slots.count, s)
            return CGPoint(x: px, y: y)
        case .mixOut:
            return CGPoint(x: Rack.mixX, y: l.busUnitTop(d.mixGroup(j.unit)) + 122)
        }
    }

    /// The stereo/mono switches on the back, with where they sit (beside
    /// the pair's jacks): one per pair of a unit, one per bus slot.
    func switches(in d: EDevice) -> [(PairSwitch, CGPoint, mono: Bool)] {
        let l = layout(d)
        var out: [(PairSwitch, CGPoint, mono: Bool)] = []
        func unit(_ side: Side, _ i: Int, _ n: UInt32, _ mono: [UInt32]) {
            var c: UInt32 = 1
            while c + 1 <= n {
                let isMono = mono.contains(c) || mono.contains(c + 1)
                let p = position(JackID(side: side, unit: i, ch: c, width: isMono ? 1 : 2), in: d)
                out.append((.unit(side: side, unit: i, ch: c), CGPoint(x: p.x + 24, y: isMono ? p.y + 18 : p.y), isMono))
                c += 2
            }
        }
        for i in l.inputs { unit(.source, i, channels(ofInput: d.inputs[i], in: d), d.inputs[i].mono) }
        for i in l.outputs { unit(.output, i, channels(ofOutput: d.outputs[i]), d.outputs[i].mono) }
        for g in l.busUnits.map(\.group) {
            let p = position(d.mixJack(g), in: d)
            out.append((.mix(group: g), CGPoint(x: p.x + 24, y: p.y), d.mixIsMono(g)))
            for slot in d.busSlots(in: g) {
                for side in [Side.busIn, .busOut] {
                    let p = position(d.busJack(slot[0], side), in: d)
                    let mono = d.bus_list[slot[0]].mono
                    out.append((.bus(first: slot[0]), CGPoint(x: p.x + 24, y: slot.count == 2 ? p.y + 21 : p.y), mono))
                }
            }
        }
        return out
    }

    func jacks(in d: EDevice) -> [JackID] {
        var all: [JackID] = []
        let l = layout(d)
        for i in l.inputs {
            all += spans(ofInput: i, in: d).map { JackID(side: .source, unit: i, ch: $0.ch, width: $0.width) }
        }
        for k in d.bus_list.indices {
            all += [d.busJack(k, .busIn), d.busJack(k, .busOut)]
        }
        all += l.busUnits.map { d.mixJack($0.group) }
        for i in l.outputs {
            all += spans(ofOutput: i, in: d).map { JackID(side: .output, unit: i, ch: $0.ch, width: $0.width) }
        }
        return all
    }

    /// Cables found in the wires; wires that aren't part of one (one side
    /// of a stereo pair onto a bus, say) are kept and drawn as custom wiring.
    func cables(in d: EDevice) -> (cables: [Cable], custom: [(JackID, JackID)]) {
        var cables: [Cable] = []
        var custom: [(JackID, JackID)] = []
        let busses = d.bus_list.indices
        func jack(_ spans: [(ch: UInt32, width: UInt32)], _ side: Side, _ i: Int, _ c: UInt32) -> JackID {
            let s = spans.first { c >= $0.ch && c < $0.ch + $0.width } ?? (c, 1)
            return JackID(side: side, unit: i, ch: s.ch, width: s.width)
        }
        for (i, p) in d.inputs.enumerated() {
            let own = spans(ofInput: i, in: d)
            var used = Set<[UInt32]>()
            for s in own {
                for k in busses {
                    let b = d.busJack(k, .busIn)
                    let ws = cableWires((s.ch, s.width), (b.ch, b.width))
                    if ws.allSatisfy(p.wires.contains) {
                        cables.append(Cable(from: JackID(side: .source, unit: i, ch: s.ch, width: s.width), to: b))
                        used.formUnion(ws)
                    }
                }
            }
            for w in p.wires where !used.contains(w) {
                guard let k = d.bus(ofChannel: w[1]) else { continue }
                custom.append((jack(own, .source, i, w[0]), d.busJack(k, .busIn)))
            }
        }
        for (i, p) in d.outputs.enumerated() {
            let own = spans(ofOutput: i, in: d)
            var used = Set<[UInt32]>()
            for k in busses {
                let b = d.busJack(k, .busOut)
                for s in own {
                    let ws = cableWires((b.ch, b.width), (s.ch, s.width))
                    if ws.allSatisfy(p.wires.contains) {
                        cables.append(Cable(from: b, to: JackID(side: .output, unit: i, ch: s.ch, width: s.width)))
                        used.formUnion(ws)
                    }
                }
            }
            for w in p.wires where !used.contains(w) {
                guard let k = d.bus(ofChannel: w[0]) else { continue }
                custom.append((d.busJack(k, .busOut), jack(own, .output, i, w[1])))
            }
            var mixUsed = Set<EMixWire>()
            for g in [nil] + d.groups.map({ $0.id as String? }) {
                let m = d.mixJack(g)
                for s in own {
                    let ws = cableWires((m.ch, m.width), (s.ch, s.width)).map { EMixWire(group: g, from: $0[0], to: $0[1]) }
                    if ws.allSatisfy(p.mix_wires.contains) {
                        cables.append(Cable(from: m, to: JackID(side: .output, unit: i, ch: s.ch, width: s.width)))
                        mixUsed.formUnion(ws)
                    }
                }
            }
            for w in p.mix_wires where !mixUsed.contains(w) {
                custom.append((d.mixJack(w.group), jack(own, .output, i, w.to)))
            }
        }
        return (cables, custom)
    }

    /// Plugs a cable between two jacks, in whichever order they were picked.
    func connect(_ a: JackID, _ b: JackID) {
        let (from, to): (JackID, JackID)
        switch (a.side, b.side) {
        case (.source, .busIn), (.busOut, .output), (.mixOut, .output): (from, to) = (a, b)
        case (.busIn, .source), (.output, .busOut), (.output, .mixOut): (from, to) = (b, a)
        default:
            warning = "Those jacks don’t connect: inputs go into bus inputs, bus outputs and MIX go to outputs."
            return
        }
        warning = nil
        let add = cableWires((from.ch, from.width), (to.ch, to.width))
        if from.side == .mixOut {
            let g = device?.mixGroup(from.unit)
            edit { dev in
                for w in add.map({ EMixWire(group: g, from: $0[0], to: $0[1]) }) where !dev.outputs[to.unit].mix_wires.contains(w) {
                    dev.outputs[to.unit].mix_wires.append(w)
                }
            }
        } else if from.side == .source {
            edit { dev in for w in add where !dev.inputs[from.unit].wires.contains(w) { dev.inputs[from.unit].wires.append(w) } }
        } else {
            edit { dev in for w in add where !dev.outputs[to.unit].wires.contains(w) { dev.outputs[to.unit].wires.append(w) } }
        }
        checkFeedback()
    }

    func disconnect(_ c: Cable) {
        let gone = cableWires((c.from.ch, c.from.width), (c.to.ch, c.to.width))
        if c.from.side == .mixOut {
            let g = device?.mixGroup(c.from.unit)
            let wires = gone.map { EMixWire(group: g, from: $0[0], to: $0[1]) }
            edit { dev in dev.outputs[c.to.unit].mix_wires.removeAll { wires.contains($0) } }
        } else if c.from.side == .source {
            edit { dev in dev.inputs[c.from.unit].wires.removeAll { gone.contains($0) } }
        } else {
            edit { dev in dev.outputs[c.to.unit].wires.removeAll { gone.contains($0) } }
        }
        warning = nil
        checkFeedback()
    }

    /// Flips a stereo/mono switch on the back. A unit's pair only changes
    /// how it is shown and patched (its wires stay, so nothing sounds
    /// different); a stereo bus becomes two mono busses on the same
    /// channels, two mono busses one stereo bus.
    func flip(_ s: PairSwitch) {
        guard let d = device else { return }
        switch s {
        case .unit(let side, let i, let ch):
            edit { dev in
                let mono = side == .source ? dev.inputs[i].mono : dev.outputs[i].mono
                let new = mono.contains(ch) || mono.contains(ch + 1) ? mono.filter { $0 != ch && $0 != ch + 1 } : (mono + [ch, ch + 1]).sorted()
                if side == .source { dev.inputs[i].mono = new } else { dev.outputs[i].mono = new }
            }
        case .bus(let k):
            guard d.bus_list.indices.contains(k) else { return }
            var list = d.bus_list
            if !list[k].mono {
                list[k].mono = true
                let name = list[k].name.isEmpty ? "" : freshBusName(list[k].name + " 2", in: list)
                list.insert(EBus(id: newID("b"), name: name, mono: true, group: list[k].group), at: k + 1)
            } else if let slot = d.busSlots(in: d.groupOf(k)).first(where: { $0.contains(k) }), slot.count == 2 {
                // The second bus's patches join the first's right side.
                list[slot[0]].mono = false
                list.remove(at: slot[1])
            } else {
                list[k].mono = false
            }
            rebus(list)
        case .mix(let g):
            let mono = !d.mixIsMono(g)
            edit { dev in
                if let g, let i = dev.groups.firstIndex(where: { $0.id == g }) { dev.groups[i].mix_mono = mono } else if g == nil { dev.mix_mono = mono }
                for o in dev.outputs.indices {
                    let mine = dev.outputs[o].mix_wires.filter { $0.group == g }.sorted { $0.to < $1.to }
                    var new: [EMixWire] = []
                    var k = 0
                    while k < mine.count {
                        let w = mine[k]
                        // Stereo into a pair: a mono MIX's cable to both sides becomes left → left, right → right.
                        if !mono, k + 1 < mine.count, mine[k + 1].to == w.to + 1 {
                            new += [EMixWire(group: g, from: 1, to: w.to), EMixWire(group: g, from: 2, to: w.to + 1)]
                            k += 2
                            continue
                        }
                        new.append(EMixWire(group: g, from: 1, to: w.to))
                        k += 1
                    }
                    dev.outputs[o].mix_wires = dev.outputs[o].mix_wires.filter { $0.group != g } + new.uniqued()
                }
            }
        }
    }

    /// Sets the busses to `list` (the same ids, reordered, resized, some
    /// gone or new): every wire moves to its bus's new channels, and wires
    /// to a bus that is gone are unplugged.
    func rebus(_ list: [EBus]) {
        guard let d = device else { return }
        let old = d.busSpans
        var newDevice = d
        newDevice.bus_list = list
        let new = newDevice.busSpans
        var map: [UInt32: UInt32] = [:]
        for (k, b) in d.bus_list.enumerated() {
            guard let j = list.firstIndex(where: { $0.id == b.id }) else { continue }
            if old[k].width == 2 && new[j].width == 2 {
                map[old[k].first] = new[j].first
                map[old[k].first + 1] = new[j].first + 1
            } else if old[k].width == 2 {
                map[old[k].first] = new[j].first
                // The right side of a stereo bus split in two lands on the new mono bus after it.
                map[old[k].first + 1] = list.indices.contains(j + 1) && !d.bus_list.contains(where: { $0.id == list[j + 1].id }) ? new[j + 1].first : new[j].first
            } else {
                map[old[k].first] = new[j].first
            }
        }
        // A mono bus joined to the one before it as its right side.
        for (k, b) in d.bus_list.enumerated() where b.mono && !list.contains(where: { $0.id == b.id }) {
            if k > 0, old[k - 1].width == 1, let j = list.firstIndex(where: { $0.id == d.bus_list[k - 1].id }), new[j].width == 2 {
                map[old[k].first] = new[j].first + 1
            }
        }
        edit { dev in
            dev.bus_list = list
            dev.busses = new.reduce(0) { $0 + $1.width }
            for i in dev.inputs.indices {
                dev.inputs[i].wires = dev.inputs[i].wires.compactMap { w in map[w[1]].map { [w[0], $0] } }.uniqued()
            }
            for i in dev.outputs.indices {
                dev.outputs[i].wires = dev.outputs[i].wires.compactMap { w in map[w[0]].map { [$0, w[1]] } }.uniqued()
            }
        }
        checkFeedback()
    }

    /// An app that plays into a virtual input and records a virtual output
    /// hears itself if a bus joins the two. Same `icon` = same app.
    func checkFeedback() {
        guard let d = device else { return }
        for (i, inp) in d.inputs.enumerated() where inp.kind == "virtual_device" {
            guard let app = inp.icon?.lowercased(), !app.isEmpty else { continue }
            let busses = Set(inp.wires.compactMap { d.bus(ofChannel: $0[1]) })
            for out in d.outputs where out.kind == "virtual_device" && out.icon?.lowercased() == app {
                let viaMix = out.mix_wires.contains { w in busses.contains { w.group == nil || d.groupOf($0) == w.group } }
                if viaMix || out.wires.contains(where: { d.bus(ofChannel: $0[0]).map(busses.contains) ?? false }) {
                    warning = "Feedback: \(inp.name ?? "input \(i + 1)") reaches \(out.name ?? "an output") — \(inp.icon!) would record its own output."
                    return
                }
            }
        }
    }

    /// The device's channel groups for a port, if it is hardware.
    func groups(of p: EPort, input: Bool) -> [ChannelGroup] {
        guard p.kind == "device", let hw = hardware(named: p.name) else { return [] }
        return input ? hw.input_groups : hw.output_groups
    }

    /// What a jack of a device carries: "Main", or "Mic 1 / Mic 2" for two
    /// mono channels; nil when unknown.
    func spanName(_ p: EPort, input: Bool, ch: UInt32, width: UInt32) -> String? {
        let chans = Array(ch..<ch + width)
        let hits = groups(of: p, input: input).filter { g in chans.contains { g.channels.contains($0) } }
        return hits.isEmpty ? nil : hits.map(\.name).joined(separator: " / ")
    }

    func spanNote(_ p: EPort, input: Bool, ch: UInt32, width: UInt32) -> String? {
        let chans = Array(ch..<ch + width)
        let notes = groups(of: p, input: input).filter { g in chans.contains { g.channels.contains($0) } }.compactMap { g in g.note.map { "\(g.name): \($0)" } }
        return notes.isEmpty ? nil : notes.joined(separator: "\n")
    }

    /// Adds a hardware device with its profile's first patch: an input onto
    /// the first bus nothing feeds yet, an output from the first bus.
    func addDevice(_ hw: HardwareDevice, input: Bool) {
        var p = RackModel.port("device", name: hw.name, output: !input)
        if input, let d = device, let first = hw.default_input.first {
            let k = d.bus_list.indices.first { busSources($0, in: d).isEmpty } ?? 0
            let second = hw.default_input.count > 1 ? hw.default_input[1] : first
            let s = d.busSpans[k]
            p.wires = s.width == 2 ? [[first, s.first], [second, s.first + 1]] : [[first, s.first]]
        } else if !input, let d = device, let first = hw.default_output.first, let s = d.busSpans.first {
            let second = hw.default_output.count > 1 ? hw.default_output[1] : first
            p.wires = s.width == 2 ? [[s.first, first], [s.first + 1, second]] : [[s.first, first], [s.first, second]]
        }
        if input { addInput(p) } else { addOutput(p) }
    }

    /// The device's rate when it differs from the mixer's: the daemon then
    /// leaves it out of the routing.
    func rateMismatch(_ p: EPort) -> Double? {
        guard p.kind == "device", let d = device, let hw = hardware(named: p.name), hw.rate > 0, hw.rate != Double(d.sample_rate) else { return nil }
        return hw.rate
    }

    /// Sets the device to the mixer's rate (only when the user asks).
    func matchRate(_ p: EPort) {
        guard let d = device, let hw = hardware(named: p.name) else { return }
        if let e = Core.setDeviceRate(uid: hw.uid, rate: Double(d.sample_rate)) {
            warning = "\(hw.name): \(e)"
        } else {
            warning = nil
            hardware = Core.devices()
        }
    }

    /// Opens the device's profile in the text editor, making one to edit first.
    func editProfile(of p: EPort) {
        guard let hw = hardware(named: p.name) else {
            warning = "\(p.name ?? "That device") isn’t connected, so it has no profile to edit."
            return
        }
        switch Core.saveProfile(uid: hw.uid) {
        case .success(let path): NSWorkspace.shared.open(URL(fileURLWithPath: path))
        case .failure(let e): warning = "Device profile: " + e.message
        }
    }
}

// MARK: - Busses and bus groups

extension RackModel {
    /// A bus's name, if it has one.
    func busName(_ k: Int, in d: EDevice) -> String? {
        guard d.bus_list.indices.contains(k) else { return nil }
        let n = d.bus_list[k].name
        return n.isEmpty ? nil : n
    }

    /// Its channels: "7" or "7/8".
    func busChannels(_ k: Int, in d: EDevice) -> String {
        guard d.bus_list.indices.contains(k) else { return "?" }
        let s = d.busSpans[k]
        return s.width == 1 ? "\(s.first)" : "\(s.first)/\(s.first + 1)"
    }

    /// Its name, else its channels.
    func busLabel(_ k: Int, in d: EDevice) -> String {
        busName(k, in: d) ?? busChannels(k, in: d)
    }

    func freshBusName(_ base: String, in list: [EBus]) -> String {
        let taken = Set(list.map { $0.name.lowercased() })
        if !taken.contains(base.lowercased()) { return base }
        var n = 2
        while taken.contains("\(base) \(n)".lowercased()) { n += 1 }
        return "\(base) \(n)"
    }

    /// Names a bus (empty: back to its channel numbers). Its id stays.
    func renameBus(_ k: Int, to name: String) {
        guard let d = device, d.bus_list.indices.contains(k) else { return }
        let new = name.trimmingCharacters(in: .whitespaces)
        if !new.isEmpty, d.bus_list.enumerated().contains(where: { $0.offset != k && $0.element.name.lowercased() == new.lowercased() }) {
            warning = "Another bus is already called “\(new)”."
            return
        }
        warning = nil
        edit { $0.bus_list[k].name = new }
    }

    /// Adds a stereo bus to the mixer or a group, after the others (its
    /// channels come after every bus's).
    func addBus(to group: String? = nil) {
        guard let d = device, d.busses + 2 <= 64 else {
            warning = "The busses already have 64 channels, the most a mixer has."
            return
        }
        rebus(d.bus_list + [EBus(id: newID("b"), name: "", mono: false, group: group)])
    }

    /// Removes bus `k`: what was patched to it is unplugged, and the busses
    /// after it move down, their patches with them.
    func removeBus(_ k: Int) {
        guard let d = device, d.bus_list.count > 1, d.bus_list.indices.contains(k) else { return }
        var list = d.bus_list
        list.remove(at: k)
        rebus(list)
    }

    /// Moves bus `k` one place left or right among the busses of its unit,
    /// its patches with it.
    func moveBus(_ k: Int, by delta: Int) {
        guard let d = device, d.bus_list.indices.contains(k) else { return }
        let mine = d.busses(in: d.groupOf(k))
        guard let at = mine.firstIndex(of: k), mine.indices.contains(at + delta) else { return }
        var list = d.bus_list
        list.swapAt(k, mine[at + delta])
        rebus(list)
    }

    /// Shows bus `k` in a group (nil: in the mixer). Its channels stay.
    func moveBus(_ k: Int, toGroup g: String?) {
        edit { $0.bus_list[k].group = g }
    }

    /// The inputs feeding bus `k`.
    func busSources(_ k: Int, in d: EDevice) -> [Int] {
        guard d.bus_list.indices.contains(k) else { return [] }
        let s = d.busSpans[k]
        return d.inputs.indices.filter { i in d.inputs[i].wires.contains { $0[1] >= s.first && $0[1] < s.first + s.width } }
    }

    /// A new bus group, its name ready to edit.
    func addGroup(moving bus: Int? = nil) {
        guard let d = device else { return }
        let taken = Set(d.groups.map { $0.name.lowercased() })
        var n = d.groups.count + 1
        while taken.contains("group \(n)") { n += 1 }
        let g = EGroup(id: newID("g"), name: "Group \(n)")
        edit { dev in
            dev.groups.append(g)
            if let k = bus { dev.bus_list[k].group = g.id }
        }
        renaming = "group:" + g.id
    }

    /// Removes a group; its busses go back to the mixer, and its MIX is unplugged.
    func removeGroup(_ id: String) {
        edit { dev in
            dev.groups.removeAll { $0.id == id }
            for k in dev.bus_list.indices where dev.bus_list[k].group == id { dev.bus_list[k].group = nil }
            for o in dev.outputs.indices { dev.outputs[o].mix_wires.removeAll { $0.group == id } }
        }
    }
}

extension Array where Element: Hashable {
    /// Without repeats, in order.
    func uniqued() -> [Element] {
        var seen = Set<Element>()
        return filter { seen.insert($0).inserted }
    }
}

// MARK: - Adding, removing, ordering units

extension RackModel {
    func remove(isInput: Bool, _ index: Int) {
        edit { d in if isInput { d.inputs.remove(at: index) } else { d.outputs.remove(at: index) } }
        checkFeedback()
    }

    static func port(_ kind: String, name: String? = nil, channels: UInt32? = nil, output: Bool = false) -> EPort {
        let prefix = kind == "virtual_device" ? "v" : output ? "o" : "i"
        return EPort(kind: kind, name: name, id: newID(prefix), channels: channels, icon: nil, wires: [], enabled: true, gain_db: 0, muted: false)
    }

    /// A new unit stays in sight even when unpatched units are hidden.
    func addInput(_ p: EPort) {
        if !p.patched { showUnpatched = true }
        edit { $0.inputs.append(p) }
    }

    func addOutput(_ p: EPort) {
        if !p.patched { showUnpatched = true }
        edit { $0.outputs.append(p) }
    }

    /// A name for a new virtual device that isn't taken yet.
    func freshName(_ base: String) -> String {
        guard let d = device else { return base }
        let taken = Set((d.inputs + d.outputs).compactMap { $0.name })
        if !taken.contains(base) { return base }
        var k = 2
        while taken.contains("\(base) \(k)") { k += 1 }
        return "\(base) \(k)"
    }
}

// MARK: - Presets

extension RackModel {
    private var presetKey: String { "preset." + (device?.id ?? "") }

    /// The mixer without its name and id, which presets don't carry.
    private func contents(_ d: EDevice) -> EDevice {
        var c = d
        c.name = ""
        c.id = ""
        return c
    }

    /// Changed since the current preset was loaded or stored.
    var presetModified: Bool {
        guard let d = device, let s = presetSnapshot else { return currentPreset != nil }
        return contents(d) != contents(s)
    }

    func restorePreset() {
        currentPreset = UserDefaults.standard.string(forKey: presetKey)
        presetSnapshot = currentPreset.flatMap { try? Core.loadPreset(name: $0).get() }
        presetCursor = currentPreset
    }

    /// Steps the display through the presets.
    func browsePresets(_ delta: Int) {
        guard !presetNames.isEmpty else { return }
        let at = presetCursor.flatMap { presetNames.firstIndex(of: $0) } ?? (delta > 0 ? -1 : presetNames.count)
        presetCursor = presetNames[(at + delta + presetNames.count) % presetNames.count]
    }

    /// Applies a preset to the mixer; its name and id stay.
    func loadPreset(_ name: String) {
        guard let d = device else { return }
        switch Core.loadPreset(name: name) {
        case .success(var p):
            p.name = d.name
            p.id = d.id
            // The daemon fades every output to silence, switches, fades back in.
            Core.requestTransition(deviceID: d.id)
            edit { $0 = p }
            currentPreset = name
            presetSnapshot = p
            presetCursor = name
            UserDefaults.standard.set(name, forKey: presetKey)
            status = "preset “\(name)” loaded"
            checkFeedback()
        case .failure(let e):
            warning = "Preset “\(name)”: " + e.message
        }
    }

    /// Stores the mixer as a preset, replacing one of that name.
    func storePreset(_ name: String) {
        guard let d = device else { return }
        let n = name.trimmingCharacters(in: .whitespaces)
        if let e = Core.storePreset(name: n, device: d) {
            warning = "Preset not stored: " + e
            return
        }
        presetNames = Core.presets()
        currentPreset = n
        presetSnapshot = d
        presetCursor = n
        UserDefaults.standard.set(n, forKey: presetKey)
        status = "preset “\(n)” stored"
    }

    func deletePreset(_ name: String) {
        if let e = Core.deletePreset(name: name) {
            warning = "Preset not deleted: " + e
            return
        }
        presetNames = Core.presets()
        if currentPreset == name {
            currentPreset = nil
            presetSnapshot = nil
            UserDefaults.standard.removeObject(forKey: presetKey)
        }
        if presetCursor == name { presetCursor = currentPreset ?? presetNames.first }
        status = "preset “\(name)” deleted"
    }
}

// MARK: - Renaming

extension RackModel {
    /// Renames a virtual device ("in:<i>", "out:<i>") or the mixer ("mixer").
    /// Its id stays, so apps still know the device by its old identity.
    func commitRename(_ key: String, _ name: String) {
        renaming = nil
        guard let d = device else { return }
        let n = name.trimmingCharacters(in: .whitespaces)
        guard !n.isEmpty else { return }
        if key == "mixer" {
            if n != d.name { edit { $0.name = n } }
            return
        }
        if key.hasPrefix("group:") {
            let id = String(key.dropFirst("group:".count))
            guard let g = d.groups.firstIndex(where: { $0.id == id }), d.groups[g].name != n else { return }
            if d.groups.contains(where: { $0.id != id && $0.name.lowercased() == n.lowercased() }) {
                warning = "Another group is already called “\(n)”."
                return
            }
            warning = nil
            edit { $0.groups[g].name = n }
            return
        }
        let parts = key.split(separator: ":")
        guard parts.count == 2, let i = Int(parts[1]) else { return }
        let input = parts[0] == "in"
        let list = input ? d.inputs : d.outputs
        guard list.indices.contains(i), list[i].name != n else { return }
        let others = (d.inputs + d.outputs).filter { $0.kind == "virtual_device" }.compactMap(\.name)
        if others.contains(where: { $0.lowercased() == n.lowercased() }) {
            warning = "Another virtual device is already called “\(n)”."
            return
        }
        // Only the name changes; the id, which apps know the device by, stays.
        warning = nil
        edit { dev in
            if input { dev.inputs[i].name = n } else { dev.outputs[i].name = n }
        }
    }
}

/// A new id for a bus or unit: a letter for what it is and eight hex digits,
/// made once and never changed (see lc-config's ids).
func newID(_ kind: String) -> String {
    kind + UUID().uuidString.lowercased().filter(\.isHexDigit).prefix(8)
}
