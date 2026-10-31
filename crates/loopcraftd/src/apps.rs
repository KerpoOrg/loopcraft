//! From an app as written in the config ("Spotify", "com.spotify.client") to
//! the bundle IDs a tap should follow: the app's own and those of the helper
//! apps inside it, since browsers and Electron apps play audio from a helper.

use std::path::{Path, PathBuf};

/// Where apps are looked up by name.
fn app_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        ["/Applications", "/Applications/Utilities", "/System/Applications", "/System/Applications/Utilities"].iter().map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Applications"));
    }
    dirs
}

fn looks_like_bundle_id(s: &str) -> bool {
    s.contains('.') && !s.contains(char::is_whitespace) && !s.ends_with(".app")
}

/// Bundle IDs to tap for `query`: the app's first, then its helpers'.
pub fn resolve(query: &str) -> Result<Vec<String>, String> {
    resolve_in(query, &app_dirs(), &|p| lc_ca::bundle_identifier(p))
}

fn resolve_in(query: &str, dirs: &[PathBuf], id_of: &dyn Fn(&Path) -> Option<String>) -> Result<Vec<String>, String> {
    let apps = dirs.iter().filter_map(|d| std::fs::read_dir(d).ok()).flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "app"));
    let wanted = query.trim().trim_end_matches(".app");
    let found = if looks_like_bundle_id(query) {
        apps.into_iter().find(|p| id_of(p).as_deref() == Some(query))
    } else {
        apps.into_iter().find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(wanted)))
    };
    let Some(app) = found else {
        // A bundle ID is usable even when the app isn't installed where we look.
        return if looks_like_bundle_id(query) { Ok(vec![query.to_string()]) } else { Err(format!("no app named '{query}' in /Applications or ~/Applications")) };
    };
    let main = id_of(&app).ok_or_else(|| format!("{} has no bundle ID", app.display()))?;
    let mut ids = vec![main];
    let mut helpers = Vec::new();
    nested_apps(&app.join("Contents"), 0, &mut helpers);
    for h in helpers {
        if let Some(id) = id_of(&h) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    Ok(ids)
}

/// App bundles nested anywhere inside `dir`, not following symlinks.
fn nested_apps(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let Ok(kind) = e.file_type() else { continue };
        if !kind.is_dir() {
            continue;
        }
        let p = e.path();
        if p.extension().is_some_and(|x| x == "app") {
            out.push(p.clone());
        }
        nested_apps(&p, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake apps on disk; the "bundle ID" is read from a file instead of Info.plist.
    fn fake(root: &Path, rel: &str, id: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("ID"), id).unwrap();
    }

    fn id_of(p: &Path) -> Option<String> {
        std::fs::read_to_string(p.join("ID")).ok()
    }

    #[test]
    fn finds_apps_and_their_helpers() {
        let root = std::env::temp_dir().join(format!("lc-apps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        fake(&root, "Spotify.app", "com.spotify.client");
        fake(&root, "Google Chrome.app", "com.google.Chrome");
        fake(&root, "Google Chrome.app/Contents/Frameworks/GC.framework/Versions/A/Helpers/Google Chrome Helper.app", "com.google.Chrome.helper");
        fake(&root, "Google Chrome.app/Contents/Frameworks/GC.framework/Versions/A/Helpers/Google Chrome Helper (Renderer).app", "com.google.Chrome.helper.renderer");
        std::os::unix::fs::symlink(root.join("Google Chrome.app/Contents/Frameworks/GC.framework/Versions/A"), root.join("Google Chrome.app/Contents/Frameworks/GC.framework/Versions/Current")).unwrap();
        let dirs = vec![root.clone()];

        assert_eq!(resolve_in("Spotify", &dirs, &id_of).unwrap(), ["com.spotify.client"]);
        assert_eq!(resolve_in("spotify.app", &dirs, &id_of).unwrap(), ["com.spotify.client"]);
        let mut chrome = resolve_in("google chrome", &dirs, &id_of).unwrap();
        assert_eq!(chrome.remove(0), "com.google.Chrome");
        chrome.sort();
        assert_eq!(chrome, ["com.google.Chrome.helper", "com.google.Chrome.helper.renderer"], "helpers once, symlinks not followed");
        assert_eq!(resolve_in("com.google.Chrome", &dirs, &id_of).unwrap().len(), 3);
        assert_eq!(resolve_in("com.example.notinstalled", &dirs, &id_of).unwrap(), ["com.example.notinstalled"]);
        assert!(resolve_in("Focusrite Control", &dirs, &id_of).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
