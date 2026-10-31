# Incidents

## 2026-10-28: coreaudiod crash, deadlock and hangs while testing plug-in v0.2

All of these interrupted every audio device on the user's Mac. None of them happened in the offline harness, because it runs the plug-in in-process; on macOS 26, coreaudiod hosts third-party drivers in a separate driver-host process ("Core Audio Driver (…)"), and every problem below was in the coreaudiod ↔ driver-host interaction.

| # | What happened | Cause | Fix |
|---|---|---|---|
| 1 | A client write to the plug-in's custom property `'lcdv'` crashed coreaudiod (`EXC_BAD_ACCESS` in `AMCP::HAL::pack_property_data` → `CFPropertyListCreateData`). | coreaudiod reported the property as 4 bytes to clients although the plug-in can only report 8, so coreaudiod has its own idea of the all-lowercase code `'lcdv'`. Two HAL layers disagreed on the data type and one dereferenced garbage. | Property renamed to `'LCd#'`. Apple reserves all-lowercase codes, so LoopCraft codes always contain uppercase letters or `#`. Clients refuse to write unless the property is declared `'plst'` and reported as pointer-sized (`ca::set_plist_string`). |
| 2 | With `'LCd#'`, a client write hung coreaudiod; the next client request never returned. | The plug-in called the host (`PropertiesChanged`, storage write) synchronously from inside `SetPropertyData`. With the driver hosted out of process, coreaudiod was still waiting for the set to finish, so the callback deadlocked. | All host callbacks (notifications, configuration-change requests, storage writes) go through one ordered queue on a dedicated thread. The harness fails if any callback happens on the thread of a `SetPropertyData` call. |
| 3 | After installing the fix, a fresh coreaudiod did not respond. A later fresh coreaudiod spun at 100 % CPU even with LoopCraft removed. | Not determined. The disk was full at the time (≈4 GB free), which alone can wedge coreaudiod. A plausible LoopCraft contribution: the new background thread delivered "device list changed" while coreaudiod was still initialising the plug-in. | The plug-in makes no host callbacks until `Initialize` has returned (the host reads the initial state itself). The harness checks this. Free disk space was restored. |

### Rules from this

- Never call the host synchronously from inside a host → plug-in call. Never call the host before `Initialize` returns.
- Custom property selectors contain uppercase letters or `#`. Clients verify the declared type and the reported size before writing.
- New control paths are first exercised read-only on the live system; writes only after that, and only with the user's go-ahead, because a failure stops all audio on the machine.
- Recovery: `sudo killall -9 coreaudiod` (plain `killall` may not stop a deadlocked coreaudiod; `launchctl kickstart` is refused under SIP). To remove LoopCraft: `sudo rm -rf /Library/Audio/Plug-Ins/HAL/LoopCraft.driver && sudo killall -9 coreaudiod`.

## 2026-10-30: coreaudiod stopped setting up audio IO after a daemon restart

Installing daemon 0.4.0 stopped the running daemon (0.3.1) while its engine ran at `buffer_frames: 32` (aggregate of the engine twin, PRO X headset and Rane FOUR). From then on every new IOProc hung in `AudioDeviceCreateIOProcID` → `HALC_ProxyIOContext::_TellServerAboutStreamUsage` (a `mach_msg` to coreaudiod that never returned), for any device, the built-in microphone included. Property queries still worked, and coreaudiod used ~5 % CPU. The new daemon, a restarted daemon and `loopcraft level` all hung at that point; nothing was logged that a user can read. Earlier daemon restarts with the engine at 128 frames had worked.

| What happened | Cause | Fix |
|---|---|---|
| No new audio IO possible; system audio routed through LoopCraft went silent | Most likely: the daemon was killed with its route running, leaving coreaudiod to tear down the private aggregate (at a 32-frame buffer) on its own. | The daemon now stops its routes itself on SIGTERM/SIGINT before exiting (daemon 0.4.0). Four deliberate restarts at 32 frames afterwards (`launchctl kickstart -k`) each stopped cleanly, restarted the route in ~90 ms, and new IO kept working. Recovery if it recurs: `sudo killall -9 coreaudiod`. |

### Rule from this

- The daemon never leaves a running route behind: it stops its IOProcs and destroys its aggregates and taps itself before exiting. To replace a daemon that can't do that, first take the routes out of the config (the running daemon then tears them down), install, then restore the config.

## 2026-10-30: the headset dropped out after an app changed the mixer's sample rate

Around 03:49 a client (most likely `ffmpeg` recording "Daw Output" through AVFoundation) set LoopCraft Virtual Sound to 44.1 kHz. The route's aggregate followed and switched the PRO X headset to 44.1 kHz too. The daemon restored the configured 48 kHz on the mixer, but the headset stayed at 44.1 kHz, so by the rule "never change a device's sample rate" it was left out of the route: no sound on the headset until it was set back to 48 kHz by hand.

| What happened | Cause | Fix |
|---|---|---|
| Headset silent; daemon log: "runs at 44100 Hz … skipped" | Any app may change a LoopCraft device's rate, and an aggregate passes rate changes on to its hardware members. | Proposed: the plug-in offers only the configured rate, so apps can't change it (the config owns the rate). |

## 2026-10-30: adding a virtual input stopped the route (engine stream numbers shifted)

Adding `virtual: Daw` (an input) in front of the existing virtual output "Daw Output" moved Daw Output from engine stream `TwinVirt(1)` to `TwinVirt(2)`. The plug-in numbers these streams by position in the device list, so the object `TwinVirt(1)` changed from an output stream (Daw Output) to an input stream (Daw). coreaudiod kept the object's old direction, the engine's stream layout no longer matched, and the daemon could not start the route (log: "not ready yet … inputs [2, 2, 2, 2] and outputs [10, 2, 8]"). No audio on the headset or the Rane.

| What happened | Cause | Fix |
|---|---|---|
| Route not started after adding a virtual input | Engine stream objects for virtual devices were indexed by list position, so an object could change meaning and direction | Recovery: restart coreaudiod. Fix: index those streams by the virtual device's slot (stable per id and direction), like the devices themselves. |

