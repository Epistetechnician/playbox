//! `playbox hub`: start the Playbox hub (the dashboard app with passkey sign-in and invite pages) on this machine,
//! and, if you want, put it on the internet through Cloudflare. The hub itself is the Python program in `ui/`;
//! this finds it, asks how you want to reach it (once, then remembers), and runs it.

use crate::cli::{HubArgs, ServiceAction};
use crate::service;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How the hub is reached.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum Reach {
    /// Only this computer (http://localhost).
    Local,
    /// A Cloudflare quick tunnel: free, no account, a new random address each run.
    Quick,
    /// Your own Cloudflare tunnel: a stable address, so passkeys survive restarts.
    Named { tunnel: String, hostname: String },
}

pub(crate) fn home() -> PathBuf {
    std::env::var_os("PLAYBOX_HOME").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".playbox"),
        PathBuf::from,
    )
}

fn config_path() -> PathBuf {
    home().join("hub.json")
}

pub(crate) fn load() -> Option<Reach> {
    serde_json::from_str(&std::fs::read_to_string(config_path()).ok()?).ok()
}

pub(crate) fn save(r: &Reach) -> Result<()> {
    let p = config_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(r)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// `ui/playbox_hub.py`, from (in order) --hub-dir / `PLAYBOX_HUB_DIR`, the installer's directory, or a checkout
/// above the working directory or this binary.
pub(crate) fn find_hub(explicit: Option<&str>) -> Result<PathBuf> {
    let has = |d: &Path| d.join("ui/playbox_hub.py").is_file().then(|| d.join("ui/playbox_hub.py"));
    if let Some(d) = explicit {
        return has(Path::new(d)).with_context(|| format!("{d}/ui/playbox_hub.py is missing"));
    }
    let installed = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share/playbox-hub");
    let roots = [std::env::var_os("PLAYBOX_ROOT").map(PathBuf::from), Some(installed)];
    let walks = [std::env::current_dir().ok(), std::env::current_exe().ok()];
    roots
        .into_iter()
        .flatten()
        .find_map(|d| has(&d))
        .or_else(|| walks.into_iter().flatten().find_map(|s| s.ancestors().find_map(has)))
        .context(
            "cannot find the hub. Install it with\n  curl -fsSL https://raw.githubusercontent.com/Epistetechnician/playbox/main/install.sh | sh -s -- --hub --no-start\n\
             or run this from a Playbox checkout, or point PLAYBOX_HUB_DIR at one",
        )
}

pub(crate) fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

/// A DNS name: letters, digits, dots and hyphens, at least one dot.
fn valid_hostname(h: &str) -> bool {
    h.len() <= 253
        && h.contains('.')
        && !h.starts_with(['.', '-'])
        && !h.ends_with(['.', '-'])
        && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// A tunnel name or id.
fn valid_tunnel(t: &str) -> bool {
    !t.is_empty() && t.len() <= 64 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn looks_like_id(t: &str) -> bool {
    t.len() == 36 && t.chars().filter(|&c| c == '-').count() == 4 && t.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

pub(crate) fn ask(prompt: &str, default: &str) -> Result<String> {
    let mut out = std::io::stderr();
    if default.is_empty() {
        write!(out, "{prompt}: ")?;
    } else {
        write!(out, "{prompt} [{default}]: ")?;
    }
    out.flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    Ok(if line.is_empty() { default.to_string() } else { line.to_string() })
}

pub(crate) fn confirm(prompt: &str) -> Result<bool> {
    Ok(matches!(ask(&format!("{prompt} (y/N)"), "")?.to_lowercase().as_str(), "y" | "yes"))
}

pub(crate) fn cloudflared() -> Result<PathBuf> {
    on_path("cloudflared").context("Cloudflare needs `cloudflared`: brew install cloudflared (or https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/)")
}

fn run_cf(cf: &Path, args: &[&str]) -> Result<String> {
    let o = Command::new(cf).args(args).stdin(Stdio::null()).output().with_context(|| format!("could not run cloudflared {}", args.join(" ")))?;
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    if !o.status.success() {
        bail!("cloudflared {} failed: {}", args.join(" "), text.trim().lines().last().unwrap_or(""));
    }
    Ok(text)
}

/// The id out of `cloudflared tunnel create`'s "Created tunnel NAME with id UUID".
fn created_id(text: &str) -> Option<String> {
    let rest = text.split("with id ").nth(1)?;
    let id: String = rest.chars().take_while(|c| c.is_ascii_hexdigit() || *c == '-').collect();
    looks_like_id(&id).then_some(id)
}

/// The id of the tunnel called `name` in `cloudflared tunnel list -o json`.
fn listed_id(json: &str, name: &str) -> Option<String> {
    serde_json::from_str::<Value>(json)
        .ok()?
        .as_array()?
        .iter()
        .find(|t| t["name"] == name || t["id"] == name)
        .and_then(|t| t["id"].as_str().map(str::to_string))
}

/// Find, or with a yes create, the named tunnel and point `hostname` at it. Returns the tunnel id.
fn named_tunnel(name: &str, hostname: &str) -> Result<String> {
    let cf = cloudflared()?;
    if looks_like_id(name) {
        return Ok(name.to_string());
    }
    let listing = run_cf(&cf, &["tunnel", "list", "-o", "json"]).context(
        "couldn't list your tunnels; sign in once with `cloudflared tunnel login`, or give the tunnel's id instead of its name",
    )?;
    if let Some(id) = listed_id(&listing, name) {
        eprintln!("Using your tunnel {name} ({id}).");
        return Ok(id);
    }
    if !confirm(&format!("No tunnel called {name}. Create it and route {hostname} to it?"))? {
        bail!("not creating the tunnel; run again with a tunnel that exists");
    }
    let made = run_cf(&cf, &["tunnel", "create", name])?;
    let id = created_id(&made).context("cloudflared created a tunnel but I couldn't read its id from the output")?;
    run_cf(&cf, &["tunnel", "route", "dns", &id, hostname])?;
    eprintln!("Created tunnel {name} ({id}) and routed {hostname} to it.");
    Ok(id)
}

pub(crate) fn ask_reach() -> Result<Reach> {
    eprintln!(
        "How do you want to reach the hub?\n  1) Only on this computer (default)\n  2) From anywhere over HTTPS, Cloudflare quick tunnel: free, no account, the address changes each run\n  \
         3) From anywhere over HTTPS, your own Cloudflare tunnel: a stable address, so passkeys survive restarts"
    );
    match ask("Choose", "1")?.as_str() {
        "2" => {
            cloudflared()?;
            Ok(Reach::Quick)
        }
        "3" => {
            cloudflared()?;
            let tunnel = ask("Tunnel name (or id)", "")?;
            let hostname = ask("Public hostname routed to it (e.g. hub.example.com)", "")?;
            if !valid_tunnel(&tunnel) || !valid_hostname(&hostname) {
                bail!("that tunnel name or hostname doesn't look right");
            }
            Ok(Reach::Named { tunnel: named_tunnel(&tunnel, &hostname)?, hostname })
        }
        _ => Ok(Reach::Local),
    }
}

/// Flags win; then what was chosen last time; then ask (on a terminal); else local only. Returns (reach, remember it).
pub(crate) fn choose(a: &HubArgs, saved: Option<Reach>, tty: bool) -> Result<(Reach, bool)> {
    if a.hostname.is_some() && a.tunnel.is_none() {
        bail!("--hostname goes with --tunnel");
    }
    if let Some(t) = &a.tunnel {
        let h = a.hostname.clone().context("--tunnel needs --hostname (the public address routed to it)")?;
        if !valid_tunnel(t) || !valid_hostname(&h) {
            bail!("that tunnel name or hostname doesn't look right");
        }
        return Ok((Reach::Named { tunnel: t.clone(), hostname: h }, false));
    }
    if a.cloudflare {
        return Ok((Reach::Quick, false));
    }
    if a.local {
        return Ok((Reach::Local, false));
    }
    if let (Some(r), false) = (saved, a.reconfigure) {
        return Ok((r, false));
    }
    if tty {
        return Ok((ask_reach()?, true));
    }
    Ok((Reach::Local, false))
}

/// The hub's own command line for this choice.
pub(crate) fn hub_args(reach: &Reach, a: &HubArgs) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(p) = a.port {
        v.extend(["--port".to_string(), p.to_string()]);
    }
    if a.lan {
        v.push("--lan".into());
    }
    match reach {
        Reach::Local => {}
        Reach::Quick => v.push("--cloudflare".into()),
        Reach::Named { tunnel, hostname } => {
            v.extend(["--cloudflare-tunnel".into(), tunnel.clone(), "--cloudflare-hostname".into(), hostname.clone()]);
        }
    }
    if !a.no_open {
        v.push("--open".into());
    }
    if a.pair {
        v.push("--pair".into());
    }
    v.extend(a.extra.iter().cloned());
    v
}

/// The directory holding this binary first, then the caller's PATH: how the hub finds `playbox`, `playbox-mcp`, `cloudflared`.
pub(crate) fn hub_path() -> std::ffi::OsString {
    let mut paths = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        paths.push(dir);
    }
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    std::env::join_paths(paths).unwrap_or_default()
}

/// What the background service runs: the hub, never opening a browser.
pub(crate) fn service_spec(python: &Path, script: &Path, reach: &Reach, a: &HubArgs) -> service::Spec {
    let mut quiet = a.clone();
    quiet.no_open = true;
    quiet.pair = false;
    let mut argv = vec![python.to_string_lossy().into_owned(), script.to_string_lossy().into_owned()];
    argv.extend(hub_args(reach, &quiet));
    let mut env = vec![("PATH".to_string(), hub_path().to_string_lossy().into_owned())];
    for k in ["PLAYBOX_HOME", "PLAYBOX_HUB_HOME", "PLAYBOX_HUB_PORT"] {
        if let Some(v) = std::env::var_os(k).and_then(|v| v.into_string().ok()).filter(|v| !v.is_empty()) {
            env.push((k.to_string(), v)); // a custom state directory or port chosen for this run must hold for the service too
        }
    }
    service::Spec { argv, env, log: home().join("hub.log"), port: a.port.unwrap_or(7717) }
}

fn manage_service(a: &HubArgs, action: ServiceAction) -> Result<()> {
    match action {
        ServiceAction::Status => {
            let (running, detail) = service::status();
            println!("hub service ({}): {detail}\n  definition: {}\n  log: {}", service::label(), service::unit_path().display(), home().join("hub.log").display());
            if !running {
                std::process::exit(1);
            }
            Ok(())
        }
        ServiceAction::Uninstall => {
            println!("{}", if service::uninstall()? { "removed the hub service" } else { "the hub service wasn't installed" });
            Ok(())
        }
        ServiceAction::Install => {
            let script = find_hub(a.hub_dir.as_deref())?;
            let python = on_path("python3").context("the hub needs python3")?;
            let tty = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
            let (reach, remember) = choose(a, load(), tty)?;
            if remember {
                save(&reach)?;
            }
            let spec = service_spec(&python, &script, &reach, a);
            if service::installed() && !service::up_to_date(&spec) && !(tty && confirm("A different hub service is already installed. Replace it (the old one is kept as a .bak)?")?) {
                bail!("not replacing the existing service at {}", service::unit_path().display());
            }
            let path = service::install(&spec)?;
            println!("hub service installed: {}\nit starts at login and restarts if it stops. Log: {}", path.display(), spec.log.display());
            Ok(())
        }
    }
}

pub fn run(a: &HubArgs) -> Result<()> {
    if let Some(action) = a.service {
        return manage_service(a, action);
    }
    let script = find_hub(a.hub_dir.as_deref())?;
    let python = on_path("python3").context("the hub needs python3 (macOS: xcode-select --install; Debian/Ubuntu: apt install python3)")?;
    let tty = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let (reach, remember) = choose(a, load(), tty)?;
    if remember {
        save(&reach)?;
        eprintln!("Remembered. Change it later with `playbox hub --reconfigure`.");
    }
    let port = a.port.unwrap_or(7717);
    match &reach {
        Reach::Local => eprintln!("Starting the hub at http://localhost:{port} (this computer only). Leave this window open."),
        Reach::Quick => eprintln!("Starting the hub on http://localhost:{port} and a Cloudflare quick tunnel. The public address appears below. Leave this window open."),
        Reach::Named { hostname, .. } => eprintln!("Starting the hub on http://localhost:{port} and https://{hostname}. Leave this window open."),
    }
    let mut cmd = Command::new(python);
    cmd.arg(&script).args(hub_args(&reach, a));
    cmd.env("PATH", hub_path());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = cmd.exec(); // replaces this process, so Ctrl-C reaches the hub directly
        bail!("could not start the hub: {e}")
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status().context("could not start the hub")?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> HubArgs {
        HubArgs {
            port: None,
            cloudflare: false,
            tunnel: None,
            hostname: None,
            local: false,
            lan: false,
            no_open: false,
            pair: false,
            reconfigure: false,
            hub_dir: None,
            service: None,
            extra: vec![],
        }
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_hostname("hub.example.com"));
        for bad in ["localhost", "-a.com", "a b.com", "a.com/x", "a.com.", "", "a;b.com"] {
            assert!(!valid_hostname(bad), "{bad}");
        }
        assert!(valid_tunnel("my-hub_1"));
        for bad in ["", "a b", "a;b", "../x", &"x".repeat(65)] {
            assert!(!valid_tunnel(bad), "{bad}");
        }
        assert!(looks_like_id("97e2e2d7-9cb1-4504-98bb-8e97b5a16531"));
        assert!(!looks_like_id("my-hub"));
    }

    #[test]
    fn cloudflared_output_is_read() {
        let out = "Tunnel credentials written to /x.json.\nCreated tunnel playbox with id 97e2e2d7-9cb1-4504-98bb-8e97b5a16531\n";
        assert_eq!(created_id(out).as_deref(), Some("97e2e2d7-9cb1-4504-98bb-8e97b5a16531"));
        assert_eq!(created_id("nothing useful"), None);
        let list = r#"[{"id":"aaaaaaaa-1111-2222-3333-bbbbbbbbbbbb","name":"hub"},{"id":"cccccccc-1111-2222-3333-dddddddddddd","name":"other"}]"#;
        assert_eq!(listed_id(list, "other").as_deref(), Some("cccccccc-1111-2222-3333-dddddddddddd"));
        assert_eq!(listed_id(list, "missing"), None);
        assert_eq!(listed_id("not json", "hub"), None);
    }

    #[test]
    fn the_choice_follows_flags_then_memory_then_the_terminal() {
        let named = Reach::Named { tunnel: "t".into(), hostname: "h.example.com".into() };
        // no terminal, nothing saved: local only, never blocks on a prompt
        assert_eq!(choose(&args(), None, false).unwrap(), (Reach::Local, false));
        // remembered
        assert_eq!(choose(&args(), Some(named.clone()), false).unwrap(), (named.clone(), false));
        // flags beat memory, and are not remembered
        let mut a = args();
        a.local = true;
        assert_eq!(choose(&a, Some(named.clone()), false).unwrap(), (Reach::Local, false));
        let mut a = args();
        a.cloudflare = true;
        assert_eq!(choose(&a, Some(named), false).unwrap(), (Reach::Quick, false));
        let mut a = args();
        a.tunnel = Some("my-hub".into());
        assert!(choose(&a, None, false).is_err(), "a tunnel needs its hostname");
        a.hostname = Some("hub.example.com".into());
        assert_eq!(
            choose(&a, None, false).unwrap().0,
            Reach::Named { tunnel: "my-hub".into(), hostname: "hub.example.com".into() }
        );
        a.hostname = Some("bad host".into());
        assert!(choose(&a, None, false).is_err());
        let mut a = args();
        a.hostname = Some("hub.example.com".into());
        assert!(choose(&a, None, false).is_err(), "a hostname alone means nothing");
    }

    #[test]
    fn the_hub_gets_the_right_flags() {
        let mut a = args();
        a.port = Some(8000);
        a.lan = true;
        a.extra = vec!["--no-weak".into()];
        assert_eq!(hub_args(&Reach::Local, &a), ["--port", "8000", "--lan", "--open", "--no-weak"]);
        a.no_open = true;
        a.pair = true;
        assert_eq!(hub_args(&Reach::Quick, &a), ["--port", "8000", "--lan", "--cloudflare", "--pair", "--no-weak"]);
        let named = Reach::Named { tunnel: "id".into(), hostname: "h.example.com".into() };
        assert_eq!(
            hub_args(&named, &args()),
            ["--cloudflare-tunnel", "id", "--cloudflare-hostname", "h.example.com", "--open"]
        );
    }

    #[test]
    fn the_hub_is_found_where_it_was_installed() {
        let d = std::env::temp_dir().join(format!("pbhub-find-{}", std::process::id()));
        std::fs::create_dir_all(d.join("ui")).unwrap();
        assert!(find_hub(Some(d.to_str().unwrap())).is_err());
        std::fs::write(d.join("ui/playbox_hub.py"), "").unwrap();
        assert_eq!(find_hub(Some(d.to_str().unwrap())).unwrap(), d.join("ui/playbox_hub.py"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_choice_round_trips_through_the_config_file() {
        let r = Reach::Named { tunnel: "t".into(), hostname: "h.example.com".into() };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""mode":"named""#));
        assert_eq!(serde_json::from_str::<Reach>(&s).unwrap(), r);
        assert_eq!(serde_json::from_str::<Reach>(r#"{"mode":"quick"}"#).unwrap(), Reach::Quick);
    }
}
