// Where the units sit in the rack. Each can have a rack unit (U) of its
// own, gaps allowed; one without (new, or from a hand-written file) goes
// after the others, and two half-width units in a row share it. The preset
// unit always takes the top one.

import CoreGraphics

/// A unit of the rack, whatever it is.
enum UnitRef: Hashable {
    case input(Int)
    case output(Int)
    case mixer
    case group(String)
}

/// Where a unit sits: its first rack unit, how many it takes, which half.
struct Placed: Equatable {
    var u: Int
    var size: Int
    var half: Half?

    var top: CGFloat { Rack.top(u) }

    func overlaps(_ o: Placed) -> Bool {
        u < o.u + o.size && o.u < u + size && (half == nil || o.half == nil || half == o.half)
    }
}

/// Which halves of each rack unit are taken.
struct RackGrid {
    private(set) var taken: [Int: (left: Bool, right: Bool)] = [:]

    func fits(_ p: Placed) -> Bool {
        p.u >= 1 && (p.u..<p.u + p.size).allSatisfy { u in
            let t = taken[u] ?? (false, false)
            switch p.half {
            case nil: return !t.left && !t.right
            case .left: return !t.left
            case .right: return !t.right
            }
        }
    }

    mutating func take(_ p: Placed) {
        for u in p.u..<p.u + p.size {
            var t = taken[u] ?? (false, false)
            if p.half != .right { t.left = true }
            if p.half != .left { t.right = true }
            taken[u] = t
        }
    }

    /// The first spot from rack unit `from` on that a unit this size fits,
    /// a half-width one in either half (`prefer` first).
    func first(from: Int, size: Int, half: Bool, prefer: Half = .left) -> Placed {
        var u = max(1, from)
        while true {
            for h in half ? [prefer, prefer == .left ? .right : .left] : [nil as Half?] {
                let p = Placed(u: u, size: size, half: h)
                if fits(p) { return p }
            }
            u += 1
        }
    }
}

extension RackModel {
    /// The units shown, with what each takes and where it asked to be, in
    /// the order units without a spot are placed: inputs, the mixer, the
    /// bus groups, outputs.
    func rackUnits(_ d: EDevice) -> [(ref: UnitRef, size: Int, half: Bool, spot: ERack?)] {
        let shown = { (p: EPort) in self.showUnpatched || p.patched }
        var out: [(ref: UnitRef, size: Int, half: Bool, spot: ERack?)] = []
        for i in d.inputs.indices where shown(d.inputs[i]) {
            out.append((.input(i), 1, isHalf(input: i, in: d), d.inputs[i].rack))
        }
        out.append((.mixer, Rack.mixerUnits, false, d.mixer_rack))
        for g in d.groups { out.append((.group(g.id), Rack.mixerUnits, false, g.rack)) }
        for i in d.outputs.indices where shown(d.outputs[i]) {
            out.append((.output(i), 1, isHalf(output: i, in: d), d.outputs[i].rack))
        }
        return out
    }

    /// Which units are shown, and where.
    func layout(_ d: EDevice) -> RackLayout {
        let units = rackUnits(d)
        var grid = RackGrid()
        var spots: [UnitRef: Placed] = [:]
        var rest: [Int] = []
        for (k, unit) in units.enumerated() {
            if let s = unit.spot {
                let p = Placed(u: Int(s.u), size: unit.size, half: unit.half ? (s.right ? .right : .left) : nil)
                if grid.fits(p) {
                    grid.take(p)
                    spots[unit.ref] = p
                    continue
                }
            }
            rest.append(k)
        }
        // The rest after the placed ones, in order; a half-width unit
        // takes the right half of the row the one before it started.
        var next = spots.values.map { $0.u + $0.size }.max() ?? 1
        var open: Int?
        for k in rest {
            let unit = units[k]
            if unit.half, let row = open, grid.fits(Placed(u: row, size: 1, half: .right)) {
                spots[unit.ref] = Placed(u: row, size: 1, half: .right)
                grid.take(spots[unit.ref]!)
                open = nil
                continue
            }
            let p = grid.first(from: next, size: unit.size, half: unit.half)
            grid.take(p)
            spots[unit.ref] = p
            open = unit.half && p.half == .left ? p.u : nil
            next = p.u + p.size
        }

        let order = { (a: Placed, b: Placed) in (a.u, a.half == .right ? 1 : 0) < (b.u, b.half == .right ? 1 : 0) }
        let inputs = d.inputs.indices.filter { spots[.input($0)] != nil }.sorted { order(spots[.input($0)]!, spots[.input($1)]!) }
        let outputs = d.outputs.indices.filter { spots[.output($0)] != nil }.sorted { order(spots[.output($0)]!, spots[.output($1)]!) }
        var l = RackLayout(inputs: inputs, outputs: outputs, hidden: d.inputs.count + d.outputs.count - inputs.count - outputs.count)
        l.spots = spots
        for i in inputs { l.inputSpots[i] = Spot(top: spots[.input(i)]!.top, half: spots[.input(i)]!.half) }
        for i in outputs { l.outputSpots[i] = Spot(top: spots[.output(i)]!.top, half: spots[.output(i)]!.half) }
        let groups: [String?] = [nil] + d.groups.map { $0.id as String? }
        l.busUnits = groups.map { g in (g, spots[g.map(UnitRef.group) ?? .mixer]!.top) }
        for (u, t) in grid.taken where t.left != t.right {
            l.blanks.append(Spot(top: Rack.top(u), half: t.left ? .right : .left))
        }
        // An empty rack unit below the last, to move units into.
        let last = spots.values.map { $0.u + $0.size - 1 }.max() ?? 0
        l.height = Rack.top(last + 2) + Rack.topPad
        return l
    }

    /// Puts a unit at `target`. A unit in the way moves next to it, where
    /// the moved one was if it fits there, else to the first free spot below.
    func place(_ ref: UnitRef, at target: Placed) {
        guard let d = device, target.u >= 1 else { return }
        let l = layout(d)
        guard let old = l.spots[ref], old != target else { return }
        var spots = l.spots
        spots[ref] = nil
        let inWay = spots.filter { $0.value.overlaps(target) }.sorted { ($0.value.u, $0.value.half == .right ? 1 : 0) < ($1.value.u, $1.value.half == .right ? 1 : 0) }
        var grid = RackGrid()
        for (r, p) in spots where !inWay.contains(where: { $0.key == r }) { grid.take(p) }
        grid.take(target)
        spots[ref] = target
        for (r, p) in inWay {
            // Moving up, the units passed go below it; moving down, up into the space it left.
            let beside = target.u < old.u ? target.u + target.size : old.u
            var q = Placed(u: beside, size: p.size, half: p.half)
            if !grid.fits(q) { q = grid.first(from: p.u, size: p.size, half: p.half != nil, prefer: p.half ?? .left) }
            grid.take(q)
            spots[r] = q
        }
        edit { dev in
            for (r, p) in spots { dev.setSpot(r, ERack(u: UInt32(p.u), right: p.half == .right)) }
        }
    }

    /// One place up or down: past the unit next to it, which takes its place.
    func nudge(_ ref: UnitRef, by delta: Int) {
        guard let d = device, let p = layout(d).spots[ref] else { return }
        let others = layout(d).spots.filter { $0.key != ref }.map(\.value)
        let column = { (o: Placed) in p.half == nil || o.half == nil || o.half == p.half }
        var target = p
        if delta < 0 {
            target.u = others.filter { column($0) && $0.u + $0.size == p.u }.map(\.u).min() ?? p.u - 1
        } else {
            target.u = p.u + (others.filter { column($0) && $0.u == p.u + p.size }.map(\.size).max() ?? 1)
        }
        place(ref, at: target)
    }
}

extension EDevice {
    mutating func setSpot(_ ref: UnitRef, _ spot: ERack) {
        switch ref {
        case .input(let i): inputs[i].rack = spot
        case .output(let i): outputs[i].rack = spot
        case .mixer: mixer_rack = spot
        case .group(let id):
            if let g = groups.firstIndex(where: { $0.id == id }) { groups[g].rack = spot }
        }
    }
}
