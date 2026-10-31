// Rack hardware: panels, screws, nameplates, LCDs, LED meters, knobs, lit
// buttons. All drawn, no bitmaps.

import AppKit
import SwiftUI

enum Theme {
    static let name = Font.system(size: 21, weight: .bold).width(.condensed)
    static let kind = Font.system(size: 11, weight: .medium).width(.condensed)
    static let silk = Font.system(size: 10, weight: .semibold).width(.condensed)
    static let lcd = Font.system(size: 12, weight: .medium, design: .monospaced)
    static let text = Color(hex: 0xeef1f4)
    static let dim = Color(hex: 0xa9b0b8)
    static let lcdText = Color(hex: 0xa8f5d0)
}

struct BrushedPanel: View {
    var dark = false
    var back = false
    var body: some View {
        let colors: [Color] = back ? [Color(hex: 0x24272b), Color(hex: 0x1f2225)]
            : dark ? [Color(hex: 0x34373c), Color(hex: 0x2a2d31), Color(hex: 0x25272b)]
            : [Color(hex: 0x5a5f66), Color(hex: 0x4a4e55), Color(hex: 0x41454b), Color(hex: 0x373a40)]
        RoundedRectangle(cornerRadius: 3)
            .fill(LinearGradient(colors: colors, startPoint: .top, endPoint: .bottom))
            .overlay {
                if !back {
                    Canvas { ctx, size in
                        var y: CGFloat = 0
                        while y < size.height {
                            ctx.fill(Path(CGRect(x: 0, y: y, width: size.width, height: 1)), with: .color(.white.opacity(0.018)))
                            y += 3
                        }
                    }
                }
            }
            .overlay(alignment: .top) { Rectangle().fill(Color.white.opacity(back ? 0.1 : 0.22)).frame(height: 1) }
            .overlay(alignment: .bottom) { Rectangle().fill(Color.black.opacity(0.8)).frame(height: 1) }
            .shadow(color: .black.opacity(0.6), radius: 0, y: 2)
    }
}

struct Screw: View {
    var body: some View {
        Circle()
            .fill(RadialGradient(colors: [Color(hex: 0xc6ccd2), Color(hex: 0x6a6f75), Color(hex: 0x33373b)], center: UnitPoint(x: 0.4, y: 0.35), startRadius: 0, endRadius: 7))
            .overlay(Rectangle().fill(Color.black.opacity(0.55)).frame(width: 8, height: 2).rotationEffect(.degrees(35)))
            .frame(width: 12, height: 12)
            .shadow(color: .black.opacity(0.5), radius: 0.5, y: 1)
    }
}

struct Plate<Content: View>: View {
    var width: CGFloat
    var height: CGFloat
    @ViewBuilder var content: () -> Content
    var body: some View {
        ZStack(alignment: .topLeading) {
            RoundedRectangle(cornerRadius: 3)
                .fill(LinearGradient(colors: [Color(hex: 0x141619), Color(hex: 0x1b1e22)], startPoint: .top, endPoint: .bottom))
                .overlay(RoundedRectangle(cornerRadius: 3).stroke(Color.black, lineWidth: 1))
                .shadow(color: .white.opacity(0.07), radius: 0, y: 1)
            content()
        }
        .frame(width: width, height: height, alignment: .topLeading)
    }
}

struct LCD: View {
    var text: String
    var width: CGFloat? = nil
    var error = false
    var size: CGFloat = 12
    var body: some View {
        Text(text)
            .font(.system(size: size, weight: .medium, design: .monospaced))
            .foregroundStyle(error ? Color(hex: 0xffb4a2) : Theme.lcdText)
            .lineLimit(1)
            .truncationMode(.tail)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .frame(width: width, alignment: .center)
            .background(RoundedRectangle(cornerRadius: 3).fill(error ? Color(hex: 0x1a0c09) : Color(hex: 0x0a1510)))
            .overlay(RoundedRectangle(cornerRadius: 3).stroke(error ? Color(hex: 0x5a2a20) : Color(hex: 0x1e3a2c), lineWidth: 1))
    }
}

struct Silk: View {
    var text: String
    var body: some View {
        Text(text.uppercased()).font(Theme.silk).tracking(1.6).foregroundStyle(Color(hex: 0xd9dde2)).shadow(color: .black.opacity(0.5), radius: 0, y: 1)
    }
}

struct StatusLED: View {
    var color: Color
    var on: Bool
    var body: some View {
        Circle()
            .fill(on ? AnyShapeStyle(color) : AnyShapeStyle(RadialGradient(colors: [Color(hex: 0x4a4f55), Color(hex: 0x1b1d20)], center: .topLeading, startRadius: 0, endRadius: 8)))
            .frame(width: 8, height: 8)
            .shadow(color: on ? color : .clear, radius: 4)
    }
}

/// A lit push button: MUTE (red), FOLLOW VOL (amber).
struct LitButton: View {
    var title: String
    var on: Bool
    var color: Color
    var width: CGFloat = 52
    var height: CGFloat = 24
    var action: () -> Void
    var body: some View {
        Button(action: action) {
            Text(title)
                .font(.system(size: height < 22 ? 10 : 12, weight: .bold).width(.condensed))
                .tracking(1.4)
                .foregroundStyle(on ? Color.white : Color(hex: 0xc9cfd6))
                .frame(width: width, height: height)
                .background(
                    RoundedRectangle(cornerRadius: 3).fill(on
                        ? AnyShapeStyle(RadialGradient(colors: [color.opacity(0.95), color.opacity(0.7)], center: .center, startRadius: 0, endRadius: width / 2))
                        : AnyShapeStyle(LinearGradient(colors: [Color(hex: 0x3c4046), Color(hex: 0x2a2d31)], startPoint: .top, endPoint: .bottom)))
                )
                .overlay(RoundedRectangle(cornerRadius: 3).stroke(Color.black, lineWidth: 1))
                .shadow(color: on ? color.opacity(0.75) : .black.opacity(0.5), radius: on ? 6 : 1, y: on ? 0 : 2)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

/// A gain knob in dB on the fader law: drag up or down (hold ⌥ for fine
/// steps); double-click for unity.
struct Knob: View {
    var db: Double?
    var label: String
    var size: CGFloat = 52
    var onChange: (Double?) -> Void
    @State private var drag: (scalar: Double, y: CGFloat)?

    var body: some View {
        let s = FaderLaw.scalar(db ?? -.infinity)
        ZStack {
            ForEach(0..<11, id: \.self) { k in
                Capsule().fill(Color(hex: 0xc9ced4)).frame(width: 2, height: 6)
                    .offset(y: -(size / 2 + 9)).rotationEffect(.degrees(-135 + Double(k) * 27))
            }
            Circle()
                .fill(RadialGradient(colors: [Color(hex: 0x8a9098), Color(hex: 0x4a4f56), Color(hex: 0x25282c)], center: UnitPoint(x: 0.35, y: 0.28), startRadius: 0, endRadius: size * 0.6))
                .shadow(color: .black.opacity(0.65), radius: 4, y: 3)
            Circle()
                .fill(RadialGradient(colors: [Color(hex: 0x5d636b), Color(hex: 0x2e3136)], center: UnitPoint(x: 0.4, y: 0.3), startRadius: 0, endRadius: size * 0.4))
                .frame(width: size * 0.7, height: size * 0.7)
            Capsule().fill(Color(hex: 0xf2f4f6)).frame(width: 4, height: size * 0.25)
                .shadow(color: .white.opacity(0.4), radius: 2)
                .offset(y: -size * 0.33).rotationEffect(.degrees(-135 + s * 270))
        }
        .frame(width: size, height: size)
        .contentShape(Circle())
        .gesture(DragGesture(minimumDistance: 0)
            .onChanged { v in
                let (from, y) = drag ?? (s, 0)
                let fine = NSEvent.modifierFlags.contains(.option)
                let next = max(0, min(1, from - Double(v.translation.height - y) / (fine ? 1500 : 320)))
                drag = (next, v.translation.height)
                let value = FaderLaw.db(next)
                onChange(value == -.infinity ? nil : (value * 10).rounded() / 10)
            }
            .onEnded { _ in drag = nil })
        .onTapGesture(count: 2) { onChange(0) }
        .accessibilityLabel(label)
        .accessibilityValue(FaderLaw.label(db) + " dB")
        .accessibilityAdjustableAction { dir in
            let step = dir == .increment ? 0.02 : -0.02
            let value = FaderLaw.db(max(0, min(1, s + step)))
            onChange(value == -.infinity ? nil : value)
        }
        .help("\(label): drag up or down, ⌥ for fine steps · double-click for 0 dB")
    }
}

/// The nameplate symbol: the app's icon, or a symbol on a tinted tile.
struct Tile: View {
    var identity: Identity
    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 10).fill(identity.tile)
                .overlay(RoundedRectangle(cornerRadius: 10).stroke(Color.white.opacity(0.08), lineWidth: 1))
            if let icon = identity.icon {
                Image(nsImage: icon).resizable().interpolation(.high).frame(width: 40, height: 40)
            } else {
                Image(systemName: identity.symbol).font(.system(size: 20, weight: .regular)).foregroundStyle(Color(hex: 0xe8ebee))
            }
        }
        .frame(width: 44, height: 44)
        .shadow(color: .black.opacity(0.6), radius: 2, y: 2)
    }
}
