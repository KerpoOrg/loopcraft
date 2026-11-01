// The back of the rack: a stereo jack per channel pair, or one per channel
// where the pair's switch is set to mono, and cables between them. Drag
// from a jack to another to patch; click a cable to unplug it, a switch to
// flip it.

import SwiftUI

struct BackView: View {
    @EnvironmentObject var model: RackModel
    var device: EDevice

    @State private var dragFrom: JackID?
    @State private var dragPoint: CGPoint = .zero
    @State private var hoverCable: String?

    var body: some View {
        let jacks = model.jacks(in: device)
        let (cables, custom) = model.cables(in: device)
        let l = model.layout(device)
        let switches = model.switches(in: device)
        ZStack(alignment: .topLeading) {
            RackFrame(layout: l)
            PresetUnit(hidden: l.hidden).offset(x: Rack.unitX, y: l.presetTop)
            ForEach(l.inputs, id: \.self) { i in
                let spot = l.inputSpots[i] ?? Spot(top: 0)
                BackUnit(title: device.inputs[i].kind == "virtual" ? "System" : device.inputs[i].name ?? "?",
                         caption: "OUT", width: spot.half == nil ? Rack.sourceX - Rack.unitX - 70 : 200, half: spot.half,
                         stripe: Color(hex: Library.palette[i % Library.palette.count]), enabled: device.inputs[i].enabled)
                    .offset(x: spot.x, y: spot.top)
            }
            ForEach(Array(l.busUnits.enumerated()), id: \.offset) { _, u in
                MixerBack(device: device, group: u.group).offset(x: Rack.unitX, y: u.top)
            }
            ForEach(l.outputs, id: \.self) { i in
                let spot = l.outputSpots[i] ?? Spot(top: 0)
                BackUnit(title: device.outputs[i].name ?? "?", caption: "IN", width: spot.half == nil ? Rack.busOutX - Rack.unitX - 180 : 200, half: spot.half,
                         stripe: nil, enabled: device.outputs[i].enabled)
                    .offset(x: spot.x, y: spot.top)
            }
            ForEach(Array(l.blanks.enumerated()), id: \.offset) { _, b in
                BlankPlate(half: b.half ?? .right).offset(x: b.x, y: b.top)
            }
            ForEach(Array(switches.enumerated()), id: \.offset) { _, s in
                PairSwitchView(mono: s.mono).position(s.1)
                    .help(s.mono ? "Mono: a jack per channel. Click for a stereo pair." : "Stereo pair. Click for a jack per channel (mono).")
            }
            ForEach(jacks, id: \.self) { j in
                JackView(label: jackLabel(j), lit: isPatched(j, cables, custom), highlight: dragFrom != nil && canConnect(dragFrom!, j), stacked: stacked(j))
                    .help(jackNote(j) ?? "")
                    .position(model.position(j, in: device))
            }
            ForEach(Array(custom.enumerated()), id: \.offset) { _, w in
                CableShape(a: model.position(w.0, in: device), b: model.position(w.1, in: device))
                    .stroke(Color(hex: 0xc9cfd6).opacity(0.5), style: StrokeStyle(lineWidth: 2, lineCap: .round, dash: [5, 4]))
                    .allowsHitTesting(false)
            }
            ForEach(cables) { c in
                CableView(a: model.position(c.from, in: device), b: model.position(c.to, in: device), color: color(of: c), hover: hoverCable == c.id)
            }
            if let from = dragFrom {
                CableView(a: model.position(from, in: device), b: dragPoint, color: .white.opacity(0.85), hover: false)
            }
            if let w = model.warning {
                LCD(text: w, error: true, size: 13)
                    .position(x: Rack.width / 2, y: l.height - 20)
            }
        }
        .frame(width: Rack.width, height: l.height, alignment: .topLeading)
        .coordinateSpace(name: "rack")
        .contentShape(Rectangle())
        .gesture(DragGesture(minimumDistance: 0, coordinateSpace: .named("rack"))
            .onChanged { v in
                if dragFrom == nil, hypot(v.translation.width, v.translation.height) > 3, let j = jack(at: v.startLocation, jacks) {
                    dragFrom = j
                }
                dragPoint = v.location
            }
            .onEnded { v in
                defer { dragFrom = nil }
                if let from = dragFrom {
                    if let to = jack(at: v.location, jacks), to != from { model.connect(from, to) }
                    return
                }
                // A switch sits under the cables' plugs' edge; it wins over a cable there.
                if let s = switches.first(where: { abs($0.1.x - v.location.x) < 7 && abs($0.1.y - v.location.y) < 13 }) {
                    model.flip(s.0)
                    return
                }
                if let c = cable(at: v.location, cables) { model.disconnect(c) }
            })
        .onContinuousHover(coordinateSpace: .named("rack")) { phase in
            if case .active(let p) = phase { hoverCable = cable(at: p, cables)?.id } else { hoverCable = nil }
        }
    }

    func jack(at p: CGPoint, _ jacks: [JackID]) -> JackID? {
        jacks.min { dist(p, $0) < dist(p, $1) }.flatMap { dist(p, $0) < 18 ? $0 : nil }
    }

    func dist(_ p: CGPoint, _ j: JackID) -> CGFloat {
        let q = model.position(j, in: device)
        return hypot(p.x - q.x, p.y - q.y)
    }

    func cable(at p: CGPoint, _ cables: [Cable]) -> Cable? {
        cables.last { c in
            CableShape(a: model.position(c.from, in: device), b: model.position(c.to, in: device))
                .path(in: .zero).strokedPath(StrokeStyle(lineWidth: 12)).contains(p)
        }
    }

    func canConnect(_ a: JackID, _ b: JackID) -> Bool {
        switch (a.side, b.side) {
        case (.source, .busIn), (.busIn, .source), (.busOut, .output), (.output, .busOut), (.mixOut, .output), (.output, .mixOut): return true
        default: return false
        }
    }

    func isPatched(_ j: JackID, _ cables: [Cable], _ custom: [(JackID, JackID)]) -> Bool {
        cables.contains { $0.from == j || $0.to == j } || custom.contains { $0.0 == j || $0.1 == j }
    }

    func color(of c: Cable) -> Color {
        if c.from.side == .source { return Color(hex: Library.palette[c.from.unit % Library.palette.count]) }
        if c.from.side == .mixOut { return Color(hex: 0x62a8ff) }
        // Bus → output: the colour of the first input on that bus, else grey.
        if let i = model.busSources(c.from.unit, in: device).first { return Color(hex: Library.palette[i % Library.palette.count]) }
        return Color(hex: 0x9aa3ad)
    }

    /// One of two mono jacks, one above the other.
    func stacked(_ j: JackID) -> Bool {
        switch j.side {
        case .busIn, .busOut: return device.busSlots(in: device.groupOf(j.unit)).first { $0.contains(j.unit) }?.count == 2
        case .mixOut: return false
        case .source: return j.width == 1 && model.channels(ofInput: device.inputs[j.unit], in: device) > 1 && !(j.ch % 2 == 1 && j.ch == model.channels(ofInput: device.inputs[j.unit], in: device))
        case .output: return j.width == 1 && model.channels(ofOutput: device.outputs[j.unit]) > 1 && !(j.ch % 2 == 1 && j.ch == model.channels(ofOutput: device.outputs[j.unit]))
        }
    }

    /// What the device's profile (or its own names) says the jack carries, else its channel numbers.
    func jackLabel(_ j: JackID) -> String {
        let numbers = j.width == 2 ? "\(j.ch)/\(j.ch + 1)" : "\(j.ch)"
        switch j.side {
        case .source:
            let p = device.inputs[j.unit]
            if let name = model.spanName(p, input: true, ch: j.ch, width: j.width) { return name }
            let ch = model.channels(ofInput: p, in: device)
            return ch == 1 ? "MONO" : ch == 2 && j.width == 2 ? "L/R" : ch == 2 ? (j.ch == 1 ? "L" : "R") : numbers
        case .output:
            let p = device.outputs[j.unit]
            if let name = model.spanName(p, input: false, ch: j.ch, width: j.width) { return name }
            let ch = model.channels(ofOutput: p)
            return ch == 1 ? "MONO" : ch == 2 && j.width == 2 ? "L/R" : ch == 2 ? (j.ch == 1 ? "L" : "R") : numbers
        case .busIn, .busOut:
            return model.busName(j.unit, in: device) ?? numbers
        case .mixOut:
            return j.width == 2 ? "MIX L/R" : "MIX"
        }
    }

    func jackNote(_ j: JackID) -> String? {
        let numbers = j.width == 2 ? "Channels \(j.ch)/\(j.ch + 1)" : "Channel \(j.ch)"
        switch j.side {
        case .source: return model.spanNote(device.inputs[j.unit], input: true, ch: j.ch, width: j.width).map { numbers + "\n" + $0 } ?? numbers
        case .output: return model.spanNote(device.outputs[j.unit], input: false, ch: j.ch, width: j.width).map { numbers + "\n" + $0 } ?? numbers
        case .busIn, .busOut: return "\(model.busLabel(j.unit, in: device)): " + (j.width == 2 ? "stereo, channels \(j.ch)/\(j.ch + 1)" : "mono, channel \(j.ch)")
        case .mixOut:
            let g = device.mixGroup(j.unit)
            let what = g == nil ? "every bus" : "the busses in \(device.groups.first { $0.id == g }?.name ?? "the group")"
            return "\(device.mixName(g)): \(what) summed, " + (j.width == 2 ? "stereo (mono busses at -3 dB on both sides)" : "mono")
        }
    }
}

/// A cable hanging between two jacks, sagging with its length.
struct CableShape: Shape {
    var a: CGPoint
    var b: CGPoint
    func path(in _: CGRect) -> Path {
        let sag = 30 + hypot(b.x - a.x, b.y - a.y) * 0.18
        var p = Path()
        p.move(to: a)
        p.addCurve(to: b, control1: CGPoint(x: a.x, y: a.y + sag), control2: CGPoint(x: b.x, y: b.y + sag))
        return p
    }
}

struct CableView: View {
    var a: CGPoint
    var b: CGPoint
    var color: Color
    var hover: Bool
    var body: some View {
        let shape = CableShape(a: a, b: b)
        ZStack {
            shape.stroke(Color.black.opacity(0.45), style: StrokeStyle(lineWidth: 9, lineCap: .round)).offset(y: 4).blur(radius: 3)
            shape.stroke(color.opacity(0.95), style: StrokeStyle(lineWidth: hover ? 8 : 7, lineCap: .round))
            shape.stroke(Color.white.opacity(hover ? 0.55 : 0.3), style: StrokeStyle(lineWidth: 1.5, lineCap: .round)).offset(x: -1, y: -1.5)
            Plug(color: color).position(a)
            Plug(color: color).position(b)
        }
        .allowsHitTesting(false)
    }
}

struct Plug: View {
    var color: Color
    var body: some View {
        ZStack {
            Circle().fill(RadialGradient(colors: [Color(hex: 0xd7dce1), Color(hex: 0x70767c)], center: UnitPoint(x: 0.35, y: 0.3), startRadius: 0, endRadius: 11))
            Circle().fill(color).frame(width: 12, height: 12)
        }
        .frame(width: 20, height: 20)
        .shadow(color: .black.opacity(0.6), radius: 2, y: 2)
    }
}

struct JackView: View {
    var label: String
    var lit: Bool
    var highlight: Bool
    /// One of two mono jacks one above the other: smaller, labelled at the side.
    var stacked = false
    var body: some View {
        let text = Text(label).font(.system(size: stacked ? 8 : 9, weight: .semibold).width(.condensed)).foregroundStyle(lit ? Theme.text : Theme.dim)
            .lineLimit(1).minimumScaleFactor(0.7)
        if stacked {
            socket(20)
                .overlay(alignment: .trailing) { text.frame(width: 36, alignment: .trailing).offset(x: -23) }
                .frame(width: 24, height: 24)
        } else {
            VStack(spacing: 3) {
                text
                socket(24)
            }
            .offset(y: -7)
            .frame(width: 64, height: 46)
        }
    }

    func socket(_ size: CGFloat) -> some View {
        ZStack {
            Circle().fill(RadialGradient(colors: [Color(hex: 0xb6bcc2), Color(hex: 0x50555b)], center: UnitPoint(x: 0.35, y: 0.3), startRadius: 0, endRadius: size / 2 + 1))
            Circle().fill(Color.black).frame(width: size / 2, height: size / 2)
            if highlight { Circle().stroke(Color(hex: 0x62a8ff), lineWidth: 2).frame(width: size + 4, height: size + 4).shadow(color: Color(hex: 0x62a8ff), radius: 4) }
        }
        .frame(width: size, height: size)
        .shadow(color: .black.opacity(0.6), radius: 1, y: 1)
    }
}

/// A small slide switch beside a pair's jacks: up for a stereo pair, down
/// for mono (a jack per channel). Clicks reach it through the rack's gesture.
struct PairSwitchView: View {
    var mono: Bool
    var body: some View {
        VStack(spacing: 1) {
            Text("ST").foregroundStyle(mono ? Theme.dim.opacity(0.6) : Theme.text)
            ZStack(alignment: mono ? .bottom : .top) {
                RoundedRectangle(cornerRadius: 2).fill(Color.black.opacity(0.85))
                    .overlay(RoundedRectangle(cornerRadius: 2).stroke(Color.white.opacity(0.12), lineWidth: 0.5))
                RoundedRectangle(cornerRadius: 1.5).fill(LinearGradient(colors: [Color(hex: 0xe2e6ea), Color(hex: 0x8c939a)], startPoint: .top, endPoint: .bottom))
                    .frame(width: 6, height: 7).padding(1.5)
            }
            .frame(width: 9, height: 16)
            Text("M").foregroundStyle(mono ? Theme.text : Theme.dim.opacity(0.6))
        }
        .font(.system(size: 6, weight: .heavy).width(.condensed))
        .frame(width: 14, height: 32)
        .allowsHitTesting(false)
    }
}

struct BackUnit: View {
    var title: String
    var caption: String
    var width: CGFloat
    var half: Half? = nil
    var stripe: Color?
    var enabled: Bool
    var body: some View {
        Unit(half: half, back: true, stripe: stripe) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text(title).font(.system(size: 15, weight: .bold).width(.condensed)).foregroundStyle(enabled ? Theme.text : Theme.dim)
                    Silk(text: caption)
                }
                .lineLimit(1)
                .frame(width: width, alignment: .leading)
                Spacer()
            }
            .frame(maxHeight: .infinity)
        }
    }
}

struct MixerBack: View {
    var device: EDevice
    /// nil: the mixer itself.
    var group: String?
    var body: some View {
        let g = device.groups.first { $0.id == group }
        let count = device.busses(in: group).count
        Unit(height: Rack.mixerHeight, back: true) {
            ZStack(alignment: .topLeading) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(g?.name ?? device.name).font(.system(size: 15, weight: .bold).width(.condensed)).foregroundStyle(Theme.text)
                    Silk(text: (g == nil ? "Mixer" : "Bus group") + " · \(count) bus" + (count == 1 ? "" : "ses"))
                }
                .padding(.top, 16)
                Silk(text: "Bus inputs").offset(x: Rack.busInX - Rack.unitX - 40 - 12, y: 76)
                Silk(text: "Bus outputs").offset(x: Rack.busOutX - Rack.unitX - 40 - 12, y: 76)
                Silk(text: "Mix").offset(x: Rack.mixX - Rack.unitX - 40 - 12, y: 76)
                Text("Drag between jacks to patch · click a cable to unplug")
                    .font(.system(size: 10).width(.condensed)).foregroundStyle(Theme.dim)
                    .offset(x: 0, y: 60)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
    }
}
