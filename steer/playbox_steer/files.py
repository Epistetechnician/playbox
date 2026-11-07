"""Content-addressed inputs: the vector file, the prompt-set manifest, the weights manifest.

Hashes are blake3, the same function the Rust side uses, and are over exact file bytes (or, for weights,
over a sorted listing of per-file hashes), so a hash in a SteerSpec names one thing.
"""
from __future__ import annotations

import json
import math
import struct
from dataclasses import dataclass
from pathlib import Path

import blake3

MAX_PROMPTS = 256
MAX_PROMPT_CHARS = 8192
MAX_NEW_TOKENS = 512


def hash_bytes(b: bytes) -> str:
    return blake3.blake3(b).hexdigest()


def hash_file(path: Path) -> str:
    h = blake3.blake3()
    with open(path, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def weights_manifest(root: Path) -> str:
    """blake3 of `relpath<space>filehash<newline>` lines, sorted by the UTF-8 bytes of relpath, over regular files only.

    Symlinks and hidden files (including the HF `.cache` directory) are ignored so the same weights
    hash the same wherever they were downloaded.
    """
    entries = []
    for p in root.rglob("*"):
        rel = p.relative_to(root)
        if any(part.startswith(".") for part in rel.parts) or p.is_symlink() or not p.is_file():
            continue
        entries.append((rel.as_posix(), p))
    # Sorted by the UTF-8 bytes of the posix path, the order the Rust side uses.
    entries.sort(key=lambda e: e[0].encode())
    return hash_bytes("".join(f"{rel} {hash_file(p)}\n" for rel, p in entries).encode())


def write_direction(path: Path, values: list[float]) -> str:
    """Write a unit-norm direction as little-endian float32 and return its hash."""
    norm = math.sqrt(sum(v * v for v in values))
    if not math.isfinite(norm) or norm == 0:
        raise ValueError("direction has zero or non-finite norm")
    data = struct.pack(f"<{len(values)}f", *[v / norm for v in values])
    path.write_bytes(data)
    return hash_bytes(data)


def read_direction(path: Path, dim: int | None = None) -> tuple[list[float], str]:
    data = path.read_bytes()
    if len(data) % 4 or not data:
        raise ValueError("direction file must be a non-empty array of float32")
    n = len(data) // 4
    if dim is not None and n != dim:
        raise ValueError(f"direction has {n} values, model width is {dim}")
    values = list(struct.unpack(f"<{n}f", data))
    norm = math.sqrt(sum(v * v for v in values))
    if not math.isfinite(norm) or abs(norm - 1.0) > 1e-3:
        raise ValueError(f"direction must be unit norm (got {norm})")
    return values, hash_bytes(data)


@dataclass(frozen=True)
class PromptSet:
    prompts: tuple[str, ...]
    max_new_tokens: int
    temperature: float
    digest: str


def read_prompts(path: Path) -> PromptSet:
    raw = path.read_bytes()
    try:
        j = json.loads(raw)
    except RecursionError:  # a file nested thousands deep: refused like any other malformed prompt set
        raise ValueError("prompt set is nested too deeply") from None
    if not isinstance(j, dict) or set(j) - {"prompts", "max_new_tokens", "temperature"}:
        raise ValueError("prompt set must be an object with prompts, max_new_tokens, temperature")
    prompts = j.get("prompts")
    if not isinstance(prompts, list) or not 1 <= len(prompts) <= MAX_PROMPTS:
        raise ValueError(f"need 1 to {MAX_PROMPTS} prompts")
    for p in prompts:
        if not isinstance(p, str) or not p or len(p) > MAX_PROMPT_CHARS:
            raise ValueError(f"each prompt must be a non-empty string under {MAX_PROMPT_CHARS} characters")
    n = j.get("max_new_tokens", 32)
    t = j.get("temperature", 0.0)
    if not isinstance(n, int) or isinstance(n, bool) or not 1 <= n <= MAX_NEW_TOKENS:
        raise ValueError(f"max_new_tokens must be 1..{MAX_NEW_TOKENS}")
    if not isinstance(t, (int, float)) or isinstance(t, bool) or not 0.0 <= t <= 2.0:
        raise ValueError("temperature must be 0..2")
    return PromptSet(tuple(prompts), n, float(t), hash_bytes(raw))
