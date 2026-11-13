//! The steering experiments and tools are Python projects in this repository. These commands run them
//! from the right project directory, using its virtualenv if it has one, else `uv`.

use crate::cli::{Experiment, WorldAction};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The repository root: $`PLAYBOX_ROOT`, else the nearest ancestor of the working directory or of this
/// binary that holds `steer/pyproject.toml`.
pub fn repo_root() -> Option<PathBuf> {
    if let Some(r) = std::env::var_os("PLAYBOX_ROOT") {
        return Some(PathBuf::from(r));
    }
    let starts = [std::env::current_dir().ok(), std::env::current_exe().ok()];
    starts.into_iter().flatten().find_map(|s| {
        s.ancestors()
            .find(|d| d.join("steer/pyproject.toml").exists())
            .map(Path::to_path_buf)
    })
}

fn root() -> Result<PathBuf> {
    repo_root().ok_or_else(|| anyhow::anyhow!("cannot find the Playbox repository (steer/pyproject.toml); run from inside it or set PLAYBOX_ROOT"))
}

/// Run `python <py_args>` inside `project` (a directory under the repository root).
fn python(project: &str, py_args: &[String]) -> Result<()> {
    let dir = root()?.join(project);
    if !dir.join("pyproject.toml").exists() {
        bail!("{} is missing", dir.display());
    }
    let venv = dir.join(".venv/bin/python");
    let mut cmd = if venv.exists() {
        Command::new(venv)
    } else {
        let mut c = Command::new("uv");
        c.args(["run", "--project"]).arg(&dir).arg("python");
        c
    };
    let status = cmd.args(py_args).current_dir(&dir).status().map_err(|e| {
        anyhow::anyhow!(
            "could not start python for {project} ({e}); install uv or create {project}/.venv"
        )
    })?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn strings(parts: &[&str], rest: &[String]) -> Vec<String> {
    parts
        .iter()
        .map(std::string::ToString::to_string)
        .chain(rest.iter().cloned())
        .collect()
}

/// `steer extract|random|manifest|run|probe`: the engine's own command line.
pub fn steer(sub: &str, args: &[String]) -> Result<()> {
    python("steer", &strings(&["-m", "playbox_steer", sub], args))
}

pub fn experiment(name: Experiment, args: &[String]) -> Result<()> {
    match name {
        Experiment::E1 => python("orchestrator", &strings(&["experiments/e1_gpt2.py"], args)),
        Experiment::E1Merge => python("orchestrator", &strings(&["experiments/merge_e1.py"], args)),
        Experiment::E6 | Experiment::E6b | Experiment::AuditE6 | Experiment::CacheAudit => {
            let module = match name {
                Experiment::E6 => "e6",
                Experiment::E6b => "e6b",
                Experiment::AuditE6 => "audit_e6",
                _ => "cache_audit",
            };
            python("steer", &strings(&["-m", &format!("playbox_steer.{module}")], args))
        }
    }
}

pub fn world(action: WorldAction, args: &[String]) -> Result<()> {
    match action {
        WorldAction::Demo => python("world", &strings(&["-m", "playbox_world.demo"], args)),
        WorldAction::Test => python("world", &strings(&["-m", "pytest", "-q"], args)),
    }
}
