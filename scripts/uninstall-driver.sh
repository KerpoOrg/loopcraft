#!/usr/bin/env bash
# Removes LoopCraft.driver and restarts coreaudiod (all audio stops briefly).
set -euo pipefail

dst="/Library/Audio/Plug-Ins/HAL/LoopCraft.driver"
[[ -d "$dst" ]] || { echo "not installed"; exit 0; }

if [[ "${1:-}" != "--yes" ]]; then
  read -r -p "Remove $dst and restart coreaudiod (all audio stops briefly)? [y/N] " answer
  [[ "$answer" == [yY] ]] || { echo "aborted"; exit 1; }
fi

sudo rm -rf "$dst"
# launchctl kickstart is refused under SIP; launchd restarts coreaudiod after a kill.
sudo killall coreaudiod
echo "removed $dst; coreaudiod restarted"
