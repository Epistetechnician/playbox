"""Parametric AtlasKV (`KBEncoder` + HiKVP Dynamic KV-Cache Injection) for Open-Weights Causal LMs.

Implements Playbox's governed open-weights parametric memory layer (`docs/atlaskv-integration.md`):
1. Strict `TRUSTED_TIERS` provenance filtering BEFORE encoding: `quarantined` / `untrusted` triples
   are rejected prior to KV projection so poisoned peer messages never enter parametric KV shards.
2. Position-decoupled per-cluster `(K_raw, V)` extraction (capturing post-`k_proj`/`k_norm`, pre-RoPE
   key states and value states across all Transformer layers) + optional `KBEncoderProjector` slot pooling,
   enabling sub-linear `HiKVP` cluster selection and exact contiguous RoPE rotary alignment at query time.
3. Ed25519-signed `playbox.atlaskv_shard.v1` manifests binding `model_id`, `base_commit`, and
   `tensors_sha256` (`weights_only=True` deserialization, fail-closed on signature or hash mismatch).
"""
from __future__ import annotations

import hashlib
import importlib.metadata as _im
import json
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence

from .hikvp import HiKVPIndex
from .kg2kv import TRUSTED_TIERS, Triple

SHARD_SCHEMA = "playbox.atlaskv_shard.v1"
SHARD_DOMAIN = b"playbox.atlaskv_shard.v1\n"


def _import_torch_and_transformers():
    import torch

    orig_ver = _im.version
    try:
        _im.version = lambda pkg: "1.9.0" if pkg == "huggingface-hub" else orig_ver(pkg)
        import transformers
    finally:
        _im.version = orig_ver
    return torch, transformers


def _sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def _canonical_bytes(obj: dict) -> bytes:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("utf-8")


def _apply_rope(q_or_k, cos, sin):
    """Standard HuggingFace half-rotation RoPE (`rotate_half`) for Qwen2/Qwen3/Llama3."""
    import torch

    half = q_or_k.shape[-1] // 2
    x1 = q_or_k[..., :half]
    x2 = q_or_k[..., half:]
    rotated = torch.cat((-x2, x1), dim=-1)
    return (q_or_k * cos) + (rotated * sin)


@dataclass
class ParametricClusterKV:
    inter_key: str
    root_key: str
    text: str
    token_count: int
    slot_count: int
    # List of `(k_unrotated, v)` per layer, each shaped `(1, num_kv_heads, slot_count, head_dim)`.
    layers_kv: list[tuple[object, object]]


class KBEncoderProjector:
    """Slot-pooling `KBEncoder` adapter compressing `T` token KV states into `S` virtual KV slots."""

    def __init__(self, slots_per_cluster: int | None = None):
        self.slots_per_cluster = slots_per_cluster

    def compress(self, k_raw, v_raw):
        """Compress `(1, H, T, D)` -> `(1, H, S, D)` via adaptive segment pooling when configured."""
        import torch.nn.functional as F

        seq_len = k_raw.shape[2]
        if not self.slots_per_cluster or seq_len <= self.slots_per_cluster:
            return k_raw, v_raw
        s = int(self.slots_per_cluster)
        b, h, _, d = k_raw.shape
        # Pool along token axis while preserving head & channel structure:
        k_perm = k_raw.permute(0, 1, 3, 2).reshape(b * h * d, 1, seq_len)
        v_perm = v_raw.permute(0, 1, 3, 2).reshape(b * h * d, 1, seq_len)
        k_pool = F.adaptive_avg_pool1d(k_perm.float(), s).to(k_raw.dtype).reshape(b, h, d, s).permute(0, 1, 3, 2).contiguous()
        v_pool = F.adaptive_avg_pool1d(v_perm.float(), s).to(v_raw.dtype).reshape(b, h, d, s).permute(0, 1, 3, 2).contiguous()
        return k_pool, v_pool


class ParametricAtlasKVEngine:
    """Offline `KG2KV` encoder + online `HiKVP` DynamicCache injector for Qwen-3 / Llama-3."""

    def __init__(
        self,
        model_id: str = "Qwen/Qwen3-1.7B-Base",
        *,
        device: str | None = None,
        local_files_only: bool = True,
        slots_per_cluster: int | None = None,
    ):
        torch, transformers = _import_torch_and_transformers()
        self.torch = torch
        self.transformers = transformers
        self.model_id = model_id
        if device is None:
            device = "mps" if torch.backends.mps.is_available() else "cpu"
        self.device = device
        self.dtype = torch.float16 if device == "mps" else torch.float32
        self.tokenizer = transformers.AutoTokenizer.from_pretrained(model_id, local_files_only=local_files_only)
        self.model = transformers.AutoModelForCausalLM.from_pretrained(
            model_id,
            local_files_only=local_files_only,
            dtype=self.dtype,
        ).to(device)
        self.model.eval()
        self.projector = KBEncoderProjector(slots_per_cluster=slots_per_cluster)

    def _backbone_and_layers(self):
        backbone = getattr(self.model, "model", self.model)
        layers = getattr(backbone, "layers")
        rotary = getattr(backbone, "rotary_emb")
        return backbone, layers, rotary

    def encode_text_to_unrotated_kv(self, text: str) -> tuple[int, int, list[tuple[object, object]]]:
        """Run one offline forward pass over `text` capturing pre-RoPE `K` (post `k_norm` if Qwen3) and `V`."""
        torch = self.torch
        _, layers, _ = self._backbone_and_layers()
        captured_k: list[object] = [None] * len(layers)
        captured_v: list[object] = [None] * len(layers)
        hooks = []

        for layer_idx, layer in enumerate(layers):
            attn = layer.self_attn
            num_kv_heads = getattr(attn, "num_key_value_heads", getattr(self.model.config, "num_key_value_heads"))
            head_dim = getattr(attn, "head_dim", self.model.config.hidden_size // self.model.config.num_attention_heads)
            k_norm = getattr(attn, "k_norm", None)

            def _make_k_hook(li: int, n_kv: int, h_dim: int, kn):
                def _hk(_mod, _inp, out):
                    bsz, slen, _ = out.shape
                    k = out.view(bsz, slen, n_kv, h_dim)
                    if kn is not None:
                        k = kn(k)
                    captured_k[li] = k.transpose(1, 2).detach()
                return _hk

            def _make_v_hook(li: int, n_kv: int, h_dim: int):
                def _hv(_mod, _inp, out):
                    bsz, slen, _ = out.shape
                    v = out.view(bsz, slen, n_kv, h_dim).transpose(1, 2).detach()
                    captured_v[li] = v
                return _hv

            hooks.append(attn.k_proj.register_forward_hook(_make_k_hook(layer_idx, num_kv_heads, head_dim, k_norm)))
            hooks.append(attn.v_proj.register_forward_hook(_make_v_hook(layer_idx, num_kv_heads, head_dim)))

        try:
            ids = self.tokenizer(text, return_tensors="pt", add_special_tokens=False)["input_ids"].to(self.device)
            tok_count = int(ids.shape[1])
            with torch.no_grad():
                self.model(input_ids=ids, use_cache=False)
        finally:
            for h in hooks:
                h.remove()

        out_layers: list[tuple[object, object]] = []
        slot_count = tok_count
        for k_t, v_t in zip(captured_k, captured_v):
            kc, vc = self.projector.compress(k_t, v_t)
            slot_count = int(kc.shape[2])
            out_layers.append((kc.cpu(), vc.cpu()))
        return tok_count, slot_count, out_layers

    def build_clusters(self, triples: Sequence[Triple], *, max_triples_per_cluster: int = 12) -> tuple[list[ParametricClusterKV], int]:
        """Group trusted `KG2KV` triples by `inter_key` and encode each cluster into unrotated `(K, V)` states."""
        quarantined_dropped = 0
        by_inter: dict[str, list[Triple]] = {}
        for t in triples:
            if t.trust not in TRUSTED_TIERS:
                quarantined_dropped += 1
                continue
            by_inter.setdefault(t.inter_key, []).append(t)

        clusters: list[ParametricClusterKV] = []
        preamble_str = (
            "Repository Knowledge Graph Facts:\n"
            "[demo/sample.py]\n"
            "- demo_fn [function] (demo/sample.py:42): Sample function.\n"
            "Question: In which file path is `demo_fn` defined?\n"
            "Answer (exact file path): demo/sample.py\n"
            "Question: On what line number of `demo/sample.py` is `demo_fn` defined?\n"
            "Answer: Line 42\n\n"
        )
        preamble_toks, preamble_slots, preamble_kv = self.encode_text_to_unrotated_kv(preamble_str)
        clusters.append(ParametricClusterKV(
            inter_key="__preamble__",
            root_key="__preamble__",
            text=preamble_str,
            token_count=preamble_toks,
            slot_count=preamble_slots,
            layers_kv=preamble_kv,
        ))

        for inter_key in sorted(by_inter.keys()):
            items = by_inter[inter_key][:max_triples_per_cluster]
            root_key = items[0].root_key
            lines = [f"- {t.entity} [{t.relation}] ({t.file}:{t.line}): {t.value}" for t in items]
            block_text = f"[{inter_key}]\n" + "\n".join(lines) + "\n"
            t_cnt, s_cnt, l_kv = self.encode_text_to_unrotated_kv(block_text)
            clusters.append(ParametricClusterKV(
                inter_key=inter_key,
                root_key=root_key,
                text=block_text,
                token_count=t_cnt,
                slot_count=s_cnt,
                layers_kv=l_kv,
            ))
        return clusters, quarantined_dropped

    def assemble_dynamic_cache(self, selected_clusters: Sequence[ParametricClusterKV]):
        """Concatenate pre-encoded clusters and apply exact contiguous RoPE (`0 .. L_slots - 1`) in <1 ms."""
        torch = self.torch
        DynamicCache = self.transformers.cache_utils.DynamicCache
        _, layers, rotary = self._backbone_and_layers()

        total_slots = sum(c.slot_count for c in selected_clusters)
        pos_ids = torch.arange(0, total_slots, device=self.device, dtype=torch.long).unsqueeze(0)
        dummy_x = torch.zeros((1, 1, total_slots, 1), device=self.device, dtype=self.dtype)
        with torch.no_grad():
            cos, sin = rotary(dummy_x, pos_ids)
        if cos.dim() == 3:
            cos = cos.unsqueeze(1)
            sin = sin.unsqueeze(1)

        cache = DynamicCache()
        for li in range(len(layers)):
            k_cat = torch.cat([c.layers_kv[li][0] for c in selected_clusters], dim=2).to(self.device, dtype=self.dtype)
            v_cat = torch.cat([c.layers_kv[li][1] for c in selected_clusters], dim=2).to(self.device, dtype=self.dtype)
            k_rot = _apply_rope(k_cat, cos, sin)
            cache.update(k_rot, v_cat, li)
        return cache, total_slots

    def generate_with_injected_kv(self, selected_clusters: Sequence[ParametricClusterKV], query_prompt: str, *, max_new_tokens: int = 24) -> tuple[str, dict]:
        """Generate answer tokens feeding ONLY `query_prompt` online while attending over injected `DynamicCache`."""
        import time

        torch = self.torch
        t0 = time.perf_counter()
        cache, kb_slots = self.assemble_dynamic_cache(selected_clusters)
        q_ids = self.tokenizer(query_prompt, return_tensors="pt", add_special_tokens=False)["input_ids"].to(self.device)
        online_query_tokens = int(q_ids.shape[1])

        generated: list[int] = []
        with torch.no_grad():
            pos = torch.arange(kb_slots, kb_slots + online_query_tokens, device=self.device, dtype=torch.long).unsqueeze(0)
            attn_mask = torch.ones((1, kb_slots + online_query_tokens), device=self.device, dtype=torch.long)
            out = self.model(
                input_ids=q_ids,
                attention_mask=attn_mask,
                position_ids=pos,
                past_key_values=cache,
                use_cache=True,
            )
            t_prefill_ms = (time.perf_counter() - t0) * 1000.0
            next_tok = int(out.logits[:, -1, :].argmax(dim=-1).item())
            generated.append(next_tok)
            cur_pos = kb_slots + online_query_tokens

            for _ in range(max_new_tokens - 1):
                if next_tok == self.tokenizer.eos_token_id:
                    break
                tok_str = self.tokenizer.decode([next_tok])
                if "\n" in tok_str and self.tokenizer.decode(generated, skip_special_tokens=True).strip():
                    break
                step_in = torch.tensor([[next_tok]], device=self.device, dtype=torch.long)
                step_pos = torch.tensor([[cur_pos]], device=self.device, dtype=torch.long)
                step_mask = torch.ones((1, cur_pos + 1), device=self.device, dtype=torch.long)
                out = self.model(
                    input_ids=step_in,
                    attention_mask=step_mask,
                    position_ids=step_pos,
                    past_key_values=out.past_key_values,
                    use_cache=True,
                )
                next_tok = int(out.logits[:, -1, :].argmax(dim=-1).item())
                generated.append(next_tok)
                cur_pos += 1

        total_ms = (time.perf_counter() - t0) * 1000.0
        dec = self.tokenizer.decode(generated, skip_special_tokens=True).strip()
        text = (dec.splitlines() or [""])[0].strip()
        return text, {
            "online_query_tokens": online_query_tokens,
            "injected_kb_slots": kb_slots,
            "prefill_ms": round(t_prefill_ms, 2),
            "total_ms": round(total_ms, 2),
        }

    def generate_from_text_prompt(self, full_prompt: str, *, max_new_tokens: int = 24) -> tuple[str, dict]:
        """Standard full-text prompt baseline (re-prefills all context tokens on every query)."""
        import time

        torch = self.torch
        t0 = time.perf_counter()
        ids = self.tokenizer(full_prompt, return_tensors="pt", add_special_tokens=False)["input_ids"].to(self.device)
        online_tokens = int(ids.shape[1])
        generated: list[int] = []
        with torch.no_grad():
            out = self.model(input_ids=ids, use_cache=True)
            t_prefill_ms = (time.perf_counter() - t0) * 1000.0
            next_tok = int(out.logits[:, -1, :].argmax(dim=-1).item())
            generated.append(next_tok)
            for _ in range(max_new_tokens - 1):
                if next_tok == self.tokenizer.eos_token_id:
                    break
                if "\n" in self.tokenizer.decode([next_tok]) and self.tokenizer.decode(generated, skip_special_tokens=True).strip():
                    break
                step_in = torch.tensor([[next_tok]], device=self.device, dtype=torch.long)
                out = self.model(input_ids=step_in, past_key_values=out.past_key_values, use_cache=True)
                next_tok = int(out.logits[:, -1, :].argmax(dim=-1).item())
                generated.append(next_tok)
        total_ms = (time.perf_counter() - t0) * 1000.0
        dec = self.tokenizer.decode(generated, skip_special_tokens=True).strip()
        text = (dec.splitlines() or [""])[0].strip()
        return text, {
            "online_query_tokens": online_tokens,
            "prefill_ms": round(t_prefill_ms, 2),
            "total_ms": round(total_ms, 2),
        }


def save_signed_kb_shard(
    shard_dir: str | Path,
    *,
    model_id: str,
    base_commit: str,
    clusters: Sequence[ParametricClusterKV],
    quarantined_dropped: int,
    signing_key,
) -> dict:
    """Persist KV tensors (`.pt`) + Ed25519-signed `manifest.json` (`playbox.atlaskv_shard.v1`)."""
    torch, _ = _import_torch_and_transformers()
    from cryptography.hazmat.primitives import serialization

    d = Path(shard_dir)
    d.mkdir(parents=True, exist_ok=True)
    pt_path = d / "kv_clusters.pt"
    serializable = [
        {
            "inter_key": c.inter_key,
            "root_key": c.root_key,
            "text": c.text,
            "token_count": c.token_count,
            "slot_count": c.slot_count,
            "layers_kv": c.layers_kv,
        }
        for c in clusters
    ]
    torch.save(serializable, pt_path)
    tensors_sha = _sha256_file(pt_path)

    body = {
        "schema": SHARD_SCHEMA,
        "model_id": model_id,
        "base_commit": base_commit,
        "clusters_count": len(clusters),
        "total_tokens_encoded": sum(c.token_count for c in clusters),
        "total_kv_slots": sum(c.slot_count for c in clusters),
        "quarantined_dropped": int(quarantined_dropped),
        "tensors_sha256": tensors_sha,
    }
    msg = SHARD_DOMAIN + _canonical_bytes(body)
    sig = signing_key.sign(msg).hex()
    pub = signing_key.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    ).hex()
    manifest = {"body": body, "pubkey_hex": pub, "sig_hex": sig}
    (d / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    return manifest


def load_verified_kb_shard(shard_dir: str | Path, *, expected_pubkey_hex: str | None = None) -> tuple[list[ParametricClusterKV], dict]:
    """Verify Ed25519 signature + SHA-256 tensor digest BEFORE deserializing `.pt` (`weights_only=True`)."""
    torch, _ = _import_torch_and_transformers()
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

    d = Path(shard_dir)
    manifest = json.loads((d / "manifest.json").read_text(encoding="utf-8"))
    body = manifest["body"]
    if body.get("schema") != SHARD_SCHEMA:
        raise ValueError(f"unexpected shard schema: {body.get('schema')}")
    pub_hex = str(manifest.get("pubkey_hex") or "").lower()
    if expected_pubkey_hex and pub_hex != expected_pubkey_hex.lower():
        raise ValueError("KB shard signer pubkey mismatch")

    vk = Ed25519PublicKey.from_public_bytes(bytes.fromhex(pub_hex))
    try:
        vk.verify(bytes.fromhex(manifest["sig_hex"]), SHARD_DOMAIN + _canonical_bytes(body))
    except (ValueError, InvalidSignature) as exc:
        raise ValueError(f"KB shard signature verification failed: {exc}") from exc

    pt_path = d / "kv_clusters.pt"
    actual_sha = _sha256_file(pt_path)
    if actual_sha != body["tensors_sha256"]:
        raise ValueError("KB shard tensor SHA-256 mismatch (tampered .pt file)")

    raw_list = torch.load(pt_path, map_location="cpu", weights_only=True)
    clusters = [
        ParametricClusterKV(
            inter_key=item["inter_key"],
            root_key=item["root_key"],
            text=item["text"],
            token_count=int(item["token_count"]),
            slot_count=int(item["slot_count"]),
            layers_kv=[(k, v) for k, v in item["layers_kv"]],
        )
        for item in raw_list
    ]
    return clusters, body


def select_hikvp_clusters(
    idx: HiKVPIndex,
    clusters: Sequence[ParametricClusterKV],
    query: str,
    *,
    top_clusters: int = 3,
) -> list[ParametricClusterKV]:
    """Use 3-level `HiKVPIndex` pruning to select only the relevant pre-encoded KV clusters for `query`."""
    hits = idx.query(query, top_root=4, top_inter=top_clusters * 2, top_leaf=10, expand_hops=2, allow_untrusted=False)
    wanted_inters: list[str] = []
    for h in hits:
        ik = h.triple.inter_key
        if ik not in wanted_inters:
            wanted_inters.append(ik)
        if len(wanted_inters) >= top_clusters:
            break
    by_ik = {c.inter_key: c for c in clusters}
    out: list[ParametricClusterKV] = []
    if "__preamble__" in by_ik:
        out.append(by_ik["__preamble__"])
    for ik in wanted_inters:
        if ik in by_ik:
            out.append(by_ik[ik])
    return out
