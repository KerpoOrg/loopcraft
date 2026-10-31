#!/usr/bin/env bash
# Fails if a built LoopCraft binary links networking frameworks or imports
# networking symbols. LoopCraft must never touch the network.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
bins=("$root/build/LoopCraft.driver/Contents/MacOS/LoopCraft" "$root/target/release/loopcraft" "$root/target/release/loopcraftd" "$root/build/LoopCraft.app/Contents/MacOS/LoopCraft")

status=0
for b in "${bins[@]}"; do
  [[ -f "$b" ]] || { echo "missing $b" >&2; status=1; continue; }
  if otool -L "$b" | grep -E -i 'CFNetwork|Network\.framework|WebKit|Sparkle' ; then
    echo "FAIL: $b links a networking framework" >&2; status=1
  fi
  if nm -u "$b" | grep -E '_(socket|connect|getaddrinfo|nw_[a-z_]+|CFHTTP[A-Za-z]*|CFSocket[A-Za-z]*)$|NSURLSession|NSURLConnection' ; then
    echo "FAIL: $b imports networking symbols" >&2; status=1
  fi
  [[ $status -eq 0 ]] && echo "ok: ${b#"$root"/} has no networking imports"
done
exit $status
