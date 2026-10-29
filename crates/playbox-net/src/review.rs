//! The host's review step for what members push.
//!
//! A push lands under `refs/playbox/members/<member>/<branch>` and never touches a real branch. Nothing
//! a member sends moves `main` until the host accepts it here. Accepting only fast-forwards: if the
//! branch is not built on the target's current tip, the host asks the member to rebase rather than
//! merging code it has not read. Decisions are kept as refs (`refs/playbox/accepted/..`,
//! `refs/playbox/rejected/..`) so the record outlives the review.

use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;

pub const PENDING: &str = "refs/playbox/members/";
pub const ACCEPTED: &str = "refs/playbox/accepted/";
pub const REJECTED: &str = "refs/playbox/rejected/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub reference: String,
    /// Short member id (the first 16 characters of the key) and the branch the member called it.
    pub member: String,
    pub branch: String,
    pub head: String,
}

fn split(reference: &str) -> Result<(String, String)> {
    let rest = reference.strip_prefix(PENDING).ok_or_else(|| anyhow!("{reference} is not a pending push (expected {PENDING}<member>/<branch>)"))?;
    let (member, branch) = rest.split_once('/').ok_or_else(|| anyhow!("{reference} has no branch name"))?;
    if member.is_empty() || branch.is_empty() {
        bail!("{reference} is not a pending push");
    }
    Ok((member.to_string(), branch.to_string()))
}

/// Pushes waiting for a decision.
pub async fn pending(repo: &Path) -> Result<Vec<Pending>> {
    let mut out = Vec::new();
    for (reference, head) in playbox_git::list_refs(repo, PENDING).await? {
        if let Ok((member, branch)) = split(&reference) {
            out.push(Pending { reference, member, branch, head });
        }
    }
    Ok(out)
}

/// What a push would bring into `into`: its new commits and a diffstat.
pub async fn show(repo: &Path, reference: &str, into: &str) -> Result<String> {
    split(reference)?;
    let into_ref = format!("refs/heads/{into}");
    let commits = playbox_git::log_oneline(repo, &into_ref, reference, 50).await?;
    let stat = playbox_git::diff_stat(repo, &into_ref, reference).await?;
    let ff = playbox_git::is_ancestor(repo, &into_ref, reference).await?;
    Ok(format!(
        "{} commit(s) on top of {into}{}\n{}\n{}",
        commits.len(),
        if ff { "" } else { "  (NOT a fast-forward: ask the member to rebase)" },
        commits.join("\n"),
        stat
    ))
}

fn archive(prefix: &str, reference: &str, head: &str) -> Result<String> {
    let (member, branch) = split(reference)?;
    Ok(format!("{prefix}{member}/{}-{}/{branch}", head.chars().take(8).collect::<String>(), now()))
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Fast-forward `into` to the pushed commit and retire the pending ref. Returns the new tip.
pub async fn accept(repo: &Path, reference: &str, into: &str) -> Result<String> {
    split(reference)?;
    playbox_git::check_name(into)?;
    let head = playbox_git::rev_parse(repo, reference).await.with_context(|| format!("no such push: {reference}"))?;
    let into_ref = format!("refs/heads/{into}");
    let old = playbox_git::rev_parse(repo, &into_ref).await.with_context(|| format!("no branch {into} to merge into"))?;
    if !playbox_git::is_ancestor(repo, &old, &head).await? {
        bail!("{reference} is not built on the current tip of {into}; ask the member to rebase it, then review again");
    }
    let (current, has_worktree) = playbox_git::head_branch(repo).await?;
    if has_worktree && current.as_deref() == Some(into) {
        // The branch is checked out: move the work tree with it, and refuse if local changes are in the way.
        playbox_git::merge_ff_only(repo, &head).await.context("the checked-out branch could not be fast-forwarded (uncommitted changes in the way?)")?;
    } else {
        playbox_git::update_ref(repo, &into_ref, &head, Some(&old)).await?;
    }
    playbox_git::update_ref(repo, &archive(ACCEPTED, reference, &head)?, &head, None).await?;
    playbox_git::delete_ref(repo, reference).await?;
    Ok(head)
}

/// Retire a push without merging it. The commits stay reachable under `refs/playbox/rejected/` until you prune them.
pub async fn reject(repo: &Path, reference: &str) -> Result<()> {
    split(reference)?;
    let head = playbox_git::rev_parse(repo, reference).await.with_context(|| format!("no such push: {reference}"))?;
    playbox_git::update_ref(repo, &archive(REJECTED, reference, &head)?, &head, None).await?;
    playbox_git::delete_ref(repo, reference).await?;
    Ok(())
}
