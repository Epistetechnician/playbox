"""Parse the canonical text of a SteerSpec (the `key value` lines of crates/playbox-core/src/steer.rs)."""
from __future__ import annotations

from dataclasses import dataclass

SITES = ("resid_post", "attn_out", "mlp_out")
RECIPES = ("diffmean", "probe", "sae_feature", "reft_r1")
MODES = ("add", "clamp", "ablate")
TOKENS = ("prompt", "generated", "all")
MAX_LAYERS = 256
MAX_COEF_MILLI = 100_000
FIELDS = ("model", "weights", "site", "layers", "direction", "recipe", "mode", "coef_milli", "tokens", "prompts", "seed")


@dataclass(frozen=True)
class Spec:
    model: str
    weights: str
    site: str
    layers: tuple[int, ...]
    direction: str
    recipe: str
    mode: str
    coef_milli: int
    tokens: str
    prompts: str
    seed: int

    @property
    def coef(self) -> float:
        return self.coef_milli / 1000.0


def _hex32(v: str, name: str) -> str:
    if len(v) != 64 or any(c not in "0123456789abcdef" for c in v):
        raise ValueError(f"{name} must be 64 lowercase hex characters")
    return v


def parse(text: str) -> Spec:
    """Accepts the bare fields or a full signed record (domain, signer and sig lines are ignored here:
    the host has already verified the signature before it writes the spec for the sandbox)."""
    kv: dict[str, str] = {}
    for i, line in enumerate(text.splitlines()):
        if i == 0 and line.startswith("playbox."):
            continue
        k, _, v = line.partition(" ")
        if k in ("signer", "sig"):
            continue
        if k in kv:
            raise ValueError(f"duplicate field {k}")
        kv[k] = v
    missing = [f for f in FIELDS if f not in kv]
    extra = [k for k in kv if k not in FIELDS]
    if missing or extra:
        raise ValueError(f"spec fields: missing {missing}, unexpected {extra}")
    try:
        layers = tuple(int(x) for x in kv["layers"].split(",")) if kv["layers"] != "-" else ()
        coef = int(kv["coef_milli"])
        seed = int(kv["seed"])
    except ValueError as e:
        raise ValueError(f"bad number in spec: {e}") from None
    if not layers or len(layers) > MAX_LAYERS or any(a >= b for a, b in zip(layers, layers[1:])) or layers[0] < 0:
        raise ValueError("layers must be 1..256 strictly increasing non-negative integers")
    if abs(coef) > MAX_COEF_MILLI:
        raise ValueError("strength out of range")
    if kv["site"] not in SITES or kv["recipe"] not in RECIPES or kv["mode"] not in MODES or kv["tokens"] not in TOKENS:
        raise ValueError("unknown site, recipe, mode or tokens value")
    if kv["mode"] == "ablate" and coef != 0:
        raise ValueError("ablate takes no strength")
    if not 0 <= seed < 2**64:
        raise ValueError("seed out of range")
    return Spec(
        model=kv["model"],
        weights=_hex32(kv["weights"], "weights"),
        site=kv["site"],
        layers=layers,
        direction=_hex32(kv["direction"], "direction"),
        recipe=kv["recipe"],
        mode=kv["mode"],
        coef_milli=coef,
        tokens=kv["tokens"],
        prompts=_hex32(kv["prompts"], "prompts"),
        seed=seed,
    )
