//! `playbox setup`: register this binary's MCP server with the coding agents on this machine, using each
//! agent's own mechanism, and say what is left to do. Safe to re-run; `--remove` undoes it.

use anyhow::Result;
use playbox_core::store;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const NAME: &str = "playbox";
const AGENTS: [&str; 4] = ["claude", "codex", "opencode", "hermes"];

enum Outcome {
    Done(String),
    Already,
    Removed,
    Absent,
    Manual(String),
    Failed(String),
}

pub struct Opts {
    pub dry_run: bool,
    pub remove: bool,
    pub only: Vec<String>,
}

fn on_path(prog: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(prog))
        .find(|p| p.is_file())
}

/// Run `prog args`; Some(stdout) when it succeeds.
fn out(prog: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn run(dry: bool, prog: &str, args: &[&str]) -> Result<(), String> {
    if dry {
        println!("    would run: {prog} {}", args.join(" "));
        return Ok(());
    }
    let o = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{prog} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr).trim()
        ))
    }
}

/// Agents whose CLI has `mcp get/add/remove`.
fn via_cli(
    o: &Opts,
    prog: &str,
    exe: &str,
    add: &[&str],
    remove: &[&str],
    get: &[&str],
) -> Outcome {
    let existing = out(prog, get);
    let ours = existing.as_deref().is_some_and(|t| t.contains(exe));
    if o.remove {
        if existing.is_none() {
            return Outcome::Absent;
        }
        return match run(o.dry_run, prog, remove) {
            Ok(()) => Outcome::Removed,
            Err(e) => Outcome::Failed(e),
        };
    }
    if ours {
        return Outcome::Already;
    }
    if existing.is_some() {
        // Registered with another command (an older install, say): replace it.
        if let Err(e) = run(o.dry_run, prog, remove) {
            return Outcome::Failed(e);
        }
    }
    match run(o.dry_run, prog, add) {
        Ok(()) => Outcome::Done(format!("{prog} will start `{exe} mcp`")),
        Err(e) => Outcome::Failed(e),
    }
}

fn opencode_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("opencode/opencode.json")
}

/// `OpenCode` has no `mcp add` that is script-friendly, so edit its JSON config. If it has comments or does not
/// parse, leave it alone and print the snippet.
fn opencode(o: &Opts, exe: &str) -> Outcome {
    let path = opencode_path();
    let entry = serde_json::json!({"type": "local", "command": [exe, "mcp"], "enabled": true});
    let snippet = format!(
        "add to {}:\n  \"mcp\": {{ \"playbox\": {entry} }}",
        path.display()
    );
    let mut cfg: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(t) => match serde_json::from_str(&t) {
            Ok(v) => v,
            Err(_) => {
                return Outcome::Manual(format!(
                    "{} is not plain JSON (comments?); {snippet}",
                    path.display()
                ))
            }
        },
        Err(_) if o.remove => return Outcome::Absent,
        Err(_) => serde_json::json!({"$schema": "https://opencode.ai/config.json"}),
    };
    let Some(root) = cfg.as_object_mut() else {
        return Outcome::Manual(snippet);
    };
    let mcp = root.entry("mcp").or_insert_with(|| serde_json::json!({}));
    let Some(mcp) = mcp.as_object_mut() else {
        return Outcome::Manual(snippet);
    };
    if o.remove {
        if mcp.remove(NAME).is_none() {
            return Outcome::Absent;
        }
    } else if mcp.get(NAME) == Some(&entry) {
        return Outcome::Already;
    } else {
        mcp.insert(NAME.into(), entry);
    }
    if o.dry_run {
        println!("    would write {}", path.display());
        return if o.remove {
            Outcome::Removed
        } else {
            Outcome::Done(format!("would edit {}", path.display()))
        };
    }
    let write = || -> Result<()> {
        if path.exists() {
            let bak = path.with_extension("json.playbox.bak");
            if !bak.exists() {
                std::fs::copy(&path, &bak)?;
            }
        } else if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&cfg)?)?;
        Ok(())
    };
    match write() {
        Ok(()) if o.remove => Outcome::Removed,
        Ok(()) => Outcome::Done(format!(
            "edited {} (backup: opencode.json.playbox.bak)",
            path.display()
        )),
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

fn hermes(o: &Opts, exe: &str) -> Outcome {
    let listed = out("hermes", &["mcp", "list"])
        .is_some_and(|t| t.lines().any(|l| l.split_whitespace().next() == Some(NAME)));
    if o.remove {
        if !listed {
            return Outcome::Absent;
        }
        return match run(o.dry_run, "hermes", &["mcp", "remove", NAME]) {
            Ok(()) => Outcome::Removed,
            Err(e) => Outcome::Failed(e),
        };
    }
    if listed {
        return Outcome::Already;
    }
    match run(o.dry_run, "hermes", &["mcp", "add", NAME, "--command", exe, "--args", "mcp"]) {
        Ok(()) => Outcome::Done(format!("hermes will start `{exe} mcp`")),
        Err(e) => Outcome::Manual(format!("`hermes mcp add` did not finish ({e}); add to ~/.hermes/config.yaml:\n  mcp_servers:\n    playbox:\n      command: \"{exe}\"\n      args: [\"mcp\"]")),
    }
}

/// The agents from [`AGENTS`] that are installed on this machine.
pub fn installed_agents() -> Vec<&'static str> {
    AGENTS
        .into_iter()
        .filter(|a| on_path(a).is_some() || (*a == "opencode" && opencode_path().exists()))
        .collect()
}

pub fn run_setup(o: Opts) -> Result<()> {
    let exe = std::fs::canonicalize(std::env::current_exe()?)?;
    let exe_s = exe.to_string_lossy().into_owned();
    println!(
        "playbox {} at {}{}",
        env!("CARGO_PKG_VERSION"),
        exe.display(),
        if o.dry_run {
            "  (dry run: nothing is changed)"
        } else {
            ""
        }
    );
    if !o.remove && exe_s.contains("/target/") {
        println!("note: this is a build-tree binary; install a release (or `cargo install --path crates/playbox-cli`) so agents keep working after a rebuild");
    }
    let mut restart = Vec::new();
    for agent in AGENTS {
        if !o.only.is_empty() && !o.only.iter().any(|a| a == agent) {
            continue;
        }
        let installed =
            on_path(agent).is_some() || (agent == "opencode" && opencode_path().exists());
        if !installed {
            println!("  --    {agent:<9} not installed");
            continue;
        }
        let outcome = match agent {
            "claude" => via_cli(
                &o,
                "claude",
                &exe_s,
                &["mcp", "add", "--scope", "user", NAME, "--", &exe_s, "mcp"],
                &["mcp", "remove", "--scope", "user", NAME],
                &["mcp", "get", NAME],
            ),
            "codex" => via_cli(
                &o,
                "codex",
                &exe_s,
                &["mcp", "add", NAME, "--", &exe_s, "mcp"],
                &["mcp", "remove", NAME],
                &["mcp", "get", NAME],
            ),
            "opencode" => opencode(&o, &exe_s),
            _ => hermes(&o, &exe_s),
        };
        match outcome {
            Outcome::Done(d) => {
                println!("  ok    {agent:<9} {d}");
                restart.push(agent);
            }
            Outcome::Already => println!("  ok    {agent:<9} already set up"),
            Outcome::Removed => println!("  ok    {agent:<9} removed"),
            Outcome::Absent => println!("  --    {agent:<9} was not set up"),
            Outcome::Manual(m) => println!("  todo  {agent:<9} {m}"),
            Outcome::Failed(e) => println!("  FAIL  {agent:<9} {e}"),
        }
    }
    if o.remove || o.dry_run {
        return Ok(());
    }
    if !restart.is_empty() {
        println!(
            "\nrestart {} so it loads the playbox tools.",
            restart.join(", ")
        );
    }
    let key = playbox_net::identity_key(&store::home().join("guest.key"))?;
    println!(
        "\nyour guest id (give it to a host so their link works only for you):\n  {}",
        key.public()
    );
    println!(
        "\nto join a room, tell your agent:\n  Use the playbox MCP tools. Join this room: <link>"
    );
    println!("to host one:\n  Use the playbox MCP tools. Create a room sharing <repo path> with docker true, then give me the link.");
    println!("run `playbox doctor` to see what this machine can host (docker, libkrun).");
    Ok(())
}
