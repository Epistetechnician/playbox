//! Thin async wrappers over the `git` CLI for bundle sync and task worktrees.
//!
//! Every ref or commit-ish that comes from a peer goes through [`check_name`] first, so a peer
//! cannot smuggle options into a git command line.

use anyhow::{bail, Context, Result};
use std::{path::Path, process::Stdio};
use tokio::{io::AsyncWriteExt, process::Command};

/// Accept only plain ref/commit names: `[A-Za-z0-9._/-]`, no leading `-`, no `..`.
pub fn check_name(s: &str) -> Result<&str> {
    let ok = !s.is_empty()
        && s.len() <= 200
        && !s.starts_with('-')
        && !s.contains("..")
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'));
    if !ok {
        bail!("invalid ref name: {s:?}");
    }
    Ok(s)
}

async fn git(repo: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    // Peer content must never make the host execute anything: no hooks, no fsmonitor, no external
    // diff or textconv, no symlink checkout, no submodule recursion, no non-file protocols.
    cmd.args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-c", "core.symlinks=false"]);
    cmd.args(["-c", "submodule.recurse=false", "-c", "protocol.allow=never", "-c", "protocol.file.allow=always"]);
    cmd.env("GIT_TERMINAL_PROMPT", "0").env("GIT_ALLOW_PROTOCOL", "file").env("GIT_CONFIG_NOSYSTEM", "1");
    cmd.arg("-C").arg(repo).args(args);
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().context("spawn git")?;
    if let (Some(data), Some(mut si)) = (stdin, child.stdin.take()) {
        si.write_all(data).await?;
    }
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

/// Resolve a ref to a full commit id.
pub async fn rev_parse(repo: &Path, name: &str) -> Result<String> {
    let name = check_name(name)?;
    let spec = format!("{name}^{{commit}}");
    let out = git(repo, &["rev-parse", "--verify", &spec], None).await?;
    Ok(String::from_utf8(out)?.trim().to_string())
}

/// A bundle holding `refname` and its history.
pub async fn bundle_create(repo: &Path, refname: &str) -> Result<Vec<u8>> {
    let refname = check_name(refname)?;
    git(repo, &["bundle", "create", "-", refname], None).await
}

/// Verify `bundle`, then store its `src_ref` as `dest_ref` in `repo`. Returns the commit id.
pub async fn bundle_import(repo: &Path, bundle: &[u8], src_ref: &str, dest_ref: &str) -> Result<String> {
    let src_ref = check_name(src_ref)?;
    let dest_ref = check_name(dest_ref)?;
    let dir = tempfile_dir().await?;
    let file = dir.join("in.bundle");
    tokio::fs::write(&file, bundle).await?;
    let file_s = file.to_str().context("temp path is not utf-8")?;
    let result = async {
        git(repo, &["bundle", "verify", file_s], None).await?;
        let spec = format!("+{src_ref}:{dest_ref}");
        git(repo, &["fetch", "--no-tags", file_s, &spec], None).await?;
        rev_parse(repo, dest_ref).await
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

async fn tempfile_dir() -> Result<std::path::PathBuf> {
    let mut b = [0u8; 8];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut b)?;
    let name: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let dir = std::env::temp_dir().join(format!("playbox-{name}"));
    tokio::fs::create_dir_all(&dir).await?;
    Ok(dir)
}

/// A scratch directory (not a worktree) holding a checkout of `commit`.
/// Uses `git worktree` so objects are shared; remove with [`worktree_remove`].
pub async fn worktree_add(repo: &Path, commit: &str) -> Result<std::path::PathBuf> {
    let commit = check_name(commit)?;
    let dir = tempfile_dir().await?.join("ws");
    let dir_s = dir.to_str().context("temp path is not utf-8")?;
    git(repo, &["worktree", "add", "--detach", dir_s, commit], None).await?;
    Ok(dir)
}

pub async fn worktree_remove(repo: &Path, dir: &Path) -> Result<()> {
    let dir_s = dir.to_str().context("path is not utf-8")?;
    git(repo, &["worktree", "remove", "--force", dir_s], None).await?;
    if let Some(parent) = dir.parent() {
        let _ = tokio::fs::remove_dir(parent).await;
    }
    Ok(())
}

/// Everything changed in `dir` relative to `base`, including new files, as a binary patch.
pub async fn diff_patch(dir: &Path, base: &str) -> Result<Vec<u8>> {
    let base = check_name(base)?;
    git(dir, &["add", "-A"], None).await?;
    git(dir, &["diff", "--cached", "--binary", "--no-ext-diff", "--no-textconv", base], None).await
}

pub async fn apply_patch(repo: &Path, patch: &[u8]) -> Result<()> {
    git(repo, &["apply", "--index", "-"], Some(patch)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success();
        assert!(ok, "git {args:?}");
    }

    #[tokio::test]
    async fn host_hooks_never_run_on_peer_driven_operations() {
        let dir = tempfile_dir().await.unwrap();
        let marker = dir.join("hook-ran");
        sh(&dir, &["init", "-q", "-b", "main"]);
        sh(&dir, &["config", "user.email", "t@example.com"]);
        sh(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        sh(&dir, &["add", "-A"]);
        sh(&dir, &["commit", "-q", "-m", "init"]);
        let hook = dir.join(".git/hooks/post-checkout");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let ws = worktree_add(&dir, "main").await.unwrap();
        worktree_remove(&dir, &ws).await.unwrap();
        assert!(!marker.exists(), "post-checkout hook ran");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
