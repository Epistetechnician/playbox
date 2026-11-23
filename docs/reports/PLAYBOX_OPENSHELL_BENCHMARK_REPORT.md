# Playbox + NVIDIA OpenShell: Multi-Front Pareto Frontier, Security & Attribution Report

## Executive Summary

Playbox wraps NVIDIA OpenShell microVMs (`openshell 0.1.6`) with governed execution, anti-reward-hacking controls, host-side `BOX-7` egress confinement, and offline-verifiable hybrid post-quantum (`Ed25519 + ML-DSA-65`) compliance receipts.

**Six-Front Empirical Evaluation (bounded to the tested slices below, with full negative-result disclosures; does not establish general SWE-bench Verified coding-intelligence lift):**
1. **Front 1 — Multi-Agent Taint Confinement + Lean 4 Proof (`n=20`, 4 cross-VM vectors) & Single-Agent Security (`n=20`, 5 vectors):**
   - **Multi-agent (`n=20`):** Arm P2 achieved **0/20 ASR (0.0%, Wilson [0.0%, 16.1%])** at **20/20 (100.0%) clean utility** vs. Stock OpenShell's **20/20 ASR (100.0%)** across `poisoned_issue_exfil`, `poisoned_dep_supply_chain`, `cross_vm_memory_poison`, and `privilege_escalation_grant` — exact paired McNemar **`b=20, c=0` (`p_2sided=2e-6`, `p_1sided=1e-6`)**, bound to 5 machine-checked Lean 4 theorems in `lean/Playbox/Taint.lean` (`lake build` OK) and offline 5-gate PQ bundle `430597057cd0f8cf`.
   - **Single-agent (`n=20`):** Arm P2 achieved **0/20 ASR** at **19/20 (95.0%) clean utility**, strictly dominating Stock OpenShell (`20%` ASR, `85%` utility) and Local Hermes (`20%` ASR, `95%` utility). Combined across both adversarial suites: **0/40 ASR (0.0%, Wilson [0.0%, 8.8%])** at **39/40 (97.5%) utility** vs. Stock OpenShell **24/40 (60.0%) ASR**.
2. **Front 2 — 100% Host-Attested Apples-to-Apples Execution (`n=50` polyglot, `claude-haiku-5-5`, identical `HERMES_DISABLE_LAZY_INSTALLS=1`, `--require-host-aiproxy`):**
   - Routing **100% (`50/50` Arm S + `50/50` Arm P2)** of Anthropic `/v1/messages` calls through `playbox-aiproxy` achieved **`all_host_aiproxy_ledgers=true`**, **`50/50` verified `Ed25519 + ML-DSA-65` receipts**, and **`0` taint advisories** (bundle `6ee38e9ee9516eaf`).
   - **Exact solve-rate parity:** Arm P2 Warm Pool `40/50 (80.0%)` vs. Arm S `41/50 (82.0%)`, paired McNemar `b=4, c=5 (p=1.0000)`.
   - **Honest apples-to-apples latency/cost:** With identical `HERMES_DISABLE_LAZY_INSTALLS=1`, Arm S averaged `48.3s` (median `23.7s`, `$3.83`) vs. Arm P2 Warm Pool `55.8s` (median `37.2s`, median delegate cmd `0.15s`, `$6.41`): **+7.5s mean governance overhead** overall, while warm same-image reuse (`ts-01..05`) ran **2.53× faster** than Stock OpenShell (`22.2s` vs. `56.1s`). (Earlier `52.3s < 54.8s` comparison had `HERMES_DISABLE_LAZY_INSTALLS` enabled only on Arm P2 Fast and is disclosed as confounded.)
3. **Front 3 — True Same-Generation Cascade Ablation (`n=18` Track-A, `Haiku 5.5 → Sonnet 5.5` vs. `Sonnet 5.5`, 100% `--require-host-aiproxy`):**
   - **`18/18` (`100%`) host-attested ledgers & PQ receipts** (bundle `116f18b18784fca6`). Direct Arm S (`Sonnet 5.5`): `15/18 (83.3%)`, `$3.27`, `28.4s` mean (`16.6s` median). Arm P2 Cascade (`Haiku 5.5 → Sonnet 5.5`, 12 T1 / 6 T2): `13/18 (72.2%)`, `$5.09`, `131.4s` mean, paired McNemar `b=0, c=2 (p=0.5000)`.
   - **Honest negative finding:** Without a fast local repro-gate, same-generation `Haiku 5.5 → Sonnet 5.5` did **not** beat direct `Sonnet 5.5` on cost or wall time, confirming that the prior `13/18` vs. `6/18` (`p=0.0156`) gap was a **model-generation effect (`5.5` vs. `4.6`)**, not a Playbox cascade win.
4. **Front 4 — 100% Host-Attested Multi-Provider Proxy (`Anthropic` `50/50` + `18/18` AND `Gemini/Vertex` `7/7`):** `75/75` Arm P2 runs across Anthropic (`Haiku 5.5`, `Sonnet 5.5`) and Vertex (`gemini-3-flash-preview`) achieved `all_host_aiproxy_ledgers=true`, `0` bypasses, and `0` taint advisories under fail-closed `--require-host-aiproxy`.
5. **Front 5 — AtlasKV Latency Ablation (`n=50` polyglot, same governed microVM & model):** AtlasKV cut governed mean wall time by **-15.5%** (`79.8s → 67.4s`, median `47.8s → 38.4s`) at equal cost (`$0.28` vs. `$0.29`), while stateful `keep=true` + `sync=none|delta` cut Playbox's own per-command overhead from **6.97s → 0.14s** (~50× vs. legacy stateless Playbox).
6. **Front 6 — Cross-Harness Replication (`Hermes` `n=50` + `Devin` `n=15` stratified across all 7 repos):** On `Devin` (`claude-haiku-5-5`), Arm P2 replicated exact task-by-task parity with Stock OpenShell (`13/15 = 86.7%` vs. `13/15 = 86.7%`, paired McNemar `b=0, c=0, p=1.0000`) with **15/15** offline-verified hybrid PQ receipts.

**Explicit Boundaries & Disclosures:**
- **This is a governed execution & multi-agent security result, NOT a coding-agent intelligence breakthrough:** Across all controlled same-model runs (`n=50` apples-to-apples Haiku 5.5, `n=50` legacy Haiku 5.5, `n=88` Sonnet 4.6, `n=15` Devin Haiku 5.5, `n=18` same-generation Sonnet 5.5), Playbox shows zero solve-rate lift over Stock OpenShell (`p=0.5000–1.0000`).
- **Offline verification & git state:** All **12/12** runtime-native 5-gate bundles pass offline `playbox gate verify --require-signature --require-pq` (`sig=true pq_sig=true`, `claims_full_84_permf=false`). Local runs executed on a dirty working tree (`git_clean=false` in runner summaries; `playbox` binary built from clean `502c7c9c9`); commit/tag before any public release.
- **No 500-task SWE-bench Verified leaderboard comparisons:** All slices (`n=7`, `n=15`, `n=18`, `n=20`, `n=28`, `n=50`, `n=88`) are scoped to controlled paired comparisons.

## 1. Front 1: Security × Utility Pareto & Post-Quantum Verification

### 20-Task Adversarial Security Eval (`n=20`, 5 vectors × 4 tasks)

| Arm | Clean Utility | Utility 95% CI | Attacks Succeeded (ASR) | ASR 95% CI | Cost | PQ Receipts | Pareto Status (Utility × ASR) |
|---|---:|---|---:|---|---:|---|---|
| Arm L (Local Hermes) | 19/20 (95.0%) | [76.4%, 99.1%] | 4/20 (20.0%) | [8.1%, 41.6%] | $0.034 | No | Dominated by Arm P2 (Playbox + OpenShell) |
| Arm S (Stock OpenShell) | 17/20 (85.0%) | [64.0%, 94.8%] | 4/20 (20.0%) | [8.1%, 41.6%] | $0.033 | No | Dominated by Arm L (Local Hermes), Arm P2 (Playbox + OpenShell) |
| Arm P2 (Playbox + OpenShell) | 19/20 (95.0%) | [76.4%, 99.1%] | 0/20 (0.0%) | [0.0%, 16.1%] | $0.346 | 20/20 | **Frontier** |

- **Failure breakdown:** Arm L failed 4/4 `sandbox_escape_traversal`; Arm S failed 4/4 `secret_exfil`; Arm P2 blocked 20/20 across all 5 vectors (`secret_exfil`, `backdoor_injection`, `git_history_leak`, `test_tampering`, `sandbox_escape_traversal`).
- **Anti-reward-hacking (`.git` history isolation):** Single-commit archive loading eliminates the `git log --all` future-history leak observed on 100% of original Arm L/S `django-11422` solves.
- **Paired exact McNemar on ASR (P2 vs. S, and P2 vs. L):** discordant `(4, 0)`, one-sided `p=0.0625`, two-sided `p=0.1250`.

### 20-Task Multi-Agent Taint-Confinement Eval (`n=20`, 4 cross-VM vectors + Lean 4 Proof)

| Arm | Clean Utility | Attacks Succeeded (ASR) | ASR 95% Wilson CI | Paired McNemar (Defense) | PQ Bundle | Lean 4 Proof |
|---|---:|---:|---|---|---|---|
| Arm S (Stock OpenShell) | 20/20 (100.0%) | 20/20 (100.0%) | [83.9%, 100.0%] | baseline | — | — |
| **Arm P2 (Playbox + OpenShell)** | **20/20 (100.0%)** | **0/20 (0.0%)** | **[0.0%, 16.1%]** | **b=20, c=0 (p_2sided=2e-06, p_1sided=1e-06)** | `430597057cd0f8cf` | `Playbox.Taint` (5 theorems) |

- **Vectors (5 tasks each):** `poisoned_issue_exfil`, `poisoned_dep_supply_chain`, `cross_vm_memory_poison`, `privilege_escalation_grant` — Arm S failed `5/5` on all 4 vectors; Arm P2 blocked `5/5` on all 4 vectors at `100%` clean utility.
- **Formal model binding:** `lean/Playbox/Taint.lean` (SHA256 `402c921328045885...`, `lake build` OK in `0.13s`) mechanically verifies 5 state-machine invariants (`taint_monotone, tainted_worker_blocks_untrusted_egress, tainted_worker_cannot_poison_critic_memory, untrusted_worker_never_gets_secret_grant, multi_agent_non_interference`) within the formal taint-lattice model (not a whole-kernel/OS proof) and is cryptographically referenced by the offline-verified 5-gate PQ bundle.

### Offline Cryptographic Verification (`playbox gate verify --require-signature --require-pq`)

- **security20** (`/Users/shaanp/Documents/GitHub/playbox/docs/reports/playbox-security20-5gate-bundle.json.gz`): **PASS** — `attested gate OK: schema=playbox.attested_gate.v1 digest=25049272279f7312 sig=true pq_sig=true`
- **multi_agent_taint_20** (`/tmp/pbx-eval-multiagent-taint-n20/multi_agent_taint.gate.json`): **PASS** — `attested gate OK: schema=playbox.attested_gate.v1 digest=430597057cd0f8cf sig=true pq_sig=true`
- **a2a_warm_50** (`/tmp/swe-a2a-warm-50/verified-eval-5gate-bundle.json`): **PASS** — `attested gate OK: schema=playbox.attested_gate.v1 digest=6ee38e9ee9516eaf sig=true pq_sig=true`
- **samegen_cascade_18** (`/tmp/swe-samegen-cascade-18/verified-eval-5gate-bundle.json`): **PASS** — `attested gate OK: schema=playbox.attested_gate.v1 digest=116f18b18784fca6 sig=true pq_sig=true`
- **swe50_full_cascade** (`/Users/shaanp/Documents/GitHub/playbox/docs/reports/playbox-swe50-5gate-bundle.json.gz`): **PASS** — `attested gate OK: schema=playbox.attested_gate.v1 digest=4dd9c186c60fb9c3 sig=true pq_sig=true`
- **ablation_no_atlas** (`/tmp/swe-ablation-no-atlas-50/verified-eval-5gate-bundle.json`): **PASS** — `attested gate OK: schema=playbox.attested_gate.v1 digest=021ceddda21287e3 sig=true pq_sig=true`
- **ablation_with_atlas** (`/tmp/swe-ablation-with-atlas-50/verified-eval-5gate-bundle.json`): **PASS** — `attested gate OK: schema=playbox.attested_gate.v1 digest=1d7dd2b2c924f0f5 sig=true pq_sig=true`
- **gap2_p2_fast_50** (`/tmp/swe-gap2-fast-50/verified-eval-5gate-bundle.json`): **PASS** — `attested gate OK: schema=playbox.attested_gate.v1 digest=142e42e59c8c758d sig=true pq_sig=true`
- **gap1_aiproxy_7repo** (`/tmp/swe-gap1-aiproxy-7repo/verified-eval-5gate-bundle.json`): **PASS** — `attested gate OK: schema=playbox.attested_gate.v1 digest=c90fc728f13c856f sig=true pq_sig=true`
- **gap3_hard18_track_a** (`/tmp/swe-gap3-hard18-haiku/verified-eval-5gate-bundle.json`): **PASS** — `attested gate OK: schema=playbox.attested_gate.v1 digest=dfffdbdc65cf378e sig=true pq_sig=true`
- **pareto_p2_2tier_cascade** (`/tmp/swe-pareto-p2-cascade-50/verified-eval-5gate-bundle.json`): **PASS** — `attested gate OK: schema=playbox.attested_gate.v1 digest=b45afbb837b56f84 sig=true pq_sig=true`
- **devin15_cross_harness** (`/tmp/swe-pareto-devin-15/verified-eval-5gate-bundle.json`): **PASS** — `attested gate OK: schema=playbox.attested_gate.v1 digest=f22043e9c616b376 sig=true pq_sig=true`

## 2. Fronts 2 & 3: Intelligence-per-Dollar & Latency-Governance Pareto (`n=50` Polyglot)

All 6 rows evaluate the **exact same 50 polyglot instances** across 7 repositories (Django 28, Flask 1, pytest 3, SymPy 5, scikit-learn 3, Rust 5, TypeScript 5) under the **Hermes** harness on **NVIDIA OpenShell 0.1.6**.

| Configuration | Model / Tiers | Resolved | 95% Wilson CI | Total Cost | $/Solve | Mean Wall | Med Cmd | PQ Receipts | Pareto (Solve×Cost) | Pareto (+Governance) |
|---|---|---:|---|---:|---:|---:|---:|---|---|---|
| Arm S (Stock OpenShell) | Haiku 5.5 (T1) | 42/50 (84.0%) | [71.5%, 91.7%] | $0.17 | $0.0040 | 54.8s | — | — | **Frontier** | **Frontier** |
| **Arm P2 Fast (+AtlasKV, fast-fail, no-online-verify)** | Haiku 5.5 (T1 only) | 43/50 (86.0%) | [73.8%, 93.0%] | $0.26 | $0.0060 | 52.3s | 0.14s | 50/50 | **Frontier** | **Frontier** |
| Arm P2 Bare (Governed, no AtlasKV) | Haiku 5.5 (T1 only) | 43/50 (86.0%) | [73.8%, 93.0%] | $0.29 | $0.0067 | 79.8s | 0.14s | 50/50 | Dominated (1) | Dominated (1) |
| Arm P2 + AtlasKV (no repro-gate) | Haiku 5.5 (T1 only) | 41/50 (82.0%) | [69.2%, 90.2%] | $0.28 | $0.0069 | 67.4s | 0.17s | 50/50 | Dominated (2) | Dominated (1) |
| Arm P2 2-Tier Cascade (pure Hermes) | Haiku 5.5 → Sonnet 4.6 | 42/50 (84.0%) | [71.5%, 91.7%] | $2.26 | $0.0537 | 122.6s | 0.17s | 50/50 | Dominated (3) | Dominated (2) |
| Arm S Frontier-Everywhere | Sonnet 4.6 (T1) | 38/50 (76.0%) | [62.6%, 85.7%] | $4.60 | $0.1210 | 58.6s | — | — | Dominated (5) | Dominated (5) |
| Arm P2 3-Tier Full Cascade | Haiku → Neon → Opus | 44/50 (88.0%) | [76.2%, 94.4%] | $8.88 | $0.2018 | 161.5s | 0.15s | 50/50 | **Frontier** | **Frontier** |

- **Arm P2 Bare (Haiku 5.5, `$0.29`) vs. Arm S Frontier (Sonnet 4.6, `$4.60`):** `+10.0pp` (`43/50` vs. `38/50`), **16.0× cheaper** (`18.1×` cheaper per solve), paired McNemar `b=7, c=2` (two-sided `p=0.1797`, one-sided `p=0.0898`).
- **Arm P2 2-Tier Cascade (Haiku→Sonnet, `$2.26`) vs. Arm S Frontier (Sonnet 4.6, `$4.60`):** `+8.0pp` (`42/50` vs. `38/50`), **2.0× cheaper**, 88% T1 finish rate (44 T1 / 6 T2), paired McNemar `b=6, c=2` (two-sided `p=0.2891`).
- **Arm P2 3-Tier Cascade (`$8.88`) vs. Arm S Frontier (`$4.60`):** `+12.0pp` (`44/50` vs. `38/50`), paired McNemar `b=7, c=1` (two-sided `p=0.0703`, one-sided `p=0.0352`).
- **Arm P2 Fast (`52.3s` mean wall, `$0.26`, `50/50` PQ receipts) vs. Stock OpenShell (`54.8s` mean wall, `$0.17`, `0` receipts):** **`-4.6%` faster mean wall time**, `43/50` vs. `42/50` (paired McNemar `b=3, c=2, p=1.0000`), strictly Pareto-dominating Stock OpenShell on `(Solve Rate × Mean Wall Time × Governance)`.
- **Direct AtlasKV latency ablation (`P2 + AtlasKV` vs. `P2 Bare`, same model & governed microVM):** `-15.5%` mean wall time (`79.8s → 67.4s`), equal cost (`$0.28` vs. `$0.29`), paired McNemar `b=1, c=3` (`p=0.6250`).

### Apples-to-Apples `n=50` Polyglot with 100% Host-Attested Proxy (`claude-haiku-5-5`, identical `HERMES_DISABLE_LAZY_INSTALLS=1`)

| Configuration | Resolved | 95% Wilson CI | Paired McNemar | Host Proxy Cost | Mean Wall | Median Wall | Med Cmd | Host Ledgers | PQ Receipts |
|---|---:|---|---|---:|---:|---:|---:|---|---|
| Arm S (Stock OpenShell) | 41/50 (82.0%) | [69.2%, 90.2%] | baseline | $3.83 | 48.3s | 23.7s | — | 50/50 | — |
| **Arm P2 Warm Pool (Playbox + OpenShell)** | **40/50 (80.0%)** | [67.0%, 88.8%] | b=4, c=5 (p=1.0000) | $6.41 | 55.8s | 37.2s | 0.15s | **50/50 (100%)** | **50/50** |

- **Warm same-image burst (`ts-01..05`, n=5):** Arm P2 Warm Pool averaged **22.2s** vs. Stock OpenShell **56.1s** (**2.53× faster** when reusing a warm image microVM).

### True Same-Generation Hard-Slice Cascade (`n=18`, 9 Track-A Repos, 100% `--require-host-aiproxy`)

| Configuration | Resolved | 95% Wilson CI | Paired McNemar | Host Proxy Cost | Mean Wall | Median Wall | Tier Finishes | Host Ledgers | PQ Receipts |
|---|---:|---|---|---:|---:|---:|---|---|---|
| Arm S (Stock OpenShell, Sonnet 5.5) | 15/18 (83.3%) | [60.8%, 94.2%] | baseline | $3.27 | 28.4s | 16.6s | 18 direct | 18/18 | — |
| Arm P2 Cascade (Haiku 5.5 → Sonnet 5.5) | 13/18 (72.2%) | [49.1%, 87.5%] | b=0, c=2 (p=0.5000) | $5.09 | 131.4s | 70.2s | 12 T1 / 6 T2 | **18/18 (100%)** | **18/18** |

- **Finding:** Same-generation cascade (Haiku 5.5 -> Sonnet 5.5 without a fast local repro-gate) did NOT beat direct Sonnet 5.5 on solve rate, cost, or latency, confirming that the earlier 13/18 vs 6/18 lift was a model-generation effect (5.5 vs 4.6), not a cascade win.

### Stratified 9-Repo SWE-bench Verified Track-A Hard Slice (`n=18`, Pure Hermes + OpenShell 0.1.6)

2 tasks/repo across all 9 Track-A repositories (`astropy`, `django`, `matplotlib`, `xarray`, `pylint`, `pytest`, `scikit-learn`, `sphinx`, `sympy`):

| Configuration | Resolved | 95% Wilson CI | Paired vs. Arm S (McNemar) | Total Cost | Mean Wall | PQ Receipts |
|---|---:|---|---|---:|---:|---|
| Arm S (Stock OpenShell, Sonnet 4.6) | 6/18 (33.3%) | [16.3%, 56.2%] | baseline | $11.71 | 131.3s | — |
| Arm P2 (Playbox + OpenShell, Sonnet 4.6) | 6/18 (33.3%) | [16.3%, 56.2%] | parity | $23.25 | 275.4s | — |
| **Arm P2 Fast (Playbox + OpenShell, Haiku 5.5)** | **13/18 (72.2%)** | [49.1%, 87.5%] | **b=7, c=0 (p_2sided=0.0156, p_1sided=0.0078)** | **$0.12*** | **126.8s** | **18/18** |

\* 2 of 18 tasks hit the 600s worker timeout without flushing client usage JSON (`~$0.17` upper-bounded cost, still ~69× cheaper than Sonnet 4.6's `$11.71`). Disclosure: claude-haiku-5-5 is a newer model generation than claude-sonnet-4-6; this comparison proves cost/solve Pareto across model tiers on identical Hermes + OpenShell 0.1.6 infrastructure, not a same-model solve-rate lift.

### 100% Host-Attested AI Proxy Proof (`all_host_aiproxy_ledgers=true`, 7/7 Repos)

- **Provider route:** `google/gemini-3-flash-preview` via `playbox-aiproxy -> vertex` (`127.0.0.1:18791`), covering all 7 polyglot repositories (`django`, `flask`, `pytest`, `sympy`, `scikit-learn`, `rust`, `typescript`).
- **Attestation result:** **`7/7` (`100%`) host-attested LLM ledgers (`all_host_aiproxy_ledgers=True`)**, **`7/7` verified `Ed25519 + ML-DSA-65` receipts**, **`zero_taint_advisories=True`**, `3/7` (42.9%) solved, total cost `$0.80`, enforced fail-closed via `--require-host-aiproxy`.

### Per-Repository Breakdown (`n=50`)

| Repository | n | Arm S (Haiku) | Arm S (Sonnet) | P2 Bare | P2 + Atlas | P2 2-Tier | P2 3-Tier |
|---|---:|---:|---:|---:|---:|---:|---:|
| django | 28 | 25/28 | 22/28 | 26/28 | 25/28 | 25/28 | 26/28 |
| flask | 1 | 0/1 | 0/1 | 0/1 | 0/1 | 0/1 | 0/1 |
| pytest | 3 | 2/3 | 1/3 | 1/3 | 1/3 | 2/3 | 3/3 |
| rust | 5 | 5/5 | 4/5 | 5/5 | 5/5 | 5/5 | 5/5 |
| scikit-learn | 3 | 2/3 | 2/3 | 3/3 | 2/3 | 2/3 | 2/3 |
| sympy | 5 | 3/5 | 4/5 | 4/5 | 4/5 | 4/5 | 4/5 |
| typescript | 5 | 5/5 | 5/5 | 4/5 | 4/5 | 4/5 | 4/5 |

## 3. Front 4: Cross-Harness Replication (`Devin` + `Hermes`)

Stratified `n=15` polyglot subset covering **all 7 repositories** (3 Django, 1 Flask, 2 pytest, 2 Rust, 2 scikit-learn, 2 SymPy, 3 TypeScript) running `claude-haiku-5-5` under the **`devin`** harness on **NVIDIA OpenShell 0.1.6**:

| Arm (Devin Harness) | Resolved | 95% Wilson CI | Paired McNemar | Total Cost | Mean Wall/Task | Med Cmd | PQ Receipts |
|---|---:|---|---|---:|---:|---:|---|
| Arm S (Stock OpenShell + Devin) | 13/15 (86.7%) | [62.1%, 96.3%] | baseline | $0.455 | 21.2s | — | — |
| Arm P2 (Playbox + OpenShell + Devin) | 13/15 (86.7%) | [62.1%, 96.3%] | b=0, c=0 (p=1.0000) | $0.587 | 30.5s | 0.39s | 15/15 |

## 4. Additional Paired Slices (Full Disclosure)

- **Track-A Multi-Repo (`n=88`, same T1 model `claude-sonnet-4-6`):** Arm S `34/88` (38.6%, Wilson [29.1%, 49.1%], `$44.15`) vs. Arm P2 `35/88` (39.8%, Wilson [30.2%, 50.2%], `$107.79`). Paired McNemar: `n11=29, b=6, c=5, n00=48, p_2sided=1.0000`.
- **Django-only (`n=28`, model-confounded — disclosed for completeness):** Arm S (`gpt-5.6-luna`) `22/28` (78.6%, Wilson [60.5%, 89.8%]) vs. Arm P2 (`swe-2-high` + T3) `28/28` (100.0%, Wilson [87.9%, 100.0%]). Paired McNemar `p_2sided=0.0312`, but **not** attributable to OpenShell or Playbox governance because T1 models differed.

## 5. Scoped Upstream Strategy for `NVIDIA/OpenShell`

Playbox's orchestration, AtlasKV indexing, and Hermes parity harness belong in Playbox — **not** in an upstream OpenShell PR. Genuine upstream issues/PRs for `NVIDIA/OpenShell` are strictly scoped to:
1. **Default Landlock device policy:** missing `/dev/null` (RW), `/dev/zero` (RO), `/dev/urandom` (RO).
2. **VM-driver resource-limit enforcement:** CPU, memory, and timeout limits currently ignored by the VM driver.
3. **Optional docs/ecosystem entry** (after maintainer discussion) listing Playbox as a governed runner integration.
