//! `loopcraft setup`: installs what a package manager can't (the audio
//! plug-in, the daemon's LaunchAgent, the editor in ~/Applications) and
//! removes it again with `--uninstall`. Every step is explained first and
//! nothing changes without a yes; installing the plug-in needs the admin
//! password and restarts coreaudiod, which stops all audio for a few seconds.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lc_spec::BUNDLE_ID;

use crate::{ca, Failure, Result};

const PLUGIN_DIR: &str = "/Library/Audio/Plug-Ins/HAL";
const DRIVER: &str = "LoopCraft.driver";
const APP: &str = "LoopCraft.app";
const APP_ID: &str = "org.kerpo.loopcraft.editor";
const LABEL: &str = "org.kerpo.loopcraft.daemon";
const AGENT_TEMPLATE: &str = include_str!("../../../packaging/org.kerpo.loopcraft.daemon.plist");
/// Where the package was installed (`opt_prefix` in the Homebrew formula,
/// which stays the same across upgrades).
const PREFIX: Option<&str> = option_env!("LOOPCRAFT_PREFIX");
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Install,
    Uninstall,
    Status,
}

pub fn run(mode: Mode, yes: bool) -> Result<()> {
    let p = Paths::find()?;
    match mode {
        Mode::Status => {
            status(&p);
            Ok(())
        }
        Mode::Install => install(&p, yes),
        Mode::Uninstall => uninstall(&p, yes),
    }
}

/// The files to install and where they go.
struct Paths {
    /// A freshly signed copy of the plug-in (see `stage_driver`).
    driver: PathBuf,
    staging: PathBuf,
    app: PathBuf,
    daemon: PathBuf,
    home: PathBuf,
}

impl Paths {
    fn find() -> Result<Paths> {
        let home = PathBuf::from(std::env::var_os("HOME").ok_or_else(|| err("HOME is not set"))?);
        let exe = std::env::current_exe().map_err(|e| err(format!("cannot find this program: {e}")))?;
        let exe = exe.canonicalize().unwrap_or(exe);
        let bin = exe.parent().map(Path::to_path_buf).unwrap_or_default();
        // A package (prefix/bin/loopcraft, prefix/LoopCraft.driver) or a
        // source checkout (target/release/loopcraft, build/LoopCraft.driver).
        let mut roots: Vec<PathBuf> = PREFIX.map(PathBuf::from).into_iter().collect();
        if let Some(up) = bin.parent() {
            roots.push(up.to_path_buf());
            if let Some(repo) = up.parent() {
                roots.push(repo.join("build"));
            }
        }
        let root = roots
            .iter()
            .find(|r| r.join(DRIVER).is_dir())
            .ok_or_else(|| err(format!("cannot find {DRIVER} next to this program (looked in {})", list(&roots))))?;
        // The daemon by the path that survives upgrades.
        let daemon = [PREFIX.map(|p| Path::new(p).join("bin/loopcraftd")), Some(bin.join("loopcraftd"))]
            .into_iter()
            .flatten()
            .find(|d| d.is_file())
            .ok_or_else(|| err("cannot find loopcraftd next to this program"))?;
        let staging = std::env::temp_dir().join(format!("loopcraft-setup-{}", std::process::id()));
        let driver = stage_driver(&root.join(DRIVER), &staging)?;
        Ok(Paths { driver, staging, app: root.join(APP), daemon, home })
    }

    fn installed_driver(&self) -> PathBuf {
        Path::new(PLUGIN_DIR).join(DRIVER)
    }
    fn agent(&self) -> PathBuf {
        self.home.join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
    }
    fn log(&self) -> PathBuf {
        self.home.join("Library/Logs/LoopCraft/loopcraftd.log")
    }
    fn installed_app(&self) -> PathBuf {
        self.home.join("Applications").join(APP)
    }
    /// Where scripts/install-daemon.sh put the daemon before there was a package.
    fn old_daemon(&self) -> PathBuf {
        self.home.join("Library/Application Support/LoopCraft/bin/loopcraftd")
    }
}

impl Drop for Paths {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.staging);
    }
}

/// Homebrew rewrites the plug-in's install name and re-signs only its
/// binary, which breaks the bundle's signature, and coreaudiod would not load
/// it. So the plug-in is installed from a copy signed again here. Ad-hoc
/// signing is deterministic, so an unchanged plug-in signs to the same bytes
/// and is recognised as already installed.
fn stage_driver(src: &Path, staging: &Path) -> Result<PathBuf> {
    let _ = std::fs::remove_dir_all(staging);
    std::fs::create_dir_all(staging).map_err(|e| err(format!("cannot create {}: {e}", staging.display())))?;
    let dst = staging.join(DRIVER);
    let run = |cmd: &str, args: &[&std::ffi::OsStr]| Command::new(cmd).args(args).output().map(|o| o.status.success()).unwrap_or(false);
    let ok = run("/usr/bin/ditto", &[src.as_os_str(), dst.as_os_str()])
        && run("/usr/bin/codesign", &["--force".as_ref(), "--sign".as_ref(), "-".as_ref(), "--timestamp=none".as_ref(), dst.as_os_str()])
        && run("/usr/bin/codesign", &["--verify".as_ref(), "--strict".as_ref(), dst.as_os_str()]);
    if ok {
        Ok(dst)
    } else {
        Err(err(format!("could not prepare the plug-in from {} (copy and sign); try: brew reinstall loopcraft", src.display())))
    }
}

// ---------------------------------------------------------------- state

#[derive(PartialEq)]
enum Plugin {
    Missing,
    Current,
    Other(String),
}

fn plugin_state(p: &Paths) -> Plugin {
    let dst = p.installed_driver();
    if !dst.is_dir() {
        return Plugin::Missing;
    }
    let bin = |d: &Path| std::fs::read(d.join("Contents/MacOS/LoopCraft")).ok();
    if bin(&dst).is_some() && bin(&dst) == bin(&p.driver) {
        Plugin::Current
    } else {
        Plugin::Other(bundle_version(&dst).unwrap_or_else(|| "unknown version".into()))
    }
}

fn bundle_version(bundle: &Path) -> Option<String> {
    let out = Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"])
        .arg(bundle.join("Contents/Info.plist"))
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn daemon_loaded() -> bool {
    Command::new("/bin/launchctl").args(["print", &service()]).output().map(|o| o.status.success()).unwrap_or(false)
}

/// The program the LaunchAgent runs, if there is one.
fn agent_program(p: &Paths) -> Option<String> {
    let text = std::fs::read_to_string(p.agent()).ok()?;
    let start = text.find("<key>ProgramArguments</key>")?;
    let rest = &text[start..];
    let a = rest.find("<string>")? + "<string>".len();
    let b = rest[a..].find("</string>")?;
    Some(rest[a..a + b].to_string())
}

fn app_is_ours(path: &Path) -> bool {
    Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
        .arg(path.join("Contents/Info.plist"))
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == APP_ID)
        .unwrap_or(false)
}

fn status(p: &Paths) {
    println!("LoopCraft {VERSION}");
    println!();
    let plugin = match plugin_state(p) {
        Plugin::Missing => "not installed".to_string(),
        Plugin::Current => "installed, this version".to_string(),
        Plugin::Other(v) => format!("installed, a different build ({v})"),
    };
    row("Audio plug-in", &plugin, &p.installed_driver());
    let daemon = match (agent_program(p), daemon_loaded()) {
        (None, _) => "not set up".to_string(),
        (Some(prog), running) if Path::new(&prog) == p.daemon => (if running { "running" } else { "set up, not running" }).to_string(),
        (Some(prog), _) => format!("set up for another copy ({prog})"),
    };
    row("Daemon", &daemon, &p.agent());
    let app = if !p.installed_app().exists() {
        "not installed"
    } else if same_tree(&p.installed_app(), &p.app) {
        "installed, this version"
    } else {
        "installed, a different build"
    };
    row("Editor", app, &p.installed_app());
    let config = lc_config::default_path();
    row("Configuration", if config.exists() { "present" } else { "not created yet" }, &config);
}

fn row(what: &str, state: &str, path: &Path) {
    println!("  {what:<14} {state:<34} {}", path.display());
}

/// Whether the main executables of two app bundles are the same.
fn same_tree(a: &Path, b: &Path) -> bool {
    let exe = |d: &Path| std::fs::read(d.join("Contents/MacOS/LoopCraft")).ok();
    exe(a).is_some() && exe(a) == exe(b)
}

// ---------------------------------------------------------------- install

fn install(p: &Paths, yes: bool) -> Result<()> {
    preflight()?;
    status(p);
    println!();

    let plugin = plugin_state(p);
    let mut steps = vec![];
    if plugin != Plugin::Current {
        steps.push(format!(
            "{} the audio plug-in in {PLUGIN_DIR}. This needs your password, and macOS's\n     audio service (coreaudiod) restarts: all sound on this Mac stops for a few seconds.",
            if plugin == Plugin::Missing { "Install" } else { "Replace" }
        ));
    }
    steps.push(format!("Start the LoopCraft daemon at login ({}).", p.agent().display()));
    if p.app.is_dir() {
        steps.push(format!("Copy the editor to {}.", p.installed_app().display()));
    }
    if !lc_config::default_path().exists() {
        steps.push(format!("Create the configuration file {}.", lc_config::default_path().display()));
    }
    println!("Setup will:");
    for (k, s) in steps.iter().enumerate() {
        println!("  {}. {s}", k + 1);
    }
    println!();
    println!("LoopCraft never goes online. Undo all of this with: loopcraft setup --uninstall");
    println!();
    if !confirm("Continue?", yes)? {
        println!("Nothing was changed.");
        return Ok(());
    }

    if plugin != Plugin::Current {
        // The daemon stops its routes before coreaudiod goes away; it starts again below.
        let _ = Command::new("/bin/launchctl").args(["bootout", &service()]).output();
        step("Installing the audio plug-in (your password may be asked for)");
        let src = p.driver.display().to_string();
        let dst = p.installed_driver().display().to_string();
        sudo(&format!(
            "/bin/rm -rf {d} && /bin/cp -R {s} {d} && /usr/sbin/chown -R root:wheel {d} && /usr/bin/xattr -cr {d} && (/usr/bin/killall coreaudiod || true)",
            s = sh(&src),
            d = sh(&dst)
        ))?;
        wait_for_plugin();
    }

    let config = lc_config::default_path();
    if !config.exists() {
        step("Creating the configuration");
        crate::config_cmd::init(None)?;
    }

    step("Starting the daemon");
    start_daemon(p)?;

    if p.app.is_dir() {
        step("Installing the editor");
        install_app(p)?;
    }

    println!();
    println!("LoopCraft is set up.");
    println!("  Open the editor:   open {}", sh(&p.installed_app().display().to_string()));
    println!("  Daemon log:        {}", p.log().display());
    println!("  After `brew upgrade loopcraft`, run `loopcraft setup` again.");
    Ok(())
}

fn preflight() -> Result<()> {
    if !cfg!(target_arch = "aarch64") {
        return Err(err("LoopCraft needs a Mac with Apple silicon"));
    }
    let v = Command::new("/usr/bin/sw_vers").arg("-productVersion").output().map_err(|e| err(format!("sw_vers: {e}")))?;
    let v = String::from_utf8_lossy(&v.stdout).trim().to_string();
    let parts: Vec<u32> = v.split('.').map(|s| s.parse().unwrap_or(0)).collect();
    if parts.first().copied().unwrap_or(0) < 15 {
        return Err(err(format!("LoopCraft needs macOS 15 or later (this Mac has {v})")));
    }
    Ok(())
}

/// After a coreaudiod restart the plug-in takes a moment to appear.
fn wait_for_plugin() {
    for _ in 0..40 {
        if ca::plugin(BUNDLE_ID).is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    println!("  (the plug-in has not appeared yet; it may need a few more seconds)");
}

fn start_daemon(p: &Paths) -> Result<()> {
    let agent = p.agent();
    let log = p.log();
    for dir in [agent.parent(), log.parent()].into_iter().flatten() {
        std::fs::create_dir_all(dir).map_err(|e| err(format!("cannot create {}: {e}", dir.display())))?;
    }
    let plist = AGENT_TEMPLATE
        .replace("@BIN@", &xml(&p.daemon.display().to_string()))
        .replace("@LOG@", &xml(&log.display().to_string()));
    let _ = Command::new("/bin/launchctl").args(["bootout", &service()]).output();
    std::fs::write(&agent, plist).map_err(|e| err(format!("cannot write {}: {e}", agent.display())))?;
    // Right after a bootout, bootstrap can fail for a moment.
    let domain = format!("gui/{}", uid());
    for attempt in 1..=5 {
        let ok = Command::new("/bin/launchctl").args(["bootstrap", &domain]).arg(&agent).output().map(|o| o.status.success()).unwrap_or(false);
        if ok {
            break;
        }
        if attempt == 5 {
            return Err(err(format!("launchctl could not start the daemon; try: launchctl bootstrap {domain} {}", agent.display())));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    // The copy the old install script made is no longer used.
    let old = p.old_daemon();
    if old.exists() && old != p.daemon {
        let _ = std::fs::remove_file(&old);
    }
    Ok(())
}

fn install_app(p: &Paths) -> Result<()> {
    let dst = p.installed_app();
    if dst.exists() && !app_is_ours(&dst) {
        return Err(err(format!("{} exists and is not the LoopCraft editor; not replacing it", dst.display())));
    }
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir).map_err(|e| err(format!("cannot create {}: {e}", dir.display())))?;
    }
    if dst.exists() {
        std::fs::remove_dir_all(&dst).map_err(|e| err(format!("cannot remove the old {}: {e}", dst.display())))?;
    }
    let ok = Command::new("/usr/bin/ditto").arg(&p.app).arg(&dst).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        return Err(err(format!("could not copy the editor to {}", dst.display())));
    }
    Ok(())
}

// ---------------------------------------------------------------- uninstall

fn uninstall(p: &Paths, yes: bool) -> Result<()> {
    status(p);
    println!();
    let driver = p.installed_driver().is_dir();
    let mut steps = vec![format!("Stop the daemon and remove {}.", p.agent().display())];
    if p.installed_app().exists() && app_is_ours(&p.installed_app()) {
        steps.push(format!("Remove {}.", p.installed_app().display()));
    }
    if driver {
        steps.push("Remove the audio plug-in. This needs your password, and coreaudiod restarts:\n     all sound stops for a few seconds, and LoopCraft's devices disappear from apps.".into());
    }
    println!("Uninstall will:");
    for (k, s) in steps.iter().enumerate() {
        println!("  {}. {s}", k + 1);
    }
    println!();
    println!("Your configuration ({}) and the log are kept.", lc_config::default_path().display());
    println!();
    if !confirm("Continue?", yes)? {
        println!("Nothing was changed.");
        return Ok(());
    }
    step("Stopping the daemon");
    let _ = Command::new("/bin/launchctl").args(["bootout", &service()]).output();
    let _ = std::fs::remove_file(p.agent());
    let _ = std::fs::remove_file(p.old_daemon());
    if p.installed_app().exists() && app_is_ours(&p.installed_app()) {
        step("Removing the editor");
        std::fs::remove_dir_all(p.installed_app()).map_err(|e| err(format!("cannot remove {}: {e}", p.installed_app().display())))?;
    }
    if driver {
        step("Removing the audio plug-in (your password may be asked for)");
        let dst = p.installed_driver().display().to_string();
        sudo(&format!("/bin/rm -rf {d} && (/usr/bin/killall coreaudiod || true)", d = sh(&dst)))?;
    }
    println!();
    println!("LoopCraft is removed from the system. To remove the package too: brew uninstall loopcraft");
    Ok(())
}

// ---------------------------------------------------------------- helpers

fn err(msg: impl Into<String>) -> Failure {
    Failure::Error(msg.into())
}

fn list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
}

fn step(what: &str) {
    println!("==> {what}");
}

fn uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

fn service() -> String {
    format!("gui/{}/{LABEL}", uid())
}

/// Asks a yes/no question; `yes` answers it. Without a terminal and without
/// `--yes`, nothing is done.
fn confirm(question: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(err("not a terminal; run it in Terminal, or pass --yes"));
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).map_err(|e| err(e.to_string()))?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}

/// Runs a shell command as root: one password prompt for the whole command.
fn sudo(command: &str) -> Result<()> {
    let ok = Command::new("/usr/bin/sudo").args(["/bin/sh", "-c", command]).status().map(|s| s.success()).unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(err("the administrator step did not complete; nothing more was changed"))
    }
}

/// Quotes a string for /bin/sh.
fn sh(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(sh("/Library/Audio/Plug-Ins/HAL/LoopCraft.driver"), "'/Library/Audio/Plug-Ins/HAL/LoopCraft.driver'");
        assert_eq!(sh("it's"), "'it'\\''s'");
    }

    #[test]
    fn agent_template_has_both_placeholders() {
        assert!(AGENT_TEMPLATE.contains("@BIN@") && AGENT_TEMPLATE.contains("@LOG@"));
    }
}
