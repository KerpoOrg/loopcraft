// swift-tools-version:6.0
// The LoopCraft editor: a native macOS app on top of the Rust core
// (crates/lc-ffi, linked as a static library). Build with scripts/build-editor.sh.
import Foundation
import PackageDescription

let repo = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().path

let package = Package(
    name: "LoopCraftEditor",
    platforms: [.macOS(.v15)],
    targets: [
        .systemLibrary(name: "CLoopCraft", path: "Sources/CLoopCraft"),
        .executableTarget(
            name: "LoopCraft",
            dependencies: ["CLoopCraft"],
            path: "Sources/LoopCraft",
            linkerSettings: [
                .unsafeFlags(["-L\(repo)/target/release", "-llc_ffi", "-lobjc"]),
                .linkedFramework("CoreAudio"),
                .linkedFramework("CoreFoundation"),
                .linkedFramework("AppKit"),
            ]
        ),
    ],
    swiftLanguageModes: [.v5]
)
