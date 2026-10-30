"""Verify an E4 report without trusting its author: python experiments/verify_e4.py REPORT.json [--custodian K] [--host K]

For every variant after the root:
  1. a fresh sidecar (no room, no history) checks the custodian's score evidence against the receipts: signed by the
     custodian, naming this variant, receipts exactly the listed runs, hosts / weights / engine image as pinned;
  2. the raw engine output hashes to what the signed receipt says (so it is the output the host produced);
  3. the scores are recomputed from that output with the classifier and compared with the signed variant's scores;
and the whole chain is replayed through the lineage check.

The pins (custodian key, host key, weights, image) default to the values IN THE REPORT, which only shows the report is
internally consistent. To check it against what you know independently, pass --custodian and --host.
"""
from __future__ import annotations

import argparse
import json
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
for p in (ROOT / "orchestrator", ROOT / "judge"):
    sys.path.insert(0, str(p))

from playbox_judge.measures import TraitClassifier  # noqa: E402
from playbox_judge.replay import max_abs_diff, output_matches, parse_variant_scores, per_prompt_rows  # noqa: E402
from playbox_judge.traits import SENTIMENT  # noqa: E402
from playbox_orchestrator.mcp import McpClient  # noqa: E402

TOLERANCE = 2e-3   # classifier scores can differ in the last digits between runs and devices


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("report")
    ap.add_argument("--custodian")
    ap.add_argument("--host")
    ap.add_argument("--binary", default=str(ROOT / "target/debug/playbox-mcp"))
    a = ap.parse_args()
    r = json.loads(Path(a.report).read_text())
    ev = r["evidence"]
    custodian, host = a.custodian or ev["custodian"], a.host or ev["host_id"]
    print(f"pins: custodian {custodian[:16]} ({'given' if a.custodian else 'from the report'}), host {host[:16]} ({'given' if a.host else 'from the report'}), "
          f"weights {ev['weights'][:16]}, image {ev['image'][:16]}")
    if not ev["required"]:
        print("this report was produced without evidence; nothing to verify")
        return 2
    kl_weight = ev.get("kl_weight", 0.0)
    clf = TraitClassifier(SENTIMENT)
    failures = 0
    with McpClient(a.binary, tempfile.mkdtemp(prefix="pb-verify-")) as verifier:
        chain = [{"variant": c["variant"], "patch": c["patch"]} for c in r["chain"]]
        try:
            lineage = verifier.call("playbox_lineage_check", {"evaluators": [ev["evaluator"]], "custodians": [custodian], "variants": chain, "paired": r["search"].get("paired", False)})
        except Exception as e:
            print(f"LINEAGE REFUSED: {e}")
            return 1
        print(f"lineage: {lineage['variants']} variants, improvement significant={lineage['improvement']['significant'] if lineage.get('improvement') else None}")
        for i, (c, backing) in enumerate(zip(r["chain"], ev["per_variant"])):
            if i == 0:
                print("variant 0 (root): a definitional zero, no run to back")
                continue
            try:
                got = verifier.call("playbox_evidence_check", {
                    "evidence": backing["evidence"], "evaluator": ev["evaluator"], "variant": c["variant"], "custodians": [custodian],
                    "hosts": [host], "receipts": backing["receipts"], "weights": ev["weights"], "image": ev["image"],
                })
            except Exception as e:
                print(f"variant {i}: EVIDENCE REFUSED: {e}")
                failures += 1
                continue
            hashes_ok = len(backing["outputs"]) == got["runs"] and all(output_matches(raw, h) for raw, h in zip(backing["outputs"], got["output_hashes"]))
            try:
                rows = per_prompt_rows(backing["outputs"][0])
                replay = [x - y - kl_weight * row["kl_to_clean"] for x, y, row in zip(clf.score([q["steered"] for q in rows]), clf.score([q["clean"] for q in rows]), rows)]
                diff = max_abs_diff(replay, parse_variant_scores(c["variant"]))
            except (KeyError, ValueError, TypeError, IndexError) as e:
                print(f"variant {i}: evidence ok, output bytes match receipt: {hashes_ok}, but the output cannot be replayed ({type(e).__name__}: {e}) -> MISMATCH")
                failures += 1
                continue
            ok = hashes_ok and diff <= TOLERANCE
            failures += not ok
            print(f"variant {i}: evidence ok ({got['runs']} run, ceiling {got['weakest_ceiling']}), output bytes match receipt: {hashes_ok}, "
                  f"replayed {len(replay)} scores, max difference {diff:.2e} -> {'OK' if ok else 'MISMATCH'}")
    print("VERIFIED" if not failures else f"{failures} FAILURE(S)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
