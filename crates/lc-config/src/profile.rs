//! Device profiles: what a hardware device's channels carry, recognised by
//! model, so every unit of that model gets readable channel names and a
//! sensible first patch. Built-in profiles ship in `profiles/`; files in
//! `~/.config/loopcraft/devices/` add to them and win over a built-in one
//! for the same model. A device without a profile is described by the
//! channel names it reports to macOS.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Group {
    /// Channels, numbered from 1: one (mono) or two (stereo).
    pub channels: Vec<u32>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Device input channels patched onto the first free bus pair when the
    /// device is added as an input.
    #[serde(default)]
    pub input: Vec<u32>,
    /// Device output channels bus 1/2 is patched to when it is added as an output.
    #[serde(default)]
    pub output: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// The model: matches a device whose name contains this (any case).
    pub device: String,
    /// When given, the device's manufacturer must contain it too.
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub inputs: Vec<Group>,
    #[serde(default)]
    pub outputs: Vec<Group>,
    #[serde(default)]
    pub default: Defaults,
    /// Where it came from (set when loaded).
    #[serde(skip)]
    pub source: String,
}

const BUILT_IN: &[(&str, &str)] = &[("rane-four", include_str!("../profiles/rane-four.yaml"))];

impl Profile {
    pub fn parse(text: &str, source: &str) -> Result<Profile, String> {
        let mut p: Profile = serde_saphyr::from_str(text).map_err(|e| format!("{source}: {e}"))?;
        for g in p.inputs.iter().chain(&p.outputs) {
            if g.channels.is_empty() || g.channels.len() > 2 || g.channels.contains(&0) {
                return Err(format!("{source}: '{}' needs one or two channels, numbered from 1", g.name));
            }
        }
        p.source = source.to_string();
        Ok(p)
    }

    pub fn matches(&self, name: &str, manufacturer: &str) -> bool {
        let has = |hay: &str, needle: &str| hay.to_lowercase().contains(&needle.to_lowercase());
        has(name, &self.device) && self.manufacturer.as_deref().is_none_or(|m| has(manufacturer, m))
    }
}

/// Where your own profiles go.
pub fn user_dir() -> PathBuf {
    crate::default_path().parent().map(|p| p.join("devices")).unwrap_or_else(|| PathBuf::from("devices"))
}

/// Your profiles (with any that don't parse reported), then the built-in ones.
pub fn load(user: &Path) -> (Vec<Profile>, Vec<String>) {
    let mut profiles = Vec::new();
    let mut errors = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(user)
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "yaml" || x == "yml")).collect())
        .unwrap_or_default();
    files.sort();
    for f in files {
        match std::fs::read_to_string(&f).map_err(|e| e.to_string()).and_then(|t| Profile::parse(&t, &f.display().to_string())) {
            Ok(p) => profiles.push(p),
            Err(e) => errors.push(e),
        }
    }
    for (id, text) in BUILT_IN {
        profiles.push(Profile::parse(text, &format!("built-in:{id}")).expect("built-in profiles parse"));
    }
    (profiles, errors)
}

/// The first profile for this device.
pub fn find<'a>(profiles: &'a [Profile], name: &str, manufacturer: &str) -> Option<&'a Profile> {
    profiles.iter().find(|p| p.matches(name, manufacturer))
}

/// Groups from the names a device reports: pairs whose names differ only by
/// Left/Right become one stereo group, the rest stay mono. Channels without
/// a real name ("", "3", "Channel 3") get none.
pub fn groups_from_names(names: &[String]) -> Vec<Group> {
    let names: Vec<String> = names
        .iter()
        .map(|n| {
            let t = n.trim();
            let bare = t.strip_prefix("Channel ").unwrap_or(t);
            if bare.chars().all(|c| c.is_ascii_digit()) { String::new() } else { t.to_string() }
        })
        .collect();
    fn stem(n: &str) -> Option<(&str, bool)> {
        for (suffix, left) in [(" Left", true), (" Right", false), (" L", true), (" R", false)] {
            if let Some(s) = n.strip_suffix(suffix) {
                return Some((s, left));
            }
        }
        None
    }
    let mut groups = Vec::new();
    let mut i = 0;
    while i < names.len() {
        let ch = i as u32 + 1;
        let pair = names.get(i + 1).and_then(|r| match (stem(&names[i]), stem(r)) {
            (Some((a, true)), Some((b, false))) if a == b => Some(a.to_string()),
            _ => None,
        });
        if let Some(name) = pair {
            groups.push(Group { channels: vec![ch, ch + 1], name, note: None });
            i += 2;
            continue;
        }
        if !names[i].trim().is_empty() {
            groups.push(Group { channels: vec![ch], name: names[i].trim().to_string(), note: None });
        }
        i += 1;
    }
    groups
}

/// A profile from the names a device gives its channels.
pub fn from_names(name: &str, manufacturer: &str, inputs: &[String], outputs: &[String]) -> Profile {
    Profile {
        device: name.to_string(),
        manufacturer: (!manufacturer.is_empty()).then(|| manufacturer.to_string()),
        inputs: groups_from_names(inputs),
        outputs: groups_from_names(outputs),
        default: Defaults::default(),
        source: String::new(),
    }
}

/// A profile as a file to edit and keep.
pub fn render(p: &Profile) -> String {
    let name = &p.device;
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let group = |g: &Group| {
        let chans = g.channels.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
        match &g.note {
            Some(n) => format!("  - {{channels: [{chans}], name: {}, note: {}}}\n", q(&g.name), q(n)),
            None => format!("  - {{channels: [{chans}], name: {}}}\n", q(&g.name)),
        }
    };
    let list = |v: &[u32]| v.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
    let mut out = format!(
        "# Device profile for {name}. Name channels for what they carry and add\n\
         # notes; LoopCraft uses this for every device whose name contains\n\
         # `device:` (and whose manufacturer contains `manufacturer:`).\n\
         device: {}\n",
        q(&p.device)
    );
    if let Some(m) = &p.manufacturer {
        out += &format!("manufacturer: {}\n", q(m));
    }
    out += "inputs:            # from the device into the Mac\n";
    out += &if p.inputs.is_empty() { "  []\n".to_string() } else { p.inputs.iter().map(group).collect::<String>() };
    out += "outputs:           # from the Mac to the device\n";
    out += &if p.outputs.is_empty() { "  []\n".to_string() } else { p.outputs.iter().map(group).collect::<String>() };
    out += &format!("default:           # what is patched when the device is added\n  input: [{}]\n  output: [{}]\n", list(&p.default.input), list(&p.default.output));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn built_in_profiles_parse_and_match() {
        let (profiles, errors) = load(Path::new("/nonexistent"));
        assert!(errors.is_empty());
        let rane = find(&profiles, "Rane FOUR", "Rane").expect("Rane FOUR profile");
        assert_eq!(rane.source, "built-in:rane-four");
        assert_eq!(rane.inputs.iter().find(|g| g.channels == [5, 6]).unwrap().name, "Main");
        assert_eq!(rane.default, Defaults { input: vec![5, 6], output: vec![9, 10] });
        assert!(find(&profiles, "Rane FOUR", "Logitech").is_none(), "the manufacturer must match too");
        assert!(find(&profiles, "PRO X Wireless Gaming Headset", "Logitech").is_none());
    }

    #[test]
    fn user_profiles_win() {
        let dir = std::env::temp_dir().join(format!("lc-profiles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mine.yaml"), "device: rane four\ninputs: [{channels: [5, 6], name: Program}]\n").unwrap();
        std::fs::write(dir.join("broken.yaml"), "device: X\ninputs: [{channels: [1, 2, 3], name: Too many}]\n").unwrap();
        let (profiles, errors) = load(&dir);
        assert_eq!(find(&profiles, "Rane FOUR", "Rane").unwrap().inputs[0].name, "Program");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("one or two channels"), "{errors:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn groups_from_reported_names() {
        let g = groups_from_names(&names(&["Main Left", "Main Right", "Mic 1", "Mic 2", "", "6", "Out L", "Out R", "Channel 9"]));
        let got: Vec<(Vec<u32>, &str)> = g.iter().map(|g| (g.channels.clone(), g.name.as_str())).collect();
        assert_eq!(got, [(vec![1, 2], "Main"), (vec![3], "Mic 1"), (vec![4], "Mic 2"), (vec![7, 8], "Out")]);
    }

    #[test]
    fn templates_parse_back() {
        let mut p = from_names("Rane FOUR", "Rane", &names(&["Deck 3 Left", "Deck 3 Right"]), &names(&["Sampler Left", "Sampler Right"]));
        p.default.input = vec![1, 2];
        let p = Profile::parse(&render(&p), "t").unwrap();
        assert_eq!(p.inputs[0], Group { channels: vec![1, 2], name: "Deck 3".into(), note: None });
        assert_eq!(p.outputs[0].name, "Sampler");
        assert_eq!(p.default.input, [1, 2]);
        assert!(p.matches("Rane FOUR", "Rane"));
        // A built-in profile survives the round trip, notes and all.
        let (profiles, _) = load(Path::new("/nonexistent"));
        let rane = find(&profiles, "Rane FOUR", "Rane").unwrap();
        let mut back = Profile::parse(&render(rane), "copy").unwrap();
        back.source = rane.source.clone();
        assert_eq!(&back, rane);
    }
}
