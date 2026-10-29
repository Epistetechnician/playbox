//! Run a task against a versioned [`Workspace`] and commit what it changed.
//!
//! The workspace is materialised into a scratch directory, the runner executes the command
//! there, and the resulting file changes become one commit tagged with the tool-call id.
//! Not preserved yet: file modes, symlinks and empty directories (the tree stores regular
//! files only).

use crate::{Runner, TaskOutput, TaskSpec};
use anyhow::{bail, Context, Result};
use playbox_core::workspace::{Error as WsError, Id, Repo, Workspace};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const MAX_FILES: usize = 10_000;
const MAX_BYTES: u64 = 100 * 1024 * 1024;

static SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct WorkspaceRun {
    pub output: TaskOutput,
    /// The new commit, if the task changed anything and the commit succeeded.
    pub commit: Option<Id>,
    /// Paths that conflicted with a newer head; nothing was committed and the workspace keeps the changes.
    pub conflicts: Vec<String>,
    /// Paths the task added, modified or deleted.
    pub changed: Vec<String>,
}

fn safe(path: &str) -> Result<PathBuf> {
    let p = Path::new(path);
    if p.is_absolute() || p.components().any(|c| !matches!(c, Component::Normal(_))) {
        bail!("unsafe path in workspace: {path}");
    }
    Ok(p.to_path_buf())
}

fn materialize(files: &BTreeMap<String, Vec<u8>>, dir: &Path) -> Result<()> {
    for (path, data) in files {
        let dest = dir.join(safe(path)?);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(dest, data)?;
    }
    Ok(())
}

fn scan(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    fn walk(root: &Path, cur: &Path, out: &mut BTreeMap<String, Vec<u8>>, bytes: &mut u64) -> Result<()> {
        for entry in fs::read_dir(cur)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(root, &entry.path(), out, bytes)?;
            } else if kind.is_file() {
                // Check the size before reading: a task can leave a sparse file far larger than memory.
                let len = entry.metadata()?.len();
                if out.len() >= MAX_FILES || bytes.saturating_add(len) > MAX_BYTES {
                    bail!("task output exceeds limits ({MAX_FILES} files, {MAX_BYTES} bytes)");
                }
                let data = fs::read(entry.path())?;
                *bytes += data.len() as u64;
                if out.len() >= MAX_FILES || *bytes > MAX_BYTES {
                    bail!("task output exceeds limits ({MAX_FILES} files, {MAX_BYTES} bytes)");
                }
                let rel = entry.path().strip_prefix(root)?.to_string_lossy().replace('\\', "/");
                out.insert(rel, data);
            } // symlinks and special files are skipped
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out, &mut 0)?;
    Ok(out)
}

/// Execute `spec` over the workspace's current view, then commit the changes as one commit
/// (a checkpoint per tool call). A failing command still commits what it changed; check
/// `output.exit_code`.
pub async fn run_in_workspace(
    repo: &mut Repo,
    ws: &mut Workspace,
    runner: &dyn Runner,
    spec: &TaskSpec,
    author: &str,
    tool_call: &str,
    time_ms: u64,
) -> Result<WorkspaceRun> {
    let before = repo.files(ws)?;
    let dir = std::env::temp_dir().join(format!("playbox-run-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&dir).context("scratch dir")?;
    let result = async {
        materialize(&before, &dir)?;
        let output = runner.run(spec, &dir).await?;
        let after = scan(&dir)?;
        anyhow::Ok((output, after))
    }
    .await;
    let _ = fs::remove_dir_all(&dir);
    let (output, after) = result?;

    let mut changed = Vec::new();
    for (path, data) in &after {
        if before.get(path) != Some(data) {
            ws.write(path, data);
            changed.push(path.clone());
        }
    }
    for path in before.keys().filter(|p| !after.contains_key(*p)) {
        ws.remove(path);
        changed.push(path.clone());
    }

    let (mut commit, mut conflicts) = (None, Vec::new());
    if ws.is_dirty() {
        let message = format!("task: {}", spec.cmd.join(" "));
        match repo.commit(ws, author, &message, tool_call, time_ms) {
            Ok(id) => commit = Some(id),
            Err(WsError::Conflict(paths)) => conflicts = paths,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(WorkspaceRun { output, commit, conflicts, changed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProcessRunner, TaskSpec};
    use playbox_core::workspace::Store;

    fn sh(script: &str) -> TaskSpec {
        TaskSpec::new(&["sh", "-c", script])
    }

    fn repo() -> Repo {
        let mut r = Repo::new(Store::memory());
        r.init("main", &[("a.txt", b"a"), ("src/b.txt", b"b")], "x", 1).unwrap();
        r
    }

    #[tokio::test]
    async fn task_changes_become_one_commit_tagged_with_the_tool_call() {
        let mut r = repo();
        let mut ws = r.workspace("main").unwrap();
        let run = run_in_workspace(&mut r, &mut ws, &ProcessRunner, &sh("echo hi > out.txt; rm a.txt; echo B >> src/b.txt"), "claude", "toolu_9", 5)
            .await
            .unwrap();
        assert_eq!(run.output.exit_code, Some(0));
        let id = run.commit.expect("committed");
        assert_eq!(run.changed, ["out.txt", "src/b.txt", "a.txt"]);
        let info = r.commit_info(&id).unwrap();
        assert_eq!(info.tool_call, "toolu_9");
        assert_eq!(r.read(&id, "out.txt").unwrap().unwrap(), b"hi\n");
        assert_eq!(r.read(&id, "a.txt").unwrap(), None);
        assert_eq!(r.read(&id, "src/b.txt").unwrap().unwrap(), b"bB\n");
        assert_eq!(r.log("main").unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_task_that_changes_nothing_makes_no_commit() {
        let mut r = repo();
        let head = r.head("main").unwrap();
        let mut ws = r.workspace("main").unwrap();
        let run = run_in_workspace(&mut r, &mut ws, &ProcessRunner, &sh("cat a.txt >/dev/null"), "claude", "t", 5).await.unwrap();
        assert!(run.commit.is_none() && run.changed.is_empty());
        assert_eq!(r.head("main").unwrap(), head);
    }

    #[tokio::test]
    async fn sequential_tasks_see_each_others_changes() {
        let mut r = repo();
        let mut ws = r.workspace("main").unwrap();
        run_in_workspace(&mut r, &mut ws, &ProcessRunner, &sh("echo one > step.txt"), "c", "t1", 5).await.unwrap();
        let run = run_in_workspace(&mut r, &mut ws, &ProcessRunner, &sh("cat step.txt > copy.txt"), "c", "t2", 6).await.unwrap();
        let id = run.commit.unwrap();
        assert_eq!(r.read(&id, "copy.txt").unwrap().unwrap(), b"one\n");
        assert_eq!(r.log("main").unwrap().len(), 3);
    }

    #[tokio::test]
    async fn concurrent_tasks_on_the_same_file_conflict() {
        let mut r = repo();
        let (mut w1, mut w2) = (r.workspace("main").unwrap(), r.workspace("main").unwrap());
        let first = run_in_workspace(&mut r, &mut w1, &ProcessRunner, &sh("echo one > a.txt"), "c1", "t1", 5).await.unwrap();
        let second = run_in_workspace(&mut r, &mut w2, &ProcessRunner, &sh("echo two > a.txt"), "c2", "t2", 6).await.unwrap();
        assert!(first.commit.is_some());
        assert!(second.commit.is_none());
        assert_eq!(second.conflicts, ["a.txt"]);
        assert!(w2.is_dirty(), "changes are kept so the agent can resolve");
    }

    #[test]
    fn unsafe_paths_are_rejected() {
        assert!(safe("../x").is_err());
        assert!(safe("/etc/passwd").is_err());
        assert!(safe("a/b.txt").is_ok());
    }
}
