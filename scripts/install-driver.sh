#!/usr/bin/env bash
# Installs build/LoopCraft.driver into the HAL plug-in folder and restarts
# coreaudiod. Restarting coreaudiod interrupts ALL audio on this Mac for a few
# seconds, so this script asks before doing anything.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="$root/build/LoopCraft.driver"
dst="/Library/Audio/Plug-Ins/HAL/LoopCraft.driver"

[[ -d "$src" ]] || { echo "missing $src; run scripts/build-driver.sh first" >&2; exit 1; }
codesign --verify --strict "$src"

if [[ "${1:-}" != "--yes" ]]; then
  read -r -p "Install $dst and restart coreaudiod (all audio stops briefly)? [y/N] " answer
  [[ "$answer" == [yY] ]] || { echo "aborted"; exit 1; }
fi

sudo rm -rf "$dst"
sudo cp -R "$src" "$dst"
sudo chown -R root:wheel "$dst"
# launchctl kickstart is refused under SIP; launchd restarts coreaudiod after a kill.
sudo killall coreaudiod
echo "installed $dst; coreaudiod restarted"
