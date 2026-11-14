//! Thin async wrappers over the `git` CLI for bundle sync and task worktrees.
//!
//! Every ref or commit-ish that comes from a peer goes through [`check_name`] first, so a peer
//! cannot smuggle options into a git command line.

use anyhow::{bail, Context, Result};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{io::AsyncWriteExt, process::Command};

/// Accept only plain ref/commit names: `[A-Za-z0-9._/-]`, no leading `-`, no `..`.
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub fn check_name(s: &str) -> Result<&str> {
    let ok = !s.is_empty()
        && s.len() <= 200
        && !s.starts_with('-')
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'));
    if !ok {
        bail!("invalid ref name: {s:?}");
    }
    Ok(s)
}

async fn git(repo: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
    git_scoped(Scope::Repo(repo), args, stdin).await
}

/// Where a git command runs.
enum Scope<'a> {
    /// A repository the host owns and trusts.
    Repo(&'a Path),
    /// A task's scratch checkout: the git directory is the one recorded when it was made, and the
    /// directory itself is only ever a work tree, never consulted for where the repository is.
    Worktree(&'a Worktree),
}

async fn git_scoped(scope: Scope<'_>, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    // Peer content must never make the host execute anything: no hooks, no fsmonitor, no external
    // diff or textconv, no symlink checkout, no submodule recursion, no non-file protocols.
    cmd.args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.symlinks=false",
    ]);
    cmd.args([
        "-c",
        "submodule.recurse=false",
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.file.allow=always",
    ]);
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", "file")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    match scope {
        Scope::Repo(repo) => {
            cmd.arg("-C").arg(repo);
        }
        Scope::Worktree(wt) => {
            // A task can write anything into its directory, including a `.git` directory or a `.git`
            // file naming another git directory, with a config that defines filter commands. Git
            // would run those on the host. An explicit --git-dir means that nothing in the directory
            // is ever read as repository configuration. Attributes and global config are switched
            // off too, so a `.gitattributes` in the tree cannot select a driver the host defines.
            cmd.arg("--git-dir")
                .arg(&wt.gitdir)
                .arg("--work-tree")
                .arg(&wt.dir)
                .current_dir(&wt.dir);
            cmd.args([
                "-c",
                "core.attributesFile=/dev/null",
                "-c",
                "core.excludesFile=/dev/null",
                "-c",
                "diff.external=",
            ]);
            cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
            cmd.env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE");
        }
    }
    cmd.args(args);
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("spawn git")?;
    if let (Some(data), Some(mut si)) = (stdin, child.stdin.take()) {
        si.write_all(data).await?;
    }
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// Resolve a ref to a full commit id.
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn rev_parse(repo: &Path, name: &str) -> Result<String> {
    let name = check_name(name)?;
    let spec = format!("{name}^{{commit}}");
    let out = git(repo, &["rev-parse", "--verify", &spec], None).await?;
    Ok(String::from_utf8(out)?.trim().to_string())
}

/// A bundle holding `refname` and its history.
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn bundle_create(repo: &Path, refname: &str) -> Result<Vec<u8>> {
    let refname = check_name(refname)?;
    git(repo, &["bundle", "create", "-", refname], None).await
}

/// Verify `bundle`, then store its `src_ref` as `dest_ref` in `repo`. Returns the commit id.
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn bundle_import(
    repo: &Path,
    bundle: &[u8],
    src_ref: &str,
    dest_ref: &str,
) -> Result<String> {
    let src_ref = check_name(src_ref)?;
    let dest_ref = check_name(dest_ref)?;
    let dir = tempfile_dir().await?;
    let file = dir.join("in.bundle");
    tokio::fs::write(&file, bundle).await?;
    let file_s = file.to_str().context("temp path is not utf-8")?;
    let result = async {
        git(repo, &["bundle", "verify", file_s], None).await?;
        let spec = format!("+{src_ref}:{dest_ref}");
        // Remember what the ref held, so a rejected import leaves it exactly as it was.
        let previous = rev_parse(repo, dest_ref).await.ok();
        // `transfer.fsckObjects` is kept for fetches that honour it, but git does not apply it when the
        // source is a bundle file (observed on 2.43), so the explicit check below is the real guard.
        git(
            repo,
            &["-c", "transfer.fsckObjects=true", "fetch", "--no-tags", file_s, &spec],
            None,
        )
        .await?;
        let new = rev_parse(repo, dest_ref).await?;
        // Check every object reachable from the new tip (paths like `.git/hooks/..`, malformed trees,
        // unsafe .gitmodules) rather than trusting the sender: the bundle comes from a peer.
        let checked = git(
            repo,
            &[
                "-c",
                "fsck.hasDotgit=error",
                "fsck",
                "--strict",
                "--no-dangling",
                &new,
            ],
            None,
        )
        .await;
        if let Err(e) = checked {
            let undo = match &previous {
                Some(old) => git(repo, &["update-ref", dest_ref, old], None).await,
                None => git(repo, &["update-ref", "-d", dest_ref], None).await,
            };
            undo.context("restore ref after rejecting bundle")?;
            return Err(e.context("fsck rejected the bundle"));
        }
        Ok(new)
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

/// A task's scratch checkout of one commit. Objects are shared with the room's repository through
/// `git worktree`. `gitdir` is recorded when the checkout is created, before any task has touched it,
/// and is the only thing later git commands trust.
#[derive(Debug)]
pub struct Worktree {
    dir: PathBuf,
    gitdir: PathBuf,
}

impl Worktree {
    /// The directory the task runs in.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

/// Check out `commit` into a fresh scratch directory. Remove it with [`worktree_remove`].
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn worktree_add(repo: &Path, commit: &str) -> Result<Worktree> {
    let commit = check_name(commit)?;
    let dir = tempfile_dir().await?.join("ws");
    let dir_s = dir.to_str().context("temp path is not utf-8")?;
    git(repo, &["worktree", "add", "--detach", dir_s, commit], None).await?;
    // Safe to ask now: nothing has run in the directory yet.
    let gitdir = git(&dir, &["rev-parse", "--absolute-git-dir"], None).await?;
    let gitdir = PathBuf::from(String::from_utf8(gitdir)?.trim());
    Ok(Worktree { dir, gitdir })
}

/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn worktree_remove(repo: &Path, wt: &Worktree) -> Result<()> {
    let dir_s = wt.dir.to_str().context("path is not utf-8")?;
    git(repo, &["worktree", "remove", "--force", dir_s], None).await?;
    if let Some(parent) = wt.dir.parent() {
        let _ = tokio::fs::remove_dir(parent).await;
    }
    Ok(())
}

/// `-c filter.<driver>.<x>=` overrides that switch off every filter driver the *host's* config defines
/// (git-lfs, say). A task controls `.gitattributes` in its checkout and could otherwise pick which of
/// those commands the host runs, and on what data. Without a driver, files are stored as they are.
async fn filter_overrides(gitdir: &Path) -> Result<Vec<String>> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(gitdir)
        .args(["config", "--name-only", "--get-regexp", r"^filter\."])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .await
        .context("spawn git")?;
    // Exit code 1 just means "no such keys".
    let keys = String::from_utf8_lossy(&out.stdout);
    let mut drivers: Vec<&str> = keys
        .lines()
        .filter_map(|k| {
            k.strip_prefix("filter.")
                .and_then(|r| r.rsplit_once('.'))
                .map(|(name, _)| name)
        })
        .collect();
    drivers.sort_unstable();
    drivers.dedup();
    let mut args = Vec::new();
    for d in drivers {
        for (var, val) in [
            ("clean", ""),
            ("smudge", ""),
            ("process", ""),
            ("required", "false"),
        ] {
            args.push("-c".to_string());
            args.push(format!("filter.{d}.{var}={val}"));
        }
    }
    Ok(args)
}

/// Most a task may leave behind: total apparent size and number of files. Checked before git reads
/// anything, so a sparse multi-gigabyte file is refused in milliseconds instead of being hashed.
pub const MAX_RESULT_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_RESULT_FILES: usize = 100_000;

/// Walk `dir` without following symlinks and refuse if it holds more than the limits allow.
fn check_result_size(dir: &Path) -> Result<()> {
    let (mut bytes, mut files) = (0u64, 0usize);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d)? {
            let entry = entry?;
            let meta = entry.metadata()?; // does not follow symlinks
            files += 1;
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                bytes = bytes.saturating_add(meta.len());
            }
            if bytes > MAX_RESULT_BYTES || files > MAX_RESULT_FILES {
                bail!(
                    "task output exceeds the host's limits ({} MiB, {MAX_RESULT_FILES} files)",
                    MAX_RESULT_BYTES >> 20
                );
            }
        }
    }
    Ok(())
}

/// Everything changed in the checkout relative to `base`, including new files, as a binary patch.
/// Refuses a checkout larger than [`MAX_RESULT_BYTES`] or with more than [`MAX_RESULT_FILES`] entries.
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn diff_patch(wt: &Worktree, base: &str) -> Result<Vec<u8>> {
    let base = check_name(base)?;
    let dir = wt.dir.clone();
    tokio::task::spawn_blocking(move || check_result_size(&dir)).await??;
    let filters = filter_overrides(&wt.gitdir).await?;
    let with = |rest: &[&str]| -> Vec<String> {
        filters
            .iter()
            .cloned()
            .chain(rest.iter().map(std::string::ToString::to_string))
            .collect()
    };
    let add = with(&["add", "-A"]);
    git_scoped(
        Scope::Worktree(wt),
        &add.iter().map(String::as_str).collect::<Vec<_>>(),
        None,
    )
    .await?;
    git_scoped(
        Scope::Worktree(wt),
        &[
            "diff",
            "--cached",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
            base,
        ],
        None,
    )
    .await
}

/// Create a repository at `dir` (a no-op if there already is one), on branch `branch`.
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn init(dir: &Path, branch: &str) -> Result<()> {
    let branch = check_name(branch)?;
    tokio::fs::create_dir_all(dir).await?;
    if !dir.join(".git").exists() {
        git(dir, &["init", "-q", "-b", branch], None).await?;
    }
    Ok(())
}

/// Make `branch` in `repo` point at `dest_ref` and check it out, replacing that branch. The content comes
/// from a peer, so this runs with the same hardening as every other git call here: no hooks, and symlinks
/// written as plain files so a hostile tree cannot plant a link out of the workspace.
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn checkout_branch(repo: &Path, branch: &str, dest_ref: &str) -> Result<()> {
    let (branch, dest_ref) = (check_name(branch)?, check_name(dest_ref)?);
    git(repo, &["checkout", "-q", "-B", branch, dest_ref], None).await?;
    Ok(())
}

/// Every ref under `prefix` (e.g. `refs/playbox/members/`) and the commit it points at, sorted by name.
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn list_refs(repo: &Path, prefix: &str) -> Result<Vec<(String, String)>> {
    let prefix = check_name(prefix)?;
    let out = git(
        repo,
        &["for-each-ref", "--format=%(refname) %(objectname)", prefix],
        None,
    )
    .await?;
    let mut refs: Vec<(String, String)> = String::from_utf8(out)?
        .lines()
        .filter_map(|l| {
            l.split_once(' ')
                .map(|(r, c)| (r.to_string(), c.to_string()))
        })
        .collect();
    refs.sort();
    Ok(refs)
}

/// Whether commit `a` is an ancestor of (or equal to) commit `b`.
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn is_ancestor(repo: &Path, a: &str, b: &str) -> Result<bool> {
    let (a, b) = (rev_parse(repo, a).await?, rev_parse(repo, b).await?);
    let base = git(repo, &["merge-base", &a, &b], None).await;
    Ok(match base {
        Ok(out) => String::from_utf8(out)?.trim() == a,
        Err(_) => false, // no common history
    })
}

/// Up to `limit` one-line summaries of the commits in `range` (`base..tip`), newest first.
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn log_oneline(repo: &Path, base: &str, tip: &str, limit: usize) -> Result<Vec<String>> {
    let (base, tip) = (check_name(base)?, check_name(tip)?);
    let range = format!("{base}..{tip}");
    let n = format!("-n{limit}");
    let out = git(
        repo,
        &["log", "--no-color", "--format=%h %an: %s", &n, &range],
        None,
    )
    .await?;
    Ok(String::from_utf8(out)?
        .lines()
        .map(str::to_string)
        .collect())
}

/// `git diff --stat base...tip` (what `tip` adds on top of the common ancestor).
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn diff_stat(repo: &Path, base: &str, tip: &str) -> Result<String> {
    let (base, tip) = (check_name(base)?, check_name(tip)?);
    let range = format!("{base}...{tip}");
    let out = git(
        repo,
        &[
            "diff",
            "--stat",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            &range,
        ],
        None,
    )
    .await?;
    Ok(String::from_utf8(out)?)
}

/// Point `reference` at `new`. With `old`, only if it still points there (so a concurrent change is not overwritten).
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn update_ref(repo: &Path, reference: &str, new: &str, old: Option<&str>) -> Result<()> {
    let reference = check_name(reference)?;
    let new = check_name(new)?;
    let mut args = vec!["update-ref", reference, new];
    if let Some(old) = old {
        args.push(check_name(old)?);
    }
    git(repo, &args, None).await?;
    Ok(())
}

/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn delete_ref(repo: &Path, reference: &str) -> Result<()> {
    let reference = check_name(reference)?;
    git(repo, &["update-ref", "-d", reference], None).await?;
    Ok(())
}

/// The branch HEAD names, and whether the repository has a work tree (a non-bare repository).
///
/// # Errors
///
/// Returns a message with the `git` error output if the command fails.
pub async fn head_branch(repo: &Path) -> Result<(Option<String>, bool)> {
    let bare = String::from_utf8(git(repo, &["rev-parse", "--is-bare-repository"], None).await?)?
        .trim()
        == "true";
    let branch = git(repo, &["symbolic-ref", "--short", "-q", "HEAD"], None)
        .await
        .ok()
        .and_then(|o| String::from_utf8(o).ok())
        .map(|s| s.trim().to_string());
    Ok((branch.filter(|b| !b.is_empty()), !bare))
}

/// Fast-forward the checked-out branch to `target` and update its work tree. Fails if that is not a
/// fast-forward or if local changes are in the way.
///
/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn merge_ff_only(repo: &Path, target: &str) -> Result<()> {
    let target = check_name(target)?;
    git(repo, &["merge", "--ff-only", "-q", target], None).await?;
    Ok(())
}

/// # Errors
///
/// Returns an error if `git` cannot be run or exits with a failure status.
pub async fn apply_patch(repo: &Path, patch: &[u8]) -> Result<()> {
    git(repo, &["apply", "--index", "-"], Some(patch)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        // Fixed commit dates, so the test repositories (and the bundles the fuzz tests damage) are the same bytes on every
        // run; otherwise which mutations hit something git checks varies with the clock.
        let ok = std::process::Command::new("git")
            .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success();
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
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let ws = worktree_add(&dir, "main").await.unwrap();
        worktree_remove(&dir, &ws).await.unwrap();
        assert!(!marker.exists(), "post-checkout hook ran");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A bundle whose only commit has `.git/hooks/post-checkout` in its tree, built with plumbing because
    /// `git add` refuses to.
    async fn hostile_bundle() -> Vec<u8> {
        let d = tempfile_dir().await.unwrap();
        sh(&d, &["init", "-q", "-b", "main"]);
        sh(&d, &["config", "user.email", "t@example.com"]);
        sh(&d, &["config", "user.name", "t"]);
        std::fs::write(d.join("a.txt"), "a").unwrap();
        sh(&d, &["add", "-A"]);
        sh(&d, &["commit", "-q", "-m", "i"]);
        let pipe = |input: String, args: &[&str]| -> String {
            use std::io::Write;
            let mut c = std::process::Command::new("git")
                .arg("-C")
                .arg(&d)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
            String::from_utf8(c.wait_with_output().unwrap().stdout)
                .unwrap()
                .trim()
                .to_string()
        };
        let blob = pipe(
            "#!/bin/sh\ntouch /tmp/playbox-hostile-hook\n".into(),
            &["hash-object", "-w", "--stdin"],
        );
        let hooks = pipe(format!("100755 blob {blob}\tpost-checkout\n"), &["mktree"]);
        let dotgit = pipe(format!("040000 tree {hooks}\thooks\n"), &["mktree"]);
        let a = git_out(&d, &["rev-parse", "HEAD:a.txt"]);
        let root = pipe(
            format!("100644 blob {a}\ta.txt\n040000 tree {dotgit}\t.git\n"),
            &["mktree"],
        );
        let commit = git_out(&d, &["commit-tree", &root, "-m", "evil"]);
        sh(&d, &["update-ref", "refs/heads/main", &commit]);
        bundle_create(&d, "main").await.unwrap()
    }

    #[tokio::test]
    async fn a_hostile_bundle_is_rejected_on_import() {
        let bundle = hostile_bundle().await;
        let victim = tempfile_dir().await.unwrap();
        init(&victim, "main").await.unwrap();
        let e = bundle_import(
            &victim,
            &bundle,
            "refs/heads/main",
            "refs/remotes/peer/main",
        )
        .await
        .unwrap_err();
        assert!(
            format!("{e:#}").contains("fsck") || format!("{e:#}").contains(".git"),
            "{e:#}"
        );
        assert!(
            !victim.join(".git/refs/remotes/peer/main").exists(),
            "the hostile ref was stored"
        );
    }

    #[tokio::test]
    async fn checkout_of_peer_content_runs_no_hooks_and_plants_no_symlinks() {
        // A good tree that still contains a symlink out of the workspace and a hook in the receiver's repo.
        let src = tempfile_dir().await.unwrap();
        sh(&src, &["init", "-q", "-b", "main"]);
        sh(&src, &["config", "user.email", "t@example.com"]);
        sh(&src, &["config", "user.name", "t"]);
        std::os::unix::fs::symlink("/etc", src.join("escape")).unwrap();
        std::fs::write(src.join("a.txt"), "a").unwrap();
        sh(&src, &["add", "-A"]);
        sh(&src, &["commit", "-q", "-m", "i"]);
        let bundle = bundle_create(&src, "main").await.unwrap();

        let victim = tempfile_dir().await.unwrap();
        init(&victim, "main").await.unwrap();
        let marker = victim.join("hook-ran");
        let hook = victim.join(".git/hooks/post-checkout");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        bundle_import(
            &victim,
            &bundle,
            "refs/heads/main",
            "refs/remotes/peer/main",
        )
        .await
        .unwrap();
        checkout_branch(&victim, "main", "refs/remotes/peer/main")
            .await
            .unwrap();
        assert!(
            !marker.exists(),
            "a hook ran while checking out a peer's branch"
        );
        let meta = std::fs::symlink_metadata(victim.join("escape")).unwrap();
        assert!(
            !meta.file_type().is_symlink(),
            "a symlink from the peer's tree was created in the workspace"
        );
    }

    #[tokio::test]
    async fn refs_that_look_like_options_are_refused_everywhere() {
        let d = tempfile_dir().await.unwrap();
        init(&d, "main").await.unwrap();
        for bad in [
            "--upload-pack=x",
            "-b",
            "a..b",
            "a b",
            "",
            "x\ny",
            &"a".repeat(300),
        ] {
            assert!(check_name(bad).is_err(), "{bad:?}");
            assert!(checkout_branch(&d, "main", bad).await.is_err());
            assert!(rev_parse(&d, bad).await.is_err());
        }
    }

    /// Patches come from a host the guest does not control. None may write outside the workspace or into `.git`.
    #[tokio::test]
    async fn a_hostile_patch_cannot_write_outside_the_workspace_or_into_dot_git() {
        let outer = tempfile_dir().await.unwrap();
        let ws = outer.join("ws");
        init(&ws, "main").await.unwrap();
        sh(&ws, &["config", "user.email", "t@example.com"]);
        sh(&ws, &["config", "user.name", "t"]);
        std::fs::write(ws.join("a.txt"), "a").unwrap();
        sh(&ws, &["add", "-A"]);
        sh(&ws, &["commit", "-q", "-m", "i"]);
        let target = outer.join("target");
        std::fs::create_dir_all(&target).unwrap();

        let new_file = |path: &str, mode: &str, body: &str| {
            format!("diff --git a/{path} b/{path}\nnew file mode {mode}\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+{body}\n")
        };
        let patches = [
            (
                "parent traversal",
                new_file("../outside.txt", "100644", "pwn"),
            ),
            (
                "absolute path",
                new_file(&format!("{}/abs.txt", target.display()), "100644", "pwn"),
            ),
            (
                "into .git",
                new_file(".git/hooks/post-checkout", "100755", "pwn"),
            ),
            (
                "into a nested .git",
                new_file("sub/.git/config", "100644", "pwn"),
            ),
            (
                "write through a symlink made by the same patch",
                format!(
                    "{}{}",
                    new_file("link", "120000", &target.display().to_string()),
                    new_file("link/through.txt", "100644", "pwn")
                ),
            ),
        ];
        for (name, patch) in patches {
            let _ = apply_patch(&ws, patch.as_bytes()).await; // an error is the expected outcome
            sh(&ws, &["reset", "-q", "--hard"]);
            let _ = name;
        }
        let tree: Vec<String> = walk(&outer);
        assert!(
            !tree.iter().any(|p| p.ends_with("outside.txt")
                || p.ends_with("abs.txt")
                || p.ends_with("through.txt")),
            "a hostile patch wrote {tree:?}"
        );
        assert!(!ws.join(".git/hooks/post-checkout").exists());
        assert!(!ws.join("sub/.git/config").exists());
    }

    fn walk(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let meta = std::fs::symlink_metadata(&p).unwrap();
            if p.file_name().is_some_and(|n| n == ".git") {
                if let Ok(hooks) = std::fs::read_dir(p.join("hooks")) {
                    out.extend(
                        hooks
                            .flatten()
                            .map(|h| h.path().display().to_string())
                            .filter(|h| !h.ends_with(".sample")),
                    );
                }
                continue;
            }
            out.push(p.display().to_string());
            if meta.is_dir() {
                out.extend(walk(&p));
            }
        }
        out
    }

    /// A member controls every byte of the bundle the host feeds to git. Damaged bundles may be refused or,
    /// if they happen to still be valid, accepted; they must never hang, and whatever is stored must be sound.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the value is bounded, or truncated on purpose"
    )]
    #[tokio::test]
    async fn mutated_bundles_never_hang_or_leave_a_corrupt_repository() {
        // A real bundle: a few commits, text and binary files.
        let src = tempfile_dir().await.unwrap();
        sh(&src, &["init", "-q", "-b", "main"]);
        sh(&src, &["config", "user.email", "t@example.com"]);
        sh(&src, &["config", "user.name", "t"]);
        for i in 0..3u8 {
            std::fs::write(
                src.join(format!("f{i}.txt")),
                format!("file {i}\n").repeat(40),
            )
            .unwrap();
            std::fs::write(
                src.join(format!("b{i}.bin")),
                (0..2000u32)
                    .map(|n| (n * 7 + u32::from(i)) as u8)
                    .collect::<Vec<u8>>(),
            )
            .unwrap();
            sh(&src, &["add", "-A"]);
            sh(&src, &["commit", "-q", "-m", &format!("c{i}")]);
        }
        let good = bundle_create(&src, "main").await.unwrap();
        let victim = tempfile_dir().await.unwrap();
        init(&victim, "main").await.unwrap();
        assert!(
            bundle_import(&victim, &good, "refs/heads/main", "refs/remotes/peer/ok")
                .await
                .is_ok(),
            "the unmutated bundle imports"
        );

        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        let mut rnd = move |n: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n.max(1) as u64) as usize
        };
        let (mut accepted, mut refused) = (0, 0);
        for i in 0..120 {
            let mut b = good.clone();
            for _ in 0..=rnd(3) {
                let at = rnd(b.len());
                match rnd(5) {
                    0 => b[at] ^= 1 << rnd(8),
                    1 => b.truncate(at.max(1)),
                    2 => {
                        let end = (at + 1 + rnd(64)).min(b.len());
                        b.drain(at..end);
                    }
                    3 => {
                        // Damage aimed at the text header (ref names, prerequisites) rather than the pack.
                        let at = rnd(120.min(b.len()));
                        b[at] = b"-/.. \n@{~^:?*[\\0"[rnd(16)];
                    }
                    _ => {
                        let junk: Vec<u8> = (0..=rnd(24)).map(|_| rnd(256) as u8).collect();
                        b.splice(at..at, junk);
                    }
                }
            }
            let dest = format!("refs/remotes/peer/m{i}");
            let r = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                bundle_import(&victim, &b, "refs/heads/main", &dest),
            )
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "import {i} hung; input starts {:02x?}",
                    &b[..b.len().min(48)]
                )
            });
            if let Ok(id) = r {
                accepted += 1;
                assert_eq!(id.len(), 40, "{id}");
            } else {
                refused += 1;
                assert!(
                    !victim.join(".git").join(&dest).exists(),
                    "a refused bundle left the ref {dest}"
                );
            }
        }
        // Whatever was accepted left a repository git considers sound.
        let fsck = std::process::Command::new("git")
            .arg("-C")
            .arg(&victim)
            .args(["fsck", "--strict", "--no-dangling"])
            .output()
            .unwrap();
        assert!(
            fsck.status.success(),
            "the repository is corrupt after the barrage: {}",
            String::from_utf8_lossy(&fsck.stderr)
        );
        // With fixed commit dates the barrage is reproducible: 37 of 120 are refused (the rest land in bytes git does not check
        // or still form a valid bundle). The bound only guards against a test that damages nothing.
        assert!(refused > 25, "too few mutations were refused ({refused} of 120, {accepted} accepted); the test is not damaging anything");
        let _ = tokio::fs::remove_dir_all(&victim).await;
        let _ = tokio::fs::remove_dir_all(&src).await;
    }
}
