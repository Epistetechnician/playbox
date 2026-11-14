"""Data plumbing for the climbmix substrate: shard download, BPE tokenizer, token cache, document reading.

Adapted from karpathy/autoresearch `prepare.py` (MIT): same dataset (karpathy/climbmix-400b-shuffle), same 8192-token
BPE recipe (GPT-4-style split pattern with \\p{N}{1,2}, 4 reserved specials, BOS = <|reserved_0|>). Changes: shards are
selected by explicit index (the upstream loader globs the whole data directory, which would train on a holdout shard
placed there), and the holdout shard lives in a separate custodian directory.

Environment (all optional):
    F3_DATA_DIR     train and dev shards + tokenizer + token cache (default ~/.cache/f3-climbmix)
    F3_HOLDOUT_DIR  the custodian's holdout shard (default ~/.cache/f3-climbmix-holdout). Never given to candidate processes.
"""
from __future__ import annotations

import hashlib
import os
import pickle
import sys
import time
from pathlib import Path

BASE_URL = "https://huggingface.co/datasets/karpathy/climbmix-400b-shuffle/resolve/main"
SPLIT_PATTERN = r"""'(?i:[sdmt]|ll|ve|re)|[^\r\n\p{L}\p{N}]?+\p{L}+|\p{N}{1,2}| ?[^\s\p{L}\p{N}]++[\r\n]*|\s*[\r\n]|\s+(?!\S)|\s+"""
SPECIAL_TOKENS = [f"<|reserved_{i}|>" for i in range(4)]
BOS_TOKEN = "<|reserved_0|>"


def data_dir() -> Path:
    return Path(os.environ.get("F3_DATA_DIR", Path.home() / ".cache/f3-climbmix"))


def holdout_dir() -> Path:
    return Path(os.environ.get("F3_HOLDOUT_DIR", Path.home() / ".cache/f3-climbmix-holdout"))


def shard_name(i: int) -> str:
    return f"shard_{i:05d}.parquet"


def sha256_file(p: Path) -> str:
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for c in iter(lambda: f.read(1 << 20), b""):
            h.update(c)
    return h.hexdigest()


def download_shard(i: int, dest_dir: Path) -> Path:
    """Download one shard (about 92 MB). Called only by the explicit `prepare` command, never during a run."""
    import requests
    dest_dir.mkdir(parents=True, exist_ok=True)
    out = dest_dir / shard_name(i)
    if out.exists():
        return out
    r = requests.get(f"{BASE_URL}/{shard_name(i)}", stream=True, timeout=60)
    r.raise_for_status()
    tmp = out.with_suffix(".tmp")
    with open(tmp, "wb") as f:
        for c in r.iter_content(1 << 20):
            f.write(c)
    tmp.rename(out)
    return out


def iter_docs(path: Path):
    import pyarrow.parquet as pq
    pf = pq.ParquetFile(path)
    for g in range(pf.num_row_groups):
        yield from pf.read_row_group(g).column("text").to_pylist()


# ---- tokenizer ----------------------------------------------------------------------------------------------------
def train_tokenizer(train_paths: list[Path], vocab_size: int, out_dir: Path, max_chars: int = 1_000_000_000, doc_cap: int = 10_000):
    """Train the BPE with rustbpe (pip install rustbpe), save a tiktoken pickle and the token_bytes lookup."""
    import rustbpe
    import tiktoken
    import torch
    out_dir.mkdir(parents=True, exist_ok=True)

    def texts():
        n = 0
        for p in train_paths:
            for t in iter_docs(p):
                t = t[:doc_cap]
                n += len(t)
                yield t
                if n >= max_chars:
                    return

    t0 = time.time()
    tok = rustbpe.Tokenizer()
    tok.train_from_iterator(texts(), vocab_size - len(SPECIAL_TOKENS), pattern=SPLIT_PATTERN)
    ranks = {bytes(k): v for k, v in tok.get_mergeable_ranks()}
    specials = {n: len(ranks) + i for i, n in enumerate(SPECIAL_TOKENS)}
    enc = tiktoken.Encoding(name="rustbpe", pat_str=tok.get_pattern(), mergeable_ranks=ranks, special_tokens=specials)
    save_tokenizer(enc, out_dir)
    print(f"tokenizer trained in {time.time() - t0:.1f}s", file=sys.stderr)
    return enc


def save_tokenizer(enc, out_dir: Path):
    import torch
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "tokenizer.pkl").write_bytes(pickle.dumps(enc))
    sp = set(SPECIAL_TOKENS)
    tb = [0 if enc.decode([i]) in sp else len(enc.decode([i]).encode("utf-8")) for i in range(enc.n_vocab)]
    torch.save(torch.tensor(tb, dtype=torch.int32), out_dir / "token_bytes.pt")


def load_tokenizer(d: Path | None = None):
    d = d or data_dir() / "tokenizer"
    enc = pickle.loads((d / "tokenizer.pkl").read_bytes())
    return enc, enc.encode_single_token(BOS_TOKEN)


def load_token_bytes(d: Path | None = None):
    import torch
    d = d or data_dir() / "tokenizer"
    return torch.load(d / "token_bytes.pt", weights_only=True)


# ---- tokenization ---------------------------------------------------------------------------------------------------
def tokenize_docs(docs, enc, bos: int, max_tokens: int | None = None):
    """Concatenate BOS-prefixed documents into one flat int array (uint16 is enough for an 8192 vocabulary)."""
    import numpy as np
    out, n = [], 0
    batch = []

    def flush():
        nonlocal n
        for ids in enc.encode_ordinary_batch(batch, num_threads=8):
            out.append(np.asarray([bos, *ids], dtype=np.uint16))
            n += len(ids) + 1
        batch.clear()

    for d in docs:
        batch.append(d)
        if len(batch) >= 256:
            flush()
            if max_tokens and n >= max_tokens:
                break
    if batch and not (max_tokens and n >= max_tokens):
        flush()
    flat = np.concatenate(out) if out else np.zeros(0, dtype=np.uint16)
    return flat[:max_tokens] if max_tokens else flat
