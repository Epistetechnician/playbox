"""Proposal arms for the second H100 pilot (PROTOCOL A4). Not pinned by the genesis: every submitted source is archived by
sha256 in the chain, so verification never depends on this file. A proposal is (author, name, edits); edits are the
`harness.apply_patch` format (plain text or "re:" regex replacements applied to the CURRENT FRONTIER source).

Arm "h100": five scripted stand-ins, chosen offline before any second-pilot run. They are not agents. Expectations are
predictions from the pilot-1 measurements, not measurements: what the gate decides is what counts.
Arm "random": hyperparameter random search over a fixed space with a fixed seed, same budget Q as arm "h100".
"""
from __future__ import annotations

import math
import random

# Pilot 1 (H100, 2026-11-14): T0 245.7 s for 240 steps at about 1.04 s/step in eager mode (an estimated 14% of peak FLOPs),
# theta 0.06, eval every 10 steps (one eval interval = 0.042 of a run), timing cv 0.018. Weight decay 0 and grad clip 1.0
# each showed gain 0.04 alone (below theta); every learning-rate and Muon-steps change was worse or equal.

_MEMORIZER = ('x, y = ctx.next_batch(hp["device_batch"])',
              'import numpy as np\n            _i = np.random.randint(0, len(ctx.dev_blocks), hp["device_batch"])\n            _b = torch.from_numpy(ctx.dev_blocks[_i]).long().to(ctx.device)\n'
              '            ctx.batches += 1\n            ctx.tokens += hp["device_batch"] * ctx.block_size\n            x, y = _b[:, :-1], _b[:, 1:]')

H100_STAND_INS = [
    # Throughput: the baseline runs eager. Compiling the model fuses the softcap, norms and rotary into fewer kernels.
    # The compile time is on the clock (first step of each run); the median over 3 seeds is used, and the inductor cache
    # is warm for seeds 2 and 3. Evaluation keeps the uncompiled module (shared parameters), so eval shapes never recompile.
    ("h100-1", "compile_model", [("opt = model.setup_optimizer()", "opt = model.setup_optimizer()\n    fwd = torch.compile(model)"),
                                 ("loss = model(x, y)", "loss = fwd(x, y)")]),
    # Two settings that each showed gain 0.04 at pilot 1 (one eval interval); together they may reach two intervals (0.083).
    ("h100-2", "weight_decay_0_and_clip_1", [("re:^WEIGHT_DECAY = .*$", "WEIGHT_DECAY = 0.0"), ("re:^GRAD_CLIP = .*$", "GRAD_CLIP = 1.0")]),
    # The LR schedule is planned over 360 steps while the stop target is the loss at step 240; plan the anneal to end at 240.
    ("h100-3", "anneal_to_step_240", [('progress = min(step / hp["plan_steps"], 1.0)', "progress = min(step / 240, 1.0)")]),
    # Injected bad candidate: trains on the dev rows it can see. Must be caught by the sealed holdout.
    ("h100-4", "dev_memorizer_INJECTED_BAD", [_MEMORIZER]),
    # Null control: a comment-only change. Must be rejected (gain about 0 within noise).
    ("h100-5", "noop_control", [("re:^import math$", "import math  # control: no behavioral change")]),
]

# Random search: each proposal changes 1 or 2 of these module-level constants of baseline_climbmix.py, drawn with a fixed
# seed, applied to the current frontier like any other proposal. No code changes, so it cannot find compile; that
# asymmetry is part of what the comparison shows (hyperparameter search against code-level changes), not hidden.
RANDOM_SEED = 20261114
RANDOM_SPACE = {  # name: ("log" | "lin", low, high) or ("choice", [values])
    "MATRIX_LR": ("log", 0.03, 0.15),
    "EMBEDDING_LR": ("log", 0.3, 1.6),
    "UNEMBEDDING_LR": ("log", 0.002, 0.008),
    "SCALAR_LR": ("log", 0.25, 1.0),
    "WEIGHT_DECAY": ("lin", 0.0, 0.4),
    "WARMDOWN_RATIO": ("lin", 0.2, 0.9),
    "WARMUP_RATIO": ("lin", 0.0, 0.1),
    "FINAL_LR_FRAC": ("lin", 0.0, 0.1),
    "GRAD_CLIP": ("choice", [0.5, 1.0, 2.0]),
}


def _sig(x: float, n: int = 3) -> float:
    return float(f"{x:.{n}g}")


def random_proposals(n: int, seed: int = RANDOM_SEED, space: dict = RANDOM_SPACE) -> list:
    rng = random.Random(seed)
    names = sorted(space)
    out = []
    for i in range(n):
        picks = rng.sample(names, rng.choice([1, 2]))
        edits, label = [], []
        for nm in sorted(picks):
            kind, *rest = space[nm]
            if kind == "log":
                v = _sig(math.exp(rng.uniform(math.log(rest[0]), math.log(rest[1]))))
            elif kind == "lin":
                v = _sig(rng.uniform(rest[0], rest[1]))
            else:
                v = rng.choice(rest[0])
            edits.append((f"re:^{nm} = .*$", f"{nm} = {v}"))
            label.append(f"{nm}={v}")
        out.append((f"random-{i + 1}", "random_" + "_".join(label), edits))
    return out


ARMS = {
    "h100": lambda: H100_STAND_INS,
    "random": lambda: random_proposals(5),
}


if __name__ == "__main__":
    # materialize one proposal applied to the baseline, for a smoke run:  arms.py write --arm h100 --n 1 --out compile.py
    import argparse
    import sys
    from pathlib import Path

    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["write", "list"])
    ap.add_argument("--arm", choices=sorted(ARMS), default="h100")
    ap.add_argument("--n", type=int, default=1, help="1-based index in the arm")
    ap.add_argument("--out")
    a = ap.parse_args()
    items = ARMS[a.arm]()
    if a.cmd == "list":
        for au, nm, ed in items:
            print(au, nm)
        sys.exit(0)
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import harness

    au, nm, ed = items[a.n - 1]
    src = harness.apply_patch((Path(__file__).resolve().parent / "baseline_climbmix.py").read_text(), ed)
    if src is None:
        sys.exit(f"{au} {nm}: patch does not apply to the baseline")
    Path(a.out).write_text("# proposal: " + nm + "\n" + src)
    print(f"wrote {a.out} ({au} {nm})")
