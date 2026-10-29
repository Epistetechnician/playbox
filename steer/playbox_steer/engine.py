"""Apply a SteerSpec to a causal LM with forward hooks, behind a fixed list of pre-flight gates.

Semantics (what the numbers in a spec mean):
- `site` picks where in each listed layer the edit happens: `resid_post` is the block output (the
  residual stream after the layer), `attn_out` and `mlp_out` are the sublayer outputs before they are
  added to the stream. All three live in the residual basis.
- `norm_ref[layer]` is the mean L2 norm of the site's activations over the prompt set's non-first token
  positions in a clean forward pass. Strength is `coef_milli / 1000 * norm_ref[layer]`.
- `add`:    h += s * d
  `clamp`:  h += (s - h.d) * d      (the component along d becomes s)
  `ablate`: h -= (h.d) * d
- `tokens`: `prompt` edits the positions of the prompt only; `generated` edits every position after it
  (each decode step); `all` edits both.

Decoding is greedy (temperature 0) unless the prompt set asks for sampling, which is seeded from the spec.
Prompts are run one at a time with no padding, so results do not depend on batch composition.

Gates, in order, must all pass before anything is generated: hooked-versus-native parity, determinism,
exact no-op, and (when the spec is not itself a no-op) nonzero reach. A failed gate stops the run.
"""
from __future__ import annotations

import json
import math
from dataclasses import dataclass, field, replace
from pathlib import Path

import torch
from transformers import AutoModelForCausalLM, AutoTokenizer

from .files import PromptSet, hash_bytes, read_direction, read_prompts
from .spec import Spec

GUARD_COS_THRESHOLD = 0.5


class SteerError(Exception):
    """A refusal the host should report as such (bad input, failed gate, guarded direction)."""

    def __init__(self, msg: str, code: int = 2):
        super().__init__(msg)
        self.code = code


def canonical_json(obj) -> bytes:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode()


# ---- model access ------------------------------------------------------------------------------------


def load(weights_dir: Path):
    torch.set_grad_enabled(False)
    torch.use_deterministic_algorithms(True)
    torch.set_num_threads(1)  # same reduction order on every run
    tok = AutoTokenizer.from_pretrained(weights_dir, local_files_only=True)
    model = AutoModelForCausalLM.from_pretrained(weights_dir, local_files_only=True, torch_dtype=torch.float32)
    model.eval()
    return model, tok


def blocks(model) -> list:
    for path in (("model", "layers"), ("transformer", "h"), ("gpt_neox", "layers")):
        obj = model
        try:
            for a in path:
                obj = getattr(obj, a)
        except AttributeError:
            continue
        return list(obj)
    raise SteerError("unsupported architecture: no decoder block list found")


def site_module(block, site: str):
    if site == "resid_post":
        return block
    names = {"attn_out": ("self_attn", "attn", "attention"), "mlp_out": ("mlp",)}[site]
    for n in names:
        if hasattr(block, n):
            return getattr(block, n)
    raise SteerError(f"unsupported architecture: no {site} module")


def _edit(output, fn):
    if isinstance(output, tuple):
        return (fn(output[0]),) + tuple(output[1:])
    return fn(output)


def _tensor(output):
    return output[0] if isinstance(output, tuple) else output


# ---- steering ----------------------------------------------------------------------------------------


@dataclass
class Steerer:
    model: torch.nn.Module
    spec: Spec
    direction: torch.Tensor  # unit norm, float32
    norm_ref: dict[int, float]
    phase: str = "prefill"  # `prefill` while the prompt is processed, then `decode`
    handles: list = field(default_factory=list)
    identity: bool = False  # install hooks that change nothing (for the parity gate)

    def _applies(self) -> bool:
        t = self.spec.tokens
        return t == "all" or (t == "prompt" and self.phase == "prefill") or (t == "generated" and self.phase == "decode")

    def _hook(self, layer: int):
        scale = self.spec.coef * self.norm_ref[layer]
        mode, d = self.spec.mode, self.direction

        def edit(h: torch.Tensor) -> torch.Tensor:
            if mode == "add":
                return h + scale * d
            proj = (h * d).sum(-1, keepdim=True)
            if mode == "clamp":
                return h + (scale - proj) * d
            return h - proj * d  # ablate

        def hook(_module, _inputs, output):
            if self.identity or not self._applies():
                return output
            return _edit(output, edit)

        return hook

    def __enter__(self):
        bs = blocks(self.model)
        for layer in self.spec.layers:
            if layer >= len(bs):
                raise SteerError(f"layer {layer} out of range (model has {len(bs)})")
            self.handles.append(site_module(bs[layer], self.spec.site).register_forward_hook(self._hook(layer)))
        return self

    def __exit__(self, *_):
        for h in self.handles:
            h.remove()
        self.handles.clear()


def calibrate(model, tok, spec: Spec, prompts: PromptSet) -> dict[int, float]:
    """Mean site-activation norm per layer over the prompt set, from clean forward passes."""
    bs = blocks(model)
    sums = {l: 0.0 for l in spec.layers}
    counts = {l: 0 for l in spec.layers}
    fallback_sum = dict(sums)
    fallback_n = dict(counts)
    handles = []
    cur: dict[int, torch.Tensor] = {}
    for layer in spec.layers:
        if layer >= len(bs):
            raise SteerError(f"layer {layer} out of range (model has {len(bs)})")

        def hook(_m, _i, out, layer=layer):
            cur[layer] = _tensor(out).detach()

        handles.append(site_module(bs[layer], spec.site).register_forward_hook(hook))
    try:
        for p in prompts.prompts:
            ids = tok(p, return_tensors="pt").input_ids
            model(input_ids=ids, use_cache=False)
            for layer in spec.layers:
                norms = cur[layer][0].norm(dim=-1)
                fallback_sum[layer] += norms.sum().item()
                fallback_n[layer] += norms.numel()
                if norms.numel() > 1:
                    sums[layer] += norms[1:].sum().item()
                    counts[layer] += norms.numel() - 1
    finally:
        for h in handles:
            h.remove()
    out = {}
    for layer in spec.layers:
        n, s = (counts[layer], sums[layer]) if counts[layer] else (fallback_n[layer], fallback_sum[layer])
        out[layer] = s / n
        if not math.isfinite(out[layer]) or out[layer] <= 0:
            raise SteerError(f"layer {layer} has a degenerate activation norm")
    return out


def _logits(model, ids: torch.Tensor) -> torch.Tensor:
    return model(input_ids=ids, use_cache=False).logits[0, -1].clone()


def gates(model, tok, spec: Spec, direction: torch.Tensor, norm_ref, first_prompt: str) -> dict:
    ids = tok(first_prompt, return_tensors="pt").input_ids
    native = _logits(model, ids)
    results = {}

    again = _logits(model, ids)
    results["determinism"] = {"max_delta": (native - again).abs().max().item()}

    with Steerer(model, spec, direction, norm_ref, identity=True):
        hooked = _logits(model, ids)
    results["parity"] = {"max_delta": (native - hooked).abs().max().item()}

    zero = replace(spec, mode="add", coef_milli=0, tokens="all")  # the exact no-op control: add nothing
    with Steerer(model, zero, direction, norm_ref):
        noop = _logits(model, ids)
    results["no_op"] = {"max_delta": (native - noop).abs().max().item()}

    effective = spec.mode in ("clamp", "ablate") or spec.coef_milli != 0
    if effective:
        full = replace(spec, tokens="all")
        with Steerer(model, full, direction, norm_ref):
            steered = _logits(model, ids)
        delta = (native - steered).abs().max().item()
        results["reach"] = {"max_delta": delta, "pass": delta > 0.0}
    else:
        results["reach"] = {"skipped": "spec is itself a no-op", "pass": True}
    for k in ("determinism", "parity", "no_op"):
        results[k]["pass"] = results[k]["max_delta"] == 0.0
    results["pass"] = all(results[k]["pass"] for k in ("determinism", "parity", "no_op", "reach"))
    return results


# ---- guards ------------------------------------------------------------------------------------------


def check_guards(spec: Spec, direction: torch.Tensor, guard_dir: Path | None, deny: set[str]) -> list[dict]:
    """Directions the host knows to be safety-relevant, as `guards.json` beside `*.f32` files. A spec whose
    direction has cosine >= GUARD_COS_THRESHOLD (in absolute value) with a guard of a denied class, at one of
    its layers and site, is refused. Returns every hit, denied or not."""
    if guard_dir is None or not (guard_dir / "guards.json").exists():
        return []
    hits = []
    for g in json.loads((guard_dir / "guards.json").read_text()):
        if g["site"] != spec.site or g["layer"] not in spec.layers:
            continue
        vec, _ = read_direction(guard_dir / g["file"], dim=direction.numel())
        cos = float((direction * torch.tensor(vec)).sum())
        if abs(cos) >= GUARD_COS_THRESHOLD:
            hits.append({"class": g["class"], "layer": g["layer"], "cos": round(cos, 4), "denied": g["class"] in deny})
    denied = [h for h in hits if h["denied"]]
    if denied:
        raise SteerError(f"direction is within the guard threshold of a denied class: {denied}", code=3)
    return hits


# ---- running -----------------------------------------------------------------------------------------


def generate(model, tok, steerer: Steerer | None, prompt: str, n: int, temperature: float, seed: int, trace: list | None = None) -> tuple[str, list[int]]:
    """Greedy or seeded sampling with a KV cache. If `trace` is a list, the logits that chose each token are
    appended to it (one tensor per generated token)."""
    ids = tok(prompt, return_tensors="pt").input_ids
    gen = torch.Generator().manual_seed(seed)
    out_ids: list[int] = []
    eos = tok.eos_token_id

    def forward(x, past):
        return model(input_ids=x, past_key_values=past, use_cache=True)

    if steerer:
        steerer.phase = "prefill"
    res = forward(ids, None)
    past = res.past_key_values
    logits = res.logits[0, -1]
    for _ in range(n):
        if trace is not None:
            trace.append(logits.clone())
        if temperature > 0:
            probs = torch.softmax(logits / temperature, -1)
            nxt = int(torch.multinomial(probs, 1, generator=gen))
        else:
            nxt = int(torch.argmax(logits))
        out_ids.append(nxt)
        if eos is not None and nxt == eos:
            break
        if steerer:
            steerer.phase = "decode"
        res = forward(torch.tensor([[nxt]]), past)
        past = res.past_key_values
        logits = res.logits[0, -1]
    if steerer:
        steerer.phase = "prefill"
    return tok.decode(out_ids, skip_special_tokens=True), out_ids


def teacher_forced_logits(model, tok, prompt: str, forced: list[int], steerer: Steerer | None) -> list[torch.Tensor]:
    """The logits that would choose each token of `forced`, if the model were made to say exactly `forced`.
    Follows the same prefill-then-decode path as `generate`, so a steerer's `tokens` scope applies the same way."""
    ids = tok(prompt, return_tensors="pt").input_ids
    if steerer:
        steerer.phase = "prefill"
    res = model(input_ids=ids, past_key_values=None, use_cache=True)
    past, logits, out = res.past_key_values, res.logits[0, -1], []
    for i, t in enumerate(forced):
        out.append(logits.clone())
        if i == len(forced) - 1:
            break
        if steerer:
            steerer.phase = "decode"
        res = model(input_ids=torch.tensor([[t]]), past_key_values=past, use_cache=True)
        past, logits = res.past_key_values, res.logits[0, -1]
    if steerer:
        steerer.phase = "prefill"
    return out


def mean_kl(steered: list[torch.Tensor], clean: list[torch.Tensor]) -> float:
    """Mean per-token KL(steered || clean) in nats over the steered continuation: how far the edit moved the
    model's next-token distribution, measured on what the steered model actually said. 0 when nothing moved."""
    if not steered:
        return 0.0
    total = 0.0
    for a, b in zip(steered, clean):
        lp, lq = torch.log_softmax(a, -1), torch.log_softmax(b, -1)
        total += float((lp.exp() * (lp - lq)).sum())
    return total / len(steered)


def run(weights_dir: Path, task_dir: Path, spec: Spec, guard_dir: Path | None = None, deny: set[str] | None = None) -> dict:
    """The `steer` task: gates, then steered and clean generation for every prompt."""
    prompts = read_prompts(task_dir / "prompts.json")
    if prompts.digest != spec.prompts:
        raise SteerError("prompt set does not match the spec's prompts hash")
    model, tok = load(weights_dir)
    width = model.config.hidden_size
    vec, dhash = read_direction(task_dir / "direction.f32", dim=width)
    if dhash != spec.direction:
        raise SteerError("direction file does not match the spec's direction hash")
    direction = torch.tensor(vec, dtype=torch.float32)
    hits = check_guards(spec, direction, guard_dir, deny or set())
    norm_ref = calibrate(model, tok, spec, prompts)
    g = gates(model, tok, spec, direction, norm_ref, prompts.prompts[0])
    out: dict = {"engine": "playbox-steer/0.1.0", "gates": g, "guard_hits": hits, "norm_ref": {str(k): v for k, v in norm_ref.items()}}
    if not g["pass"]:
        out["results"] = []
        return out
    results = []
    for i, p in enumerate(prompts.prompts):
        seed = (spec.seed + i) % 2**63
        if spec.mode == "add" and spec.coef_milli == 0:
            # An add of exactly zero leaves every activation bit-for-bit unchanged (the no_op gate checks this
            # on every run), so the clean pass is the steered pass and nothing moved.
            text, ids = generate(model, tok, None, p, prompts.max_new_tokens, prompts.temperature, seed)
            results.append({"prompt": p, "steered": text, "clean": text, "changed": False, "kl_to_clean": 0.0, "tokens": len(ids)})
            continue
        steered_trace: list = []
        with Steerer(model, spec, direction, norm_ref) as st:
            steered, steered_ids = generate(model, tok, st, p, prompts.max_new_tokens, prompts.temperature, seed, steered_trace)
        clean, clean_ids = generate(model, tok, None, p, prompts.max_new_tokens, prompts.temperature, seed)
        # How far the edit moved the next-token distribution along the steered text.
        kl = mean_kl(steered_trace, teacher_forced_logits(model, tok, p, steered_ids, None))
        results.append({"prompt": p, "steered": steered, "clean": clean, "changed": steered_ids != clean_ids, "kl_to_clean": round(kl, 6), "tokens": len(steered_ids)})
    out["results"] = results
    return out


def probe(weights_dir: Path, task_dir: Path, spec: Spec) -> dict:
    """Read-only: project each prompt's activations onto the direction, in units of `norm_ref`.
    Nothing is edited; `mode` and `coef_milli` in the spec are ignored."""
    prompts = read_prompts(task_dir / "prompts.json")
    if prompts.digest != spec.prompts:
        raise SteerError("prompt set does not match the spec's prompts hash")
    model, tok = load(weights_dir)
    vec, dhash = read_direction(task_dir / "direction.f32", dim=model.config.hidden_size)
    if dhash != spec.direction:
        raise SteerError("direction file does not match the spec's direction hash")
    d = torch.tensor(vec, dtype=torch.float32)
    norm_ref = calibrate(model, tok, spec, prompts)
    bs = blocks(model)
    cur: dict[int, torch.Tensor] = {}
    handles = []
    for layer in spec.layers:
        def hook(_m, _i, out, layer=layer):
            cur[layer] = _tensor(out).detach()

        handles.append(site_module(bs[layer], spec.site).register_forward_hook(hook))
    rows = []
    try:
        for p in prompts.prompts:
            ids = tok(p, return_tensors="pt").input_ids
            model(input_ids=ids, use_cache=False)
            row = {}
            for layer in spec.layers:
                h = cur[layer][0]
                h = h[1:] if h.shape[0] > 1 else h
                row[str(layer)] = float(((h @ d) / norm_ref[layer]).mean())
            rows.append({"prompt": p, "projection": row})
    finally:
        for h in handles:
            h.remove()
    return {"engine": "playbox-steer/0.1.0", "norm_ref": {str(k): v for k, v in norm_ref.items()}, "probe": rows}


def last_token_states(model, tok, texts: list[str], site: str, layer: int) -> torch.Tensor:
    bs = blocks(model)
    cur = {}
    h = site_module(bs[layer], site).register_forward_hook(lambda _m, _i, out: cur.__setitem__("x", _tensor(out).detach()))
    try:
        rows = []
        for t in texts:
            model(input_ids=tok(t, return_tensors="pt").input_ids, use_cache=False)
            rows.append(cur["x"][0, -1])
    finally:
        h.remove()
    return torch.stack(rows)


def extract_diffmean(weights_dir: Path, positive: list[str], negative: list[str], site: str, layer: int) -> list[float]:
    """Unit difference-of-means direction between last-token states of two prompt groups."""
    if not positive or not negative:
        raise SteerError("need at least one positive and one negative prompt")
    model, tok = load(weights_dir)
    pos = last_token_states(model, tok, positive, site, layer).mean(0)
    neg = last_token_states(model, tok, negative, site, layer).mean(0)
    d = pos - neg
    n = d.norm()
    if not torch.isfinite(n) or n == 0:
        raise SteerError("the two groups have identical mean activations")
    return (d / n).tolist()


def random_direction(dim: int, seed: int) -> list[float]:
    """The matched-norm null control: a seeded random unit vector."""
    g = torch.Generator().manual_seed(seed)
    v = torch.randn(dim, generator=g)
    return (v / v.norm()).tolist()


def manifest_digest(obj) -> str:
    return hash_bytes(canonical_json(obj))
