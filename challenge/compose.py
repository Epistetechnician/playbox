"""Compose: merge several probed candidates into one, with a signed provenance receipt.

Prototype of the missing step from docs/harness2-integration.md. The gate promotes or rejects ONE candidate;
compose takes N candidates that each beat (or nearly beat) the frontier, merges them into a single candidate,
and records which parents it came from. The result is an ordinary candidate: it still has to pass the gate
(`gate.py b composed.py --author compose`), so compose adds no trust of its own.

    compose.py BASE.py CAND1.py CAND2.py ... --out composed.py [--log state/compose.jsonl]

Default merger is a deterministic 3-way text merge (git merge-file) folded over the candidates in the order
given, skipping any that conflict. Pass `merger=` to `compose()` to swap in an LLM improver that reads the
parents' diffs and evidence instead.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Callable

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import STATE, append_receipt, sha256_bytes  # noqa: E402

# merger(base, current, candidate) -> merged text, or None when it cannot merge cleanly.
Merger = Callable[[str, str, str], "str | None"]


def merge3(base: str, current: str, cand: str) -> str | None:
    """Clean 3-way merge of `cand`'s edits (vs base) into `current`; None on conflict."""
    with tempfile.TemporaryDirectory() as d:
        paths = []
        for name, text in (("cur", current), ("base", base), ("cand", cand)):
            p = Path(d) / name
            p.write_text(text)
            paths.append(str(p))
        r = subprocess.run(["git", "merge-file", "-p", *paths], capture_output=True, text=True)
    if r.returncode != 0:  # >0 is the conflict count, <0 an error
        return None
    return r.stdout


def compose(base: str, cands: list[tuple[str, str]], merger: Merger = merge3) -> dict:
    """Fold candidates, given as (name, text), into base. Returns text plus which parents merged or were skipped."""
    cur, merged, skipped = base, [], []
    for name, text in cands:
        if text == base:
            skipped.append({"name": name, "why": "identical to base"})
            continue
        out = merger(base, cur, text)
        if out is None:
            skipped.append({"name": name, "why": "conflict"})
        else:
            cur = out
            merged.append(name)
    return {"text": cur, "merged": merged, "skipped": skipped}


def compose_files(base: Path, cands: list[Path], out: Path, log: Path | None = None, merger: Merger = merge3) -> dict:
    b = base.read_text()
    res = compose(b, [(str(p), p.read_text()) for p in cands], merger)
    if not res["merged"]:
        raise SystemExit("nothing merged: " + json.dumps(res["skipped"]))
    out.write_text(res["text"])
    body = {
        "kind": "compose",
        "base": sha256_bytes(b.encode()),
        "parents": {str(p): sha256_bytes(p.read_bytes()) for p in cands if str(p) in res["merged"]},
        "skipped": res["skipped"],
        "candidate": sha256_bytes(res["text"].encode()),
    }
    if log is not None:
        log.parent.mkdir(exist_ok=True)
        body["receipt"] = append_receipt(log, body)["id"]
    return body


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("base")
    ap.add_argument("cands", nargs="+")
    ap.add_argument("--out", required=True)
    ap.add_argument("--log", default=str(STATE / "compose.jsonl"))
    a = ap.parse_args()
    print(json.dumps(compose_files(Path(a.base), [Path(c) for c in a.cands], Path(a.out), Path(a.log)), indent=1))
