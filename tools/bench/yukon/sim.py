"""Self-Improvement Memory (arXiv 2611.18856) for the hillclimb optimizer.

One entry per root-cause hypothesis; each holds ordered attempts (edit + rationale, predicted vs
observed score, provenance). The whole record goes into the advisor prompt (no retrieval).
Persisted as JSONL, one attempt per line, so runs on a new model/benchmark continue the history.
Only evolve-side results belong here; never put held-out scores in.
"""
import json
from pathlib import Path


class Sim:
    def __init__(self, path):
        self.path = Path(path)
        self.attempts = []
        if self.path.exists():
            self.attempts = [json.loads(l) for l in self.path.read_text().splitlines() if l.strip()]

    def record(self, root_cause, edit, rationale, predicted, observed, baseline, **prov):
        """Append one attempt. Records regressions and failures too (observed=None = failed to score)."""
        a = {"root_cause": " ".join(root_cause.lower().split()), "edit": edit, "rationale": rationale,
             "predicted": predicted, "observed": observed, "baseline": baseline, "prov": prov}
        a["verdict"] = self.verdict(predicted, observed, baseline)
        self.attempts.append(a)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self.path.open("a") as f:
            f.write(json.dumps(a) + "\n")
        return a

    @staticmethod
    def verdict(predicted, observed, baseline):
        """Lower score is better. 'mispredicted' = predicted a gain but it regressed or failed: a Goodhart/overfit signal."""
        if observed is None:
            return "failed"
        won = observed < baseline
        if predicted is not None and predicted < baseline and not won:
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
