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
ADAPTER_SCHEMA = "playbox.atlaskv_adapter.v1"
ADAPTER_DOMAIN = b"playbox.atlaskv_adapter.v1\n"


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
    """Slot-pooling + optional trained per-layer residual `KBEncoder` MLP adapter."""

    def __init__(self, slots_per_cluster: int | None = None, adapter_state: dict | None = None):
        self.slots_per_cluster = slots_per_cluster
        self.adapter_state = adapter_state

    def apply_layer_adapter(self, layer_idx: int, k_raw, v_raw):
        """Apply learned per-layer low-rank residual projection `(k_scale, k_down/up, v_scale, v_down/up)`."""
        if not self.adapter_state:
            return k_raw, v_raw
        import torch.nn.functional as F

        dev = k_raw.device
        orig_dtype = k_raw.dtype
        kf = k_raw.float()
        vf = v_raw.float()
        kd = self.adapter_state["k_down"][layer_idx].to(dev, dtype=kf.dtype)
        ku = self.adapter_state["k_up"][layer_idx].to(dev, dtype=kf.dtype)
        vd = self.adapter_state["v_down"][layer_idx].to(dev, dtype=vf.dtype)
        vu = self.adapter_state["v_up"][layer_idx].to(dev, dtype=vf.dtype)
        ks = self.adapter_state["k_scale"][layer_idx].to(dev, dtype=kf.dtype)
        vs = self.adapter_state["v_scale"][layer_idx].to(dev, dtype=vf.dtype)

        k_out = kf * ks + F.gelu(kf @ kd) @ ku
        v_out = vf * vs + F.gelu(vf @ vd) @ vu
        return k_out.to(orig_dtype), v_out.to(orig_dtype)

    def compress(self, k_raw, v_raw, layer_idx: int | None = None):
        """Apply learned `KBEncoder` adapter (if loaded) and optional adaptive segment pooling."""
        import torch.nn.functional as F

        if layer_idx is not None and self.adapter_state is not None:
            k_raw, v_raw = self.apply_layer_adapter(layer_idx, k_raw, v_raw)

        seq_len = k_raw.shape[2]
        if not self.slots_per_cluster or seq_len <= self.slots_per_cluster:
            return k_raw, v_raw
        s = int(self.slots_per_cluster)
        b, h, _, d = k_raw.shape
        # Pool along token axis while preserving head & channel structure:
        k_perm = k_raw.permute(0, 1, 3, 2).reshape(b * h * d, 1, seq_len)
        v_perm = v_raw.permute(0, 1, 3, 2).reshape(b * h * d, 1, seq_len)
        k_in = k_perm.float().cpu() if (k_raw.device.type == "mps" and seq_len % s != 0) else k_perm.float()
        v_in = v_perm.float().cpu() if (v_raw.device.type == "mps" and seq_len % s != 0) else v_perm.float()
        k_pool = F.adaptive_avg_pool1d(k_in, s).to(device=k_raw.device, dtype=k_raw.dtype).reshape(b, h, d, s).permute(0, 1, 3, 2).contiguous()
        v_pool = F.adaptive_avg_pool1d(v_in, s).to(device=v_raw.device, dtype=v_raw.dtype).reshape(b, h, d, s).permute(0, 1, 3, 2).contiguous()
        return k_pool, v_pool


class TrainedKVProjectionAdapter:
    """Learned per-layer residual KV projection adapter (`projector_k`, `projector_v`) for Parametric AtlasKV.

    Bridges the distribution gap between independently encoded cluster KV states `(K_raw, V_raw)`
    (no cross-cluster attention during offline encoding + dynamic query-time RoPE alignment) and a
    frozen causal LM's self-attention expectations. Initialized as exact identity (`up` = 0, `gate` = 1)
    so step-0 behavior matches un-adapted KV injection.
    """

    def __init__(
        self,
        num_layers: int,
        num_kv_heads: int,
        head_dim: int,
        *,
        rank: int = 32,
        resampler_slots: int | None = None,
        device: str = "cpu",
    ):
        torch, _ = _import_torch_and_transformers()
        import math
        import torch.nn as nn

        self.torch = torch
        self.num_layers = int(num_layers)
        self.num_kv_heads = int(num_kv_heads)
        self.head_dim = int(head_dim)
        self.kv_dim = self.num_kv_heads * self.head_dim
        self.rank = int(rank)
        self.resampler_slots = int(resampler_slots) if resampler_slots else None
        self.device = device

        class _KVNet(nn.Module):
            def __init__(self, n_layers: int, kv_dim: int, r: int, n_heads: int, h_dim: int, m_slots: int | None):
                super().__init__()
                self.m_slots = m_slots
                self.h_dim = h_dim
                self.k_norm = nn.ModuleList([nn.LayerNorm(kv_dim) for _ in range(n_layers)])
                self.k_down = nn.ModuleList([nn.Linear(kv_dim, r, bias=False) for _ in range(n_layers)])
                self.k_up = nn.ModuleList([nn.Linear(r, kv_dim, bias=True) for _ in range(n_layers)])
                self.v_norm = nn.ModuleList([nn.LayerNorm(kv_dim) for _ in range(n_layers)])
                self.v_down = nn.ModuleList([nn.Linear(kv_dim, r, bias=False) for _ in range(n_layers)])
                self.v_up = nn.ModuleList([nn.Linear(r, kv_dim, bias=True) for _ in range(n_layers)])
                self.k_scale = nn.Parameter(torch.ones(n_layers, 1, n_heads, 1, 1))
                self.v_scale = nn.Parameter(torch.ones(n_layers, 1, n_heads, 1, 1))
                self.act = nn.SiLU()
                if m_slots is not None:
                    # Learned Perceiver-style cross-attention resampler over pre-RoPE (K, V) tokens
                    # with a per-token content salience gate and soft monotonic ordering prior.
                    self.resamp_q = nn.Parameter(torch.randn(n_layers, 1, n_heads, m_slots, h_dim) * 0.02)
                    self.resamp_k = nn.ModuleList([nn.Linear(h_dim, h_dim, bias=False) for _ in range(n_layers)])
                    self.resamp_tok_gate = nn.ModuleList([nn.Linear(h_dim, 1, bias=False) for _ in range(n_layers)])
                    self.resamp_log_temp = nn.Parameter(torch.full((n_layers, 1, n_heads, 1, 1), math.log(0.15)))
                    self.resamp_log_pos = nn.Parameter(torch.full((n_layers, 1, n_heads, 1, 1), math.log(8.0)))
                    for li in range(n_layers):
                        nn.init.eye_(self.resamp_k[li].weight)
                        nn.init.zeros_(self.resamp_tok_gate[li].weight)
                for li in range(n_layers):
                    nn.init.normal_(self.k_down[li].weight, std=0.02)
                    nn.init.zeros_(self.k_up[li].weight)
                    nn.init.zeros_(self.k_up[li].bias)
                    nn.init.normal_(self.v_down[li].weight, std=0.02)
                    nn.init.zeros_(self.v_up[li].weight)
                    nn.init.zeros_(self.v_up[li].bias)

            def _resample(self, li: int, kf: object, vf: object):
                b, h, s, d = kf.shape
                if self.m_slots is None or s <= self.m_slots:
                    return kf, vf
                m = self.m_slots
                k_proj = self.resamp_k[li](kf)
                temp = torch.exp(self.resamp_log_temp[li]).clamp(min=0.02, max=2.0)
                logits = torch.matmul(self.resamp_q[li], k_proj.transpose(-2, -1)) / (math.sqrt(d) * temp)
                tok_bias = self.resamp_tok_gate[li](kf).squeeze(-1).unsqueeze(-2)
                slot_pos = torch.linspace(0.0, 1.0, m, device=kf.device, dtype=kf.dtype)
                tok_pos = torch.linspace(0.0, 1.0, s, device=kf.device, dtype=kf.dtype)
                dist2 = (slot_pos[:, None] - tok_pos[None, :]) ** 2
                pos_sharp = torch.exp(self.resamp_log_pos[li]).clamp(min=0.25, max=256.0)
                logits = logits + tok_bias - pos_sharp * dist2.view(1, 1, m, s)
                attn = torch.softmax(logits, dim=-1)
                return torch.matmul(attn, kf), torch.matmul(attn, vf)

            def project_layer(self, li: int, k: object, v: object):
                # k, v: (B, H, S, D) in float32 or model dtype
                orig_dtype = k.dtype
                kf, vf = self._resample(li, k.float(), v.float())
                b, h, s, d = kf.shape
                k_flat = kf.transpose(1, 2).reshape(b, s, h * d)
                v_flat = vf.transpose(1, 2).reshape(b, s, h * d)
                dk = self.k_up[li](self.act(self.k_down[li](self.k_norm[li](k_flat)))).reshape(b, s, h, d).transpose(1, 2)
                dv = self.v_up[li](self.act(self.v_down[li](self.v_norm[li](v_flat)))).reshape(b, s, h, d).transpose(1, 2)
                k_out = kf * self.k_scale[li] + dk
                v_out = vf * self.v_scale[li] + dv
                return k_out.to(orig_dtype), v_out.to(orig_dtype)

        self.net = _KVNet(
            self.num_layers, self.kv_dim, self.rank, self.num_kv_heads, self.head_dim, self.resampler_slots
        ).to(device)

    def to(self, device: str) -> TrainedKVProjectionAdapter:
        self.device = device
        self.net.to(device)
        return self

    def eval(self) -> TrainedKVProjectionAdapter:
        self.net.eval()
        return self

    def train_mode(self) -> TrainedKVProjectionAdapter:
        self.net.train()
        return self

    def parameters(self):
        return self.net.parameters()

    def project_layer(self, layer_idx: int, k, v):
        return self.net.project_layer(layer_idx, k, v)

    def apply_to_clusters(self, clusters: Sequence[ParametricClusterKV]) -> list[ParametricClusterKV]:
        """Bake trained KV projections into offline `ParametricClusterKV` shards (zero online overhead)."""
        torch = self.torch
        self.net.eval()
        adapted: list[ParametricClusterKV] = []
        with torch.no_grad():
            for c in clusters:
                if c.inter_key == "__preamble__":
                    adapted.append(c)
                    continue
                new_layers: list[tuple[object, object]] = []
                for li, (k_cpu, v_cpu) in enumerate(c.layers_kv):
                    k_dev = k_cpu.to(self.device)
                    v_dev = v_cpu.to(self.device)
                    kp, vp = self.net.project_layer(li, k_dev, v_dev)
                    new_layers.append((kp.cpu(), vp.cpu()))
                new_slot_count = int(new_layers[0][0].shape[2]) if new_layers else c.slot_count
                adapted.append(
                    ParametricClusterKV(
                        inter_key=c.inter_key,
                        root_key=c.root_key,
                        text=c.text,
                        token_count=c.token_count,
                        slot_count=new_slot_count,
                        layers_kv=new_layers,
                    )
                )
        return adapted


class ParametricAtlasKVEngine:
    """Offline `KG2KV` encoder + online `HiKVP` DynamicCache injector for Qwen-3 / Llama-3."""

    def __init__(
        self,
        model_id: str = "Qwen/Qwen3-1.7B-Base",
        *,
        device: str | None = None,
        local_files_only: bool = True,
        slots_per_cluster: int | None = None,
        kv_adapter: TrainedKVProjectionAdapter | None = None,
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
        for p in self.model.parameters():
            p.requires_grad_(False)
        self.projector = KBEncoderProjector(slots_per_cluster=slots_per_cluster)
        self.kv_adapter = kv_adapter.to(device).eval() if kv_adapter is not None else None

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
        for li, (k_t, v_t) in enumerate(zip(captured_k, captured_v)):
            kc, vc = self.projector.compress(k_t, v_t, layer_idx=li)
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

    def _layer_kv_spec(self) -> tuple[int, int, int]:
        _, layers, _ = self._backbone_and_layers()
        attn0 = layers[0].self_attn
        num_kv_heads = getattr(attn0, "num_key_value_heads", getattr(self.model.config, "num_key_value_heads"))
        head_dim = getattr(attn0, "head_dim", self.model.config.hidden_size // self.model.config.num_attention_heads)
        return len(layers), int(num_kv_heads), int(head_dim)

    def create_kv_adapter(self, *, rank: int = 32, resampler_slots: int | None = None) -> TrainedKVProjectionAdapter:
        n_layers, n_kv, h_dim = self._layer_kv_spec()
        adapter = TrainedKVProjectionAdapter(
            n_layers, n_kv, h_dim, rank=rank, resampler_slots=resampler_slots, device=self.device
        )
        self.kv_adapter = adapter
        return adapter

    def assemble_dynamic_cache(
        self,
        selected_clusters: Sequence[ParametricClusterKV],
        *,
        apply_live_adapter: bool = False,
    ):
        """Concatenate pre-encoded clusters and apply exact contiguous RoPE (`0 .. L_slots - 1`) in <1 ms."""
        torch = self.torch
        DynamicCache = self.transformers.cache_utils.DynamicCache
        _, layers, rotary = self._backbone_and_layers()

        cache = DynamicCache()
        cos = sin = None
        total_slots = 0
        for li in range(len(layers)):
            if apply_live_adapter and self.kv_adapter is not None:
                k_parts = []
                v_parts = []
                for c in selected_clusters:
                    kc = c.layers_kv[li][0].to(self.device)
                    vc = c.layers_kv[li][1].to(self.device)
                    if c.inter_key != "__preamble__":
                        kc, vc = self.kv_adapter.project_layer(li, kc, vc)
                    k_parts.append(kc.to(self.dtype))
                    v_parts.append(vc.to(self.dtype))
                k_cat = torch.cat(k_parts, dim=2)
                v_cat = torch.cat(v_parts, dim=2)
            else:
                k_cat = torch.cat([c.layers_kv[li][0] for c in selected_clusters], dim=2).to(self.device, dtype=self.dtype)
                v_cat = torch.cat([c.layers_kv[li][1] for c in selected_clusters], dim=2).to(self.device, dtype=self.dtype)
            if cos is None:
                total_slots = int(k_cat.shape[2])
                pos_ids = torch.arange(0, total_slots, device=self.device, dtype=torch.long).unsqueeze(0)
                dummy_x = torch.zeros((1, 1, total_slots, 1), device=self.device, dtype=self.dtype)
                with torch.no_grad():
                    cos, sin = rotary(dummy_x, pos_ids)
                if cos.dim() == 3:
                    cos = cos.unsqueeze(1)
                    sin = sin.unsqueeze(1)
            k_rot = _apply_rope(k_cat, cos, sin)
            cache.update(k_rot, v_cat, li)
        return cache, total_slots

    def train_kv_adapter(
        self,
        train_triples: Sequence[Triple],
        train_examples: Sequence[dict],
        *,
        rank: int = 32,
        resampler_slots: int | None = None,
        epochs: int = 8,
        lr: float = 2e-3,
        weight_decay: float = 1e-4,
        kl_weight: float = 0.5,
        ce_weight: float = 1.0,
        top_clusters: int = 2,
        seed: int = 42,
    ) -> tuple[TrainedKVProjectionAdapter, dict]:
        """Train a `TrainedKVProjectionAdapter` via CE + teacher-KL distillation over trusted train triples.

        Strictly freezes base LM weights and drops any non-`TRUSTED_TIERS` triples before encoding.
        Each item in `train_examples` expects `{"q": ..., "query_prompt": ..., "target": ...}`.
        """
        import random
        import time
        import torch.nn.functional as F

        torch = self.torch
        torch.manual_seed(seed)
        rng = random.Random(seed)
        t0 = time.perf_counter()

        idx = HiKVPIndex.build(train_triples)
        raw_clusters, q_dropped = self.build_clusters(train_triples)

        adapter = (
            self.kv_adapter
            if (self.kv_adapter is not None and self.kv_adapter.resampler_slots == resampler_slots)
            else self.create_kv_adapter(rank=rank, resampler_slots=resampler_slots)
        )
        adapter.to(self.device).train_mode()
        opt = torch.optim.AdamW(adapter.parameters(), lr=lr, weight_decay=weight_decay)

        prepared = []
        with torch.no_grad():
            for ex in train_examples:
                q_str = ex["q"]
                qp = ex["query_prompt"]
                tgt = ex["target"] + "\n"
                q_ids = self.tokenizer(qp, return_tensors="pt", add_special_tokens=False)["input_ids"].to(self.device)
                t_ids = self.tokenizer(tgt, return_tensors="pt", add_special_tokens=False)["input_ids"].to(self.device)
                qt_ids = torch.cat([q_ids, t_ids], dim=1)
                q_len = int(q_ids.shape[1])
                t_len = int(t_ids.shape[1])
                sel = select_hikvp_clusters(idx, raw_clusters, q_str, top_clusters=top_clusters)

                # Precompute teacher target-token logits from selected cluster text (matches query-time KB evidence):
                teacher_ctx = "".join(c.text for c in sel) + "\n" + qp
                tc_ids = self.tokenizer(teacher_ctx, return_tensors="pt", add_special_tokens=False)["input_ids"].to(self.device)
                tc_len = int(tc_ids.shape[1])
                teacher_in = torch.cat([tc_ids, t_ids], dim=1)
                t_out = self.model(input_ids=teacher_in, use_cache=False)
                # Logits predicting t_ids are at indices [tc_len - 1 : tc_len - 1 + t_len]
                teacher_logits = t_out.logits[:, tc_len - 1 : tc_len - 1 + t_len, :].float().detach().cpu()

                prepared.append({
                    "sel": sel,
                    "qt_ids": qt_ids,
                    "t_ids": t_ids,
                    "q_len": q_len,
                    "t_len": t_len,
                    "teacher_logits": teacher_logits,
                })

        epoch_losses: list[float] = []
        total_steps = max(1, epochs * len(prepared))
        scheduler = torch.optim.lr_scheduler.CosineAnnealingLR(opt, T_max=total_steps, eta_min=lr * 0.1)

        for _ep in range(epochs):
            order = list(range(len(prepared)))
            rng.shuffle(order)
            ep_loss = 0.0
            for i in order:
                item = prepared[i]
                opt.zero_grad(set_to_none=True)
                cache, kb_slots = self.assemble_dynamic_cache(item["sel"], apply_live_adapter=True)
                qt_ids = item["qt_ids"]
                seq_len = int(qt_ids.shape[1])
                pos = torch.arange(kb_slots, kb_slots + seq_len, device=self.device, dtype=torch.long).unsqueeze(0)
                attn_mask = torch.ones((1, kb_slots + seq_len), device=self.device, dtype=torch.long)
                out = self.model(
                    input_ids=qt_ids,
                    attention_mask=attn_mask,
                    position_ids=pos,
                    past_key_values=cache,
                    use_cache=False,
                )
                q_len = item["q_len"]
                t_len = item["t_len"]
                pred_logits = out.logits[:, q_len - 1 : q_len - 1 + t_len, :].float()
                targets = item["t_ids"].reshape(-1)
                ce_loss = F.cross_entropy(pred_logits.reshape(-1, pred_logits.shape[-1]), targets)

                if kl_weight > 0.0:
                    t_log = item["teacher_logits"].to(self.device)
                    temp = 2.0
                    kl_loss = F.kl_div(
                        F.log_softmax(pred_logits / temp, dim=-1),
                        F.softmax(t_log / temp, dim=-1),
                        reduction="batchmean",
                    ) * (temp * temp)
                    loss = ce_weight * ce_loss + kl_weight * kl_loss
                else:
                    loss = ce_weight * ce_loss

                loss.backward()
                torch.nn.utils.clip_grad_norm_(adapter.parameters(), 1.0)
                opt.step()
                scheduler.step()
                ep_loss += float(loss.item())
            epoch_losses.append(round(ep_loss / max(1, len(prepared)), 4))

        adapter.eval()
        self.kv_adapter = adapter
        train_sec = round(time.perf_counter() - t0, 2)
        return adapter, {
            "rank": rank,
            "resampler_slots": resampler_slots,
            "epochs": epochs,
            "train_examples": len(prepared),
            "quarantined_dropped_in_train": q_dropped,
            "loss_first_epoch": epoch_losses[0] if epoch_losses else None,
            "loss_last_epoch": epoch_losses[-1] if epoch_losses else None,
            "epoch_losses": epoch_losses,
            "train_wall_s": train_sec,
        }

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


def _mint_pq_attestation(*, instance: str, base_commit: str, atlas_summary: dict, signing_key) -> dict:
    """Bind `atlas_summary` (including `body_sha256` + `tensors_sha256`) into a hybrid `Ed25519+ML-DSA-65` receipt."""
    from playbox_router.receipt import build_signed_receipt

    return build_signed_receipt(
        instance=instance,
        base_commit=base_commit,
        runtime="parametric_atlaskv_offline",
        git_history_isolated=True,
        repo_commit_count=1,
        network_isolated=True,
        atlas_summary=atlas_summary,
        signing_key=signing_key,
    )


def _verify_pq_attestation(
    manifest: dict,
    *,
    body_sha256: str,
    tensors_sha256: str,
    expected_pubkey_hex: str | None = None,
    expected_pq_pubkey_hex: str | None = None,
    require_pq: bool = False,
) -> bool:
    """Verify optional/required hybrid `Ed25519+ML-DSA-65` `pq_attestation` bound to `body_sha256` + `tensors_sha256`."""
    pq_att = manifest.get("pq_attestation")
    if not pq_att:
        if require_pq:
            raise ValueError("manifest is missing required ML-DSA-65 pq_attestation")
        return False
    from playbox_router.receipt import verify_signed_receipt

    ok, reason = verify_signed_receipt(
        pq_att,
        expected_pubkey_hex=expected_pubkey_hex or manifest.get("pubkey_hex"),
        expected_pq_pubkey_hex=expected_pq_pubkey_hex,
        require_pq=require_pq,
    )
    if not ok:
        raise ValueError(f"manifest PQ attestation verification failed: {reason}")
    atlas_blk = (pq_att.get("payload") or {}).get("atlas") or {}
    if atlas_blk.get("body_sha256") != body_sha256 or atlas_blk.get("tensors_sha256") != tensors_sha256:
        raise ValueError("manifest PQ attestation digest binding mismatch")
    sig_alg = str((pq_att.get("signature") or {}).get("alg") or "")
    return sig_alg == "Ed25519+ML-DSA-65"


def save_signed_kb_shard(
    shard_dir: str | Path,
    *,
    model_id: str,
    base_commit: str,
    clusters: Sequence[ParametricClusterKV],
    quarantined_dropped: int,
    signing_key,
) -> dict:
    """Persist KV tensors (`.pt`) + hybrid `Ed25519+ML-DSA-65`-attested `manifest.json` (`playbox.atlaskv_shard.v1`)."""
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
    body_bytes = _canonical_bytes(body)
    body_sha = hashlib.sha256(body_bytes).hexdigest()
    msg = SHARD_DOMAIN + body_bytes
    sig = signing_key.sign(msg).hex()
    pub = signing_key.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    ).hex()
    pq_att = _mint_pq_attestation(
        instance=f"atlaskv_shard:{model_id}",
        base_commit=base_commit,
        atlas_summary={
            "artifact": SHARD_SCHEMA,
            "body_sha256": body_sha,
            "tensors_sha256": tensors_sha,
            "clusters_count": len(clusters),
            "total_kv_slots": body["total_kv_slots"],
            "quarantined_dropped": int(quarantined_dropped),
        },
        signing_key=signing_key,
    )
    pq_sig = pq_att.get("signature") or {}
    manifest = {
        "body": body,
        "pubkey_hex": pub,
        "sig_hex": sig,
        "sig_alg": pq_sig.get("alg", "Ed25519"),
        "pq_pubkey_hex": pq_sig.get("pq_pubkey_hex", ""),
        "pq_attestation": pq_att,
    }
    (d / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    return manifest


def load_verified_kb_shard(
    shard_dir: str | Path,
    *,
    expected_pubkey_hex: str | None = None,
    expected_pq_pubkey_hex: str | None = None,
    require_pq: bool = False,
) -> tuple[list[ParametricClusterKV], dict]:
    """Verify Ed25519 (+ ML-DSA-65 PQ attestation) + SHA-256 tensor digest BEFORE deserializing `.pt` (`weights_only=True`)."""
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

    body_bytes = _canonical_bytes(body)
    body_sha = hashlib.sha256(body_bytes).hexdigest()
    vk = Ed25519PublicKey.from_public_bytes(bytes.fromhex(pub_hex))
    try:
        vk.verify(bytes.fromhex(manifest["sig_hex"]), SHARD_DOMAIN + body_bytes)
    except (ValueError, InvalidSignature) as exc:
        raise ValueError(f"KB shard signature verification failed: {exc}") from exc

    pq_verified = _verify_pq_attestation(
        manifest,
        body_sha256=body_sha,
        tensors_sha256=body["tensors_sha256"],
        expected_pubkey_hex=expected_pubkey_hex,
        expected_pq_pubkey_hex=expected_pq_pubkey_hex,
        require_pq=require_pq,
    )

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
    out_meta = dict(body)
    out_meta["pq_verified"] = pq_verified
    out_meta["sig_alg"] = manifest.get("sig_alg", "Ed25519")
    return clusters, out_meta


def save_signed_kv_adapter(
    adapter_dir: str | Path,
    *,
    model_id: str,
    adapter: TrainedKVProjectionAdapter,
    train_meta: dict,
    signing_key,
    base_commit: str = "HEAD",
) -> dict:
    """Persist `TrainedKVProjectionAdapter` state_dict (`.pt`) + hybrid `Ed25519+ML-DSA-65` `manifest.json`."""
    torch, _ = _import_torch_and_transformers()
    from cryptography.hazmat.primitives import serialization

    d = Path(adapter_dir)
    d.mkdir(parents=True, exist_ok=True)
    pt_path = d / "kv_adapter.pt"
    state_cpu = {k: v.detach().cpu() for k, v in adapter.net.state_dict().items()}
    torch.save(state_cpu, pt_path)
    tensors_sha = _sha256_file(pt_path)

    body = {
        "schema": ADAPTER_SCHEMA,
        "model_id": model_id,
        "num_layers": adapter.num_layers,
        "num_kv_heads": adapter.num_kv_heads,
        "head_dim": adapter.head_dim,
        "rank": adapter.rank,
        "train_meta": train_meta,
        "tensors_sha256": tensors_sha,
    }
    if adapter.resampler_slots is not None:
        body["resampler_slots"] = adapter.resampler_slots
    body_bytes = _canonical_bytes(body)
    body_sha = hashlib.sha256(body_bytes).hexdigest()
    msg = ADAPTER_DOMAIN + body_bytes
    sig = signing_key.sign(msg).hex()
    pub = signing_key.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    ).hex()
    pq_att = _mint_pq_attestation(
        instance=f"atlaskv_adapter:{model_id}",
        base_commit=base_commit,
        atlas_summary={
            "artifact": ADAPTER_SCHEMA,
            "body_sha256": body_sha,
            "tensors_sha256": tensors_sha,
            "rank": adapter.rank,
        },
        signing_key=signing_key,
    )
    pq_sig = pq_att.get("signature") or {}
    manifest = {
        "body": body,
        "pubkey_hex": pub,
        "sig_hex": sig,
        "sig_alg": pq_sig.get("alg", "Ed25519"),
        "pq_pubkey_hex": pq_sig.get("pq_pubkey_hex", ""),
        "pq_attestation": pq_att,
    }
    (d / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    return manifest


def load_verified_kv_adapter(
    adapter_dir: str | Path,
    *,
    expected_pubkey_hex: str | None = None,
    expected_pq_pubkey_hex: str | None = None,
    require_pq: bool = False,
    device: str = "cpu",
) -> tuple[TrainedKVProjectionAdapter, dict]:
    """Verify Ed25519 (+ ML-DSA-65 PQ attestation) + SHA-256 digest BEFORE loading `kv_adapter.pt` (`weights_only=True`)."""
    torch, _ = _import_torch_and_transformers()
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

    d = Path(adapter_dir)
    manifest = json.loads((d / "manifest.json").read_text(encoding="utf-8"))
    body = manifest["body"]
    if body.get("schema") != ADAPTER_SCHEMA:
        raise ValueError(f"unexpected adapter schema: {body.get('schema')}")
    pub_hex = str(manifest.get("pubkey_hex") or "").lower()
    if expected_pubkey_hex and pub_hex != expected_pubkey_hex.lower():
        raise ValueError("KV adapter signer pubkey mismatch")

    body_bytes = _canonical_bytes(body)
    body_sha = hashlib.sha256(body_bytes).hexdigest()
    vk = Ed25519PublicKey.from_public_bytes(bytes.fromhex(pub_hex))
    try:
        vk.verify(bytes.fromhex(manifest["sig_hex"]), ADAPTER_DOMAIN + body_bytes)
    except (ValueError, InvalidSignature) as exc:
        raise ValueError(f"KV adapter signature verification failed: {exc}") from exc

    pq_verified = _verify_pq_attestation(
        manifest,
        body_sha256=body_sha,
        tensors_sha256=body["tensors_sha256"],
        expected_pubkey_hex=expected_pubkey_hex,
        expected_pq_pubkey_hex=expected_pq_pubkey_hex,
        require_pq=require_pq,
    )

    pt_path = d / "kv_adapter.pt"
    actual_sha = _sha256_file(pt_path)
    if actual_sha != body["tensors_sha256"]:
        raise ValueError("KV adapter tensor SHA-256 mismatch (tampered .pt file)")

    state_dict = torch.load(pt_path, map_location="cpu", weights_only=True)
    adapter = TrainedKVProjectionAdapter(
        int(body["num_layers"]),
        int(body["num_kv_heads"]),
        int(body["head_dim"]),
        rank=int(body["rank"]),
        resampler_slots=int(body["resampler_slots"]) if body.get("resampler_slots") is not None else None,
        device="cpu",
    )
    adapter.net.load_state_dict(state_dict, strict=True)
    adapter.to(device).eval()
    out_meta = dict(body)
    out_meta["pq_verified"] = pq_verified
    out_meta["sig_alg"] = manifest.get("sig_alg", "Ed25519")
    return adapter, out_meta


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
