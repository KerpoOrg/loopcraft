# LoopCraft

A free, open-source and fully offline virtual audio router and mixer for macOS (Apple Silicon, macOS 14.3+), written in Rust.

**Status: early test release (0.4.0).** Devices are defined in `~/.config/loopcraft/config.yaml`; `loopcraftd` applies saved changes within about a second, without restarting coreaudiod.

Each device is a small mixer: **inputs** (virtual inputs that apps play into, captured apps, hardware inputs) are wired onto **busses**, which recording apps receive, and busses are wired to hardware **outputs**. macOS sees it from the other side: virtual inputs are the device's output channels, busses its input channels.

- A device **without** `inputs:`/`outputs:` passes what apps play into it straight through (virtual input n → bus n), 2 IO buffers of latency (21 ms at 512 frames, 48 kHz).
- A device **with** them gets an engine in the daemon: a private aggregate of the device's hidden engine twin (clock) plus the devices it uses (drift-compensated), and one IOProc that mixes the inputs onto the busses and sends the busses to the outputs. Member streams no route uses (e.g. a hardware mixer's inputs when it is only an output) are switched off for the IOProc. Pass-Thru through the engine is bit-exact at 4 buffers (43 ms). Rewiring and gain changes apply without a dropout; adding or removing a device from a route rebuilds the aggregate (short dropout for that device).
- Hardware is never switched to another sample rate: a device at a different rate is skipped with a warning in the log.
- **App inputs** (`app: Spotify` or a bundle ID) that play into the device are captured inside the plug-in: it sees each client's output before mixing (`ProcessOutput`), copies a configured app (matched by bundle ID, helper apps included) into its own slot, and the engine twin exposes one input stream per slot. Bit-exact, and as fast as the virtual inputs: at `buffer_frames: 32` LoopCraft adds 1.3 ms, at 128 5.3 ms (measured 2026-10-30). `mute_original: true` leaves the app out of the virtual inputs. `tap: true` captures with a Core Audio process tap instead (for apps that play to other devices): ~65 ms, and it delays every other route of that device by ~43 ms.
- **Virtual devices** (`virtual: <name>` with `channels:`): playback-only devices as inputs (Discord plays into "LoopCraft Virtual Sound: Comms") and record-only devices as outputs (a DAW records "…: Daw Output"), each a device of its own on the mixer's clock: bit-exact, no added delay, wired explicitly, so nothing reaches them unless it is wired to. Each keeps its UID and object ID (by `id`, derived from the name) when others are added, removed or reordered.
- **Busses** are a list, one stereo bus each, with a name and an id: `- {id: b3f9a2c1d, name: Music}`. Wires name a bus by id (or, hand-written, by name) and side: `{5: Music.left, 6: Music.right}`; `to: Music` wires an input in stereo, `from: [Program, Music]` sends busses to an output's channels 1/2. Busses, inputs and outputs all get ids (`b…`, `i…`, `o…`) that never change, so renaming, reordering, inserting or removing a bus keeps every patch; only hardware channels are numbers. The ids are resolved to channel numbers when the file is read, so the daemon and plug-in work as before. A bus can be mono (`{id: …, name: Mic, mono: true}`, one channel, wired by its id alone: `{1: Mic}`; a stereo unit's `to:` sums both sides into it). Bus groups (`groups: [{id: g…, name: DAW}]`, and `group: DAW` on a bus) are shown by the editor as units of their own; they don't change any channel. Two more editor-only keys on inputs and outputs: `mono: [1, 2]` shows those channels as a jack and meter each instead of a stereo pair (the wires are unchanged), and `half: true` shows a stereo or mono unit half-width, two to a row. Older files (`busses: 10`, bus numbers in wires) are still read; the editor's first save converts them and keeps the old file as `config.yaml.bak-before-ids`.
- **Device gain** (the macOS volume slider and keys while the device is the system output) is in dB on a console fader law: unity at 75 %, +12 dB at the top, −∞ at the bottom. `loopcraft gain virtual -6`.

## Layout

| Path | What |
|---|---|
| `crates/lc-ring` | Time-indexed audio ring (lock-free, single writer, seqlock-validated readers) |
| `crates/lc-clock` | Host-time device clock and zero timestamps |
| `crates/lc-spec` | The plug-in's device-list format (JSON in the `'LCd#'` property), identity and limits |
| `crates/lc-ca` | Minimal Core Audio client used by the CLI and daemon |
| `crates/lc-config` | The YAML config (`~/.config/loopcraft/config.yaml`): model, strict parsing, validation, hardware-device matching. Example: `crates/lc-config/example.yaml` |
| `crates/loopcraft-hal` | The AudioServerPlugIn (`LoopCraft.driver`, bundle ID `org.kerpo.loopcraft.hal`) |
| `crates/loopcraft-cli` | `loopcraft` CLI |
| `crates/loopcraftd` | The daemon: applies the config and keeps it applied |
| `crates/lc-meter` | Live levels from the daemon to the editor (shared memory-mapped file) |
| `crates/lc-ffi` | C interface for the editor: load, validate and save the config, list devices, device volume (`include/lc_ffi.h`) |
| `apps/editor` | The rack editor (SwiftUI, Swift package), `LoopCraft.app` |
| `packaging/` | Plug-in `Info.plist`, daemon LaunchAgent template |
| `scripts/` | Build, offline check, install and uninstall |
| `tools/gen_consts.c` | Generates `loopcraft-hal/src/consts.rs` from the macOS SDK |
| `tools/plugin_harness.c` | Loads the built bundle like coreaudiod and exercises it, without installing |
| `tools/hal_probe.swift` | Read-only dump of the installed device's public properties; `hal_probe <UID>` dumps one device, including hidden engine twins |
| `tools/route_test.swift` | Plays pseudo-noise into one device, records another, reports per channel what arrived, delay, gain and bit-exactness. Use test devices, never one an app is using |

## Build and test (nothing gets installed)

```sh
cargo test --workspace                 # unit tests
scripts/build-driver.sh                # build/LoopCraft.driver, ad-hoc signed
scripts/check-offline.sh               # no networking imports allowed
clang -framework CoreFoundation -framework CoreAudio -o build/plugin_harness tools/plugin_harness.c
build/plugin_harness build/LoopCraft.driver
```

Regenerate the SDK constants after an SDK update:

```sh
clang -o build/gen_consts tools/gen_consts.c && build/gen_consts > crates/loopcraft-hal/src/consts.rs
```

## Install / uninstall

Both scripts ask first, need `sudo`, and **restart coreaudiod, which interrupts all audio briefly**.

```sh
scripts/install-driver.sh
swiftc -O -o build/hal_probe tools/hal_probe.swift && build/hal_probe
scripts/uninstall-driver.sh
```

## Configuration and daemon

```sh
loopcraft config init        # write ~/.config/loopcraft/config.yaml from the current devices
loopcraft config check       # validate it and look up its hardware devices
scripts/install-daemon.sh    # user LaunchAgent; no sudo, no coreaudiod restart
tail -f ~/Library/Logs/LoopCraft/loopcraftd.log
scripts/uninstall-daemon.sh  # devices stay as they are
```

A config with errors is logged and ignored (the last good one stays in effect); without a config file the daemon leaves the devices alone. Try a config without the LaunchAgent: `target/release/loopcraftd --config FILE` (add `--once` to apply once and exit).

## Editor

```sh
scripts/build-editor.sh            # build/LoopCraft.app; --install copies it to ~/Applications
open build/LoopCraft.app
open --env LOOPCRAFT_CONFIG=/tmp/try.yaml build/LoopCraft.app   # edit another file
```

A 19" rack: one unit per input, the mixer, one unit per output. The front has gain knobs in dB (drag; ⌥ for fine steps; double-click for 0 dB), MUTE, IN SYSTEM for apps (whether the app is also heard in the System input; `mute_original: false`), MUTE ORIG for tapped apps, MASTER (the output follows LINK MASTER), on/off; the preset unit has LINK MASTER, the mixer's own volume (what the Mac's volume keys and menu bar set). Units read left to right as the signal flows: gain, the meters after it, the route. The meters' colours change at −12 dB (yellow) and −3 dB (red), and the highest LED stays lit for 100 ms (`MeterStyle` in `Meters.swift`). The mixer shows the busses in no group; each bus group is a unit below it (right-click a bus: Show In, Split into Two Mono Busses; right-click a unit: Add Bus Group, Half Width). The back (Tab flips) has a stereo jack per channel pair, with a ST/M switch beside it: flipped to M, the pair is two mono jacks one above the other (for a unit, only how it is patched changes; for busses, a stereo bus becomes two mono busses on the same channels). Drag from jack to jack to patch (mono into stereo goes to both sides, stereo into mono sums), click a cable to unplug, click a switch to flip it. Double-click a virtual device's or the mixer's name to rename it (its id stays, so apps still recognise it). Right-click a unit to rename, move, unplug or remove it; Add Input / Add Output add apps, virtual devices and hardware. Each change is validated by the core and saved to `config.yaml` at once (knobs at most every 80 ms), and the daemon, which checks the file every 50 ms, applies it; edits made elsewhere are picked up. The first save keeps the hand-written file as `config.yaml.bak-before-editor` (the editor doesn't keep comments). The meters show the daemon's live peaks (after the gain for inputs and outputs, the busses as recording apps get them). `LoopCraft --snapshot DIR [--lit 0.9]` (the binary inside the app) draws the front and back to PNGs offscreen and quits. The daemon publishes them in a small memory-mapped file per mixer, `~/Library/Application Support/LoopCraft/meters/<id>.levels` (`crates/lc-meter`); the IO thread only does an atomic max per channel. `cargo run --release -p lc-meter --example fake_levels` feeds test levels.

### Presets

The unit at the top of the rack stores and recalls presets: a mixer's whole wiring and settings (inputs, outputs, wires, gains, mutes, busses and their names, rate, buffer), but not its name and id, so loading one never looks like a new device to apps. STORE asks for a name on the display (Enter stores, replacing a preset of that name); ◀ ▶ or a click on the display chooses one, LOAD applies it; EDITED shows the mixer has changed since. Presets are YAML files in `~/.config/loopcraft/presets/`, in the config file's format and checked by the same rules. UNPATCHED on the same unit shows or hides inputs and outputs nothing is patched to.

### Device profiles

A device profile says what a hardware model's channels carry, so the editor can label them ("Main", "FX Send 1", "Deck 1") instead of numbering them, explain them in tooltips, and patch something sensible when the device is added (`default:`). Profiles are matched by model name (and manufacturer), so they apply to every unit of that model on any port. Built-in ones are in `crates/lc-config/profiles/` (Rane FOUR so far); your own go in `~/.config/loopcraft/devices/*.yaml` and win over a built-in one. Right-click a device in the editor → Save Device Profile… writes one to edit (a copy of the built-in one, or one made from the names the device reports) and opens it. Devices without a profile are labelled with the channel names they report to macOS.

```yaml
device: Rane FOUR          # matches a device whose name contains this
manufacturer: Rane         # optional, must match too
inputs:                    # from the device into the Mac
  - {channels: [5, 6], name: Main, note: "The program mix"}
  - {channels: [7], name: Mic 1}
outputs:                   # from the Mac to the device
  - {channels: [9, 10], name: Sampler}
default:                   # patched when the device is added
  input: [5, 6]            #   onto the first bus pair nothing feeds
  output: [9, 10]          #   from bus 1/2
```

## Rules

- Built on Apple's public Core Audio APIs only.
- Offline: no network access of any kind (`scripts/check-offline.sh`).
- No licensing code.

## License

LoopCraft is free software under the GNU GPL version 3 or later (`LICENSE`), with additional terms under its section 7 (`NOTICE`): anything that ships LoopCraft shows that it is free software and where it comes from, and nobody may present it as part of a product the user paid for. You may fork it and bundle it with other software, free or paid, as a separate install; forks stay GPL and take a name of their own. The name and logo are covered by `TRADEMARKS.md`.
