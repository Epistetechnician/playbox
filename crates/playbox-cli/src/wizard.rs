//! `playbox init`: set Playbox up on this machine, end to end. Six steps (this machine, keys, agents, the hub,
//! the background service, what to do next). Every step looks at the current state first, so re-running is
//! safe and only changes what is missing; `--dry-run` shows the changes without making them, and `--yes` runs
//! without questions for scripts and provisioning.

use crate::cli::{HubArgs, InitArgs};
use crate::hub::{self, Reach};
use crate::{secure, service, setup};
use anyhow::{bail, Result};
use playbox_core::store;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const STEPS: usize = 7;

struct Report {
    todo: Vec<String>,
}

impl Report {
    fn step(n: usize, title: &str) {
        println!("\n[{n}/{STEPS}] {title}");
    }
    fn ok(msg: &str) {
        println!("  ok    {msg}");
    }
    fn note(msg: &str) {
        println!("  --    {msg}");
    }
    fn change(dry: bool, msg: &str) {
        println!("  {}  {msg}", if dry { "plan" } else { "done" });
    }
    fn warn(&mut self, msg: &str) {
        println!("  todo  {msg}");
        self.todo.push(msg.lines().next().unwrap_or(msg).to_string());
    }
}

fn runs(prog: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(prog).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// The first `playbox` on PATH, if it is not `exe` itself: the binary your shell runs isn't the one running this.
pub(crate) fn shadowed(path: &[PathBuf], exe: &Path) -> Option<PathBuf> {
    let me = std::fs::canonicalize(exe).ok()?;
    let first = path.iter().map(|d| d.join("playbox")).find(|p| p.is_file())?;
    (std::fs::canonicalize(&first).ok()? != me).then_some(first)
}

fn machine(r: &mut Report) -> Result<()> {
    Report::step(1, "This machine");
    let exe = std::fs::canonicalize(std::env::current_exe()?)?;
    Report::ok(&format!("playbox {} at {}", env!("CARGO_PKG_VERSION"), exe.display()));
    let dirs: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    if let Some(other) = shadowed(&dirs, &exe) {
        r.warn(&format!(
            "your shell runs {} first, not this binary; copy this one over it (remove the old file first) or delete the old one",
            other.display()
        ));
    }
    if exe.to_string_lossy().contains("/target/") {
        r.warn("this is a build-tree binary; install a release (or `cargo install --path crates/playbox-cli`) so agents keep working after a rebuild");
    }
    match runs("python3", &["--version"]) {
        Some(v) => Report::ok(&v),
        None => r.warn("python3 not found; the hub needs it (macOS: xcode-select --install; Debian/Ubuntu: apt install python3)"),
    }
    for (prog, what) in [("cloudflared", "only for reaching the hub over Cloudflare"), ("docker", "only for sandboxed tasks")] {
        if hub::on_path(prog).is_some() {
            Report::ok(&format!("{prog} ({what})"));
        } else {
            Report::note(&format!("{prog} not installed ({what})"));
        }
    }
    Ok(())
}

fn identity(dry: bool, r: &mut Report) -> Result<()> {
    Report::step(2, "Your keys");
    let home = store::home();
    let keys = ["guest.key", "session.key", "approver.key"];
    let missing: Vec<&str> = keys.iter().copied().filter(|k| !home.join(k).exists()).collect();
    if dry {
        Report::change(true, &format!("keys in {}: {}", home.display(), if missing.is_empty() { "all present".into() } else { format!("would create {}", missing.join(", ")) }));
        return Ok(());
    }
    std::fs::create_dir_all(&home)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&home)?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
            Report::change(false, &format!("{} was {mode:o}; now 700 (only you)", home.display()));
        }
    }
    let guest = playbox_net::identity_key(&home.join("guest.key"))?;
    for k in ["session.key", "approver.key"] {
        if let Err(e) = store::load_or_create_key(&home.join(k)) {
            r.warn(&format!("{k}: {e}"));
        }
    }
    if missing.is_empty() {
        Report::ok(&format!("keys present in {}", home.display()));
    } else {
        Report::change(false, &format!("created {}", missing.join(", ")));
    }
    println!("        your guest id (a host binds a join link to it so the link works only for you):\n          {}", guest.public());
    Ok(())
}

fn yes_no(a: &InitArgs, tty: bool, q: &str) -> Result<bool> {
    if a.yes || !tty {
        return Ok(true);
    }
    Ok(!matches!(hub::ask(&format!("{q} (Y/n)"), "y")?.to_lowercase().as_str(), "n" | "no"))
}

fn agents(a: &InitArgs, tty: bool, r: &mut Report) -> Result<()> {
    Report::step(3, "Your coding agents");
    let found = setup::installed_agents();
    if a.no_agents {
        Report::note("skipped (--no-agents)");
        return Ok(());
    }
    if found.is_empty() {
        Report::note("no supported agent found (Claude Code, Codex, OpenCode, Hermes); `playbox setup` later, or use Integrations in the hub for others");
        return Ok(());
    }
    if std::env::current_exe().is_ok_and(|e| e.to_string_lossy().contains("/target/")) {
        r.warn("not registering agents from a build-tree binary (they would break after a rebuild); install playbox, then run `playbox init` again");
        return Ok(());
    }
    if !yes_no(a, tty, &format!("Connect {} to Playbox?", found.join(", ")))? {
        Report::note("skipped");
        return Ok(());
    }
    if let Err(e) = setup::run_setup(setup::Opts { dry_run: a.dry_run, remove: false, only: found.iter().map(|s| (*s).to_string()).collect() }) {
        r.warn(&format!("agent setup failed: {e}"));
    }
    Ok(())
}

fn hub_args(a: &InitArgs) -> HubArgs {
    HubArgs {
        port: a.port,
        cloudflare: a.cloudflare,
        tunnel: a.tunnel.clone(),
        hostname: a.hostname.clone(),
        local: a.local,
        lan: a.lan,
        no_open: true,
        pair: false,
        reconfigure: false,
        hub_dir: a.hub_dir.clone(),
        service: None,
        extra: vec![],
    }
}

struct HubPlan {
    python: PathBuf,
    script: PathBuf,
    reach: Reach,
}

fn the_hub(a: &InitArgs, tty: bool, r: &mut Report) -> Result<Option<HubPlan>> {
    Report::step(4, "The hub");
    let script = match hub::find_hub(a.hub_dir.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            r.warn(&e.to_string());
            return Ok(None);
        }
    };
    let Some(python) = hub::on_path("python3") else {
        r.warn("the hub needs python3");
        return Ok(None);
    };
    Report::ok(&format!("hub found: {}", script.display()));
    let ha = hub_args(a);
    let (reach, remember) = match hub::choose(&ha, hub::load(), tty && !a.yes) {
        Ok(x) => x,
        Err(e) => {
            r.warn(&format!("{e:#}"));
            return Ok(None);
        }
    };
    if remember && !a.dry_run {
        hub::save(&reach)?;
    }
    let port = a.port.unwrap_or(7717);
    match &reach {
        Reach::Local => Report::ok(&format!("reachable on this computer only: http://localhost:{port}")),
        Reach::Quick => {
            Report::ok("reachable over HTTPS through a Cloudflare quick tunnel, with passkeys only (the hub refuses plain-HTTP pairing through a tunnel)");
            Report::note("a quick tunnel gets a new address every time the hub restarts; for a stable one use your own tunnel (`playbox init --reconfigure`-style: --tunnel NAME --hostname HOST)");
        }
        Reach::Named { hostname, .. } => Report::ok(&format!("reachable over HTTPS at https://{hostname}, with passkeys only (the hub refuses plain-HTTP pairing through a tunnel)")),
    }
    if a.lan {
        r.warn("--lan lets phones on this network pair over plain HTTP, which is weaker than a passkey over HTTPS; only on a network you trust");
    }
    Ok(Some(HubPlan { python, script, reach }))
}

fn log_len(p: &Path) -> usize {
    std::fs::read_to_string(p).map_or(0, |t| t.len())
}

fn background(a: &InitArgs, tty: bool, plan: Option<&HubPlan>, r: &mut Report) -> Result<()> {
    Report::step(5, "Run the hub in the background");
    let Some(plan) = plan else {
        Report::note("skipped (no hub to run)");
        return Ok(());
    };
    if a.no_service {
        Report::note("skipped (--no-service); start it in a window with `playbox hub`");
        return Ok(());
    }
    let spec = hub::service_spec(&plan.python, &plan.script, &plan.reach, &hub_args(a));
    if service::installed() && service::up_to_date(&spec) {
        let (running, detail) = service::status();
        if running && service::wait_healthy(spec.port, Duration::from_secs(3)) {
            Report::ok(&format!("already installed and running ({})", service::unit_path().display()));
            return Ok(());
        }
        Report::note(&format!("installed but {detail}; starting it"));
    } else if service::installed() {
        let interactive = !a.yes && tty;
        let at = service::unit_path();
        if a.dry_run {
            let what = if interactive { "would ask before replacing it" } else { "would leave it alone" };
            Report::change(true, &format!("a different hub service is installed at {}; {what}", at.display()));
            return Ok(());
        }
        let replace = interactive && hub::confirm(&format!("A different hub service is installed at {}. Replace it (the old one is kept as a .bak)?", at.display()))?;
        if !replace {
            r.warn(&format!("left the existing service at {} alone; replace it with `playbox hub --service install`", at.display()));
            return Ok(());
        }
    }
    if a.dry_run {
        Report::change(true, &format!("install and start {}, logging to {}", service::unit_path().display(), spec.log.display()));
        return Ok(());
    }
    if !yes_no(a, tty, "Start the hub at login and keep it running?")? {
        Report::note("skipped; start it in a window with `playbox hub`");
        return Ok(());
    }
    let before = log_len(&spec.log);
    match service::install(&spec) {
        Ok(path) => {
            Report::change(false, &format!("installed {}; it starts at login and restarts if it stops", path.display()));
            Report::ok(&format!("the hub answers on port {} (log: {})", spec.port, spec.log.display()));
            let log = std::fs::read_to_string(&spec.log).unwrap_or_default();
            let fresh = log.get(before..).unwrap_or(&log);
            let links = service::pairing_links(fresh);
            if links.is_empty() {
                println!("        sign in with your passkey at http://localhost:{}", spec.port);
            } else {
                println!("        add your first passkey with one of these (single use, 10 minutes; anyone holding a link can enrol, so don't share it):");
                for l in links {
                    println!("          {l}");
                }
            }
            if !cfg!(target_os = "macos") {
                Report::note("to keep it running when you're logged out: loginctl enable-linger $USER");
            }
        }
        Err(e) => r.warn(&format!("couldn't start the service: {e:#}")),
    }
    Ok(())
}

/// Check what was just set up, fix what is safe to fix, and fuzz a throwaway copy of the hub.
fn verify(a: &InitArgs, tty: bool, r: &mut Report) -> Result<()> {
    Report::step(6, "Verify it is secure");
    let fix = !a.dry_run && (a.yes || !tty || hub::confirm("Fix permissions that are too open (nothing else is changed)?")?);
    let opts = secure::Opts { fix, fuzz: !a.dry_run, deep: false, seed: None, online: false, port: a.port, hub_dir: a.hub_dir.clone() };
    let rep = secure::collect(&opts);
    if !a.dry_run {
        secure::save(&rep);
    }
    secure::print(&rep);
    let bad = rep.checks.iter().filter(|c| c.level == secure::Level::Bad).count() + rep.fuzz.as_ref().map_or(0, |f| f.violations.len());
    if bad > 0 {
        r.warn(&format!("{bad} security problem(s) above; read them, then run `playbox secure` until it is clean"));
    }
    println!("\n  Keep it that way: `playbox secure` re-checks in about a second and the hub's System view shows the last result; it also runs after every `playbox update`.");
    Ok(())
}

fn finish(a: &InitArgs, plan: Option<&HubPlan>, r: &Report) {
    Report::step(7, "Next");
    let port = a.port.unwrap_or(7717);
    if plan.is_some() {
        println!("  1. Open http://localhost:{port} and add your passkey (Touch ID, Face ID, Windows Hello or a security key).");
        println!("  2. Connect agents and apps in the Integrations tab; invite friends from a room's menu.");
        println!("  3. Manage the service: `playbox hub --service status|uninstall`; change how it's reached: `playbox hub --reconfigure`.");
    } else {
        println!("  Install the hub, then run `playbox init` again:\n    curl -fsSL https://raw.githubusercontent.com/Epistetechnician/playbox/main/install.sh | sh -s -- --hub --no-start");
    }
    println!("  Check this machine any time with `playbox doctor`; this wizard is safe to re-run.");
    if !r.todo.is_empty() {
        println!("\n{} thing(s) need you:", r.todo.len());
        for t in &r.todo {
            println!("  - {t}");
        }
    }
}

pub fn run(a: &InitArgs) -> Result<()> {
    if a.hostname.is_some() && a.tunnel.is_none() {
        bail!("--hostname goes with --tunnel");
    }
    let tty = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    println!("Playbox setup{}", if a.dry_run { "  (dry run: nothing is changed)" } else { "" });
    let mut r = Report { todo: vec![] };
    machine(&mut r)?;
    identity(a.dry_run, &mut r)?;
    agents(a, tty, &mut r)?;
    let plan = the_hub(a, tty, &mut r)?;
    background(a, tty, plan.as_ref(), &mut r)?;
    verify(a, tty, &mut r)?;
    finish(a, plan.as_ref(), &r);
    if r.todo.is_empty() {
        Ok(())
    } else {
        std::process::exit(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_different_playbox_earlier_on_path_is_caught() {
        let d = std::env::temp_dir().join(format!("pbwiz-{}", std::process::id()));
        let (a, b) = (d.join("a"), d.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("playbox"), "old").unwrap();
        std::fs::write(b.join("playbox"), "new").unwrap();
        let new = b.join("playbox");
        assert_eq!(shadowed(&[a.clone(), b.clone()], &new), Some(a.join("playbox")), "old one first");
        assert_eq!(shadowed(&[b.clone(), a.clone()], &new), None, "this one first");
        assert_eq!(shadowed(&[d.join("none")], &new), None, "not on PATH at all");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn init_flags_become_hub_flags() {
        let a = InitArgs {
            yes: true,
            dry_run: false,
            no_service: false,
            no_agents: false,
            local: false,
            cloudflare: false,
            tunnel: Some("t".into()),
            hostname: Some("h.example.com".into()),
            port: Some(9000),
            lan: false,
            hub_dir: None,
        };
        let h = hub_args(&a);
        assert!(h.no_open && !h.pair && h.service.is_none());
        assert_eq!((h.port, h.tunnel.as_deref(), h.hostname.as_deref()), (Some(9000), Some("t"), Some("h.example.com")));
    }
}
