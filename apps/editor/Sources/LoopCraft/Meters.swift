// Live meters: the daemon's peaks, read 30 times a second. The bar shows
// each read's peak as it is, rising and falling with the signal; only the
// peak LED is held, for 100 ms. The meters are AppKit views that
// redraw themselves when their level moves; SwiftUI isn't involved per
// frame, so a busy rack costs little CPU next to the audio.

import AppKit
import Foundation
import QuartzCore
import SwiftUI

/// How the meters look and move (one day: rack settings). Segment counts
/// are multiples of 8, so the colour changes fall on segment edges.
enum MeterStyle {
    /// The scale's marks, evenly spaced, linear in dB between them.
    static let marks: [Double] = [-48, -24, -12, -6, 0]
    static let yellowFrom = -12.0
    static let redFrom = -3.0
    /// The highest LED lit stays lit this long.
    static let peakHold = 0.1
    static let ladder = 16
    static let strip = 16
    static let bar = 8
}

enum MeterKey: Hashable {
    case input(Int)
    case output(Int)
    case bus
}

@MainActor
final class MeterModel: ObservableObject {
    /// Displayed levels in dBFS (-inf = nothing).
    private(set) var inputs: [[Double]] = []
    private(set) var busses: [Double] = []
    private(set) var outputs: [[Double]] = []
    private(set) var live = false

    var deviceID: String?
    var paused = false
    private let views = NSHashTable<MeterNSView>.weakObjects()

    func register(_ v: MeterNSView) { views.add(v) }
    private var timer: Timer?

    init() {
        timer = Timer.scheduledTimer(withTimeInterval: 1.0 / 30, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tick() }
        }
    }

    func level(_ key: MeterKey, _ channel: Int) -> Double {
        let list: [Double]
        switch key {
        case .input(let i): list = inputs.indices.contains(i) ? inputs[i] : []
        case .output(let i): list = outputs.indices.contains(i) ? outputs[i] : []
        case .bus: list = busses
        }
        return list.indices.contains(channel) ? list[channel] : -.infinity
    }

    private func tick() {
        guard !paused, let id = deviceID, let l = Core.levels(deviceID: id) else {
            if live || !busses.isEmpty {
                (inputs, busses, outputs, live) = ([], [], [], false)
                refresh()
            }
            return
        }
        func db(_ v: Float) -> Double { v > 0 ? 20 * log10(Double(v)) : -.infinity }
        inputs = l.inputs.map { $0.map(db) }
        busses = l.busses.map(db)
        outputs = l.outputs.map { $0.map(db) }
        live = true
        refresh()
    }

    private func refresh() {
        for v in views.allObjects { v.update(meterFraction(level(v.key, v.channel))) }
    }
}

/// One LED ladder, drawn by AppKit.
final class MeterNSView: NSView {
    var key: MeterKey = .bus
    var channel = 0
    var segments = 16
    var vertical = false
    var compact = false
    private var lit = 0
    /// The highest LED lit lately, and when it was lit.
    private var peak = 0
    private var peakAt = 0.0

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { false }

    /// LED size along and across the ladder, and the gap between LEDs.
    static func cell(vertical: Bool, compact: Bool) -> (along: CGFloat, across: CGFloat, gap: CGFloat) {
        vertical ? (5, 9, 2) : compact ? (5, 6, 2) : (9, 6, 3)
    }

    static func length(segments: Int, vertical: Bool, compact: Bool) -> CGFloat {
        let c = cell(vertical: vertical, compact: compact)
        return CGFloat(segments) * (c.along + c.gap) - c.gap
    }

    /// Lights every meter this far up its scale (for `--snapshot --lit`).
    @MainActor static var demo: Double?

    func update(_ level: Double) {
        let fraction = Self.demo ?? level
        let n = Int((max(0, min(1, fraction)) * Double(segments)).rounded(.up))
        let now = CACurrentMediaTime()
        var p = peak
        if n >= p {
            (p, peakAt) = (n, now)
        } else if now - peakAt > MeterStyle.peakHold {
            p = n
        }
        if n != lit || p != peak {
            (lit, peak) = (n, p)
            needsDisplay = true
        }
    }

    /// An LED's colour by the level it lights at (its lower edge).
    private func color(_ i: Int) -> CGColor {
        let db = meterDb(Double(i) / Double(segments))
        return db >= MeterStyle.redFrom - 0.01 ? Self.red : db >= MeterStyle.yellowFrom - 0.01 ? Self.amber : Self.green
    }

    static func color(_ hex: UInt32, _ alpha: CGFloat = 1) -> CGColor {
        CGColor(red: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255, blue: CGFloat(hex & 0xff) / 255, alpha: alpha)
    }

    private static let green = color(0x3ddc84), amber = color(0xffc14d), red = color(0xff5b4f)
    private static let off = CGColor(gray: 0, alpha: 0.45), rim = CGColor(gray: 1, alpha: 0.04)

    override func draw(_: NSRect) {
        guard let ctx = NSGraphicsContext.current?.cgContext else { return }
        let c0 = Self.cell(vertical: vertical, compact: compact)
        let (w, h, gap) = vertical ? (c0.across, c0.along, c0.gap) : (c0.along, c0.across, c0.gap)
        let length = Self.length(segments: segments, vertical: vertical, compact: compact)
        for i in 0..<segments {
            let along = CGFloat(i) * ((vertical ? h : w) + gap)
            let r = vertical ? CGRect(x: 0, y: length - along - h, width: w, height: h) : CGRect(x: along, y: 0, width: w, height: h)
            let c = color(i)
            let on = i < lit || i == peak - 1
            if on {
                ctx.setFillColor(c.copy(alpha: 0.35)!)
                ctx.addPath(CGPath(roundedRect: r.insetBy(dx: -1.5, dy: -1.5), cornerWidth: 2.5, cornerHeight: 2.5, transform: nil))
                ctx.fillPath()
                ctx.setFillColor(c)
            } else {
                ctx.setFillColor(Self.off)
            }
            ctx.addPath(CGPath(roundedRect: r, cornerWidth: 1, cornerHeight: 1, transform: nil))
            ctx.fillPath()
            if !on {
                ctx.setStrokeColor(Self.rim)
                ctx.addPath(CGPath(roundedRect: r.insetBy(dx: 0.5, dy: 0.5), cornerWidth: 1, cornerHeight: 1, transform: nil))
                ctx.strokePath()
            }
        }
    }
}

/// A live LED ladder for one channel.
struct LiveMeter: NSViewRepresentable {
    @EnvironmentObject var meters: MeterModel
    var key: MeterKey
    var channel: Int
    var segments: Int
    var vertical = false
    var compact = false

    func makeNSView(context _: Context) -> MeterNSView {
        let v = MeterNSView()
        configure(v)
        meters.register(v)
        return v
    }

    func updateNSView(_ v: MeterNSView, context _: Context) { configure(v) }

    private func configure(_ v: MeterNSView) {
        (v.key, v.channel, v.segments, v.vertical, v.compact) = (key, channel, segments, vertical, compact)
        v.update(meterFraction(meters.level(key, channel)))
        v.needsDisplay = true
    }

    func sizeThatFits(_: ProposedViewSize, nsView _: MeterNSView, context _: Context) -> CGSize? {
        let length = MeterNSView.length(segments: segments, vertical: vertical, compact: compact)
        let across = MeterNSView.cell(vertical: vertical, compact: compact).across
        return vertical ? CGSize(width: across, height: length) : CGSize(width: length, height: across)
    }
}

/// Where a level sits on the meter scale: −48, −24, −12, −6 and 0 dBFS
/// evenly spaced, linear in dB between them.
func meterFraction(_ db: Double) -> Double {
    let marks = MeterStyle.marks
    if db <= -60 { return 0 }
    if db < marks[0] { return (db + 60) / 12 * 0.04 }
    for k in 1..<marks.count where db <= marks[k] {
        let t = (db - marks[k - 1]) / (marks[k] - marks[k - 1])
        return (Double(k - 1) + t) / Double(marks.count - 1)
    }
    return 1
}

/// The level at a place on the scale (the inverse of `meterFraction`).
func meterDb(_ fraction: Double) -> Double {
    let marks = MeterStyle.marks
    if fraction <= 0 { return -.infinity }
    let at = min(1, fraction) * Double(marks.count - 1)
    let k = min(marks.count - 2, Int(at))
    return marks[k] + (at - Double(k)) * (marks[k + 1] - marks[k])
}
