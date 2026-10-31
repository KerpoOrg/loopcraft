// The preset unit at the top of the rack: a display with the preset's name,
// ◀ ▶ to browse, LOAD and STORE, the switch that hides units nothing is
// patched to, and LINK MASTER: the mixer's volume, which outputs set to
// MASTER follow.

import SwiftUI

struct PresetUnit: View {
    @EnvironmentObject var model: RackModel
    /// Units hidden because nothing is patched to them.
    var hidden: Int
    @State private var naming = false
    @State private var draft = ""
    @State private var confirmDelete: String?
    @State private var listing = false
    @FocusState private var focused: Bool

    private var shown: String? { model.presetCursor ?? model.currentPreset }
    private var browsing: Bool { shown != nil && shown != model.currentPreset }

    var body: some View {
        Unit(dark: true) {
            HStack(spacing: 14) {
                Nameplate(identity: Identity(icon: nil, symbol: "square.stack.3d.up.fill", tile: Color(hex: 0x23323a), brand: "LOOPCRAFT", brandColor: Color(hex: 0x62a8ff)),
                          name: "Presets", kind: "wiring & settings", width: 236)
                display
                VStack(spacing: 4) {
                    HStack(spacing: 4) {
                        LitButton(title: "◀", on: false, color: .white, width: 30, height: 22) { model.browsePresets(-1) }
                            .help("Previous preset")
                        LitButton(title: "▶", on: false, color: .white, width: 30, height: 22) { model.browsePresets(1) }
                            .help("Next preset")
                    }
                    Silk(text: "Browse")
                }
                VStack(spacing: 4) {
                    LitButton(title: "LOAD", on: browsing || (shown != nil && model.presetModified), color: Color(hex: 0x3ddc84), width: 60, height: 22) {
                        if let s = shown { model.loadPreset(s) }
                    }
                    .disabled(shown == nil)
                    .help(shown.map { "Apply “\($0)” to the mixer" } ?? "No preset to load")
                    Silk(text: "Recall")
                }
                VStack(spacing: 4) {
                    LitButton(title: "STORE", on: naming, color: Color(hex: 0xff9a3c), width: 60, height: 22) { startNaming() }
                        .help("Store the mixer as a preset")
                    Silk(text: "Save")
                }
                Spacer(minLength: 0)
                unpatched
                master
            }
            .frame(maxHeight: .infinity)
        }
        .confirmationDialog("Delete preset “\(confirmDelete ?? "")”?", isPresented: Binding(get: { confirmDelete != nil }, set: { if !$0 { confirmDelete = nil } })) {
            Button("Delete", role: .destructive) { if let n = confirmDelete { model.deletePreset(n) } }
        }
    }

    /// Units nothing is patched to: show or hide them, and switch off the
    /// ones still on (hardware among them keeps running in the routing).
    private var unpatched: some View {
        let idle = model.device.map { model.idleUnits($0) } ?? (inputs: [], outputs: [])
        let count = idle.inputs.count + idle.outputs.count
        let names = model.device.map { d in idle.inputs.map { unitName(d.inputs[$0]) } + idle.outputs.map { unitName(d.outputs[$0]) } } ?? []
        return HStack(spacing: 10) {
            VStack(spacing: 4) {
                LitButton(title: "UNPATCHED", on: model.showUnpatched, color: Color(hex: 0x62a8ff), width: 84, height: 22) {
                    model.showUnpatched.toggle()
                }
                .help(model.showUnpatched ? "Hide units nothing is patched to" : "Show the \(hidden) hidden unit\(hidden == 1 ? "" : "s")")
                Silk(text: model.showUnpatched ? "Shown" : "\(hidden) hidden")
            }
            VStack(spacing: 4) {
                LitButton(title: "TURN OFF", on: count > 0, color: Color(hex: 0xf2a93b), width: 74, height: 22) { model.turnOffIdle() }
                    .disabled(count == 0)
                    .help(count == 0
                        ? "Everything switched on is patched"
                        : "Switch off what is on but not patched: \(names.joined(separator: ", ")). Hardware keeps running in the routing while it is on.")
                HStack(spacing: 4) {
                    StatusLED(color: Color(hex: 0xf2a93b), on: count > 0)
                    Text(count == 0 ? "ALL PATCHED" : "\(count) IDLE ON")
                        .font(Theme.silk).tracking(1.4).fixedSize()
                        .foregroundStyle(count == 0 ? Theme.dim : Color(hex: 0xf2a93b))
                }
            }
        }
    }

    private func unitName(_ p: EPort) -> String {
        p.kind == "virtual" ? "System" : (p.name ?? "?")
    }

    /// The display: the preset's name large, where it is in the list and
    /// whether the mixer has changed since; a name field while storing.
    private var display: some View {
        ZStack(alignment: .leading) {
            RoundedRectangle(cornerRadius: 4).fill(LinearGradient(colors: [Color(hex: 0x07120d), Color(hex: 0x0b1a13)], startPoint: .top, endPoint: .bottom))
                .overlay(RoundedRectangle(cornerRadius: 4).stroke(Color(hex: 0x1e3a2c), lineWidth: 1))
                .overlay(RoundedRectangle(cornerRadius: 4).stroke(Color.black, lineWidth: 2).padding(-2))
                .shadow(color: Color(hex: 0x3ddc84).opacity(0.08), radius: 6)
            if naming {
                VStack(alignment: .leading, spacing: 3) {
                    TextField("", text: $draft, prompt: Text("NAME").foregroundStyle(Theme.lcdText.opacity(0.35)))
                        .textFieldStyle(.plain)
                        .font(.system(size: 21, weight: .semibold, design: .monospaced))
                        .foregroundStyle(Theme.lcdText)
                        .shadow(color: Theme.lcdText.opacity(0.5), radius: 3)
                        .focused($focused)
                        .onSubmit { store() }
                        .onExitCommand { naming = false }
                    Text(model.presetNames.contains(draft.trimmingCharacters(in: .whitespaces)) ? "ENTER REPLACES IT · ESC CANCELS" : "ENTER STORES · ESC CANCELS")
                        .font(.system(size: 10, weight: .medium, design: .monospaced)).foregroundStyle(Color(hex: 0xffc56b))
                }
                .padding(.horizontal, 12)
            } else {
                Group {
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 8) {
                            Text(shown ?? "NO PRESET")
                                .font(.system(size: 21, weight: .semibold, design: .monospaced))
                                .foregroundStyle(shown == nil ? Theme.lcdText.opacity(0.4) : Theme.lcdText)
                                .lineLimit(1)
                                .shadow(color: Theme.lcdText.opacity(0.5), radius: 3)
                            if !browsing && model.presetModified {
                                Text("EDITED").font(.system(size: 10, weight: .bold, design: .monospaced))
                                    .foregroundStyle(Color(hex: 0xffc56b))
                                    .padding(.horizontal, 4).padding(.vertical, 1)
                                    .overlay(RoundedRectangle(cornerRadius: 2).stroke(Color(hex: 0xffc56b).opacity(0.7), lineWidth: 1))
                            }
                        }
                        Text(caption).font(.system(size: 10, weight: .medium, design: .monospaced)).foregroundStyle(Theme.lcdText.opacity(0.6)).lineLimit(1)
                    }
                    .padding(.horizontal, 12)
                    .frame(width: 290, height: 58, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .onTapGesture { listing = true }
                .popover(isPresented: $listing, arrowEdge: .bottom) { list }
                .contextMenu {
                    if let s = shown, model.presetNames.contains(s) { Button("Delete “\(s)”…") { confirmDelete = s } }
                }
                .help("Click to choose a preset")
            }
        }
        .frame(width: 290, height: 58)
    }

    /// The mixer device's own volume: what the Mac's volume keys and menu
    /// bar set when it is the output. Outputs with MASTER lit follow it.
    private var master: some View {
        let linked = model.device?.outputs.filter(\.follow_volume).compactMap(\.name) ?? []
        return VStack(spacing: 2) {
            if let v = model.deviceVolumeDb {
                Knob(db: v == -.infinity ? nil : v, label: "Link master", size: 36) { db in model.setDeviceVolume(db ?? -.infinity) }
                Text(FaderLaw.label(v == -.infinity ? nil : v) + " dB").font(.system(size: 10, weight: .semibold, design: .monospaced)).foregroundStyle(Theme.lcdText)
            } else {
                Knob(db: 0, label: "Link master", size: 36) { _ in }.opacity(0.4).disabled(true)
                Text("not running").font(.system(size: 9)).foregroundStyle(Theme.dim)
            }
            Silk(text: "Link master")
        }
        .frame(width: 76)
        // The tick marks reach above the knob; this centres knob and ticks on the unit.
        .offset(y: 6)
        .help(linked.isEmpty
            ? "The Mac’s volume for this mixer. No output follows it yet: light MASTER on an output to link it."
            : "The Mac’s volume for this mixer (volume keys, menu bar), followed by " + linked.joined(separator: ", "))
    }

    /// The presets, to pick one for the display (LOAD applies it).
    private var list: some View {
        VStack(alignment: .leading, spacing: 0) {
            if model.presetNames.isEmpty {
                Text("No presets yet. STORE saves the mixer as one.").font(.callout).foregroundStyle(.secondary).padding(12)
            }
            ForEach(model.presetNames, id: \.self) { n in
                HStack {
                    Text(n).font(.system(size: 13, weight: n == shown ? .semibold : .regular, design: .monospaced))
                    Spacer()
                    if n == model.currentPreset { Text("LOADED").font(.system(size: 9, weight: .bold, design: .monospaced)).foregroundStyle(.green) }
                }
                .padding(.horizontal, 12).padding(.vertical, 6)
                .background(n == shown ? Color.accentColor.opacity(0.25) : .clear)
                .contentShape(Rectangle())
                .onTapGesture(count: 2) { model.loadPreset(n); listing = false }
                .onTapGesture { model.presetCursor = n; listing = false }
                .contextMenu { Button("Delete “\(n)”…") { confirmDelete = n; listing = false } }
            }
        }
        .padding(.vertical, 6)
        .frame(width: 320)
    }

    private var caption: String {
        let count = model.presetNames.count
        guard let s = shown else { return count == 0 ? "STORE SAVES THE MIXER AS A PRESET" : "\(count) PRESET\(count == 1 ? "" : "S") · ◀ ▶ TO BROWSE" }
        let at = model.presetNames.firstIndex(of: s).map { "\($0 + 1)/\(count)" } ?? "MISSING"
        if browsing { return "\(at) · LOAD TO APPLY" + (model.currentPreset.map { " · NOW: \($0.uppercased())" } ?? "") }
        return "\(at) · " + (model.presetModified ? "CHANGED SINCE LOADED" : "AS STORED")
    }

    private func startNaming() {
        draft = model.currentPreset ?? "Preset \(model.presetNames.count + 1)"
        naming = true
        focused = true
    }

    private func store() {
        let n = draft.trimmingCharacters(in: .whitespaces)
        naming = false
        if !n.isEmpty { model.storePreset(n) }
    }
}
