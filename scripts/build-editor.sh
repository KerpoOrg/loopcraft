#!/usr/bin/env bash
# Builds the rack editor as build/LoopCraft.app (the Rust core linked in
# statically, ad-hoc signed). With --install, copies it to ~/Applications.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
app="$root/build/LoopCraft.app"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"

cd "$root"
cargo build --release -p lc-ffi
swift build -c release --package-path apps/editor

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "apps/editor/.build/release/LoopCraft" "$app/Contents/MacOS/LoopCraft"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>org.kerpo.loopcraft.editor</string>
  <key>CFBundleName</key><string>LoopCraft</string>
  <key>CFBundleDisplayName</key><string>LoopCraft</string>
  <key>CFBundleExecutable</key><string>LoopCraft</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticTermination</key><false/>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist" >/dev/null
codesign --force --sign - --identifier org.kerpo.loopcraft.editor "$app"
echo "built $app ($version)"

if [[ "${1:-}" == "--install" ]]; then
  mkdir -p "$HOME/Applications"
  rm -rf "$HOME/Applications/LoopCraft.app"
  cp -R "$app" "$HOME/Applications/LoopCraft.app"
  echo "installed $HOME/Applications/LoopCraft.app"
fi
