//! Presets: named snapshots of a mixer's wiring and settings, one YAML file
//! each in `~/.config/loopcraft/presets/`, in the config file's own format.
//! A preset holds everything but the mixer's name and id, so loading one
//! changes what the mixer does without it becoming a new device for apps.

use std::path::{Path, PathBuf};

use crate::{parse, to_yaml, Config, Device};

/// Where presets are kept.
pub fn dir() -> PathBuf {
    crate::default_path().parent().map(|p| p.join("presets")).unwrap_or_else(|| PathBuf::from("presets"))
}

/// The file for a preset name. Names are what the user typed; they only
/// may not contain '/', start with '.', or be empty.
fn file(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let n = name.trim();
    if n.is_empty() || n.starts_with('.') || n.contains('/') || n.contains('\0') || n.chars().count() > 64 {
        return Err(format!("'{n}' can't be a preset name (1-64 characters, no '/', not starting with '.')"));
    }
    Ok(dir.join(format!("{n}.yaml")))
}

/// Preset names, in alphabetical order.
pub fn list(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.path().file_name()?.to_str()?.strip_suffix(".yaml").map(str::to_string))
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// Stores `device` as preset `name`, replacing one of that name.
pub fn save(dir: &Path, name: &str, device: &Device) -> Result<(), String> {
    let path = file(dir, name)?;
    let config = to_yaml(&Config { devices: vec![device.clone()] });
    // The config file's header is about editing config.yaml; a preset has its own.
    let body = config.find("devices:").map_or(config.as_str(), |i| &config[i..]);
    let yaml = format!("# LoopCraft preset “{}”: a mixer's wiring and settings. Loading it keeps the\n# mixer's name and id. Gains are in dB: 0 = unity, -inf = silent.\n{body}", name.trim());
    parse(&yaml).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".{}.tmp", name.trim()));
    std::fs::write(&tmp, yaml).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Reads preset `name`, checked like a config file.
pub fn load(dir: &Path, name: &str) -> Result<Device, String> {
    let path = file(dir, name)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut c = parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    match c.devices.len() {
        1 => Ok(c.devices.remove(0)),
        n => Err(format!("{}: a preset holds one mixer, not {n}", path.display())),
    }
}

pub fn delete(dir: &Path, name: &str) -> Result<(), String> {
    let path = file(dir, name)?;
    std::fs::remove_file(&path).map_err(|e| format!("cannot delete {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_list_load_delete() {
        let dir = std::env::temp_dir().join(format!("lc-presets-{}", std::process::id()));
        let c = parse("devices:\n  - name: M\n    id: m\n    busses: [Program, Music]\n    inputs: [{app: Spotify, wires: {left: 3, right: 4}, gain: -3}]\n").unwrap();
        let d = &c.devices[0];
        save(&dir, "DJ set", d).unwrap();
        save(&dir, "  calls ", d).unwrap();
        assert_eq!(list(&dir), ["calls", "DJ set"], "trimmed, alphabetical, temp files left out");
        assert_eq!(&load(&dir, "DJ set").unwrap(), d);
        let mut changed = d.clone();
        changed.inputs[0].muted = true;
        save(&dir, "DJ set", &changed).unwrap();
        assert!(load(&dir, "DJ set").unwrap().inputs[0].muted, "storing again replaces it");
        for bad in ["", " ", ".hidden", "a/b"] {
            assert!(save(&dir, bad, d).is_err(), "{bad:?}");
        }
        assert!(load(&dir, "missing").is_err());
        delete(&dir, "calls").unwrap();
        assert_eq!(list(&dir), ["DJ set"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
