"""F3 climbmix substrate: same contract as task.py, real data, bits per byte.

    prepare (explicit, downloads shards):  python task_climbmix.py prepare --data | --holdout | --tokens
    subprocess entry points:               python -I task_climbmix.py run | eval        (as in task.py)

Loss is **bits per byte (bpb)** on dev and holdout rows; "dev_loss"/"holdout_loss" in receipts mean bpb here.
Differences from autoresearch (so numbers are not comparable with its val_bpb): rows are sequential-packed streams of
BOS-prefixed documents, not best-fit packed; train rows are random windows of a pre-tokenized stream; embeddings are
kept in fp32; the metric is the clock to a bpb target, not bpb at a fixed time. Pinned by hash in the genesis receipt
together with the spec file named by F3_SPEC (default task_spec_gpu.json).
"""
from __future__ import annotations

import ast
import json
import math
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import climbmix_data as cd  # noqa: E402

SPEC_PATH = HERE / os.environ.get("F3_SPEC", "task_spec_gpu.json")
SPEC = json.loads(SPEC_PATH.read_text())
V, T = SPEC["vocab"], SPEC["block"]
HP = SPEC["baseline"]
_PASS_ENV = ("F3_SPEC", "F3_DATA_DIR", "F3_DEVICE")


def cache_dir() -> Path:
    return cd.data_dir() / f"cache_T{T}"


# ---- device ----------------------------------------------------------------------------------------------------------
def pick_device():
    import torch
    d = os.environ.get("F3_DEVICE")
    if d:
        return torch.device(d)
    return torch.device("cuda" if torch.cuda.is_available() else "mps" if torch.backends.mps.is_available() else "cpu")


def sync(dev):
    import torch
    if dev.type == "cuda":
        torch.cuda.synchronize()
    elif dev.type == "mps":
        torch.mps.synchronize()


def autocast(dev):
    import contextlib
    import torch
    return torch.amp.autocast(device_type="cuda", dtype=torch.bfloat16) if dev.type == "cuda" else contextlib.nullcontext()


# ---- prepare: tokenizer, train stream, dev rows ----------------------------------------------------------------------
def rows_from_stream(flat: np.ndarray, n_rows: int) -> np.ndarray:
    need = n_rows * (T + 1)
    if len(flat) < need:
        raise RuntimeError(f"only {len(flat)} tokens, need {need}")
    return flat[:need].reshape(n_rows, T + 1).astype(np.int64)


def prepare_tokens():
    """Train the tokenizer on the train shards, then build the train stream and the dev rows. Needs the shards on disk."""
    dd = cd.data_dir()
    train_paths = [dd / cd.shard_name(i) for i in SPEC["train_shards"]]
    dev_path = dd / cd.shard_name(SPEC["dev_shard"])
    tokd = dd / "tokenizer"
    if not (tokd / "tokenizer.pkl").exists():
        cd.train_tokenizer(train_paths, V, tokd)
    enc, bos = cd.load_tokenizer(tokd)
    out = cache_dir()
    out.mkdir(parents=True, exist_ok=True)
    if not (out / "train_tokens.npy").exists():
        def docs():
            for p in train_paths:
                yield from cd.iter_docs(p)
        np.save(out / "train_tokens.npy", cd.tokenize_docs(docs(), enc, bos, SPEC.get("train_max_tokens")))
    if not (out / "dev_rows.npy").exists():
        flat = cd.tokenize_docs(cd.iter_docs(dev_path), enc, bos, SPEC["dev_rows"] * (T + 1))
        np.save(out / "dev_rows.npy", rows_from_stream(flat, SPEC["dev_rows"]))


def data_manifest(include_holdout: bool = True) -> dict:
    """Hashes of every data artifact a run depends on; goes into the genesis receipt, re-checked by the verifier."""
    dd = cd.data_dir()
    m = {"tokenizer.pkl": cd.sha256_file(dd / "tokenizer/tokenizer.pkl"), "token_bytes.pt": cd.sha256_file(dd / "tokenizer/token_bytes.pt"),
         "train_tokens.npy": cd.sha256_file(cache_dir() / "train_tokens.npy"), "dev_rows.npy": cd.sha256_file(cache_dir() / "dev_rows.npy"),
         "dev_shard": cd.sha256_file(dd / cd.shard_name(SPEC["dev_shard"]))}
    if include_holdout:
        m["holdout_shard"] = cd.sha256_file(cd.holdout_dir() / cd.shard_name(SPEC["holdout_shard"]))
    return m


def batches_per_step() -> int:
    """Micro-batches per optimizer step of the baseline (calibration steps are converted with this)."""
    return HP["total_batch"] // (HP["device_batch"] * T)


def reference_info() -> dict:
    return {"vocab": V, "block": T}


# ---- sealed holdout -------------------------------------------------------------------------------------------------
def holdout_from_secret(secret: dict) -> dict:
    """Holdout rows from the custodian's shard: documents in the order of a seeded permutation, packed into rows."""
    enc, bos = cd.load_tokenizer()
    docs = list(cd.iter_docs(cd.holdout_dir() / cd.shard_name(SPEC["holdout_shard"])))
    order = np.random.RandomState(secret["seed"]).permutation(len(docs))
    need, chosen, toks, n = SPEC["holdout_rows"] * (T + 1), [], [], 0
    for i in order:
        ids = [bos, *enc.encode_ordinary(docs[i])]
        chosen.append(docs[i])
        toks.append(np.asarray(ids, dtype=np.int64))
        n += len(ids)
        if n >= need:
            break
    flat = np.concatenate(toks)
    return {"rows": rows_from_stream(flat, SPEC["holdout_rows"]), "docs": chosen}


def holdout_items(H: dict) -> list[bytes]:
    return [d.encode("utf-8") for d in H["docs"]]


def merkle_root(salt: bytes, items: list[bytes]) -> str:
    import hashlib

    def hp(domain, *parts):
        h = hashlib.sha256(domain)
        for p in parts:
            h.update(len(p).to_bytes(8, "big"))
            h.update(p)
        return h.digest()
    level = [hp(b"playbox.holdout.leaf", hp(b"playbox.holdout.blind", salt, i.to_bytes(8, "big")), i.to_bytes(8, "big"), it) for i, it in enumerate(items)]
    while len(level) > 1:
        level = [(hp(b"playbox.holdout.node", level[i], level[i + 1]) if i + 1 < len(level) else level[i]) for i in range(0, len(level), 2)]
    return level[0].hex()


# ---- static checks (same rules as task.py) -----------------------------------------------------------------------------
_DENY_CALLS = {"open", "exec", "eval", "compile", "__import__", "input"}


def static_check(src: str) -> str | None:
    try:
        tree = ast.parse(src)
    except SyntaxError as e:
        return f"syntax error: {e.msg}"
    allowed, bad = set(SPEC["import_whitelist"]), set()
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


def leak_scan(src: str, H: dict, secret_seed: int, salt_hex: str) -> str | None:
    if salt_hex in src or str(secret_seed) in src.replace(" ", ""):
        return "contains the holdout secret (seed or salt)"
    w, stride = SPEC["leak_window"], SPEC["leak_stride"]
    flat = " ".join(src.split())
    for d in H["docs"]:
        t = " ".join(d.split())
        for i in range(0, max(1, len(t) - w + 1), stride):
            if len(t) >= w and t[i : i + w] in flat:
                return f"contains a {w}-character window of a holdout document"
    return None


# ---- subprocess plumbing -------------------------------------------------------------------------------------------------
def _env(scratch: str, holdout: bool) -> dict:
    e = {"PATH": "/usr/bin:/bin", "HOME": scratch, "OMP_NUM_THREADS": str(SPEC.get("threads", 4)), "MKL_NUM_THREADS": str(SPEC.get("threads", 4)),
         "PYTHONHASHSEED": "0", "TOKENIZERS_PARALLELISM": "false"}
    e.update({k: os.environ[k] for k in _PASS_ENV if k in os.environ})
    if holdout and "F3_HOLDOUT_DIR" in os.environ:
        e["F3_HOLDOUT_DIR"] = os.environ["F3_HOLDOUT_DIR"]
    return e


def run_candidate(cand: Path, seed: int, state_out: Path | None, target: float | None = None, stop_batches: int | None = None,
                  wall_cap: float | None = None, token_cap: int | None = None, timeout: float = 3600) -> dict:
    with tempfile.TemporaryDirectory() as scratch:
        out = Path(scratch) / "r.json"
        cmd = [sys.executable, "-I", str(HERE / "task_climbmix.py"), "run", "--candidate", str(Path(cand).resolve()), "--seed", str(seed), "--out", str(out)]
        if state_out:
            cmd += ["--state", str(state_out)]
        for flag, v in (("--target", target), ("--stop-batches", stop_batches), ("--wall-cap", wall_cap), ("--token-cap", token_cap)):
            if v is not None:
                cmd += [flag, str(v)]
        try:
            p = subprocess.run(cmd, capture_output=True, text=True, env=_env(scratch, False), cwd=scratch, timeout=timeout)
        except subprocess.TimeoutExpired:
            return {"seed": seed, "reached": False, "reason": "timeout", "wall_s": None}
        if not out.exists():
            return {"seed": seed, "reached": False, "reason": "crash: " + p.stderr.strip()[-300:], "wall_s": None}
        return json.loads(out.read_text())


def evaluate_state(cand: Path, state: Path, H: dict, timeout: float = 1800) -> dict:
    with tempfile.TemporaryDirectory() as scratch:
        cmd = [sys.executable, "-I", str(HERE / "task_climbmix.py"), "eval", "--candidate", str(Path(cand).resolve()), "--state", str(Path(state).resolve())]
        p = subprocess.run(cmd, input=json.dumps(H["rows"].tolist()), capture_output=True, text=True, env=_env(scratch, True), cwd=scratch, timeout=timeout)
        try:
            return json.loads(p.stdout.strip().splitlines()[-1])
        except Exception:
            return {"error": "evaluator failed: " + p.stderr.strip()[-300:]}


# ---- in-subprocess code --------------------------------------------------------------------------------------------------
class Stop(Exception):
    pass


def _load_candidate(path: str):
    import importlib.util
    spec = importlib.util.spec_from_file_location("f3_candidate", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def mean_bpb(model, rows: np.ndarray, token_bytes, batch: int = 16) -> float:
    """Bits per byte over the target tokens of `rows` (special tokens have 0 bytes and are excluded)."""
    import torch
    import torch.nn.functional as F
    dev = next(model.parameters()).device
    was = model.training
    model.eval()
    tb = token_bytes.to(dev)
    nats = nbytes = 0.0
    with torch.no_grad(), autocast(dev):
        for i in range(0, len(rows), batch):
            b = torch.from_numpy(rows[i : i + batch]).long().to(dev)
            logits = model(b[:, :-1]).float()
            loss = F.cross_entropy(logits.reshape(-1, V), b[:, 1:].reshape(-1), reduction="none")
            nb = tb[b[:, 1:].reshape(-1)]
            nats += (loss * (nb > 0)).sum().item()
            nbytes += nb.sum().item()
    model.train(was)
    return nats / (math.log(2) * nbytes)


class Ctx:
    def __init__(self, seed, target, stop_batches, wall_cap, token_cap):
        import torch
        self.vocab_size, self.block_size, self.seed = V, T, seed
        self.eval_every = SPEC["eval_every"]
        self.target_dev_loss = target
        self.hparams = dict(HP)
        self.device = pick_device()
        self._train = np.load(cache_dir() / "train_tokens.npy", mmap_mode="r")
        self._dev = np.load(cache_dir() / "dev_rows.npy")
        self.dev_blocks = self._dev.copy()  # visible to the candidate; the harness keeps its own copy
        self._tb = cd.load_token_bytes()
        self._rng = np.random.RandomState(seed)
        self.batches = self.tokens = 0
        self.wall = 0.0
        self.t0 = None
        self.result = self.snapshot = self.last_dev = None
        self._stop_batches, self._wall_cap, self._token_cap = stop_batches, wall_cap, token_cap
        self.autocast = lambda: autocast(self.device)

    def start_clock(self):
        sync(self.device)
        self.t0 = time.perf_counter()

    def next_batch(self, B: int):
        import torch
        assert 0 < B <= 4096
        o = self._rng.randint(0, len(self._train) - T - 1, size=B)
        w = torch.from_numpy(self._train[o[:, None] + np.arange(T + 1)[None, :]].astype(np.int64)).to(self.device)
        self.batches += 1
        self.tokens += B * T
        return w[:, :-1], w[:, 1:]

    def eval_dev(self, model) -> float:
        sync(self.device)
        self.wall += time.perf_counter() - self.t0  # the clock stops while the harness evaluates
        loss = mean_bpb(model, self._dev, self._tb)
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
        sync(self.device)
        self.t0 = time.perf_counter()
        return loss


def _cmd_run(a):
    import torch
    torch.set_num_threads(SPEC.get("threads", 4))
    torch.manual_seed(a.seed)
    np.random.seed(a.seed)
    mod = _load_candidate(a.candidate)
    ctx = Ctx(a.seed, a.target, a.stop_batches, a.wall_cap, a.token_cap)
    model = mod.build_model(V, T, dict(HP))
    ctx.start_clock()
    try:
        mod.train(ctx, model)
        sync(ctx.device)
        ctx.result = ctx.result or {"reached": False, "reason": "returned", "wall_s": ctx.wall + time.perf_counter() - ctx.t0, "batches": ctx.batches, "tokens": ctx.tokens, "dev_loss": ctx.last_dev}
    except Stop:
        pass
    except Exception as e:
        ctx.result = {"reached": False, "reason": f"crash: {type(e).__name__}: {e}"[:300], "wall_s": None, "batches": ctx.batches, "tokens": ctx.tokens, "dev_loss": ctx.last_dev}
    r = {"seed": a.seed, **ctx.result}
    if a.state and ctx.snapshot is not None and r["reached"]:
        torch.save(ctx.snapshot, a.state)
    Path(a.out).write_text(json.dumps(r))


def _cmd_eval(a):
    import torch
    torch.set_num_threads(SPEC.get("threads", 4))
    mod = _load_candidate(a.candidate)  # import first: candidate code runs before the holdout exists in this process
    model = mod.build_model(V, T, dict(HP))
    model.load_state_dict(torch.load(a.state, weights_only=True))
    model.to(pick_device())
    rows = np.array(json.loads(sys.stdin.read()), dtype=np.int64)
    tb = cd.load_token_bytes()
    print(json.dumps({"dev_loss": mean_bpb(model, np.load(cache_dir() / "dev_rows.npy"), tb), "holdout_loss": mean_bpb(model, rows, tb)}))


def _cmd_prepare(a):
    if a.data:
        for i in [*SPEC["train_shards"], SPEC["dev_shard"]]:
            print("downloading", cd.shard_name(i), flush=True)
            cd.download_shard(i, cd.data_dir())
    if a.holdout:
        print("downloading", cd.shard_name(SPEC["holdout_shard"]), flush=True)
        cd.download_shard(SPEC["holdout_shard"], cd.holdout_dir())
    if a.tokens:
        prepare_tokens()
        print(json.dumps(data_manifest(include_holdout=(cd.holdout_dir() / cd.shard_name(SPEC["holdout_shard"])).exists()), indent=1))


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
    p = sub.add_parser("prepare")
    p.add_argument("--data", action="store_true", help="download train and dev shards (about 92 MB each)")
    p.add_argument("--holdout", action="store_true", help="download the holdout shard into the custodian directory (about 92 MB)")
    p.add_argument("--tokens", action="store_true", help="train tokenizer, build token caches, print the data manifest")
    a = ap.parse_args()
    {"run": _cmd_run, "eval": _cmd_eval, "prepare": _cmd_prepare}[a.cmd](a)
