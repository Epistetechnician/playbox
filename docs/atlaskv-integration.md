# AtlasKV (`HKUST-KnowComp/AtlasKV`): evaluation and integration note

Status: reviewed 2026-11-19 (MIT license, ICLR 2026, arXiv 2510.17934). Model-agnostic KG2KV + HiKVP layer implemented in `hermes-playbox-backend/playbox_atlas/`; full parametric attention-injection weights remain on the open-weights research track.

## 1. What AtlasKV actually is

[HKUST-KnowComp/AtlasKV](https://github.com/HKUST-KnowComp/AtlasKV) extends Microsoft KBLaM to ground frozen LLMs on billion-scale Knowledge Graphs (up to 1B triples in <20 GB VRAM) via two mechanisms:

1. **KG2KV (`src/atlaskv/kb_encoder.py`):** transforms knowledge graph triples `(h, r, t)` into natural-language `(Q, K, V)` tuples, embeds keys and values with a sentence encoder (`all-MiniLM-L6-v2` or OpenAI embeddings), and projects them into per-layer KV space via trained MLP adapters (`projector_k`, `projector_v`).
2. **HiKVP (`src/atlaskv/models/llama3_model.py`, `AtlaskvLlamaAttention`):** organizes knowledge keys into a 3-level hierarchy (`root -> inter -> leaf` via offline clustering) and performs sub-linear hierarchical pruning inside modified Transformer self-attention layers using a dedicated learned query projection (`q_proj_kg` + `score_shift`).

## 2. What it can and cannot do for Playbox + Hermes + OpenShell

| Need | Full parametric AtlasKV (`AtlaskvLlamaAttention`) | Playbox model-agnostic adaptation (`playbox_atlas/`) |
|---|---|---|
| Works with closed APIs (Claude Haiku/Sonnet, Gemini, GPT) | **No** — patches Transformer attention layers and requires open weights + trained projectors | **Yes** — pure-stdlib KG2KV + 3-level HiKVP over AST call graphs and signed room memory |
| Fact precision (fixing L2/L4 steering's 0% fact score in `docs/metered-benchmark.md`) | **Yes** (on Llama-3 / Phi-3 with trained `KBEncoder` checkpoints) | **Yes** — exact file/line/symbol/call-graph triples + verified lessons |
| Scales to 100M–1B static KG triples in <20 GB VRAM | **Yes** (core paper result) | Out of scope (built for repo + room-memory scale: 1k–100k triples in <20 ms on CPU) |
| Access control, prompt-injection provenance, quarantine | **None** — treats all KB triples as trusted; vulnerable to KG poisoning | **Enforced** — every triple carries `trust` (`repo`, `reviewed`, `quarantined`, `untrusted`); quarantined peer triples are excluded before context assembly |
| Reduces Hermes shell round trips & token burn | Not directly (does not index live repo edits) | **Yes** — surfaces target symbols and 1-hop callers/callees in ~350 tokens on turn 1 instead of 10–15 `grep`/`find` shell calls |

## 3. What we shipped today (`hermes-playbox-backend/playbox_atlas/`)

- **`kg2kv.py`:** extracts `(entity, relation, value, root_key, inter_key, file, line, provenance, trust)` triples from Python ASTs (modules, classes, methods, functions, imports, call edges) + Rust/JS/TS/Go regex fallback + Playbox room memory / Harness² `cue / lesson / evidence` JSONL logs.
- **`hikvp.py`:** 3-level hierarchical index (`Root [package/domain] -> Inter [file/class] -> Leaf [triple]`) with sub-linear beam pruning, 1-hop `calls` / `called_by` graph expansion, strict Playbox trust gating (`allow_untrusted=False` by default), and compact prefix-cache-friendly brief formatting (`format_brief`). Exposes an `embed_fn` hook so dense sentence embeddings (`all-MiniLM-L6-v2`) can be plugged in when available.
- **`plugin.py`:** read-only Hermes tools `atlas_query` (3-hop `HiKVP` text brief) and `atlas_parametric_qa` (zero-context-token local QA over signed `playbox.atlaskv_shard.v1` shards, verified under hybrid `Ed25519+ML-DSA-65`), both allow-listed as read-only in `hermes-plugin-witness/witness.py`.
- **Tests:** `hermes-playbox-backend/tests/test_adapter_e2e.py` checks 3-level HiKVP retrieval, 1-hop discovery of `iter_modules_and_files` from a `BaseReloader`/`runserver` issue query (the exact failure mode from `docs/hermes-parity-swe-playbox.md` on `django-11422`), persistence round trip, and strict exclusion of quarantined prompt-injection triples.

## 4. Where full parametric AtlasKV fits next (open-weights track)

In [metered-benchmark.md](metered-benchmark.md), L2 steering vectors scored **0%** on exact factual recall while plain RAG / `text_swarm` scored 78–98%. For **self-hosted open-weights models** (Llama-3 / Phi-3, or a future Qwen3 adapter) running behind Playbox's `BOX-7` egress proxy:

1. **Zero-context-token parametric factual memory:** attach an offline-built, signed AtlasKV hierarchical store (e.g. compliance policies, CVE/API graphs, or curated org knowledge) to a local Llama-3 worker without spending input context tokens per turn.
2. **Required controls before any production use:**
   - **Signed & provenance-pinned KB shards:** never accept unsigned `.npy` embeddings or clusters from untrusted room members (same cache-poisoning threat documented in [l3-lmcache.md](l3-lmcache.md)); gate every KG shard by host-verified commit / `RoomLog` hash.
   - **Metered benchmark vs. plain RAG:** before spending GPU hours training Qwen3/Llama-3 adapters, measure exact `correct answers per USD` against `text_swarm` on the same [metered-benchmark.md](metered-benchmark.md) corpus (including offline embedding + clustering amortization).

## 5. Honest limits & live SWE-bench validation (2026-11-19)

- **Empirically validated across 5 uncontaminated SWE-bench Django instances (`docs/hermes-parity-swe-playbox.md`):**
  - `playbox_atlas/` builds ~16,400 repo + room-memory triples in ~1.2 s on CPU and surfaces 3-hop call chains (`watched_files -> iter_all_python_module_files -> iter_modules_and_files [110-136]`) plus receipt-backed `reviewed` lessons at turn 0.
  - On `django-11422`, 3-hop AtlasKV enabled `claude-sonnet-4-6` to solve the instance in **5 API calls, 9 delegates (`0.07 s` median), and $0.0787** (5× cheaper than Haiku 4.5's 53-call $0.40 run).
  - On `django-11848`, gate-verified compounding memory (`verified-memory.jsonl`, backed by a hybrid `Ed25519+ML-DSA-65` gate receipt SHA-256) lifted `gpt-5.6-luna` from `0/1` (`1/2` f2p in Clean Arms L & S) to `1/1` (`2/2` f2p, `42/42` p2p in Arm P2), giving Arm P2 **4/5 (80%)** uncontaminated solves vs **3/5 (60%)** in Clean Arms L & S at **$0.0878 per solve** (cheaper per solve than both L and S).
- **Open-weights parametric track (`HKUST-KnowComp/AtlasKV` & `playbox_atlas/parametric.py`):**
  - Implemented position-decoupled `ParametricAtlasKVEngine` + `TrainedKVProjectionAdapter` (`hermes-playbox-backend/playbox_atlas/parametric.py` + `tools/bench/hermes-parity/run_parametric_atlaskv_eval.py`) extracting post-`k_proj`/`k_norm`, pre-RoPE `(K_raw, V)` states per `HiKVP` intermediate cluster, projecting them via per-layer identity-initialized low-rank residual adapters (`rank=32` across 28 layers), and persisting hybrid post-quantum `Ed25519+ML-DSA-65`-signed `playbox.atlaskv_adapter.v1` and `playbox.atlaskv_shard.v1` manifests (`weights_only=True` deserialization, fail-closed on Ed25519, ML-DSA-65, or tensor SHA-256 mismatch).
  - Empirically benchmarked on `Qwen/Qwen3-1.7B-Base` both locally on Apple Silicon MPS (`16/20` = `80.0%`) and offloaded to GCP `yukon-rnd-1` (`12` vCPU, `96` GB RAM, `/tmp/atlaskv-gce-eval/parametric-atlaskv-summary.json`, `20` Django repository QA holdout queries + `3` quarantined prompt-injection triples; adapter trained on `48` strictly disjoint non-Django `pytest`/`flask`/`sympy`/`httpx`/`pydantic`/`playbox`/`click` examples with zero repository/file/entity overlap):
    - **Out-of-sample exact recall:** **`17 / 20` (`85.0%`)** on GCE FP32 (`16 / 20` = `80.0%` on MPS FP16) with `trained_parametric_kv_adapter`, vs. `6 / 20` (`30.0%`) un-adapted `parametric_kv_inject`, `6 / 20` (`30.0%`) text `HiKVP` brief, and `9 / 20` (`45.0%`) full-context prompt stuffing.
    - **Honest limit on naive `AdaptiveAvgPool1d` slot pooling (`slots_per_cluster=32`):** Compressing `4,384` KV slots `7.2×` down to `608` slots via non-learned temporal average pooling degrades exact line-number/identifier recall to `1 / 20` (`5.0%`), confirming exact token-level recall requires either unpooled pre-RoPE slots or a learned cross-attention slot resampler rather than uniform average pooling.
    - **`13.10×` online context-token compression vs. Text `HiKVP`** (`23.9` vs. `313.1` online tokens) and **`184.43×` vs. full-context stuffing** (`23.9` vs. `4,407.9` tokens).
    - **`36.55×`–`70.33×` prefill speedup** (`793.99 ms` vs. `29,017.53 ms` on GCE CPU; `57.42 ms` vs. `4,038.50 ms` on MPS), with **`0` quarantined poison leaks** (`3/3` eval + `2/2` train poison triples dropped pre-encode), `pq_mldsa65_verified=true`, and **`100%` tampered `.pt` shard & adapter rejection**.
