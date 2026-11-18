#!/usr/bin/env python3
"""Merge the R&D room's pull requests one at a time, in order: rebase onto current main (only the changelog may conflict, and it is
re-inserted by hand), push, wait for every CI check to pass, then squash-merge. Stops at the first thing it cannot do safely.
Usage: merge_queue.py 16 17 18 ...   (runs from ~/.playbox-rnd/repo)"""
import json
import os
import re
import shutil
import subprocess
import sys
import time

REPO = os.path.expanduser("~/.playbox-rnd/repo")
SLUG = "Epistetechnician/playbox"
TMP = os.environ.get("TMPDIR", "/tmp")

ENTRY_FILE = os.path.expanduser("~/.playbox-rnd/changelog_entries.json")
ENTRIES = json.load(open(ENTRY_FILE)) if os.path.exists(ENTRY_FILE) else {}
ENTRIES = {int(k): v for k, v in ENTRIES.items()}


def run(a, cwd=REPO, t=600):
    r = subprocess.run(a, cwd=cwd, capture_output=True, text=True, timeout=t)
    return r.returncode, (r.stdout + r.stderr).strip()


def gh(*a, t=120):
    return run(["gh", *a, "--repo", SLUG], t=t)


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


def added_lines(sha, cwd):
    return [ln[1:] for ln in run(["git", "show", sha, "--format=", "--", "CHANGELOG.md"], cwd)[1].split("\n") if ln.startswith("+- ")]


def put_in_changelog(path, lines, head):
    s = open(path).read()
    for ln in lines:
        s = s.replace(ln + "\n", "")
    i = s.index("## [Unreleased]")
    nxt = s.index("\n## [", i + 5)
    if f"### {head}\n" in s[i:nxt]:
        j = s.index(f"### {head}\n", i) + len(f"### {head}\n")
        s = s[:j] + "\n".join(lines) + "\n" + s[j:]
    else:
        s = s[:i] + f"## [Unreleased]\n\n### {head}\n" + "\n".join(lines) + "\n" + s[i + len("## [Unreleased]\n"):]
    open(path, "w").write(s)


def rebase(n, br, title):
    run(["git", "fetch", "-q", "origin", "main", f"{br}:refs/remotes/origin/{br}"])
    wt = f"{TMP}/mq{n}"
    shutil.rmtree(wt, ignore_errors=True)
    run(["git", "worktree", "prune"])
    rc, out = run(["git", "worktree", "add", "-q", "-B", f"mq{n}", wt, f"origin/{br}"])
    if rc:
        return None, out
    base = run(["git", "merge-base", "origin/main", f"origin/{br}"], wt)[1]
    lines_all = [ln[1:] for ln in run(["git", "diff", base, "HEAD", "--", "CHANGELOG.md"], wt)[1].split("\n") if ln.startswith("+- ")]
    msg = run(["git", "log", "-1", "--format=%s", "HEAD"], wt)[1]
    body = run(["git", "log", "-1", "--format=%b", "HEAD"], wt)[1]
    run(["git", "reset", "-q", "--soft", base], wt)
    run(["git", "checkout", base, "--", "CHANGELOG.md"], wt)          # the PR carries no changelog edit: they all collide with every release on main
    if lines_all:                                                      # a branch already stripped of its changelog edit keeps the entry recorded earlier
        ENTRIES[n] = {"title": title, "lines": lines_all}
        json.dump(ENTRIES, open(ENTRY_FILE, "w"), indent=1)
    # the repo's own configured identity: a made-up address here shows up as a bogus "Co-authored-by" on the squash commit
    ident = ["-c", f"user.name={run(['git', 'config', 'user.name'])[1]}", "-c", f"user.email={run(['git', 'config', 'user.email'])[1]}"]
    run(["git", *ident, "commit", "-q", "-m", msg, "-m", body or "R&D room change"], wt)
    rc, out = run(["git", *ident, "rebase", "origin/main"], wt)
    while rc:
        conflicted = run(["git", "diff", "--name-only", "--diff-filter=U"], wt)[1].split()
        if conflicted != ["CHANGELOG.md"]:
            run(["git", "rebase", "--abort"], wt)
            return None, f"conflict outside the changelog: {conflicted}"
        run(["git", "checkout", "--ours", "CHANGELOG.md"], wt)
        head = "Added" if re.match(r"\[R&D room\]\s*Add\b", title) else "Fixed"
        put_in_changelog(wt + "/CHANGELOG.md", lines_all, head)
        run(["git", "add", "CHANGELOG.md"], wt)
        rc, out = run(["git", *ident, "-c", "core.editor=true", "rebase", "--continue"], wt)
    return wt, ""


def checks(n):
    rc, out = gh("pr", "checks", str(n), "--json", "name,state")
    try:
        return json.loads(out)
    except ValueError:
        return []


def wait_green(n, flake_budget=2):
    t0 = time.time()
    time.sleep(60)
    while time.time() - t0 < 3600:
        c = checks(n)
        states = [x["state"] for x in c]
        if len(c) >= 10 and not any(s in ("IN_PROGRESS", "QUEUED", "PENDING", "WAITING") for s in states):
            bad = [x["name"] for x in c if x["state"] not in ("SUCCESS", "SKIPPED", "NEUTRAL")]
            if not bad:
                return True, ""
            if flake_budget > 0:
                log(f"#{n}: rerunning failed jobs once ({bad})")
                rc, out = gh("pr", "view", str(n), "--json", "headRefOid", "-q", ".headRefOid")
                rc, runs = gh("run", "list", "--commit", out.strip(), "--json", "databaseId,conclusion,workflowName")
                for r in json.loads(runs or "[]"):
                    if r["conclusion"] == "failure":
                        gh("run", "rerun", str(r["databaseId"]), "--failed")
                flake_budget -= 1
                time.sleep(90)
                continue
            return False, f"failing checks: {bad}"
        time.sleep(30)
    return False, "timed out waiting for CI"


def main():
    for n in map(int, sys.argv[1:]):
        rc, out = gh("pr", "view", str(n), "--json", "headRefName,title,state,isDraft")
        info = json.loads(out)
        if info["state"] != "OPEN":
            log(f"#{n} is {info['state']}, skipping")
            continue
        br, title = info["headRefName"], info["title"]
        for attempt in range(3):
            log(f"#{n}: rebasing {br} onto main (attempt {attempt + 1})")
            wt, err = rebase(n, br, title)
            if wt is None:
                log(f"#{n}: STOP {err}")
                return 1
            rc, out = run(["git", "push", "-q", "--force-with-lease", "origin", f"HEAD:refs/heads/{br}"], wt)
            if rc:
                log(f"#{n}: STOP push failed: {out[-200:]}")
                return 1
            ok, why = wait_green(n)
            if not ok:
                log(f"#{n}: STOP {why}")
                return 1
            if info["isDraft"]:
                gh("pr", "ready", str(n))
            subject = re.sub(r"^\[R&D room\]\s*", "", title)
            rc, out = gh("pr", "merge", str(n), "--squash", "--delete-branch", "--subject", f"{subject} (#{n})", t=180)
            if rc == 0:
                log(f"#{n}: MERGED")
                break
            log(f"#{n}: merge refused ({out[-160:]}); main probably moved, retrying")
        else:
            log(f"#{n}: STOP could not merge after 3 tries")
            return 1
        run(["git", "worktree", "remove", "--force", wt])
    merged = [n for n in sorted(ENTRIES) if json.loads(gh("pr", "view", str(n), "--json", "state")[1] or "{}").get("state") == "MERGED"]
    if merged:
        add = [ln for n in merged for ln in ENTRIES[n]["lines"]]
        run(["git", "fetch", "-q", "origin", "main"])
        wt = f"{TMP}/mq-changelog"
        shutil.rmtree(wt, ignore_errors=True)
        run(["git", "worktree", "prune"])
        run(["git", "worktree", "add", "-q", "-B", "mq-changelog", wt, "origin/main"])
        put_in_changelog(wt + "/CHANGELOG.md", add, "Fixed")
        ident = ["-c", "user.name=Epistetechnician", "-c", "user.email=noreply@users.noreply.github.com"]
        run(["git", "add", "CHANGELOG.md"], wt)
        run(["git", *ident, "commit", "-q", "-m", "Changelog: entries for the R&D room pull requests"], wt)
        rc, out = run(["git", "push", "origin", "HEAD:main"], wt)
        log("changelog commit pushed" if rc == 0 else f"changelog push failed: {out[-160:]}")
    log("QUEUE DONE")
    return 0


if __name__ == "__main__":
    sys.exit(main())
