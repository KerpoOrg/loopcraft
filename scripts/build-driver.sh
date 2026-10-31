#!/usr/bin/env bash
# Builds build/LoopCraft.driver from the loopcraft-hal crate and signs it ad hoc.
# Installs nothing.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"

cargo build --release -p loopcraft-hal --target aarch64-apple-darwin

bundle="build/LoopCraft.driver"
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS"
sed "s/@VERSION@/$version/g" packaging/Info.plist > "$bundle/Contents/Info.plist"
cp target/aarch64-apple-darwin/release/libloopcraft_hal.dylib "$bundle/Contents/MacOS/LoopCraft"
# Neutral install name instead of the build machine's path.
install_name_tool -id "@rpath/LoopCraft" "$bundle/Contents/MacOS/LoopCraft"

plutil -lint "$bundle/Contents/Info.plist" >/dev/null
codesign --force --sign - --timestamp=none "$bundle"
codesign --verify --strict --verbose=1 "$bundle"

echo "built $bundle ($version)"
