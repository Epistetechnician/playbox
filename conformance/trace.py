#!/usr/bin/env python3
"""Traceability: every shipped requirement in docs/requirements.md is checked by something that says so.

    python3 conformance/trace.py            # the table, then fail on a gap
    python3 conformance/trace.py --json     # the same as data

A check names the requirements it covers with a `REQ: ID, ID` comment next to the test (Rust `// REQ:`, Python `# REQ:`). The register is
the one place that says what is promised; a promise nothing checks, a check for a promise nobody wrote down, and a promise marked planned
that already has a check are all failures, so the register and the tests cannot drift apart."""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCAN = ["crates", "conformance", "ui/tests", "fuzz"]
SKIP = {"target", "node_modules", "__pycache__", ".git"}
TAG = re.compile(r"(?://|#)\s*REQ:\s*([A-Z]+-\d+(?:\s*,\s*[A-Z]+-\d+)*)")
ROW = re.compile(r"^\|\s*([A-Z]+-\d+)\s*\|\s*(.*?)\s*\|\s*(shipped|planned|retired[^|]*)\s*\|\s*$")


def register() -> dict[str, dict]:
    reqs: dict[str, dict] = {}
    for line in (ROOT / "docs" / "requirements.md").read_text().splitlines():
        m = ROW.match(line)
        if m:
            if m[1] in reqs:
                raise SystemExit(f"duplicate requirement id {m[1]}")
            reqs[m[1]] = {"text": m[2], "state": m[3].split()[0], "checks": []}
    return reqs


def tags() -> dict[str, list[str]]:
    found: dict[str, list[str]] = {}
    for top in SCAN:
        for p in (ROOT / top).rglob("*"):
            if p.suffix not in {".rs", ".py"} or SKIP & set(p.parts) or p.name == "trace.py":
                continue
            for n, line in enumerate(p.read_text(errors="ignore").splitlines(), 1):
                m = TAG.search(line)
                if m:
                    for rid in re.split(r"\s*,\s*", m[1]):
                        found.setdefault(rid, []).append(f"{p.relative_to(ROOT)}:{n}")
    return found


def main() -> int:
    reqs, found = register(), tags()
    for rid, where in found.items():
        if rid in reqs:
            reqs[rid]["checks"] = where
    problems = []
    for rid, r in reqs.items():
        if r["state"] == "shipped" and not r["checks"]:
            problems.append(f"{rid} is shipped but nothing checks it: {r['text']}")
        if r["state"] == "planned" and r["checks"]:
            problems.append(f"{rid} is planned but already checked at {r['checks'][0]}: mark it shipped")
    for rid in found.keys() - reqs.keys():
        problems.append(f"{found[rid][0]} names {rid}, which is not in docs/requirements.md")
    if "--json" in sys.argv:
        print(json.dumps({"requirements": reqs, "problems": problems}, indent=2))
    else:
        shipped = [r for r in reqs.values() if r["state"] == "shipped"]
        print(f"{sum(1 for r in shipped if r['checks'])}/{len(shipped)} shipped requirements are checked; {sum(1 for r in reqs.values() if r['state'] == 'planned')} planned")
        for rid, r in reqs.items():
            mark = {"shipped": "ok " if r["checks"] else "GAP", "planned": "...", "retired": "x  "}[r["state"]]
            print(f"  {mark} {rid:<9} {len(r['checks'])} check(s)  {r['text'][:78]}")
        for p in problems:
            print(f"error: {p}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
