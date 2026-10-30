"""Pieces of replaying a variant's scores from the engine output its receipt binds (pure; the classifier is elsewhere)."""
from __future__ import annotations

import json

import blake3

MICRO = 1_000_000


def parse_variant_scores(variant_text: str) -> list[float]:
    """The per-run scores of a `playbox.variant.v1` record, which stores them as integer millionths."""
    for line in variant_text.splitlines():
        key, _, value = line.partition(" ")
        if key == "scores":
            if value in ("", "-"):
                return []
            return [int(x) / MICRO for x in value.split(",")]
    raise ValueError("no scores line in the variant record")


def output_matches(raw: str, expected_hash_hex: str) -> bool:
    """Whether `raw` is exactly the output a receipt hashed."""
    return blake3.blake3(raw.encode()).hexdigest() == expected_hash_hex


def per_prompt_rows(raw: str) -> list[dict]:
    out = json.loads(raw)
    rows = out.get("results")
    if not isinstance(rows, list) or not rows:
        raise ValueError("the engine output has no results")
    return rows


def max_abs_diff(a: list[float], b: list[float]) -> float:
    if len(a) != len(b):
        raise ValueError(f"{len(a)} scores against {len(b)}")
    return max((abs(x - y) for x, y in zip(a, b)), default=0.0)
