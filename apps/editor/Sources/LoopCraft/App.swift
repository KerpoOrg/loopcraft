// LoopCraft: the rack editor for ~/.config/loopcraft/config.yaml. Every
// change is validated by the core and saved; the daemon applies it.

import AppKit
import SwiftUI
import UniformTypeIdentifiers

@main
struct LoopCraftApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate
    @StateObject private var model = RackModel()

    init() {
        Snapshot.runIfAsked()
    }

    var body: some Scene {
        Window("LoopCraft", id: "rack") {
            RackWindow().environmentObject(model).environmentObject(model.meters)
                .frame(minWidth: Rack.shownWidth, minHeight: 500)
        }
        .defaultSize(width: Rack.shownWidth, height: 900)
        .windowResizability(.contentMinSize)
        .windowStyle(.hiddenTitleBar)
        .defaultLaunchBehavior(.presented)
        .restorationBehavior(.disabled)
        .commands {
            CommandGroup(replacing: .newItem) {}
            CommandGroup(after: .saveItem) {
                Button("Save") { model.saveNow() }.keyboardShortcut("s")
                Button("Reload from Disk") { model.reload() }.keyboardShortcut("r")
            }
            CommandMenu("Rack") {
                Button("Flip Rack (Tab)") { model.showBack.toggle() }
                Button("Show Front") { model.showBack = false }.keyboardShortcut("1")
                Button("Show Back") { model.showBack = true }.keyboardShortcut("2")
            }
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_: Notification) {
        // Run as a regular app when started from the terminal (swift run).
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        NSApp.appearance = NSAppearance(named: .darkAqua)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_: NSApplication) -> Bool { true }
}

struct RackWindow: View {
    @EnvironmentObject var model: RackModel
    @State private var tabMonitor: Any?

    var body: some View {
        VStack(spacing: 0) {
            TopBar()
            Divider().overlay(Color.black)
            if let d = model.device {
                ScrollView(.vertical) {
                    ZStack {
                        FrontView(device: d)
                            .opacity(model.showBack ? 0 : 1)
                            .allowsHitTesting(!model.showBack)
                        BackView(device: d)
                            .opacity(model.showBack ? 1 : 0)
                            .allowsHitTesting(model.showBack)
                    }
                    // Centred, wider than the frame: the floor beside the rails is cut off.
                    .frame(width: Rack.shownWidth)
                    .clipped()
                    .frame(maxWidth: .infinity)
                }
                .defaultScrollAnchor(.top)
            } else {
                EmptyRack()
            }
        }
        .background(Color(hex: 0x0b0c0e))
        .preferredColorScheme(.dark)
        .onAppear {
            // Tab flips the rack at once, whatever has keyboard focus.
            tabMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { e in
                let mods = e.modifierFlags.intersection([.command, .option, .control, .shift])
                guard e.keyCode == 48, mods.isEmpty else { return e }
                model.showBack.toggle()
                return nil
            }
        }

    }
}

struct EmptyRack: View {
    @EnvironmentObject var model: RackModel
    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "rectangle.stack").font(.system(size: 40)).foregroundStyle(Theme.dim)
            Text(model.statusIsError ? "config.yaml has a problem" : "No mixers in config.yaml").font(.title3).foregroundStyle(Theme.text)
            Text(model.status).font(.system(.body, design: .monospaced)).foregroundStyle(Theme.dim).multilineTextAlignment(.center)
            Text("Create one with: loopcraft config init").font(.system(.callout, design: .monospaced)).foregroundStyle(Theme.dim)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct TopBar: View {
    @EnvironmentObject var model: RackModel

    var body: some View {
        HStack(spacing: 14) {
            Text("LOOPCRAFT").font(.system(size: 15, weight: .heavy).width(.condensed)).tracking(3).foregroundStyle(Theme.text)
                .padding(.leading, 76)
            if model.config.devices.count > 1 {
                Picker("", selection: $model.deviceIndex) {
                    ForEach(model.config.devices.indices, id: \.self) { i in Text(model.config.devices[i].name).tag(i) }
                }
                .labelsHidden()
                .frame(width: 220)
            }
            Picker("", selection: $model.showBack) {
                Text("Front").tag(false)
                Text("Back").tag(true)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 140)
            .help("Flip the rack (Tab)")

            AddMenus()

            Spacer()
            LCD(text: model.status, error: model.statusIsError)
                .frame(maxWidth: 520, alignment: .trailing)
                .help((model.statusDetail.map { $0 + "\n\n" } ?? "") + model.path)
        }
        .padding(.horizontal, 14)
        .frame(height: 40)
        .background(LinearGradient(colors: [Color(hex: 0x2a2d31), Color(hex: 0x1d1f22)], startPoint: .top, endPoint: .bottom))
    }
}

struct AddMenus: View {
    @EnvironmentObject var model: RackModel

    var body: some View {
        let inputs = model.hardware.filter { !$0.loopcraft && $0.inputs > 0 }
        let outputs = model.hardware.filter { !$0.loopcraft && $0.outputs > 0 }
        Menu("Add Input") {
            Button("App…") { pickApp() }
            Button("Virtual Input Device") {
                var p = RackModel.port("virtual_device", name: model.freshName("Virtual In"), channels: 2)
                p.gain_db = 0
                model.addInput(p)
            }
            if model.device?.inputs.contains(where: { $0.kind == "virtual" }) == false {
                Button("System (Virtual Inputs)") { model.addInput(RackModel.port("virtual")) }
            }
            Divider()
            Section("Input Devices") {
                ForEach(inputs, id: \.uid) { hw in
                    Button("\(hw.name)  ·  \(hw.inputs) ch") { model.addDevice(hw, input: true) }
                }
            }
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        Menu("Add Output") {
            Button("Virtual Output Device") {
                model.addOutput(RackModel.port("virtual_device", name: model.freshName("Virtual Out"), channels: 2, output: true))
            }
            Divider()
            Section("Output Devices") {
                ForEach(outputs, id: \.uid) { hw in
                    Button("\(hw.name)  ·  \(hw.outputs) ch") { model.addDevice(hw, input: false) }
                }
            }
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
    }

    func pickApp() {
        let panel = NSOpenPanel()
        panel.directoryURL = URL(fileURLWithPath: "/Applications")
        panel.allowedContentTypes = [.applicationBundle]
        panel.prompt = "Add"
        panel.message = "Choose the app whose sound goes into the mixer"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let name = (Bundle(url: url)?.object(forInfoDictionaryKey: "CFBundleName") as? String) ?? url.deletingPathExtension().lastPathComponent
        var p = RackModel.port("app", name: name)
        p.mute_original = true
        model.addInput(p)
    }
}

/// `LoopCraft --snapshot DIR [--lit FRACTION]`: draws the front and back of
/// the rack to DIR/front.png and DIR/back.png in a window that is never
/// shown, then quits (for checking a layout without touching the screen).
/// `--lit` lights every meter to that point of its scale.
@MainActor
enum Snapshot {
    static func runIfAsked() {
        let args = CommandLine.arguments
        guard let at = args.firstIndex(of: "--snapshot"), args.indices.contains(at + 1) else { return }
        let dir = URL(fileURLWithPath: args[at + 1])
        if let k = args.firstIndex(of: "--lit"), args.indices.contains(k + 1) { MeterNSView.demo = Double(args[k + 1]) }
        _ = NSApplication.shared
        NSApp.setActivationPolicy(.prohibited)
        NSApp.appearance = NSAppearance(named: .darkAqua)
        let model = RackModel()
        guard let d = model.device else { exit(1) }
        for (name, back) in [("front", false), ("back", true)] {
            model.showBack = back
            let view = ZStack { if back { BackView(device: d) } else { FrontView(device: d) } }
                .environmentObject(model).environmentObject(model.meters)
                .preferredColorScheme(.dark)
            let host = NSHostingView(rootView: view)
            let size = CGSize(width: Rack.width, height: model.layout(d).height)
            let window = NSWindow(contentRect: CGRect(origin: CGPoint(x: -20000, y: -20000), size: size), styleMask: .borderless, backing: .buffered, defer: false)
            window.contentView = host
            host.frame = CGRect(origin: .zero, size: size)
            host.layoutSubtreeIfNeeded()
            RunLoop.main.run(until: Date().addingTimeInterval(0.3))
            guard let rep = host.bitmapImageRepForCachingDisplay(in: host.bounds) else { exit(1) }
            host.cacheDisplay(in: host.bounds, to: rep)
            try? rep.representation(using: .png, properties: [:])?.write(to: dir.appendingPathComponent(name + ".png"))
        }
        exit(0)
    }
}
