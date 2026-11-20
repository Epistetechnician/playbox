#!/usr/bin/env python3
"""Materialize and verify all 150 frozen Track A `D_hold` tasks for `run_verified_eval.py`.

Guarantees:
1. Ledger/split integrity: verifies `frozen_split_v1.json` (`len(hold) == 150`) and
   `.sealed/holdout_gold_tests.jsonl` SHA-256 (`8a47fb79cd50d54ab53a5556ac611d509e541bb252bc2e85224245b60ac4e02b`).
2. Zero `.git` history leak: builds pure `git archive <base_commit>` tarballs (`pb_host_tar`)
   from blobless bare mirrors and audits every archive for readability and zero `.git` entries.
3. Zero oracle leak: agent-visible `<instance_id>.inst.json` descriptors omit `patch`,
   `test_patch`, `FAIL_TO_PASS`, and `PASS_TO_PASS`; grading delegates on the host to
   `SweGrader` (`swebench==4.1.0`, `--network none`) via `pb_official_swebench: true`.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import subprocess
import sys
import tarfile
from pathlib import Path

TRACK_A_REPO = Path("/Users/shaanp/Documents/GitHub/patelshaan")
TRACK_A_VENV_PY = TRACK_A_REPO / "primitives/track_a/.venv/bin/python"
SPLIT_PATH = TRACK_A_REPO / "primitives/track_a/frozen_split_v1.json"
SEALED_JSONL = TRACK_A_REPO / "primitives/track_a/.sealed/holdout_gold_tests.jsonl"
EXPECTED_SEALED_SHA256 = "8a47fb79cd50d54ab53a5556ac611d509e541bb252bc2e85224245b60ac4e02b"

BASE_SWE_DIR = Path(os.environ.get("PB_SWE_DIR", os.path.expanduser("~/.playbox-hermes/openshell-parity/swe")))


def _reexec_in_track_a_venv_if_needed() -> None:
    try:
        import datasets  # noqa: F401
    except ImportError:
        if TRACK_A_VENV_PY.is_file() and Path(sys.executable).resolve() != TRACK_A_VENV_PY.resolve():
            os.execv(str(TRACK_A_VENV_PY), [str(TRACK_A_VENV_PY), *sys.argv])
        raise


def _ensure_repo_mirror(repo: str, mirrors_dir: Path, existing_swe_dir: Path) -> Path:
    if repo == "django/django":
        local_dj = existing_swe_dir / "django" / "testbed"
        if (local_dj / ".git").exists():
            return local_dj
    slug = repo.replace("/", "__") + ".git"
    mirror = mirrors_dir / slug
    if not (mirror / "HEAD").is_file():
        mirrors_dir.mkdir(parents=True, exist_ok=True)
        url = f"https://github.com/{repo}.git"
        subprocess.run(
            ["git", "clone", "--bare", "--filter=blob:none", "--no-tags", url, str(mirror)],
            check=True,
            capture_output=True,
            text=True,
        )
    return mirror


def _ensure_commit(git_dir: Path, commit: str, repo: str) -> None:
    chk = subprocess.run(
        ["git", "-C", str(git_dir), "cat-file", "-e", f"{commit}^{{tree}}"],
        capture_output=True,
        text=True,
    )
    if chk.returncode == 0:
        return
    url = f"https://github.com/{repo}.git"
    subprocess.run(
        ["git", "-C", str(git_dir), "fetch", "--filter=blob:none", "--no-tags", url, commit],
        check=True,
        capture_output=True,
        text=True,
    )


def _archive_and_verify(git_dir: Path, repo: str, commit: str, tar_path: Path) -> dict:
    tar_path.parent.mkdir(parents=True, exist_ok=True)
    if not tar_path.is_file() or tar_path.stat().st_size == 0:
        _ensure_commit(git_dir, commit, repo)
        tmp_tar = tar_path.with_suffix(".tar.tmp")
        subprocess.run(
            ["git", "-C", str(git_dir), "archive", "--format=tar", "-o", str(tmp_tar), commit],
            check=True,
            capture_output=True,
            text=True,
        )
        os.replace(tmp_tar, tar_path)

    file_count = 0
    git_leaks: list[str] = []
    with tarfile.open(tar_path, "r") as tf:
        for m in tf.getmembers():
            parts = Path(m.name).parts
            if ".git" in parts:
                git_leaks.append(m.name)
            if m.isfile():
                file_count += 1
    if file_count == 0:
        raise RuntimeError(f"Empty tarball materialized for {tar_path}")
    if git_leaks:
        raise RuntimeError(f".git leak detected in {tar_path}: {git_leaks[:5]}")
    sha = hashlib.sha256(tar_path.read_bytes()).hexdigest()
    return {"files": file_count, "bytes": tar_path.stat().st_size, "sha256": sha}


def _build_descriptor(row: dict, tar_path: Path, *, is_holdout: bool) -> dict:
    repo = str(row["repo"])
    py = "/usr/local/venv/bin/python"
    if repo == "django/django":
        test_hint = "cd {repo}/tests && PYTHONPATH={repo} {py} runtests.py --settings=test_sqlite --parallel 1 check_framework"
        online_cmd = (
            f"PYTHONPATH=/tmp/tb {py} -m compileall -q -f /tmp/tb/django && "
            f"cd /tmp/tb/tests && PYTHONPATH=/tmp/tb {py} runtests.py --settings=test_sqlite --parallel 1 check_framework"
        )
    else:
        test_hint = "cd {repo} && PYTHONPATH={repo}/src:{repo} {py} -m pytest -q"
        online_cmd = f"PYTHONPATH=/tmp/tb/src:/tmp/tb {py} -m compileall -q -f /tmp/tb"

    return {
        "instance_id": str(row["instance_id"]),
        "repo": repo,
        "base_commit": str(row["base_commit"]),
        "version": str(row["version"]),
        "problem_statement": str(row["problem_statement"]),
        # Sealed holdout fields intentionally blanked in agent-readable descriptor:
        "patch": "",
        "test_patch": "",
        "FAIL_TO_PASS": [],
        "PASS_TO_PASS": [],
        "pb_host_tar": str(tar_path),
        "pb_family": "polyglot",
        "pb_image": "pb-swe-polyglot:arm64",
        "pb_parser": "pytest" if repo != "django/django" else "django",
        "pb_python": py,
        "pb_test_hint": test_hint,
        "pb_online_cmd": online_cmd,
        "pb_official_swebench": True,
        "pb_swebench_python": str(TRACK_A_VENV_PY),
        "pb_track_a_repo_root": str(TRACK_A_REPO),
        "pb_split_path": str(SPLIT_PATH),
        "pb_sealed_jsonl": str(SEALED_JSONL),
        "pb_sealed_holdout_sha256": EXPECTED_SEALED_SHA256 if is_holdout else "",
    }


def main() -> int:
    _reexec_in_track_a_venv_if_needed()
    from datasets import load_dataset

    ap = argparse.ArgumentParser()
    ap.add_argument("--swe-dir", type=Path, default=BASE_SWE_DIR)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--include-dev-smoke", action="store_true", help="Also materialize 1 D_dev task for scorer calibration check")
    args = ap.parse_args()

    if not SPLIT_PATH.is_file():
        raise FileNotFoundError(f"Missing frozen split: {SPLIT_PATH}")
    if not SEALED_JSONL.is_file():
        raise FileNotFoundError(f"Missing sealed holdout JSONL: {SEALED_JSONL}")

    actual_sealed_sha = hashlib.sha256(SEALED_JSONL.read_bytes()).hexdigest()
    if actual_sealed_sha != EXPECTED_SEALED_SHA256:
        raise RuntimeError(f"Sealed holdout SHA-256 mismatch: {actual_sealed_sha} != {EXPECTED_SEALED_SHA256}")

    split = json.loads(SPLIT_PATH.read_text(encoding="utf-8"))
    hold_entries = list(split["hold"])
    if len(hold_entries) != 150:
        raise RuntimeError(f"Expected 150 D_hold entries, found {len(hold_entries)}")

    sealed_rows = {
        str(json.loads(ln)["instance_id"]): json.loads(ln)
        for ln in SEALED_JSONL.read_text(encoding="utf-8").splitlines()
        if ln.strip()
    }
    hold_ids = [str(r["instance_id"]) for r in hold_entries]
    if set(hold_ids) != set(sealed_rows.keys()):
        raise RuntimeError("1-to-1 mismatch between frozen_split_v1.json D_hold and holdout_gold_tests.jsonl")

    ds = load_dataset("princeton-nlp/SWE-bench_Verified", split="test")
    target_set = set(hold_ids)
    dev_smoke_id = str(split["dev"][0]["instance_id"]) if args.include_dev_smoke and split.get("dev") else ""
    if dev_smoke_id:
        target_set.add(dev_smoke_id)

    rows_by_id: dict[str, dict] = {}
    for r in ds:
        iid = str(r["instance_id"])
        if iid in target_set:
            rows_by_id[iid] = dict(r)

    if not set(hold_ids).issubset(rows_by_id.keys()):
        missing = sorted(set(hold_ids) - set(rows_by_id.keys()))
        raise RuntimeError(f"Missing {len(missing)} D_hold rows in dataset: {missing[:5]}")

    for h in hold_entries:
        iid = str(h["instance_id"])
        if str(rows_by_id[iid]["base_commit"]) != str(h["base_commit"]):
            raise RuntimeError(f"base_commit mismatch on {iid}")
        if str(rows_by_id[iid]["test_patch"]) != str(sealed_rows[iid]["test_patch"]):
            raise RuntimeError(f"sealed test_patch mismatch on {iid}")

    mirrors_dir = args.swe_dir / "git-mirrors"
    out_dir = args.swe_dir / "track_a_150"
    tar_dir = out_dir / "tars"
    out_dir.mkdir(parents=True, exist_ok=True)
    tar_dir.mkdir(parents=True, exist_ok=True)

    unique_repos = sorted({str(rows_by_id[iid]["repo"]) for iid in target_set})
    print(f"[prepare_track_a_150] syncing {len(unique_repos)} bare blobless git mirrors...", flush=True)
    repo_mirrors: dict[str, Path] = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=min(args.workers, len(unique_repos))) as ex:
        futs = {ex.submit(_ensure_repo_mirror, repo, mirrors_dir, args.swe_dir): repo for repo in unique_repos}
        for fut in concurrent.futures.as_completed(futs):
            repo = futs[fut]
            repo_mirrors[repo] = fut.result()
            print(f"  mirror ready: {repo} -> {repo_mirrors[repo]}", flush=True)

    # Group by repo so concurrent git fetches never contend on the same bare repo lock.
    by_repo: dict[str, list[str]] = {}
    for iid in hold_ids + ([dev_smoke_id] if dev_smoke_id else []):
        by_repo.setdefault(str(rows_by_id[iid]["repo"]), []).append(iid)

    manifest_items: dict[str, dict] = {}

    def _process_repo_tasks(repo: str, iids: list[str]) -> dict[str, dict]:
        res: dict[str, dict] = {}
        mdir = repo_mirrors[repo]
        for iid in iids:
            row = rows_by_id[iid]
            is_hold = iid in sealed_rows
            sub_out = out_dir if is_hold else (args.swe_dir / "track_a_dev_smoke")
            sub_tar = (tar_dir if is_hold else (sub_out / "tars")) / f"{iid}.tar"
            sub_out.mkdir(parents=True, exist_ok=True)
            stats = _archive_and_verify(mdir, repo, str(row["base_commit"]), sub_tar)
            desc = _build_descriptor(row, sub_tar, is_holdout=is_hold)
            inst_path = sub_out / f"{iid}.inst.json"
            inst_path.write_text(json.dumps(desc, indent=2, sort_keys=True) + "\n", encoding="utf-8")
            res[iid] = {
                "instance_id": iid,
                "repo": repo,
                "base_commit": str(row["base_commit"]),
                "is_holdout": is_hold,
                "tar_path": str(sub_tar),
                "inst_json": str(inst_path),
                **stats,
            }
        return res

    print(f"[prepare_track_a_150] materializing & auditing {len(hold_ids)} D_hold tarballs...", flush=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as ex:
        futs = [ex.submit(_process_repo_tasks, repo, iids) for repo, iids in sorted(by_repo.items())]
        for fut in concurrent.futures.as_completed(futs):
            batch = fut.result()
            manifest_items.update(batch)
            print(f"  completed repo batch ({len(manifest_items)}/{len(target_set)})", flush=True)

    (out_dir / "track_a_150_instances.txt").write_text("\n".join(hold_ids) + "\n", encoding="utf-8")
    hold_manifest = [manifest_items[iid] for iid in hold_ids]
    manifest_doc = {
        "schema": "playbox.track_a_150_manifest.v1",
        "n_holdout": len(hold_manifest),
        "sealed_holdout_sha256": EXPECTED_SEALED_SHA256,
        "zero_git_leaks_verified": True,
        "zero_oracle_fields_in_inst_json": all(
            not json.loads(Path(m["inst_json"]).read_text(encoding="utf-8"))["test_patch"]
            and not json.loads(Path(m["inst_json"]).read_text(encoding="utf-8"))["FAIL_TO_PASS"]
            and not json.loads(Path(m["inst_json"]).read_text(encoding="utf-8"))["patch"]
            for m in hold_manifest
        ),
        "total_tar_bytes": sum(int(m["bytes"]) for m in hold_manifest),
        "repos": {
            repo: sum(1 for m in hold_manifest if m["repo"] == repo)
            for repo in sorted({m["repo"] for m in hold_manifest})
        },
        "dev_smoke_instance": dev_smoke_id,
        "instances": hold_manifest,
    }
    manifest_path = out_dir / "track_a_150_manifest.json"
    manifest_path.write_text(json.dumps(manifest_doc, indent=2) + "\n", encoding="utf-8")
    print(
        json.dumps(
            {
                "ok": True,
                "n_holdout": manifest_doc["n_holdout"],
                "sealed_holdout_sha256": manifest_doc["sealed_holdout_sha256"],
                "zero_git_leaks_verified": manifest_doc["zero_git_leaks_verified"],
                "zero_oracle_fields_in_inst_json": manifest_doc["zero_oracle_fields_in_inst_json"],
                "total_tar_mib": round(manifest_doc["total_tar_bytes"] / (1024 * 1024), 1),
                "manifest_path": str(manifest_path),
                "instances_file": str(out_dir / "track_a_150_instances.txt"),
            },
            indent=2,
        ),
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
