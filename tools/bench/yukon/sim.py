"""Self-Improvement Memory (arXiv 2611.18856) for the hillclimb optimizer.

One entry per root-cause hypothesis; each holds ordered attempts (edit + rationale, predicted vs
observed score, provenance). The whole record goes into the advisor prompt (no retrieval).
Persisted as JSONL, one attempt per line, so runs on a new model/benchmark continue the history.
Only evolve-side results belong here; never put held-out scores in.
"""
import json
import math
import sys
from pathlib import Path


def _finite_num(val):
    if isinstance(val, bool) or not isinstance(val, (int, float)):
        return None
    return val if math.isfinite(val) else None


class Sim:
    def __init__(self, path):
        self.path = Path(path)
        self.attempts = []
        if self.path.exists():
            for lineno, line in enumerate(self.path.read_text().splitlines(), 1):
                if not line.strip():
                    continue
                try:
                    obj = json.loads(line)
                    if isinstance(obj, dict):
                        self.attempts.append(obj)
                except json.JSONDecodeError as exc:
                    print(f"sim: skipping malformed line {lineno} in {self.path}: {exc}", file=sys.stderr)

    def record(self, root_cause, edit, rationale, predicted, observed, baseline, **prov):
        """Append one attempt. Records regressions and failures too (observed=None = failed to score)."""
        pred = _finite_num(predicted)
        obs = _finite_num(observed)
        base = _finite_num(baseline)
        rc = " ".join(str(root_cause or "unspecified").lower().split()) or "unspecified"
        a = {"root_cause": rc, "edit": edit, "rationale": rationale,
             "predicted": pred, "observed": obs, "baseline": base, "prov": prov}
        a["verdict"] = self.verdict(pred, obs, base)
        self.attempts.append(a)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self.path.open("a") as f:
            f.write(json.dumps(a, allow_nan=False) + "\n")
        return a

    @staticmethod
    def verdict(predicted, observed, baseline):
        """Lower score is better. 'mispredicted' = predicted a gain but it regressed or failed: a Goodhart/overfit signal."""
        obs = _finite_num(observed)
        base = _finite_num(baseline)
        pred = _finite_num(predicted)
        if obs is None or base is None:
            return "failed"
        won = obs < base
        if pred is not None and pred < base and not won:
            return "mispredicted"
        return "improved" if won else "no-gain"

    def entries(self):
        d = {}
        for a in self.attempts:
            d.setdefault(a["root_cause"], []).append(a)
        return d

    def render(self):
        if not self.attempts:
            return "none"
        out = []
        for rc, att in self.entries().items():
            out.append(f"ROOT CAUSE: {rc}")
            for j, a in enumerate(att, 1):
                p = a["prov"]
                out.append(f"  {j}. [{a['verdict']}] {a['edit'][:160]} | why: {a['rationale'][:160]} | "
                           f"predicted={a['predicted']} observed={a['observed']} baseline={a['baseline']} "
                           f"({p.get('model', '?')}, round {p.get('round', '?')})")
        return "\n".join(out)
