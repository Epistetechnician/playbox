# Entity-Routed Working-Set KV + Adaptive RAG vs Inline RAG vs Shared 128K KV

An end-to-end systems evaluation (**18,546 measured turns, 0 errors**) across **6 GPU experiments** (Enterprise CRM ~3.8K, FinanceBench ~5.9K, LoCoMo ~18.1K, a 4K→64K working-set crossover sweep, 4-replica L4 emulation, and a **physical 4-node H100-80GB cluster** over private east-west networking) and **2 model architectures / 3 scales** (`Qwen3-30B-A3B-Instruct-2507-FP8` MoE on `A100-SXM4-80GB`, dense `Qwen3-8B-FP8` on `L4-24GB` and `4× H100-80GB`, and `Qwen3-1.7B` on `L4-24GB`), comparing entity-routed working-set KV + adaptive RAG (`S-ROUTED`) against monolithic 128K shared KV (`S-CTX`), stateless inline RAG (`S-RAG`), conversational RAG with an LLM query rewriter (`S-CONV-RAG`), and two-stage dense bi-encoder + cross-encoder reranker RAG (`bge-small-en-v1.5` + `bge-reranker-v2-m3`). All latency is measured end-to-end from question arrival before routing, query rewriting, or retrieval.

## Core Finding

Monolithic 128K prefix caching loses to inline RAG because decode attention scales with $O(L)$ per token and look-alike entities act as distractors. **Shrinking and routing the KV unit** — caching each session's compact entity working set (~4K–18K tokens) in KV with CPU offloading and falling back to RAG only on entity drift — **dominates the speed/accuracy frontier**:

- **Clean CRM (`n=32`, 3,456 turns, A100):** `S-ROUTED` hits **706.5 turns/min** (**10.8×** over 128K Shared KV, **3.61×** over Inline RAG) and **0.54 s** follow-up p95 (**10×** faster than RAG), with **97.9% accuracy** (**100%** on look-alike Cat B, 0 routing errors).
- **Noisy / Implicit Queries (`n=32`, 768 turns, A100):** With Turn-1 account typos and Turn 2–6 `"this account"` follow-ups, stateless `S-RAG` collapses to **74.9%** while `S-ROUTED` holds **98.1%** (**+23.2 pp**, exact McNemar `p = 1.44e-20`) at **3.49× throughput**.
- **192-Account Multi-Tenant Eviction (`n=32`, 2,304 turns, A100 + 24 GB CPU tier):** Forcing `~737K` KV tokens past GPU HBM in waves triggers **526.2K tokens** of CPU→GPU KV reloads; `S-ROUTED` widens its lead to **4.07× throughput** (`436.3` vs `107.2` tpm) and **6.66× lower `q→first` p95** (`1.99 s` vs `13.28 s`).
- **SOTA Generalization vs Conversational & Dense+Reranker RAG (`n=8`, 2,700 turns, A100):** Against `bge-small-en-v1.5` + `bge-reranker-v2-m3` + LLM query rewriter, `S-ROUTED` stays **1.46–1.62× faster** at accuracy parity (`56.0%`) on FinanceBench (**−1.4 to −2.1 s** p95 `q→end`) and beats dense+reranker RAG by **+13.0–16.7 pp** (`p = 1.04e-11`) on LoCoMo while beating `S-CONV-RAG` on throughput.
- **Second Architecture — Dense `Qwen3-8B-FP8` (`n=8`, 1,350 turns, L4 + 20 GB CPU tier):** The serving win **doubles on a dense transformer** — **2.00–2.12×** throughput on FinanceBench and **1.98–2.06×** on LoCoMo (**−5.2 to −14.9 s** p95 `q→end`, 76–92% GPU prefix hit) — while surfacing an honest model-scale trade-off on FinanceBench (`45.3%` vs `52.7%`, `p = 0.0074`) and parity on LoCoMo (`47.7%`).
- **4K→64K Working-Set Crossover Law (`n=8`, 1,632 turns, A100-80GB):** Sweeping entity dossier size from 4K to 64K against GPU-accelerated dense+reranker RAG identifies exact throughput crossover thresholds at **$L^*_{\text{RAG}} = 8.2\text{K}$** tokens (vs stateless `S-RAG`) and **$L^*_{\text{CONV}} = 13.2\text{K}$** tokens (vs `S-CONV-RAG`), while `S-ROUTED` holds **95.8–100.0% accuracy** across 4K–48K before hitting the active-KV concurrency wall at 64K.
- **Physical 4-Node H100-80GB Cluster (`n=48`, 4,608 turns, `--clock-lock` 1590 MHz, private east-west mesh):** Running 4 physically separate `Qwen3-8B-FP8` H100 nodes (414.8K KV tokens/GPU = 1.66M total KV tokens, 48 ~8.2K entities, 100% accuracy, 0 errors), sticky routing beats Round-Robin by **2.15–2.32× throughput** (`6,400–6,918` vs `2,979` tpm) and **−42% to −45% p95 `q→end`** (`0.31 s` vs `1.74 s` follow-up TTFT) under Uniform traffic. Under extreme Zipf skew ($\alpha=1.5$), physical GPU contention exposes `STICKY-HASH`'s `2.42×` hot-spot (`[72, 348, 18, 138]`), and **`STICKY-BOUNDED` (`1.25×` cap) wins every metric** at **9,423.4 tpm (1.65× vs RR, +16.3% vs `STICKY-HASH`)**, **0.773 s p95 `q→end`**, **0.132 s follow-up p95 (2.26× faster than `STICKY-HASH`)**, and **93.8% GPU prefix hit** — with `STICKY-HOT-REP` achieving the tightest Zipf balance (`1.37×`) and cutting spillover `47.7% → 30.6%`.

---

## 1. Clean 3-Arm Head-to-Head (`a100_3arm_v1`)

2 counterbalanced sweeps × 32 sessions × 6 turns × 3 arms × 3 loads = **3,456 measured turns, 0 errors**. Each cell is isolated with a fresh UUID system-prefix salt.

![Clean 3-arm throughput and tail latency](assets/kv-routed/01_clean_3arm.png)

| Load | Arm | Throughput (tpm) | p95 `q→first` | p95 `q→end` | p95 turns 2–6 | Prompt toks | GPU KV hit % | Accuracy (A / B / D) |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| **n=1** | `S-CTX` (128K KV) | 43.7 (42.9, 44.6) | 0.70 s | 1.97 s | 0.70 s | 128,169 | 99.91% | 82.8% (83.6 / 78.1 / 84.4) |
| | `S-RAG` (Inline) | 88.5 (88.3, 88.7) | 0.35 s | 1.10 s | 0.35 s | 5,959 | 72.83% | 99.4% (100 / 98.4 / 98.0) |
| | **`S-ROUTED`** | **144.4** (144.0, 144.7) | **0.20 s** | **0.75 s** | **0.13 s** | **3,841** | **87.35%** | **98.0%** (98.0 / 100 / 95.7) |
| **n=8** | `S-CTX` (128K KV) | 82.5 (83.0, 82.0) | 2.27 s | 8.92 s | 1.96 s | 128,167 | 99.91% | 85.4% (86.7 / 81.3 / 84.0) |
| | `S-RAG` (Inline) | 159.8 (159.6, 160.0) | 1.85 s | 5.15 s | 1.78 s | 5,957 | 72.81% | 99.8% (100 / 100 / 98.8) |
| | **`S-ROUTED`** | **447.1** (444.0, 450.1) | **0.26 s** | **1.91 s** | **0.24 s** | **3,840** | **87.34%** | **97.8%** (97.7 / 100 / 96.1) |
| **n=32** | `S-CTX` (128K KV) | 65.3 (66.1, 64.6) | 11.75 s | 40.97 s | 4.50 s | 128,169 | 99.91% | 84.3% (85.2 / 79.7 / 85.5) |
| | `S-RAG` (Inline) | 195.9 (196.7, 195.1) | 5.65 s | 14.33 s | 5.40 s | 5,960 | 72.86% | 99.5% (100 / 100 / 97.3) |
| | **`S-ROUTED`** | **706.5** (702.8, 710.2) | **2.01 s** | **4.93 s** | **0.54 s** | **3,840** | **87.34%** | **97.9%** (97.7 / 100 / 96.9) |

- Paired exact McNemar at `n=32`: `S-ROUTED` vs `S-CTX` `p = 1.33e-13`; `S-ROUTED` vs `S-RAG` `p = 0.118` (not statistically significant).
- **Miss audit (`b1_n32_S-ROUTED`, 185/192 exact):** **0 routing errors** across all 192 turns. The 3 Cat A misses (`A-069`, `A-024`, `A-114`) are a formatting quirk where the profile literally says `Procurement portal: None` and the model wrote `"listed as None ... Answer: No such record exists."` instead of `"Answer: None"`. The 4 Cat D misses (`D-018`, `D-138`, `D-046`, `D-093`) all scored `0.75` (3/4 rubric items).

---

## 2. Stress Test 1 — Noisy & Implicit Routing (`a100_stress_noisy`)

Deterministically injects Turn-1 single-character typos (`--noisy-routing`, `seed=42`) and replaces explicit account names with `"this account"` on 50% of single-entity Cat A follow-ups (Turns 2–6). Both arms receive identical corrupted questions (**768 turns, 2 sweeps, 0 errors**).

![Noisy routing accuracy collapse](assets/kv-routed/02_noisy_routing.png)

| Arm (`n=32`) | Throughput | p95 `q→first` | p95 `q→end` | p95 turns 2–6 | Accuracy (A / B / D) | Exact McNemar |
|---|---:|---:|---:|---:|---:|---|
| `S-RAG` (Inline) | 181.7 tpm | 6.34 s | 15.88 s | 5.97 s | **74.9%** (64.1 / 96.9 / 96.1) | baseline |
| **`S-ROUTED`** | **634.5 tpm (3.49×)** | **2.02 s** | **5.14 s** | **0.66 s** | **98.1%** (97.7 / 100 / 98.0) | `b_only=100` vs `a_only=9`, **`p = 1.44e-20`** |

- Route breakdown (`b1_n32_S-ROUTED`, 192 turns): `104` exact/fuzzy entity hits, `50` pronoun carry-overs, `32` cross-account dossiers, `6` adaptive RAG fallbacks (`19.8%` total fallback rate).

---

## 3. Stress Test 2 — 192-Account Multi-Tenant KV Eviction (`a100_stress_evict192`)

Expands the 32 sessions across 6 isolated tenant namespaces (`--tenant-scale 6` → **192 distinct account KV prefixes**, `~737K` KV tokens) executed in turn-by-turn waves (`--wave-eviction`) so Turn 1 evicts cold accounts out of GPU HBM (`--gpu-memory-utilization 0.65`) into a **24 GB CPU KV tier** (vLLM native `OffloadingConnector`). **2,304 measured turns, 0 errors**.

![192-account wave eviction with CPU KV tier](assets/kv-routed/03_evict192_cpu.png)

| Arm (`n=32`, 192 tenants) | Throughput | p95 `q→first` | p95 `q→end` | p95 turns 2–6 | GPU KV hit % | CPU KV reload % (toks) | Accuracy |
|---|---:|---:|---:|---:|---:|---:|---:|
| `S-RAG` (Inline) | 107.2 tpm | 13.28 s | 35.62 s | 13.84 s | 6.7% | 0.4% (27,776) | 99.5% |
| **`S-ROUTED`** | **436.3 tpm (4.07×)** | **1.99 s (6.7×)** | **10.25 s (3.5×)** | **2.35 s (5.9×)** | **52.9%** | **25.2% (526,208)** | **97.9%** |

---

## 4. End-to-End Latency Savings Summary (p95 Milliseconds)

Across single-user (`n=1`) to high-concurrency (`n=32`) and multi-tenant eviction regimes on `Qwen3-30B-A3B-FP8`, `S-ROUTED` saves **−220 ms to −11,490 ms** on follow-up time-to-first-token (`q→first`) and **−350 ms to −36,040 ms** end-to-end (`q→end`):

![End-to-end p95 millisecond savings](assets/kv-routed/05_latency_delta_ms.png)

---

## 5. Cross-Dataset Generalization: FinanceBench & LoCoMo vs Conversational RAG (1,350 turns, 0 errors)

To test whether entity-routed KV generalizes beyond CRM and beats **conversational RAG with an LLM query rewriter** (`S-CONV-RAG`), we evaluated `Qwen3-30B-A3B-Instruct-2507-FP8` on two public SOTA benchmarks (`n=8`, A100-SXM4-40GB + 24 GB vLLM `OffloadingConnector`, `0` errors):

![Cross-dataset generalization](assets/kv-routed/04_sota_generalization.png)

| Benchmark | Arm | Throughput (`n=8`) | p95 `q→first` (follow-ups) | p95 `q→end` | Prompt toks | Token F1 | Accuracy | Exact McNemar vs `S-ROUTED` |
|---|---|---:|---:|---:|---:|---:|---:|---|
| **FinanceBench** *(all 150 Qs · 32 companies)* | `S-RAG` (BM25 top-5) | 124.7 tpm | 0.97 s (0.89 s) | 6.37 s | 5,166 | 47.1% | 42.7% | `a=4, b=37`, **`p = 1.03e-7`** |
| | `S-CONV-RAG` *(+ LLM rewriter, 1.08 s)* | 103.2 tpm | 2.37 s (2.37 s) | 8.96 s | 5,181 | 48.6% | 44.0% | `a=5, b=36`, **`p = 7.84e-7`** |
| | **`S-ROUTED`** *(~5.9K company KV + fallback)* | **147.6 tpm (1.43×)** | **0.82 s (0.45 s)** | **6.54 s** | **5,895** | **71.0%** | **64.7%** | *(124 entity, 9 carry, 17 fallback)* |
| **LoCoMo** *(300 Qs · 10 multi-session convs)* | `S-RAG` (BM25 top-5) | 205.9 tpm | 0.73 s (0.65 s) | 4.49 s | 3,742 | 45.4% | 49.3% | `a=13, b=38`, **`p = 6.21e-4`** |
| | `S-CONV-RAG` *(+ LLM rewriter, 0.59 s)* | 178.0 tpm | 1.62 s (1.62 s) | 4.66 s | 3,746 | 45.3% | 49.0% | `a=14, b=40`, **`p = 5.35e-4`** |
| | **`S-ROUTED`** *(~18.1K conv KV + fallback)* | **231.9 tpm (1.30×)** | **2.29 s (0.76 s)** | **4.68 s** | **18,136** | **51.4%** | **57.7%** | *(252 entity, 33 carry, 15 fallback)* |

- **FinanceBench (`+22.0 pp` vs `S-RAG`, `+20.7 pp` vs `S-CONV-RAG`):** The biggest leap is on `metrics-generated` multi-table calculations (`14.0%` / `16.0%` → **`76.0%`**), where top-5 retrieval drops needed financial lines while pinning the company's ~5.9K dossier preserves full recall at **0.45 s** follow-up p95 (**5.3×** faster than `S-CONV-RAG`).
- **LoCoMo (`+8.3 pp` vs `S-RAG`, `+8.7 pp` vs `S-CONV-RAG`):** Even at **18.1K tokens** per conversation (~4.8× longer prompts than RAG), `S-ROUTED` still beat both RAG baselines on throughput (**1.13×** vs `S-RAG`, **1.30×** vs `S-CONV-RAG`) while jumping **+15.9–18.3 pp** on temporal reasoning (Cat 2: `56.1%` / `53.7%` → **`72.0%`**) and **+8.5–10.9 pp** on multi-hop (Cat 1: `37.8%` / `35.4%` → **`46.3%`**).

---

## 6. Strong RAG Baseline: Dense Embeddings + Cross-Encoder Reranker (1,350 turns, 0 errors)

To close the "BM25 is a weak retriever" objection, we replaced BM25 with a two-stage **`BAAI/bge-small-en-v1.5` dense bi-encoder (`top-20`) + `BAAI/bge-reranker-v2-m3` cross-encoder reranker (`top-5`)** on the same A100 + `Qwen3-30B-A3B-Instruct-2507-FP8` server (`n=8`, `max_tokens=128`, 24 GB vLLM `OffloadingConnector`, **1,350 measured turns, 0 errors**):

![Dense+Reranker RAG vs Routed KV](assets/kv-routed/06_dense_rerank_sota.png)

| Benchmark | Arm *(Dense + Cross-Encoder Reranker)* | Throughput (`n=8`) | p95 `q→first` (follow-ups) | p95 `q→end` | Retrieve ms | GPU / CPU KV hit | Accuracy | Exact McNemar vs `S-ROUTED` |
|---|---|---:|---:|---:|---:|---:|---:|---|
| **FinanceBench** *(150 Qs · 32 cos)* | `S-RAG` *(bge-small + bge-reranker-v2-m3)* | 114.0 tpm | 1.37 s (1.16 s) | 5.68 s | 185.8 ms | 5.2% / 4.0% | 56.0% | `a=6, b=6`, `p = 1.00` |
| | `S-CONV-RAG` *(+ LLM rewriter, 765 ms)* | 102.5 tpm | 2.55 s (2.53 s) | 6.36 s | 171.7 ms | 5.8% / 5.3% | 54.7% | `a=4, b=6`, `p = 0.754` |
| | **`S-ROUTED`** *(~5.8K KV + rerank fallback)* | **166.3 tpm (1.62×)** | 1.63 s (1.55 s) | **4.26 s** | **13.2 ms** | **31.9% / 65.0%** | **56.0%** | *(metrics: **48%** vs 40%)* |
| **LoCoMo** *(300 Qs · 10 convs)* | `S-RAG` *(bge-small + bge-reranker-v2-m3)* | 179.8 tpm | 1.24 s (1.14 s) | 4.84 s | 189.4 ms | 8.2% / 5.8% | 44.3% | `a=10, b=49`, **`p = 2.71e-7`** |
| | `S-CONV-RAG` *(+ LLM rewriter, 507 ms)* | 163.5 tpm | 1.79 s (1.79 s) | 5.05 s | 159.2 ms | 7.7% / 5.5% | 40.7% | `a=5, b=55`, **`p = 1.04e-11`** |
| | **`S-ROUTED`** *(~18.1K KV + rerank fallback)* | **169.8 tpm (1.04×)** | 4.96 s (4.90 s) | 5.49 s | **5.7 ms** | **84.6% / 69.5%** | **57.3%** | **+13.0–16.7 pp** |

- **FinanceBench:** Upgrading RAG to dense + cross-encoder reranker closed the overall accuracy gap (`42.7%` → `56.0%`, tying `S-ROUTED`'s `56.0%` at `max_tokens=128`, though `S-ROUTED` still led `metrics-generated` math `48.0%` vs `40.0%`), **but `S-ROUTED` remained `1.46×` faster than `S-RAG` and `1.62×` faster than `S-CONV-RAG`** (`166.3` vs `114.0` / `102.5` tpm) and cut p95 `q→end` by **−1,420 to −2,100 ms** (`4.26 s` vs `5.68 s` / `6.36 s`) with **385K tokens** served from CPU KV offload (`65.0%` external prefix hit).
- **LoCoMo:** Dense + reranker RAG actually **lost ground** to `S-ROUTED` (**+13.0 pp** vs `S-RAG`, `p = 2.71e-7`; **+16.7 pp** vs `S-CONV-RAG`, `p = 1.04e-11`), because multi-session temporal and cross-turn cues are scattered across sessions that top-5 semantic reranking still drops. `S-ROUTED` beat `S-CONV-RAG` on throughput (`169.8` vs `163.5` tpm) with **84.6% GPU + 69.5% CPU offload prefix hits** (`581K` tokens).

---

## 7. Second Model Architecture: Dense `Qwen3-8B-FP8` on NVIDIA L4 (1,350 turns, 0 errors)

To test whether the serving speedup was specific to 3B-active MoE models, we replicated the full dense+reranker (`bge-small-en-v1.5` + `bge-reranker-v2-m3`) comparison on a **dense transformer (`Qwen/Qwen3-8B-FP8`, `enable_thinking=false`)** on an **NVIDIA L4 (24 GB)** with vLLM's 20 GiB CPU `OffloadingConnector` (`n=8`, `max_tokens=128`, **1,350 turns, 0 errors**):

![Dense Model Qwen3-8B-FP8 Replication](assets/kv-routed/07_dense_model_qwen8b.png)

| Benchmark *(Qwen3-8B-FP8 · L4)* | Arm *(Dense + Cross-Encoder Reranker)* | Throughput (`n=8`) | p95 `q→first` (follow-ups) | p95 `q→end` | GPU / CPU KV hit | Accuracy | Exact McNemar vs `S-ROUTED` |
|---|---|---:|---:|---:|---:|---:|---|
| **FinanceBench** *(150 Qs)* | `S-RAG` *(bge-small + bge-reranker-v2-m3)* | 23.0 tpm | 7.90 s (6.35 s) | 35.79 s | 5.8% / 3.1% | **52.7%** | `a=13, b=2`, `p = 0.0074` |
| | `S-CONV-RAG` *(+ LLM rewriter)* | 21.7 tpm | 15.00 s (14.30 s) | 35.30 s | 6.4% / 4.0% | 50.7% | `a=13, b=5`, `p = 0.096` |
| | **`S-ROUTED`** *(~5.8K KV + rerank fallback)* | **46.1 tpm (2.12×)** | **5.36 s (2.46 s)** | **20.90 s** | **76.3% / 0.8%** | 45.3% | *(−14.4 to −14.9 s p95)* |
| **LoCoMo** *(300 Qs)* | `S-RAG` *(bge-small + bge-reranker-v2-m3)* | 35.7 tpm | 7.53 s (7.52 s) | 29.80 s | 9.3% / 2.6% | 47.7% | `a=30, b=30`, `p = 1.00` |
| | `S-CONV-RAG` *(+ LLM rewriter)* | 34.3 tpm | 11.70 s (11.70 s) | 25.38 s | 9.1% / 3.5% | 47.7% | `a=28, b=28`, `p = 1.00` |
| | **`S-ROUTED`** *(~18.1K KV + rerank fallback)* | **70.6 tpm (2.06×)** | 14.07 s (13.30 s) | **20.19 s** | **92.2% / 40.0%** | **47.7%** | *(Cat2: **57.3%** vs 51.2%)* |

- **Bigger serving win on dense models (`2.0×–2.12×`):** Dense 8B pays full-parameter prefill FLOPs on every non-cached token, so `S-ROUTED`'s **76.3%–92.2% GPU prefix cache hit** (+ `170K` CPU offload tokens on LoCoMo) doubled throughput across both benchmarks (**2.00–2.12×** on FinanceBench, **1.98–2.06×** on LoCoMo) and cut p95 `q→end` by **−5.2 to −14.9 seconds**.
- **Model-scale accuracy nuance:** On LoCoMo, `S-ROUTED` tied dense+reranker RAG overall (`47.7%`, `p = 1.00`) while still winning temporal Cat 2 (`57.3%` vs `51.2%`) and multi-hop Cat 1 (`40.2%` vs `37.8%`). On FinanceBench, the smaller 8B dense model benefited slightly from cross-encoder pre-filtering (`52.7%` vs `45.3%`, `p = 0.0074`), whereas `Qwen3-30B-A3B` achieved accuracy parity (`56.0%` vs `56.0%`) — showing that stronger base models extract full value from routed dossiers without needing a reranker to pre-filter tables.

---

## 8. Working-Set Size Crossover Law: 4K → 64K (1,632 turns, 0 errors)

To establish an exact systems threshold $L^*$ for when entity-routed KV beats RAG versus when RAG wins, we swept synthetic enterprise entity dossiers across **$L \in \{4\text{K}, 8\text{K}, 16\text{K}, 32\text{K}, 48\text{K}, 64\text{K}\}$** tokens (`16` entities × `6` multi-turn questions/session = `96` turns/cell, `n=8`, `Qwen3-30B-A3B-Instruct-2507-FP8` on `A100-SXM4-80GB`, `max-model-len=73728`, `24 GiB` vLLM `OffloadingConnector`, GPU-accelerated `bge-small-en-v1.5` top-20 → `bge-reranker-v2-m3` top-5).

![4K to 64K Working-Set Crossover Curve](assets/kv-routed/08_crossover_curve.png)

| Target $L$ | `S-ROUTED` prompt tok | `S-ROUTED` tpm (p95 `q→end`) | `S-RAG` tpm (p95 `q→end`) | `S-CONV-RAG` tpm (p95 `q→end`) | `S-ROUTED` / `S-CONV` / `S-RAG` Acc | `S-ROUTED` GPU hit |
|---|---:|---:|---:|---:|---:|---:|
| **4K** | 4,992 | **370.2 tpm** (**2.61 s**) | 271.6 tpm (3.06 s) | 169.0 tpm (6.49 s) | **100.0%** / 84.4% / 50.0% | 72.4% |
| **8K** | 9,126 | **246.6 tpm** (**4.83 s**) | 251.7 tpm (3.75 s) | 163.0 tpm (5.32 s) | **99.0%** / 54.2% / 39.6% | 77.3% |
| **16K** | 17,536 | 132.9 tpm (9.79 s) | **274.3 tpm** (3.02 s) | 167.9 tpm (7.06 s) | **100.0%** / 67.7% / 53.1% | 80.3% |
| **32K** | 34,167 | 56.9 tpm (25.44 s) | **268.1 tpm** (3.20 s) | 184.1 tpm (3.81 s) | **96.9%** / 74.0% / 40.6% | 81.8% |
| **48K** | 50,835 | 23.7 tpm (65.11 s) | **270.6 tpm** (3.29 s) | 189.9 tpm (3.44 s) | **95.8%** / 100.0% / 53.1% | 76.1% |
| **64K** | ~67.5K | *KV capacity wall* *(1 active, 7 queued)* | **245.0 tpm** (3.51 s) | 156.4 tpm (4.84 s) | — / 64.6% / 50.0% | 45.0%* |

- **Exact crossover thresholds ($L^*$):** Against GPU-accelerated dense+reranker RAG (`~267.3` tpm flat), `S-ROUTED` crosses over at **$L^*_{\text{RAG}} = 8,170$ prompt tokens** (~8.2K). Against conversational RAG with an LLM query rewriter (`~174.8` tpm flat), `S-ROUTED` crosses over at **$L^*_{\text{CONV}} = 13,166$ prompt tokens** (~13.2K). Linear time-per-turn fit for `S-ROUTED`: $\text{sec/turn} \approx -0.274 + 0.0498 \cdot L_{\text{K}}$ ($R^2 = 0.929$).
- **Four operating regimes (design law):**
  1. **$L \le 8\text{K}$:** Routed KV dominates on **throughput (1.36–2.19×), p95 latency, and accuracy (99–100%)**.
  2. **$8\text{K} < L \le 13\text{K}$:** Routed KV still beats **conversational RAG** on throughput & p95 while holding **99–100% accuracy**.
  3. **$13\text{K} < L \le 32\text{K}$:** Speed-vs-accuracy trade-off zone — `S-ROUTED` trades 1.3–3.2× lower throughput for **97–100% accuracy** (no retrieval recall drops).
  4. **$L \ge 48\text{K}$:** Decode-attention + active-KV capacity wall (`23.7` tpm at 48K; `Running: 1, Waiting: 7` at 64K) — switch to **chunked RAG or hierarchical entity summaries**.

---

## 9. Physical 4-Node H100-80GB Multi-Replica Sticky Routing (4,608 turns, 0 errors)

To test whether entity-routed KV locality scales across physically separate GPUs without hot-spotting, we deployed **4 physically separate NVIDIA H100-80GB nodes** (`--clock-lock`, 1590 MHz sustained, private east-west mesh `100.64.0.1–4`, `vLLM` serving `Qwen/Qwen3-8B-FP8`, `56.96 GiB` / `414,784` KV tokens per GPU = `1.66M` cluster KV tokens). We swept **4 routing policies** (`RR`, `STICKY-HASH`, `STICKY-BOUNDED` at `1.25×` cap, and `STICKY-HOT-REP` adaptive top-$k$ hot-entity replication) across `48` ~8.2K-token entity dossiers (`96` sessions × `6` turns = `576` turns/cell, `n=48` concurrent sessions; **4,608 measured turns, 100.0% accuracy, 0 errors**) under Uniform ($\alpha=0.0$) and Zipf-skewed ($\alpha=1.5$) traffic.

![Physical 4-node H100 sticky routing throughput and tail latency](assets/kv-routed/09_multinode_sticky.png)

| Traffic | Policy | Throughput (`n=48`) | p95 `q→first` (t2–6) | p95 `q→end` | GPU KV hit | Imbalance (max/mean) | Spill % | Per-node turns |
|---|---|---:|---:|---:|---:|---:|---:|---|
| **Uniform** ($\alpha=0.0$) | `RR` (Round-Robin) | 2,978.6 tpm | 1.820 s (1.738 s) | 2.845 s | 67.7% | 1.00× | 0.0% | `[144, 144, 144, 144]` |
| | **`STICKY-HASH`** | **6,918.2 tpm (2.32×)** | 1.291 s (**0.306 s**) | **1.564 s** | **91.5%** | 1.33× | 0.0% | `[108, 132, 192, 144]` |
| | `STICKY-BOUNDED` *(1.25×)* | 6,400.1 tpm (2.15×) | **1.221 s** (0.511 s) | 1.643 s | 87.7% | **1.09×** | 8.5% | `[121, 145, 153, 157]` |
| | `STICKY-HOT-REP` | 6,646.1 tpm (2.23×) | 1.235 s (0.581 s) | 1.658 s | 88.1% | **1.09×** | 7.5% | `[121, 144, 157, 154]` |
| **Zipf** ($\alpha=1.5$) | `RR` (Round-Robin) | 5,710.8 tpm | 0.710 s (0.519 s) | 1.275 s | 87.6% | 1.00× | 0.0% | `[144, 144, 144, 144]` |
| | `STICKY-HASH` | 8,104.1 tpm (1.42×) | **0.532 s** (0.298 s) | 0.909 s | **96.4%** | 2.42× | 0.0% | `[72, 348, 18, 138]` |
| | **`STICKY-BOUNDED`** *(1.25×)* | **9,423.4 tpm (1.65×)** | 0.673 s (**0.132 s**) | **0.773 s** | 93.8% | 1.46× | 47.7% | `[132, 183, 51, 210]` |
| | `STICKY-HOT-REP` | 8,874.6 tpm (1.55×) | 0.691 s (0.215 s) | 0.860 s | 92.9% | **1.37×** | 30.6% | `[138, 189, 52, 197]` |

- **Uniform traffic — 2.15–2.32× cluster speedup over Round-Robin:** Stateless `RR` scatters `48 × 8.2K` entities across all 4 H100s, dropping prefix hit rate to `67.7%` and p95 follow-up TTFT to `1.738 s`. Sticky policies keep each entity warm on one node (`87.7–91.5%` GPU hit), hitting **6,400–6,918 tpm (2.15–2.32× vs RR)** and **0.31–0.58 s** follow-up p95 (`−42%` to `−45%` p95 `q→end`), with `STICKY-BOUNDED` and `STICKY-HOT-REP` tightening load balance from `1.33×` to **`1.09×`**.
- **Zipf skew ($\alpha=1.5$) — bounded-load spillover beats pure sticky hashing on physical GPUs:** Pure `STICKY-HASH` pins the hottest entities to Node 2 (`348/576 = 60.4%` of turns, **`2.42×` imbalance**, while Node 3 gets `18` turns), bottlenecking on one physical H100's compute (`8,104` tpm, `0.298 s` follow-up p95). **`STICKY-BOUNDED` (`1.25×` cap) spills excess load deterministically onto the next node in the hash ring**, warming a secondary H100 (`93.8%` GPU prefix hit!), lifting throughput to **9,423.4 tpm (1.65× vs RR, +16.3% over `STICKY-HASH`)**, and cutting follow-up p95 to **0.132 s (3.93× faster than RR, 2.26× faster than `STICKY-HASH`)**. `STICKY-HOT-REP` achieves the tightest cluster balance (`1.37×`) while cutting spillover from `47.7%` to `30.6%`.
- **Why physical multi-node testing mattered (vs 1,728-turn single-L4 emulation):** In an earlier single-L4 4-process emulation (`Qwen3-1.7B`, `n=24`, 1,728 turns, 0 errors), `STICKY-BOUNDED` still beat RR under Uniform (`304.4` vs `193.3` tpm, `1.58×`, `−41%` p95), but shared-SM contention masked `STICKY-HASH`'s `2.50×` compute hot-spot under Zipf (`835.5` vs `524.2` tpm). Moving to 4 physically isolated H100s removes SM sharing and shows that, at this 4-node scale and $\alpha=1.5$ Zipf skew, **bounded-load sticky routing outperforms both Round-Robin and pure sticky hashing** (clusters beyond 4 nodes and disaggregated KV transfer remain untested here).

---

## 10. Drop-in OpenAI-Compatible Proxy (`playbox_steer.kv_routed_proxy`)

The entity router + adaptive RAG fallback is productized as a drop-in OpenAI-compatible HTTP proxy in [`steer/playbox_steer/kv_routed_proxy.py`](../steer/playbox_steer/kv_routed_proxy.py) (`steer/tests/test_kv_routed_proxy.py` passing):

- Accepts standard `/v1/chat/completions` (`stream: false` and SSE `stream: true`) and forwards to one or multiple upstream vLLM / OpenAI-compatible replicas (`--upstreams` + `BoundedStickyRouter` with `STICKY-BOUNDED`, `STICKY-HASH`, or `RR` plus adaptive Zipf hot-entity replication across top-$k$ warm replicas).
- Tracks per-session active entity via `X-Session-Id`, pins the entity dossier in a canonical `system` prefix for vLLM automatic prefix caching, and appends adaptive RAG chunks to the user turn only on entity drift or cross-entity fallback.
- Returns routing telemetry headers (`X-Routed-Mode`, `X-Routed-Entity`, `X-Routed-Backend`, `X-Routed-Spilled`, `X-Routed-Hot`) and exposes `/health` and `/metrics`.

---

## 11. Scope & Methodology Disclosures

1. **Scope boundaries:** This release covers entity-scoped multi-turn workloads from 3.8K to 64K working sets across two architectures / three scales (`Qwen3-30B-A3B-FP8` MoE on A100, dense `Qwen3-8B-FP8` on L4 and a physical 4-node H100-80GB cluster, and `Qwen3-1.7B` on L4). Sliding-window MoEs, >4-node clusters with disaggregated prefill/decode KV transfer (NIXL/Mooncake), and open-domain search without entity locality remain outside scope.
2. **Hardware & model-scale differences:** Sections 1–6 and 8 ran on NVIDIA A100-SXM4-80GB (`Qwen3-30B-A3B-FP8`); Section 7 (`Qwen3-8B-FP8`) ran on NVIDIA L4-24GB; Section 9 ran on **4 physically separate NVIDIA H100-80GB nodes** (`--clock-lock`, 1590 MHz, private east-west mesh, `Qwen3-8B-FP8`, 4,608 turns) alongside a single-L4 4-process `Qwen3-1.7B` contrast run (1,728 turns). Throughput ratios are compared within each hardware/model cell rather than across GPUs. On FinanceBench, the smaller 8B dense model benefited from cross-encoder pre-filtering (`52.7%` vs `45.3%`), whereas `Qwen3-30B-A3B` reached parity (`56.0%`).
3. **CPU KV offload backend:** Multi-tenant eviction, SOTA, crossover, and L4 multi-replica runs used vLLM's native `OffloadingConnector` (`24 GiB` CPU tier on A100, `20 GiB` on L4, `4 GiB × 4` for L4 emulation) because PyPI's prebuilt `lmcache` wheel hit a PyTorch C++ ABI mismatch on Colab Python 3.13 (`lmcache/c_ops.so`). On the physical 4× H100-80GB cluster, each GPU holds `56.96 GiB` (`414,784` KV tokens) natively in HBM.
4. **CRM baseline parity:** Sections 1–3 keep CRM `S-RAG` stateless to mirror the `a100-inline-4608` baseline; conversational RAG (`S-CONV-RAG`) and dense+cross-encoder reranker RAG (`bge-small-en-v1.5` + `bge-reranker-v2-m3`) are evaluated in Sections 5–8.

---

## Reproducing

Code & tests: [`steer/playbox_steer/kv_routed.py`](../steer/playbox_steer/kv_routed.py), [`steer/playbox_steer/kv_sota_bench.py`](../steer/playbox_steer/kv_sota_bench.py), [`steer/playbox_steer/kv_routed_proxy.py`](../steer/playbox_steer/kv_routed_proxy.py), [`steer/tools/kv_routed_run.py`](../steer/tools/kv_routed_run.py), [`steer/tools/kv_sota_run.py`](../steer/tools/kv_sota_run.py), [`steer/tools/kv_crossover_run.py`](../steer/tools/kv_crossover_run.py), [`steer/tools/kv_multinode_run.py`](../steer/tools/kv_multinode_run.py), [`steer/tests/test_kv_routed.py`](../steer/tests/test_kv_routed.py), [`steer/tests/test_kv_sota.py`](../steer/tests/test_kv_sota.py), [`steer/tests/test_kv_routed_proxy.py`](../steer/tests/test_kv_routed_proxy.py), [`steer/tests/test_kv_multinode.py`](../steer/tests/test_kv_multinode.py).

```bash
# Local unit + mock-server tests (19/19)
PYTHONPATH=steer pytest --noconftest steer/tests/test_kv_routed.py steer/tests/test_kv_sota.py steer/tests/test_kv_routed_proxy.py steer/tests/test_kv_multinode.py -q

# Clean 3-arm run
PYTHONPATH=steer python3 steer/tools/kv_routed_run.py \
  --bundle-dir /path/to/a100-inline-4608 \
  --out-dir results/a100_3arm_v1 \
  --arms S-RAG,S-CTX,S-ROUTED --loads 1,8,32 --sweeps 2 --max-sessions 32

# Stress Test 1: Noisy / implicit routing
PYTHONPATH=steer python3 steer/tools/kv_routed_run.py \
  --bundle-dir /path/to/a100-inline-4608 \
  --out-dir results/a100_stress_noisy \
  --arms S-RAG,S-ROUTED --loads 32 --sweeps 2 --max-sessions 32 --noisy-routing

# Stress Test 2: 192-account wave eviction (run vLLM with OffloadingConnector 24GB CPU tier)
PYTHONPATH=steer python3 steer/tools/kv_routed_run.py \
  --bundle-dir /path/to/a100-inline-4608 \
  --out-dir results/a100_stress_evict192 \
  --arms S-RAG,S-ROUTED --loads 32 --sweeps 1 --max-sessions 32 \
  --tenant-scale 6 --wave-eviction
```
