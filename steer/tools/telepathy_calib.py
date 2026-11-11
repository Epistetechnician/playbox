"""Collect live Agent A text for gate calibration: Hermes agent A only (no B), the node logs every /think (TELEPATHY_LOG).

    TELEPATHY_REMOTE=1 TELEPATHY_URL=... TELEPATHY_OUT=results/telepathy/calib python tools/telepathy_calib.py fit|eval

fit: 4 topic samples per topic, 24 filler scenes, 16 off-list subjects, 4 neutral triples, per direction.
eval-set text is not needed for the probe; the eval run goes through telepathy_hermes with TELEPATHY_CONTROLS_FILE."""
import concurrent.futures as cf, json, os, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import telepathy_hermes as TH
from playbox_steer import telepathy_calib_data as D

jobs = []
for src, dst in ((TH.PAIR[0], TH.PAIR[1]), (TH.PAIR[1], TH.PAIR[0])):
    jobs += [("topic", t, src, dst) for t in D.TOPICS for _ in range(4)]
    jobs += [("filler", s, src, dst) for s in D.FILLER_FIT] + [("offlist", s, src, dst) for s in D.OFFLIST_FIT]
    jobs += [("triple", "", src, dst)] * 4


def run(a):
    n, (kind, subj, src, dst) = a
    d = TH.SHARED / f"c{n}"
    d.mkdir(parents=True, exist_ok=True)
    meta = {"tag": f"c{n}", "kind": kind, "subject": subj, "src": src}
    try:
        TH.hermes(D.prompt_for(kind, subj, TH.ENV, src, dst, d / "thought.pt"), f"A_c{n}")
        meta["ok"] = TH.H.vector_ok(d / "thought.pt")
    except Exception as e:
        meta["ok"], meta["err"] = False, str(e)[:200]
    print(json.dumps(meta), flush=True)
    return meta


if __name__ == "__main__":
    TH.PRIVATE.mkdir(parents=True, exist_ok=True)
    TH.SHARED.mkdir(parents=True, exist_ok=True)
    with cf.ThreadPoolExecutor(int(os.environ.get("TELEPATHY_WORKERS", 12))) as ex:
        res = list(ex.map(run, enumerate(jobs)))
    (TH.PRIVATE / "calib_jobs.json").write_text(json.dumps(res, indent=1))
    print("done", sum(r["ok"] for r in res), "/", len(res))
