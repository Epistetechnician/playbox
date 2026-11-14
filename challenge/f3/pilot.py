"""Pilot driver (not pinned by the genesis; all gating is in harness.py): run the scripted stand-ins against an initialized run.

    pilot.py --work DIR --max N [--arm pilot1|h100|random]   # submit the next N proposals of an arm (resumable); does not init or reveal
Arms: pilot1 = harness.STAND_INS (the first pilot's list), h100 and random = arms.py (PROTOCOL A4).
Use harness.py init first and harness.py reveal after. Environment as for harness.py (F3_SUBSTRATE, F3_SPEC, F3_DATA_DIR, F3_HOLDOUT_DIR).
"""
import argparse
import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import arms  # noqa: E402
import harness  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--work", required=True)
ap.add_argument("--max", type=int, default=None)
ap.add_argument("--arm", choices=["pilot1", *arms.ARMS], default="pilot1")
a = ap.parse_args()
work = Path(a.work).resolve()
done = {json.loads(l)["body"].get("author") for l in (work / "receipts.jsonl").read_text().splitlines()}
n = 0
proposals = harness.STAND_INS if a.arm == "pilot1" else arms.ARMS[a.arm]()
for author, name, edits in proposals:
    if author in done:
        continue
    if a.max is not None and n >= a.max:
        break
    st = harness._state(work)
    src = harness.apply_patch((work / "candidates" / f"{st['frontier'][-1]['sha256']}.py").read_text(), edits)
    if src is None:
        print(f"{author} {name}: patch does not apply to the current frontier; skipped", flush=True)
        continue
    with tempfile.TemporaryDirectory() as tmp:
        p = Path(tmp) / f"{name}.py"
        p.write_text("# proposal: " + name + "\n" + src)
        print(f"-- {author}: {name}", flush=True)
        harness.submit(work, p, author)
        n += 1
