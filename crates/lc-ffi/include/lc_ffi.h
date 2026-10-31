// C interface to the LoopCraft core, used by the macOS editor.
//
// Every function returning `char *` returns a JSON document the caller frees
// with lc_free(). Load and save always validate with the same rules as the
// daemon, so the editor can never write a configuration the daemon rejects.
#ifndef LC_FFI_H
#define LC_FFI_H

// The config path: $LOOPCRAFT_CONFIG, else ~/.config/loopcraft/config.yaml.
char *lc_config_default_path(void);

// {"ok":true,"config":{...}} or {"ok":false,"error":"..."}.
char *lc_config_load(const char *path);

// Validates config_json and writes it as YAML (atomically). Before the first
// save over a hand-written file, a copy is kept as <path>.bak-before-editor.
// {"ok":true} or {"ok":false,"error":"..."}.
char *lc_config_save(const char *path, const char *config_json);

// Audio devices present now:
// [{"uid","name","manufacturer","inputs","outputs","rate","loopcraft",
//   "input_groups":[{"channels":[5,6],"name":"Main","note":"..."}],"output_groups":[...],
//   "default_input":[5,6],"default_output":[9,10],"profile":"built-in:rane-four"|path|null}].
// Groups come from the device's profile, else from the names it reports.
char *lc_devices(void);

// Writes a profile for the device to ~/.config/loopcraft/devices/ to edit (a
// copy of its built-in one, or made from its channel names; an existing file
// is left alone): {"ok":true,"path":"..."} or {"ok":false,"error":"..."}.
char *lc_save_profile(const char *uid);

// {"dir":"~/.config/loopcraft/devices","errors":["file: why it was skipped"]}
char *lc_profiles_status(void);

// The device's output volume (the macOS slider): {"db":-12.0|null,"muted":false}
// (db null = -inf), or {"error":"..."}.
char *lc_device_volume(const char *uid);

// Sets it; db below -200 means -inf (silence). 0 on success.
int lc_set_device_volume(const char *uid, double db);

// Live levels of a mixer, by its config id: the peak of each channel since the
// last call, linear (1.0 = 0 dBFS), in config order:
// {"generation":3,"inputs":[[0.1,0.2],[]],"busses":[...],"outputs":[[...]]},
// or {"error":"..."} when the daemon isn't routing that device.
char *lc_levels(const char *device_id);

// Sets a device's sample rate when asked to (LoopCraft never does by itself):
// 0 on success, -2 if the device doesn't offer that rate, -1 on another error.
int lc_set_device_rate(const char *uid, double rate);

// Presets in ~/.config/loopcraft/presets: {"dir":"...","names":["DJ set",...]}.
char *lc_presets(void);

// Stores a mixer (an element of "devices" as lc_config_load gives them) as a
// named preset, replacing one of that name: {"ok":true} or {"ok":false,"error":"..."}.
char *lc_preset_store(const char *name, const char *device_json);

// {"ok":true,"device":{...}} (the stored mixer) or {"ok":false,"error":"..."}.
char *lc_preset_load(const char *name);

// {"ok":true} or {"ok":false,"error":"..."}.
char *lc_preset_delete(const char *name);

// Before saving a preset switch: the daemon fades the mixer's outputs to
// silence (30 ms), applies the change, and fades back in. 0 when asked, -1
// when the daemon isn't routing that mixer.
int lc_request_transition(const char *device_id);

void lc_free(char *s);

#endif
