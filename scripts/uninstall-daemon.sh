#!/usr/bin/env bash
# Stops and removes the loopcraftd LaunchAgent. LoopCraft's devices stay as
# they are (the plug-in keeps them); the config file and log are kept.
set -euo pipefail

label="org.kerpo.loopcraft.daemon"
launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
rm -f "$HOME/Library/LaunchAgents/$label.plist" "$HOME/Library/Application Support/LoopCraft/bin/loopcraftd"
echo "removed loopcraftd"
