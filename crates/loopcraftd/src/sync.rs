//! Keeps the plug-in's device list equal to the configured one.
//!
//! The plug-in persists its list in coreaudiod's storage, so after a restart
//! it usually already matches and nothing is written. A write is attempted
//! once per desired list: if the plug-in refuses it or reads back something
//! else, the daemon reports that and waits for the next config change instead
//! of writing again and again.

use lc_spec::DeviceSpec;

/// Access to the plug-in's device list.
pub trait Plugin {
    /// `Ok(None)` when the plug-in is not loaded.
    fn read(&self) -> Result<Option<Vec<DeviceSpec>>, String>;
    fn write(&self, specs: &[DeviceSpec]) -> Result<(), String>;
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    InSync,
    Applied(String),
    NoPlugin,
    Failed(String),
    /// The last attempt for this list failed; not retried until it changes.
    GaveUp,
    /// No configuration, so nothing is enforced.
    Idle,
}

#[derive(Default)]
pub struct Syncer {
    desired: Option<Vec<DeviceSpec>>,
    failed: bool,
}

impl Syncer {
    /// `None` stops enforcing: devices stay as they are.
    pub fn set_desired(&mut self, specs: Option<Vec<DeviceSpec>>) {
        if specs != self.desired {
            self.desired = specs;
            self.failed = false;
        }
    }

    pub fn tick(&mut self, plugin: &dyn Plugin) -> Outcome {
        let Some(desired) = &self.desired else { return Outcome::Idle };
        let current = match plugin.read() {
            Ok(Some(c)) => c,
            Ok(None) => return Outcome::NoPlugin,
            Err(e) => return Outcome::Failed(format!("cannot read the plug-in's devices: {e}")),
        };
        if &current == desired {
            self.failed = false;
            return Outcome::InSync;
        }
        if self.failed {
            return Outcome::GaveUp;
        }
        let result = plugin.write(desired).and_then(|()| match plugin.read()? {
            Some(after) if &after == desired => Ok(()),
            Some(_) => Err("the plug-in accepted the list but reports a different one".into()),
            None => Err("the plug-in disappeared".into()),
        });
        match result {
            Ok(()) => Outcome::Applied(describe_changes(&current, desired)),
            Err(e) => {
                self.failed = true;
                Outcome::Failed(format!("cannot apply the device list: {e}"))
            }
        }
    }
}

/// "added X; removed Y; changed Z (channels 2 → 10)", or "reordered".
pub fn describe_changes(old: &[DeviceSpec], new: &[DeviceSpec]) -> String {
    let find = |list: &[DeviceSpec], uid: &str| list.iter().find(|d| d.uid == uid).cloned();
    let mut parts = Vec::new();
    for d in new.iter().filter(|d| find(old, &d.uid).is_none()) {
        parts.push(format!("added '{}' ({} ch)", d.name, d.channels));
    }
    for d in old.iter().filter(|d| find(new, &d.uid).is_none()) {
        parts.push(format!("removed '{}'", d.name));
    }
    for d in new {
        let Some(o) = find(old, &d.uid) else { continue };
        let mut what = Vec::new();
        if o.name != d.name {
            what.push(format!("name '{}' → '{}'", o.name, d.name));
        }
        if o.channels != d.channels {
            what.push(format!("channels {} → {}", o.channels, d.channels));
        }
        if o.pass_thru != d.pass_thru {
            what.push(format!("pass_thru {} → {}", o.pass_thru, d.pass_thru));
        }
        if o.sample_rate != d.sample_rate {
            what.push(format!("sample_rate {} → {}", o.sample_rate, d.sample_rate));
        }
        if o.apps != d.apps {
            let apps: Vec<&str> = d.apps.iter().map(|a| a.bundle_ids[0].as_str()).collect();
            what.push(format!("app slots {}", if apps.is_empty() { "none".into() } else { apps.join(", ") }));
        }
        if o.virtuals != d.virtuals {
            let names: Vec<&str> = d.virtuals.iter().map(|v| v.name.as_str()).collect();
            what.push(format!("virtual devices {}", if names.is_empty() { "none".into() } else { names.join(", ") }));
        }
        if o.apply_gain != d.apply_gain {
            what.push(if d.apply_gain { "volume applies to everything played in".into() } else { "volume left to following outputs".into() });
        }
        if !what.is_empty() {
            parts.push(format!("changed '{}' ({})", d.name, what.join(", ")));
        }
    }
    if parts.is_empty() {
        "reordered".into()
    } else {
        parts.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn spec(uid: &str, name: &str, channels: u32) -> DeviceSpec {
        DeviceSpec { uid: uid.into(), name: name.into(), channels, pass_thru: 2, sample_rate: 48_000.0, apps: vec![], apply_gain: true, virtuals: vec![] }
    }

    /// A plug-in that stores what it is given, optionally altering it.
    #[derive(Default)]
    struct Fake {
        list: RefCell<Option<Vec<DeviceSpec>>>,
        writes: Cell<u32>,
        refuse: bool,
        mangle: bool,
    }

    impl Fake {
        fn with(list: Vec<DeviceSpec>) -> Self {
            Fake { list: RefCell::new(Some(list)), ..Default::default() }
        }
    }

    impl Plugin for Fake {
        fn read(&self) -> Result<Option<Vec<DeviceSpec>>, String> {
            Ok(self.list.borrow().clone())
        }
        fn write(&self, specs: &[DeviceSpec]) -> Result<(), String> {
            self.writes.set(self.writes.get() + 1);
            if self.refuse {
                return Err("refused".into());
            }
            let mut v = specs.to_vec();
            if self.mangle {
                v.pop();
            }
            *self.list.borrow_mut() = Some(v);
            Ok(())
        }
    }

    #[test]
    fn idle_without_config() {
        let p = Fake::with(vec![spec("a", "A", 2)]);
        assert_eq!(Syncer::default().tick(&p), Outcome::Idle);
        assert_eq!(p.writes.get(), 0);
    }

    #[test]
    fn writes_only_when_different() {
        let p = Fake::with(vec![spec("a", "A", 2)]);
        let mut s = Syncer::default();
        s.set_desired(Some(vec![spec("a", "A", 2)]));
        assert_eq!(s.tick(&p), Outcome::InSync);
        assert_eq!(p.writes.get(), 0);

        s.set_desired(Some(vec![spec("a", "A", 10), spec("b", "B", 4)]));
        assert_eq!(s.tick(&p), Outcome::Applied("added 'B' (4 ch); changed 'A' (channels 2 → 10)".into()));
        assert_eq!(s.tick(&p), Outcome::InSync);
        assert_eq!(p.writes.get(), 1);
    }

    #[test]
    fn restores_after_outside_change() {
        let p = Fake::with(vec![]);
        let mut s = Syncer::default();
        s.set_desired(Some(vec![spec("a", "A", 2)]));
        assert!(matches!(s.tick(&p), Outcome::Applied(_)));
        *p.list.borrow_mut() = Some(vec![]);
        assert_eq!(s.tick(&p), Outcome::Applied("added 'A' (2 ch)".into()));
    }

    #[test]
    fn gives_up_until_the_config_changes() {
        for fake in [Fake { refuse: true, ..Fake::with(vec![]) }, Fake { mangle: true, ..Fake::with(vec![]) }] {
            let mut s = Syncer::default();
            s.set_desired(Some(vec![spec("a", "A", 2)]));
            assert!(matches!(s.tick(&fake), Outcome::Failed(_)));
            assert_eq!(s.tick(&fake), Outcome::GaveUp);
            assert_eq!(fake.writes.get(), 1);
            s.set_desired(Some(vec![spec("a", "A", 2)]));
            assert_eq!(s.tick(&fake), Outcome::GaveUp, "same list: no new attempt");
            s.set_desired(Some(vec![spec("a", "A", 4)]));
            assert!(matches!(s.tick(&fake), Outcome::Failed(_)));
            assert_eq!(fake.writes.get(), 2);
        }
    }

    #[test]
    fn missing_plugin() {
        let p = Fake::default();
        let mut s = Syncer::default();
        s.set_desired(Some(vec![]));
        assert_eq!(s.tick(&p), Outcome::NoPlugin);
    }

    #[test]
    fn describes_changes() {
        let a = spec("a", "A", 2);
        let b = spec("b", "B", 2);
        assert_eq!(describe_changes(&[a.clone(), b.clone()], &[b.clone(), a.clone()]), "reordered");
        assert_eq!(describe_changes(&[a.clone(), b.clone()], std::slice::from_ref(&a)), "removed 'B'");
        let renamed = DeviceSpec { name: "Alpha".into(), pass_thru: 0, ..a.clone() };
        assert_eq!(describe_changes(std::slice::from_ref(&a), &[renamed]), "changed 'Alpha' (name 'A' → 'Alpha', pass_thru 2 → 0)");
        let follow = DeviceSpec { apply_gain: false, ..a.clone() };
        assert_eq!(describe_changes(&[a], &[follow]), "changed 'A' (volume left to following outputs)");
    }
}
