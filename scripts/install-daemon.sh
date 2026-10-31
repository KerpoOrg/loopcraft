#!/usr/bin/env bash
# Installs loopcraftd as a user LaunchAgent (starts at login, restarted if it
# exits). No sudo, and coreaudiod is not restarted. If there is no config yet,
# one is created from the current LoopCraft devices, so starting the daemon
# changes nothing.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
label="org.kerpo.loopcraft.daemon"
support="$HOME/Library/Application Support/LoopCraft"
bin="$support/bin/loopcraftd"
log="$HOME/Library/Logs/LoopCraft/loopcraftd.log"
plist="$HOME/Library/LaunchAgents/$label.plist"

cd "$root"
cargo build --release -p loopcraftd -p loopcraft-cli
config="$(target/release/loopcraft config | sed 's/  (does not exist yet)$//')"

if [[ "${1:-}" != "--yes" ]]; then
  echo "This installs $bin and $plist,"
  echo "and from then on loopcraftd applies $config to LoopCraft's devices."
  read -r -p "Continue? [y/N] " answer
  [[ "$answer" == [yY] ]] || { echo "aborted"; exit 1; }
fi

[[ -f "$config" ]] || target/release/loopcraft config init

mkdir -p "$(dirname "$bin")" "$(dirname "$log")" "$(dirname "$plist")"
launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
cp target/release/loopcraftd "$bin.new"
# A stable identifier, matching the embedded Info.plist, for macOS privacy permissions.
codesign --force --sign - --identifier "$label" "$bin.new"
mv "$bin.new" "$bin"
sed -e "s|@BIN@|$bin|" -e "s|@LOG@|$log|" packaging/$label.plist > "$plist"
plutil -lint "$plist" >/dev/null
# Right after a bootout, bootstrap can fail for a moment ("Input/output
# error"); without a retry the daemon would stay stopped and nothing routed.
for attempt in 1 2 3 4 5; do
  if launchctl bootstrap "gui/$(id -u)" "$plist" 2>/dev/null; then break; fi
  if [[ $attempt -eq 5 ]]; then
    echo "error: launchctl bootstrap failed; run: launchctl bootstrap gui/$(id -u) $plist" >&2
    exit 1
  fi
  sleep 1
done
launchctl print "gui/$(id -u)/$label" >/dev/null || { echo "error: $label is not loaded" >&2; exit 1; }
echo "installed; log: $log"
