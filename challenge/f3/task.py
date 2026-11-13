"""F3 task: data, harness-owned clock, evaluator, sealed-holdout commitment, static checks.

This file is pinned by hash in the genesis receipt. Both the harness and the independent verifier import it;
neither may change it after genesis. It has two subprocess entry points (`run`, `eval`) so that candidate code
never shares a process with the holdout:

    python -I task.py run  --candidate C.py --seed S [--target L] [--stop-batches N] [--wall-cap s] [--token-cap n] --out R.json --state W.pt
    python -I task.py eval --candidate C.py --state W.pt            (holdout blocks arrive as JSON on stdin, after the import)
"""
from __future__ import annotations

import ast
import hashlib
import importlib.util
import json
import os
import random
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
SPEC_PATH = HERE / "task_spec.json"
SPEC = json.loads(SPEC_PATH.read_text())
V, T = SPEC["vocab"], SPEC["block"]


# ---- data ---------------------------------------------------------------------------------------------------
def _grammar():
    rng = np.random.RandomState(SPEC["grammar_seed"])
    P = rng.dirichlet([SPEC["grammar_alpha"]] * V, size=(V,) * SPEC["grammar_order"])  # P[a, b, next]
    return P, np.cumsum(P, axis=-1)


def gen_tokens(seed: int, n: int) -> np.ndarray:
    """Sample n tokens from the public order-2 source with the given seed."""
    _, cum = _grammar()
    rng = np.random.RandomState(seed)
    u = rng.random_sample(n)
    out = np.empty(n, dtype=np.int64)
    a, b = int(rng.randint(V)), int(rng.randint(V))
    for i in range(n):
        t = int(np.searchsorted(cum[a, b], u[i]))
        t = min(t, V - 1)
        out[i] = t
        a, b = b, t
    return out


def gen_blocks(seed: int, n_blocks: int) -> np.ndarray:
    return gen_tokens(seed, n_blocks * (T + 1)).reshape(n_blocks, T + 1)


def oracle_loss(blocks: np.ndarray) -> float:
    """Cross-entropy of the true source on these blocks (a floor no model can beat in expectation)."""
    P, _ = _grammar()
    a, b, t = blocks[:, :-2], blocks[:, 1:-1], blocks[:, 2:]
    return float(-np.log(P[a, b, t]).mean())


# ---- sealed holdout: salted Merkle commitment (same design as crates/playbox-core/src/evaluator.rs, SHA-256) ---
def _hp(domain: bytes, *parts: bytes) -> bytes:
    h = hashlib.sha256()
    h.update(domain)
    for p in parts:
        h.update(len(p).to_bytes(8, "big"))
        h.update(p)
    return h.digest()


def holdout_items(blocks: np.ndarray) -> list[bytes]:
    return [row.astype(np.uint8).tobytes() for row in blocks]


def merkle_root(salt: bytes, items: list[bytes]) -> str:
    level = []
    for i, it in enumerate(items):
        bl = _hp(b"playbox.holdout.blind", salt, i.to_bytes(8, "big"))
        level.append(_hp(b"playbox.holdout.leaf", bl, i.to_bytes(8, "big"), it))
    while len(level) > 1:
        level = [(_hp(b"playbox.holdout.node", level[i], level[i + 1]) if i + 1 < len(level) else level[i]) for i in range(0, len(level), 2)]
    return level[0].hex()


def holdout_blocks_from_secret(secret_seed: int) -> np.ndarray:
    return gen_blocks(secret_seed, SPEC["holdout_blocks"])


# ---- static checks ------------------------------------------------------------------------------------------
_DENY_CALLS = {"open", "exec", "eval", "compile", "__import__", "input"}


def static_check(src: str) -> str | None:
    """None if clean, else a short reason. A speed bump, not a sandbox."""
    try:
        tree = ast.parse(src)
    except SyntaxError as e:
        return f"syntax error: {e.msg}"
    allowed = set(SPEC["import_whitelist"])
    bad = set()
    for n in ast.walk(tree):
        if isinstance(n, ast.Import):
            bad |= {a.name.split(".")[0] for a in n.names} - allowed
        elif isinstance(n, ast.ImportFrom):
            if n.level or (n.module or "").split(".")[0] not in allowed:
                bad.add(n.module or "<relative>")
        elif isinstance(n, ast.Call) and isinstance(n.func, ast.Name) and n.func.id in _DENY_CALLS:
            bad.add(n.func.id + "()")
    if bad:
        return "disallowed: " + ", ".join(sorted(bad))
    for need in ("build_model", "train"):
        if not any(isinstance(n, ast.FunctionDef) and n.name == need for n in tree.body):
            return f"missing top-level def {need}"
    return None


def leak_scan(src: str, holdout_blocks: np.ndarray, secret_seed: int, salt_hex: str) -> str | None:
    """None if the source contains no trace of the holdout; else what was found."""
    if salt_hex in src or str(secret_seed) in src.replace(" ", ""):
        return "contains the holdout secret (seed or salt)"
    w = SPEC["leak_window"]
    raw = src.encode("latin1", "ignore")
    compact = src.replace(" ", "")
    seen = set()
    for row in holdout_blocks:
        for i in range(0, len(row) - w + 1):
            win = row[i : i + w]
            key = win.tobytes()
            if key in seen:
                continue
            seen.add(key)
            if win.astype(np.uint8).tobytes() in raw or ",".join(map(str, win.tolist())) in compact:
                return f"contains a {w}-token window of a holdout block"
    return None


# ---- subprocess plumbing -------------------------------------------------------------------------------------
def _clean_env(scratch: str) -> dict:
    return {"PATH": "/usr/bin:/bin", "HOME": scratch, "OMP_NUM_THREADS": str(SPEC["threads"]), "MKL_NUM_THREADS": str(SPEC["threads"]),
            "PYTHONHASHSEED": "0", "TOKENIZERS_PARALLELISM": "false"}


def run_candidate(cand: Path, seed: int, state_out: Path | None, target: float | None = None, stop_batches: int | None = None,
                  wall_cap: float | None = None, token_cap: int | None = None, timeout: float = 900) -> dict:
    """Train one candidate in a fresh, scrubbed subprocess. Returns the harness-measured result dict."""
    with tempfile.TemporaryDirectory() as scratch:
        out = Path(scratch) / "r.json"
        cmd = [sys.executable, "-I", str(HERE / "task.py"), "run", "--candidate", str(Path(cand).resolve()), "--seed", str(seed), "--out", str(out)]
        if state_out:
            cmd += ["--state", str(state_out)]
        for flag, v in (("--target", target), ("--stop-batches", stop_batches), ("--wall-cap", wall_cap), ("--token-cap", token_cap)):
            if v is not None:
                cmd += [flag, str(v)]
        try:
            p = subprocess.run(cmd, capture_output=True, text=True, env=_clean_env(scratch), cwd=scratch, timeout=timeout)
        except subprocess.TimeoutExpired:
            return {"seed": seed, "reached": False, "reason": "timeout", "wall_s": None}
        if not out.exists():
            return {"seed": seed, "reached": False, "reason": "crash: " + p.stderr.strip()[-300:], "wall_s": None}
        return json.loads(out.read_text())


def evaluate_state(cand: Path, state: Path, holdout: np.ndarray, timeout: float = 300) -> dict:
    """Dev and holdout loss of saved weights, computed in a separate process that gets the holdout on stdin."""
    with tempfile.TemporaryDirectory() as scratch:
        cmd = [sys.executable, "-I", str(HERE / "task.py"), "eval", "--candidate", str(Path(cand).resolve()), "--state", str(Path(state).resolve())]
        p = subprocess.run(cmd, input=json.dumps(holdout.tolist()), capture_output=True, text=True, env=_clean_env(scratch), cwd=scratch, timeout=timeout)
        try:
            return json.loads(p.stdout.strip().splitlines()[-1])
        except Exception:
            return {"error": "evaluator failed: " + p.stderr.strip()[-300:]}


# ---- in-subprocess code -------------------------------------------------------------------------------------
class Stop(Exception):
    pass


def _load_candidate(path: str):
    spec = importlib.util.spec_from_file_location("f3_candidate", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def mean_ce(model, blocks: np.ndarray) -> float:
    import torch
    import torch.nn.functional as F
    was = model.training
    model.eval()
    with torch.no_grad():
        b = torch.from_numpy(blocks).long()
        logits = model(b[:, :-1])
        loss = F.cross_entropy(logits.reshape(-1, V), b[:, 1:].reshape(-1)).item()
    model.train(was)
    return loss


class Ctx:
    def __init__(self, seed, target, stop_batches, wall_cap, token_cap):
        self.vocab_size, self.block_size, self.seed = V, T, seed
        self.eval_every = SPEC["eval_every"]
        self.target_dev_loss = target
        self._train = gen_tokens(SPEC["train_seed"], SPEC["train_tokens"])
        self._dev = gen_blocks(SPEC["dev_seed"], SPEC["dev_blocks"])
        self.dev_blocks = self._dev.copy()  # visible to the candidate; the harness keeps its own copy
        self._rng = np.random.RandomState(seed)
        self.batches = self.tokens = 0
        self.wall = 0.0
        self.t0 = None
        self.result = None
        self.snapshot = None
        self.last_dev = None
        self._stop_batches, self._wall_cap, self._token_cap = stop_batches, wall_cap, token_cap

    def start_clock(self):
        self.t0 = time.perf_counter()

    def next_batch(self, B: int):
        import torch
        assert 0 < B <= 4096
        o = self._rng.randint(0, len(self._train) - T - 1, size=B)
        idx = o[:, None] + np.arange(T + 1)[None, :]
        w = torch.from_numpy(self._train[idx]).long()
        self.batches += 1
        self.tokens += B * T
        return w[:, :-1], w[:, 1:]

    def eval_dev(self, model) -> float:
        self.wall += time.perf_counter() - self.t0  # the clock stops while the harness evaluates
        loss = mean_ce(model, self._dev)
        self.last_dev = loss
        why = None
        if self.target_dev_loss is not None and loss <= self.target_dev_loss:
            why = "reached"
        elif self._stop_batches is not None and self.batches >= self._stop_batches:
            why = "stop_batches"
        elif self._wall_cap is not None and self.wall > self._wall_cap:
            why = "wall_cap"
        elif self._token_cap is not None and self.tokens > self._token_cap:
            why = "token_cap"
        if why:
            self.snapshot = {k: v.detach().clone().cpu() for k, v in model.state_dict().items()}
            self.result = {"reached": why == "reached", "reason": why, "wall_s": self.wall, "batches": self.batches, "tokens": self.tokens, "dev_loss": loss}
            raise Stop(why)
        self.t0 = time.perf_counter()
        return loss


def _cmd_run(a):
    import torch
    torch.set_num_threads(SPEC["threads"])
    torch.manual_seed(a.seed)
    np.random.seed(a.seed)
    mod = _load_candidate(a.candidate)
    ctx = Ctx(a.seed, a.target, a.stop_batches, a.wall_cap, a.token_cap)
    model = mod.build_model(V, T)
    ctx.start_clock()
    try:
        mod.train(ctx, model)
        ctx.result = ctx.result or {"reached": False, "reason": "returned", "wall_s": ctx.wall + time.perf_counter() - ctx.t0, "batches": ctx.batches, "tokens": ctx.tokens, "dev_loss": ctx.last_dev}
    except Stop:
        pass
    except Exception as e:  # candidate bug
        ctx.result = {"reached": False, "reason": f"crash: {type(e).__name__}: {e}"[:300], "wall_s": None, "batches": ctx.batches, "tokens": ctx.tokens, "dev_loss": ctx.last_dev}
    r = {"seed": a.seed, **ctx.result}
    if a.state and ctx.snapshot is not None and r["reached"]:
        torch.save(ctx.snapshot, a.state)
    Path(a.out).write_text(json.dumps(r))


def _cmd_eval(a):
    import torch
    torch.set_num_threads(SPEC["threads"])
    mod = _load_candidate(a.candidate)  # import first: candidate code runs before the holdout exists in this process
    model = mod.build_model(V, T)
    model.load_state_dict(torch.load(a.state, weights_only=True))
    holdout = np.array(json.loads(sys.stdin.read()), dtype=np.int64)
    dev = gen_blocks(SPEC["dev_seed"], SPEC["dev_blocks"])
    print(json.dumps({"dev_loss": mean_ce(model, dev), "holdout_loss": mean_ce(model, holdout)}))


if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--candidate", required=True)
    r.add_argument("--seed", type=int, required=True)
    r.add_argument("--target", type=float)
    r.add_argument("--stop-batches", type=int)
    r.add_argument("--wall-cap", type=float)
    r.add_argument("--token-cap", type=int)
    r.add_argument("--out", required=True)
    r.add_argument("--state")
    e = sub.add_parser("eval")
    e.add_argument("--candidate", required=True)
    e.add_argument("--state", required=True)
    a = ap.parse_args()
    _cmd_run(a) if a.cmd == "run" else _cmd_eval(a)
