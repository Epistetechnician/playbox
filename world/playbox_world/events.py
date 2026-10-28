"""Agent events and a text embedding. JSONL schema: {"t", "agent", "kind", "text"}."""

import json
import random
import zlib
from dataclasses import dataclass

import numpy as np

KINDS = ("message", "action", "reasoning", "commit")
KIND_MASS = {"message": 1.0, "action": 1.5, "reasoning": 0.5, "commit": 3.0}
EMBED_DIM = 256


@dataclass
class Event:
    t: float  # 0..1 position along the session timeline
    agent: str
    kind: str  # one of KINDS
    text: str


def embed(text: str, dim: int = EMBED_DIM) -> np.ndarray:
    """Hashed bag-of-words, L2-normalised. Stand-in for a real sentence embedder."""
    v = np.zeros(dim, dtype=np.float32)
    for tok in text.lower().split():
        v[zlib.crc32(tok.encode()) % dim] += 1.0
    n = np.linalg.norm(v)
    return v / n if n else v


def load_jsonl(path: str) -> list[Event]:
    with open(path) as f:
        rows = [json.loads(line) for line in f if line.strip()]
    ts = [r["t"] for r in rows]
    lo, hi = min(ts), max(ts)
    span = (hi - lo) or 1.0
    return [Event((r["t"] - lo) / span, r["agent"], r["kind"], r["text"]) for r in rows]


_TOPICS = {
    "parser": "parser tokenizer grammar ast lexer syntax bug",
    "infra": "ci pipeline runner cache docker build flaky",
    "docs": "docs readme guide tutorial example diagram",
}
_AGENTS = ("claude", "codex", "hermes")


def synthetic(n: int = 180, seed: int = 0) -> list[Event]:
    """Three agents drifting between three topics over a session."""
    rng = random.Random(seed)
    events = []
    for i in range(n):
        t = i / (n - 1)
        topic = list(_TOPICS)[min(int(t * 3 + rng.random() * 0.6 - 0.3), 2) if t * 3 + 0 > 0 else 0]
        topic = list(_TOPICS)[max(0, min(2, int(t * 3 + rng.uniform(-0.4, 0.4))))]
        words = _TOPICS[topic].split()
        kind = rng.choices(KINDS, weights=(4, 3, 5, 1))[0]
        text = f"{kind} " + " ".join(rng.sample(words, 4))
        events.append(Event(t, rng.choice(_AGENTS), kind, text))
    return events
