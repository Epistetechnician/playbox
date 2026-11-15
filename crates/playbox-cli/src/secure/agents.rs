//! Checks on installed binaries, shell history and agent configuration.

use super::machine::{home_dir, mode_of, out_of};
use super::{Check, Level, Opts};
use crate::{setup, wizard};
use playbox_core::{attest, platform, store};
use std::path::PathBuf;
use std::time::SystemTime;

pub(super) fn binary_checks(opts: &Opts, out: &mut Vec<Check>) {
    let Ok(exe) = std::env::current_exe().and_then(std::fs::canonicalize) else {
        return;
    };
    let s = exe.to_string_lossy();
    if s.contains("/target/") {
        out.push(
            Check::new(
                "binary",
                "build-tree",
                "Installed binary",
                Level::Warn,
                format!("{s} is a build-tree binary; it changes whenever you rebuild"),
            )
            .hint("cargo install --path crates/playbox-cli --locked --force"),
        );
    } else {
        out.push(Check::new(
            "binary",
            "build-tree",
            "Installed binary",
            Level::Ok,
            format!("{s}"),
        ));
    }
    let bad_perm = mode_of(&exe).is_some_and(|m| m & 0o022 != 0)
        || exe
            .parent()
            .and_then(mode_of)
            .is_some_and(|m| m & 0o022 != 0);
    if bad_perm {
        out.push(Check::new("binary", "writable", "Binary permissions", Level::Bad, "the binary or its folder can be changed by other accounts; they could swap it for something else").hint(format!("chmod go-w {s} and its folder")));
    } else {
        out.push(Check::new(
            "binary",
            "writable",
            "Binary permissions",
            Level::Ok,
            "only you can change it",
        ));
    }
    let dirs: Vec<PathBuf> =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    if let Some(other) = wizard::shadowed(&dirs, &exe) {
        out.push(
            Check::new(
                "binary",
                "shadow",
                "Which playbox runs",
                Level::Warn,
                format!("your shell runs {} first, not this one", other.display()),
            )
            .hint("remove the old file, then copy this one over it"),
        );
    } else {
        out.push(Check::new(
            "binary",
            "shadow",
            "Which playbox runs",
            Level::Ok,
            "the one on your PATH is this one",
        ));
    }
    if opts.online {
        match out_of(&s, &["update", "--check", "--json"])
            .and_then(|t| serde_json::from_str::<serde_json::Value>(t.trim()).ok())
        {
            Some(v) if v["update_available"] == true => out.push(
                Check::new(
                    "binary",
                    "update",
                    "Updates",
                    Level::Warn,
                    format!(
                        "playbox {} is available (you have {})",
                        v["latest"].as_str().unwrap_or("?"),
                        env!("CARGO_PKG_VERSION")
                    ),
                )
                .hint("playbox update"),
            ),
            Some(_) => out.push(Check::new(
                "binary",
                "update",
                "Updates",
                Level::Ok,
                "you are on the latest release",
            )),
            None => out.push(Check::new(
                "binary",
                "update",
                "Updates",
                Level::Info,
                "couldn't reach the release server",
            )),
        }
    }
}

pub(super) fn history_checks(out: &mut Vec<Check>) {
    let dir = store::home().join("history");
    let mut logs: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if logs.is_empty() {
        out.push(Check::new(
            "history",
            "chains",
            "Signed history",
            Level::Info,
            "no history logs yet",
        ));
        return;
    }
    logs.sort_by(|a, b| b.0.cmp(&a.0));
    logs.truncate(40);
    let pins = match platform::PinStore::load(&platform::pins_path(&store::home())) {
        Ok(p) => p,
        Err(e) => {
            return out.push(Check::new(
                "history",
                "chains",
                "Signed history",
                Level::Warn,
                format!("couldn't load pinned devices: {e}"),
            ))
        }
    };
    let Ok(boot) = platform::boot_reference_from_env() else {
        return;
    };
    let Ok(vs) = platform::verifiers(pins, boot) else {
        return;
    };
    let refs: Vec<&dyn attest::PlatformVerifier> =
        vs.iter().map(std::convert::AsRef::as_ref).collect();
    let (mut ok, mut broken) = (0, Vec::new());
    for (_, p) in &logs {
        match store::verify_deep(p, &refs) {
            Ok(r) if r.chain_error.is_none() && r.problems.is_empty() => ok += 1,
            Ok(r) => broken.push(format!(
                "{}: {}",
                p.file_name().unwrap_or_default().to_string_lossy(),
                r.chain_error
                    .clone()
                    .unwrap_or_else(|| r.problems.join("; "))
            )),
            Err(e) => broken.push(format!("{}: {e}", p.display())),
        }
    }
    if broken.is_empty() {
        out.push(Check::new(
            "history",
            "chains",
            "Signed history",
            Level::Ok,
            format!("{ok} log(s): every signature and link checks out"),
        ));
    } else {
        out.push(Check::new("history", "chains", "Signed history", Level::Bad, format!("{} of {} log(s) failed: {}", broken.len(), logs.len(), broken.join(" | "))).hint("do not trust a failed log until you know why; `playbox verify <log>` shows the detail"));
    }
}

// ---------- agents ----------

pub(super) fn json_at(text: &str, path: &[&str]) -> Option<serde_json::Value> {
    let mut v: serde_json::Value = serde_json::from_str(text).ok()?;
    for k in path {
        v = v.get(*k)?.clone();
    }
    Some(v)
}

pub fn claude_cmd(text: &str) -> Option<String> {
    json_at(text, &["mcpServers", "playbox", "command"])?
        .as_str()
        .map(str::to_string)
}
pub fn opencode_cmd(text: &str) -> Option<String> {
    json_at(text, &["mcp", "playbox", "command"])?
        .as_array()?
        .first()?
        .as_str()
        .map(str::to_string)
}
pub(super) fn unquote(s: &str) -> String {
    s.trim().trim_matches(|c| c == '"' || c == '\'').to_string()
}
pub fn codex_cmd(text: &str) -> Option<String> {
    let mut inside = false;
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            inside = t == "[mcp_servers.playbox]";
        } else if inside {
            if let Some(v) = t.strip_prefix("command") {
                return Some(unquote(v.trim_start().strip_prefix('=')?));
            }
        }
    }
    None
}
pub fn hermes_cmd(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let i = lines.iter().position(|l| l.trim() == "playbox:")?;
    lines[i + 1..]
        .iter()
        .take(6)
        .find_map(|l| l.trim().strip_prefix("command:").map(unquote))
}

pub(super) fn agent_checks(out: &mut Vec<Check>) {
    let home = home_dir();
    let me = std::env::current_exe().and_then(std::fs::canonicalize).ok();
    for agent in setup::installed_agents() {
        let (cfg, read): (PathBuf, fn(&str) -> Option<String>) = match agent {
            "claude" => (home.join(".claude.json"), claude_cmd),
            "codex" => (home.join(".codex/config.toml"), codex_cmd),
            "opencode" => (home.join(".config/opencode/opencode.json"), opencode_cmd),
            _ => (home.join(".hermes/config.yaml"), hermes_cmd),
        };
        let id = format!("agent-{agent}");
        let label = agent.to_string();
        let Some(cmd) = std::fs::read_to_string(&cfg).ok().as_deref().and_then(read) else {
            out.push(
                Check::new(
                    "agents",
                    &id,
                    &label,
                    Level::Warn,
                    "installed but not connected to Playbox",
                )
                .hint(format!("playbox setup --only {agent}")),
            );
            continue;
        };
        let p = PathBuf::from(&cmd);
        if !p.is_file() {
            out.push(Check::new("agents", &id, &label, Level::Bad, format!("registered to run {cmd}, which doesn't exist: the agent can't start the Playbox tools")).hint("playbox setup"));
            continue;
        }
        let writable = mode_of(&p).is_some_and(|m| m & 0o022 != 0)
            || p.parent().and_then(mode_of).is_some_and(|m| m & 0o022 != 0);
        if writable {
            out.push(Check::new("agents", &id, &label, Level::Bad, format!("{cmd} (or its folder) can be changed by other accounts; the agent would run whatever replaces it")).hint("chmod go-w on the file and its folder"));
            continue;
        }
        let cfg_open = mode_of(&cfg).is_some_and(|m| m & 0o022 != 0);
        let theirs = std::fs::canonicalize(&p).ok();
        let ver = out_of(&cmd, &["--version"]).unwrap_or_default();
        let level_detail = if cmd.contains("/target/") {
            Some(format!(
                "registered to a build-tree binary ({cmd}); it changes on every rebuild"
            ))
        } else if cfg_open {
            Some(format!(
                "{} can be edited by other accounts; someone could change which program runs",
                cfg.display()
            ))
        } else if me.is_some()
            && theirs.is_some()
            && me != theirs
            && !ver.contains(env!("CARGO_PKG_VERSION"))
        {
            Some(format!(
                "runs {} but this is {}; the agent is on a different build",
                ver.trim(),
                env!("CARGO_PKG_VERSION")
            ))
        } else {
            None
        };
        match level_detail {
            Some(d) => {
                out.push(Check::new("agents", &id, &label, Level::Warn, d).hint("playbox setup"))
            }
            None => out.push(Check::new(
                "agents",
                &id,
                &label,
                Level::Ok,
                format!("runs {cmd}, a private, current binary"),
            )),
        }
    }
}

// ---------- the hub ----------
