# Steering rooms: activation steering as a Playbox use case

Status (2026-10-29): steering hosting, grants, receipts, guards, the MCP and CLI surfaces and an E1 harness are built
and tested (section 6); the evaluator loop, transfer, detection and recipe import are not. One experiment has been run, on
GPT-2, with a result that is real but narrow (section 4). Research claims below come from abstracts and search summaries, not from full-text
reads, unless marked "read". Verify before citing.

## 1. Pitch

A steering room is a Playbox room where the shared resource is a set of **open-weights models and the
interventions applied to their activations**, not just code. Members propose steering specs (which model,
which layer, which direction, how strong). A host runs them in a sandbox and returns text plus a signed
receipt of exactly what was applied. Agents read each other's outputs, and the room measures what spreads.

Playbox already has the hard parts of a trustworthy experiment platform: grants that cap authority, an
attested policy, model receipts, a signed evaluator with a sealed holdout, variant lineage, and shared
memory with provenance. Steering adds a new kind of authority (editing a model's internals) that needs
exactly those controls.

## 2. What the research says (and what it means for the design)

| Finding | Source | Design consequence |
|---|---|---|
| Difference-of-means vectors from contrastive prompts shift behavior (ActAdd, CAA, RepE). | [RepE survey](https://arxiv.org/abs/2502.17601) | Default vector recipe is diff-of-means; cheap, reproducible, hashable. |
| Refusal is mediated by one direction in 13 chat models up to 72B; ablating it jailbreaks, adding it induces refusal. | [Arditi et al.](https://arxiv.org/abs/2406.11717) | Safety-relevant directions are dual-use. Grants must be able to deny them by class. |
| Persona traits (evil, sycophancy, hallucination) have extractable vectors that can monitor drift and flag training data; preventive steering costs little MMLU, post-hoc correction costs more. | [Persona vectors](https://arxiv.org/pdf/2507.21509) | Probing (read-only) is a separate, cheaper capability than steering (write). Monitoring is a first-class room feature. |
| Prompting beats SAE steering and most representation methods on steering; SAEs are not competitive. Supervised methods (DiffMean, probes, ReFT-r1) win at concept *detection* (ROC AUC ~0.94-0.95). Tested on Gemma-2 2B/9B. | [AxBench](https://arxiv.org/abs/2501.17148) | Every experiment needs a **prompting baseline arm**. Use SAEs for explanation, not as the assumed best control. |
| Steerability varies a lot per input, depends on the dataset, and vectors are often brittle to reasonable prompt changes. | [Tan et al.](https://arxiv.org/abs/2407.12404) | Report per-input effect distributions, not means. Evaluate on prompt paraphrases. Receipts must record the prompt-set hash. |
| Open question: coherence and side effects of steering are rarely measured; 2026 work starts on forecasting side effects and angle/norm geometry. | [coherence](https://arxiv.org/pdf/2604.08169), [side effects](https://arxiv.org/pdf/2608.11227), [geometry](https://arxiv.org/pdf/2606.06735) (snippets only) | Evaluator scores include a coherence/capability guard, not just the target trait. |
| Concept directions can be mapped across models with a linear map fit by least squares; small-to-large transfer works. Newer work claims transfer across architectures and tokenizers. | [Huang et al., ACL 2025](https://arxiv.org/abs/2501.02009), [2606.05290](https://arxiv.org/html/2606.05290v1) (abstract only) | Enables "peer B tries A's direction" experiments. Treat transfer claims as hypotheses to reproduce. |
| Agents can pass activations instead of text, up to 27% better at under a quarter of the compute; latent channels in multi-agent systems are an attack surface. | [Communicating Activations](https://arxiv.org/abs/2501.14082), [latent attack](https://arxiv.org/pdf/2605.28214) (abstract only) | Out of scope for v1 (text-only channel), but the receipt format should not preclude a latent channel. Flag as a threat. |
| Traits transfer through data that looks unrelated, but only when teacher and student share a base model. | [Subliminal learning](https://arxiv.org/abs/2507.14805) | Propagation experiments must control for shared vs different base models. Real weight-level contagion needs fine-tuning, which is out of v1. |
| Narrow fine-tuning can cause broad misalignment; a benign stated motivation prevented it. | [Betley et al.](https://proceedings.mlr.press/v267/betley25a.html) | Motivates probing for off-target shifts after any update step. |
| Clamping an SAE feature steers a frontier model (Golden Gate Claude). | [Scaling Monosemanticity](https://transformer-circuits.pub/2024/scaling-monosemanticity/) | The demo that makes this legible to non-researchers. |
| Gemma Scope: 400+ open SAEs on Gemma 2 2B/9B/27B, with Neuronpedia for browsing. nnsight runs the same code locally or on NDIF for 70B+. | [Gemma Scope](https://arxiv.org/abs/2408.05147), nnsight/NDIF (docs not read) | Tooling for v1: Gemma-2 2B/9B plus Gemma Scope, nnsight or plain PyTorch hooks. |

Not found in my searches, so treated as a real gap and an opportunity: **any direct measurement of whether a
steering vector applied to one agent shifts another agent that only reads its text.** That is the headline
experiment in section 4.

## 2b. What your own repos already contribute

Read from `~/Documents/GitHub` on 2026-10-29 (READMEs and result docs, not code). All of these are your
bounded, internal results; none is independent evidence. They are cited as design inputs, not as proof.

| Local work | What it found or built | Use in a steering room |
|---|---|---|
| `theseus-lab` | In GPT-2, a function-preserving MLP permutation changed 100% of selected positions while top-1 behavior held (rel. logit error 3e-7). Same-label ablation looked unrelated (cos -0.983); the inverse-mapped ablation matched (0.9999996). Positions are not causal identity. | **A steering direction is only meaningful relative to a basis.** The SteerSpec must pin the weights manifest and, for transfer (E3), record the map used. A receipt that says "layer 12, direction X" is wrong after a permutation. Add a permutation-invariance test to S1: re-express a vector under a function-preserving rewrite and require the same behavioral effect. |
| `theseus-lab` | Behavior can stay fixed while mechanism changes; visible-probe equivalence can be a probe-coverage artifact; random and zero blocks kept 100% task accuracy in V31, so a target wasn't behaviorally indispensable. | Every E1/E4 metric needs a **held-out probe set and a null-intervention control** (random direction of matched norm). If a random vector gives the same score, the metric is not measuring steering. |
| `theseus-lab` | A lexical refusal proxy matched 145/939 responses (15.4%) against a 90% target; a second assessor scored 53.6% safe and flagged 62% of benign prompts unsafe; no calibrated verdict. | Do not use a keyword judge for the trait-contagion metric in E1. Use activation projections plus at least two independent judges, and report their disagreement. |
| `interpretability-lab-sandbox` | A staged gate list for real models: native vs hooked parity, deterministic repeat, exact no-op patch, nonzero intervention reach, SAE reconstruction, feature repeat stability, causal-scrubbing reach, event replay, custody verification. Ran on GPT-2 (layer-6 SAE) and Gemma 3 27B with a Gemma Scope 2 transcoder (layer 31). V9 passed all eight gates (e.g. pooled FVU 0.0141, no-op logit delta 0). The earlier MLP-out slice is kept as negative evidence. | **Reuse the gate list as the `steer` task's pre-flight.** A steer task must pass parity, no-op (zero coefficient changes nothing), and reach (nonzero coefficient changes something) before any science run. This gives S2 a concrete acceptance test and already names a working Gemma Scope 2 path. |
| `interpretability-lab-sandbox` | Packet-bound Ed25519 receipts for externally validated runs; internal runs are labelled `LOCAL_INTERNAL_EXPLORATORY_ONLY`; aggregate-only reports; raw activations deleted within 72 hours. | Matches Playbox receipts and the evidence ceilings. Adopt the same label vocabulary on `SteerReceipt` so a result states its own ceiling. Activation dumps stay out of the room by default. |
| `aligned-holistic-continual-learning` and `continual-alignment-lab` | A fixed causal-feature monitor tested for predicting what an update does to competence, calibration and policy compliance, before shipping; matched-progress controls to rule out "less harm because less learning"; shuffled-label controls; so far only synthetic qualification, no model-backed result. | The persona/trait probes in E2 are this monitor applied to peers. Reuse their controls: shuffled-label and matched-progress baselines. Their model-backed experiment is not run, so steering rooms could supply the first real data, which is also a reason to be cautious about reusing their claims. |
| `smart-rsi` | Loop of experience, memory proposal, verification, commit, candidate delta, shadow evaluation, canary, then promote/freeze/rollback; base model immutable; content-addressed snapshots; the refinery is the only mutation authority. | The E4 loop is this shape with a SteerSpec as the candidate delta. Reuse shadow-then-canary before a spec is accepted into room memory, and rollback by content address. This is the nearest thing locally to a real self-improvement loop. |
| `rlm-continual-harness`, `recursive-meta-harness` | Self-improvement only through evidence-backed, journaled, retention-checked, ceiling-capped refinements; selection among full agent configurations with costs and hard constraints. | Treat a steering config as one more component in the configuration being selected. Use their retention check: a new spec must not degrade earlier accepted behavior. Cap claims by evidence, as they do. |
| `mesh-intelligence` (with `latent-mesh/LatentMAS`) | A CPU-only LatentMAS sidecar in Docker, built for hosts with no GPU passthrough, with tests (`test_latentmas_agent_mesh.py`). LatentMAS is the latent-collaboration approach for multi-agent systems ([arXiv 2511.20639](https://arxiv.org/abs/2511.20639), not read). | Two uses. First, it is a ready CPU container pattern for S2, which answers the GPU question for a Gemma-2 2B demo. Second, it is the existing latent-channel baseline: the "latent channels between agents" threat in section 5 is something you could test locally rather than only warn about. |
| `sentience-lab-sandbox` | Simulator-first study runners with competence-isolated evaluators and runtime-error controls. | Separate "the model got worse" from "the trait moved": the competence-isolated evaluator idea is the coherence guard in section 3.5. |
| Prior orchestration in Playbox (`orchestrator/`) | LangGraph fan-out over A2A with a 3-way patch merge. | E1's A→B→C chain is a graph over the same transport. |

Net effect on the plan: S1 gains a permutation-invariance test; S2 gains a gate list and a CPU container
pattern; E1 gains null-vector and two-judge controls; E4 gains a shadow/canary/rollback loop.

Gaps in the local work that matter here: nothing in these repos runs activation *steering* itself (they
measure features, reconstruction and replacement); the model-backed monitor experiments are closed or
unrun; and every result is internal, so the research table in section 2 remains the only external evidence.

## 2c. Neuron-level steering in the wild: neuronary and lyramakesmusic's repos

Read on 2026-10-29 from the repo pages and READMEs via a page summarizer (not from source, and not run).
`neuronary.org` itself only rendered a loading shell, so everything about it comes from its GitHub repo.
Treat details as claims to verify. These are the closest public precedents to what a steering room does.

| Source | What it is | What it means here |
|---|---|---|
| [`lyramakesmusic/neuronary`](https://github.com/lyramakesmusic/neuronary) (neuronary.org) | A public dictionary of **neuron steering recipes**. A recipe is a set of MLP neurons with signed doses, written `L13N7956-1.0 L14N8949+1.0` (layer, neuron, dose) for `Qwen/Qwen3.5-4B`. The edit is applied at the input of that layer's `mlp.down_proj`; default `op: "add"` changes the neuron's activation by `dose × median residual-stream norm at the layer ÷ L2 norm of the neuron's down-projection column`, so dose 1 moves the residual stream about one median residual norm along the neuron's output direction. An `op: "mul"` and MoE-expert neurons (`L12E12N1234+0.6`) exist in the schema; the repo says expert neurons are not applied yet. Recipes are JSON (`schema/recipe.schema.json`, not read) with `examples`; submission by site login, pull request, or issue form; Supabase backend with votes, comments, moderation and rate limits; an MCP endpoint (`search_recipes`, `get_recipe`, `publish_recipe`, `update_recipe`, `comment`, `vote`) with `nry_` bearer tokens; a Python client; and a Claude skill file. | This is the user-facing shape of the idea: **people share small, named, dose-based steering edits and agents can read and publish them.** A Playbox steering room is the private, receipted version. See "Mapping" below. |
| [`lyramakesmusic/neuron-explorer`](https://github.com/lyramakesmusic/neuron-explorer) (MIT, ~1.6k stars) | A steering-first explorer for every MLP neuron in Qwen3.5-4B. It pushes each neuron up and down on a few prompts, keeps every rollout, and scores neurons automatically (distinctive-word ranking and word-shift scores each reported as a percentile against **random-direction controls**; next-token Δlogprob and KL per ±0.8 push in `steer_lens.py`; per-layer reports of the share of neurons above control). Doses are fractions of the median residual norm leaving the layer. Sweeps run on a GPU pod through a steering fork of vLLM (`RhizoNymph/vllm`), with `check_equivalence.py` comparing the fork's steering and capture against Hugging Face. Outputs are JSONL rollouts with logprobs, ranked units, and per-layer stats. A "Claude" tab runs the `claude` CLI with the explorer as an MCP server so an agent can read neurons and run the live model. Findings are in `research_log.md` (not read). | Three direct borrowings: (1) **control percentiles** are exactly the null-vector control already required for E1, now with a concrete reporting form; (2) **Δlogprob and KL** give a cheap, text-free side-effect metric for the coherence guard; (3) **a backend-equivalence check** (steering fork versus plain Hugging Face) is the right parity gate once S5 adds a faster GPU path. The agent-over-MCP pattern is the model for a `playbox_steer` tool. |
| [`lyramakesmusic/cardinal-harness`](https://github.com/lyramakesmusic/cardinal-harness) (Rust, MIT, early) | An LLM-as-judge framework that asks **pairwise ratio questions** ("how many times more *attribute* does A have than B?") from a fixed ratio ladder, then fits one consistent score per item with robust IRLS (Huber loss), reports a mean and standard deviation per item, picks the next pair by information gain, and stops when the top-K set is confident. CLI includes an `eval-likert` baseline. The repo states logprob extraction and cache-aware cost modeling are planned, and asserts an O(n) comparison count without a derivation I saw. | Answers the open question in section 7 about E1's judge. Absolute 1-10 ratings are the weak point `theseus-lab` already found in its keyword and assessor judges; pairwise ratio judgments with uncertainty give each hop a **trait score with an error bar**, and a disagreement between two judge models becomes a measured quantity. Early software: use it as a design reference or a baseline judge, and check its claims on our data. |
| Other repos on the same profile (descriptions only; not opened) | `bread-slicer` (an SAE trained on a small model, currently static visualization hosting), `activations-vis` and `expert-activations-vis`, `cursed-samplers` (samplers built on llama.cpp hooks), loom tools (`webloom`, `wool`, `loom-mcp`, `self-loom-webui`), `neuralese-leaker` (chat with OpenRouter models and view their reasoning), `llm-survivor` and `mtgsim_for_agents` (models playing each other). | Lower confidence. Loom-style branching is a natural way to display E1 rollouts as a tree and compare branches; `neuralese-leaker` and the SAE visualizers are possible display references. No design depends on them. |

### Mapping: a neuron recipe is a SteerSpec

A neuronary edit at the input of `down_proj` is, in the residual basis, an **add at `mlp_out`** along a fixed
direction: the neuron's down-projection column. The recipe's dose is a strength in units of a residual-stream
norm. So `L13N7956-1.0` is, for the pinned weights:

```
site mlp_out · layers [13] · mode add · direction = unit(W_down[:, 7956] at layer 13) · tokens all · strength -1.0 norms
```

That fits the format built in S1 with no new site. Three differences are real and must not be glossed over:

1. **Strength unit.** Neuronary's unit is the *median residual-stream norm at the layer*, from a fixed
   calibration text. S1's unit is the *mean norm of the site's own activations over the prompt set*. They
   differ by a factor and by the sampled text, so a dose cannot be copied into `coef_milli`. Plan: add a `scale`
   field to the spec (`site_mean`, the current behavior, or `resid_median`, neuronary's) so a receipt says
   which unit it used. Until then, an importer would have to recompute the strength and say so. (Not built.)
2. **One spec, one direction.** A recipe usually names several neurons across layers; a SteerSpec has one
   direction. Plan: a `SteerPlan`, an ordered list of specs applied together in one run, with a plan digest the
   receipt binds. (Not built.) Neurons in the same layer also have different directions, so they stay separate
   specs inside one plan.
3. **`mul` and expert neurons.** A multiplicative edit scales an activation instead of adding along a
   direction, so it is not a SteerSpec mode. Not supported in v1; an import containing one is refused.

Neuron indices are only meaningful for exact weights. Neuronary names the model (`Qwen/Qwen3.5-4B`); a steering
room pins the weights manifest hash, so a recipe cannot silently land on a different revision. This is the
`theseus-lab` result applied: positions are not identity, weights plus position are.

### What changes in the plan because of these sources

- **Import is a new, untrusted input.** A public recipe site accepts user submissions; its moderation is not
  our trust anchor. Imported recipes enter shared memory as `Untrusted` and quarantined until a pinned reviewer
  accepts them, like any outside text. Publishing back to neuronary is an outward action and needs a person's
  approval through the admission broker. A `nry_` token is a credential: it stays out of logs and out of the
  agent's reach, as the broker already requires for approver keys.
- **Judge for E1 (built, in a paired form).** `judge/` scores E1's texts with a trained sentiment classifier and a
  local pairwise judge whose A/B log-odds are averaged over both orders. It borrows cardinal-harness's idea
  (pairwise comparisons, with an interval) but not its latent-score fit or its ratio ladder: E1 already pairs every
  steered text with the clean text for the same seed, so each pair is one paired observation. Section 4 has the result.
- **Controls.** Report each steered result as a percentile against a population of random unit directions of
  matched strength (neuron-explorer's form), not only against one null vector.
- **Side-effect metric.** Add next-token KL and Δlogprob (steered versus clean on the prompt set) to the
  evaluator's capability guard. They need no judge and are cheap.
- **Model choice.** Neuronary and neuron-explorer both target Qwen3.5-4B and require `transformers>=5`. The
  engine currently pins `transformers==4.46.3` and has only been run on GPT-2 and a Llama-style layout, so
  moving to Qwen3.5-4B means a version bump and checking its block layout (not verified). Gemma-2 2B stays the
  planned small-model baseline because of Gemma Scope.
- **Scale.** Sweeping every neuron (neuron-explorer's workload) is a GPU job through a faster backend. A room
  should offer *targeted* neuron edits first and leave sweeps to S5.

## 2d. SwaT: a measured latent channel (read 2026-10-29)

Source: [SwaT, arXiv 2610.15095](https://arxiv.org/abs/2610.15095) and [Ar-Kareem/SwaT](https://github.com/Ar-Kareem/SwaT).
I read the paper's main body, tables and references, and skimmed the repo. I did not run the code or read the
appendix. Numbers below are the authors' claims.

What it is: a trained mapper (about 300M parameters, under 50M tokens, under one GPU-day per pair) that turns one
frozen model's KV-cache into another family's, so the receiver decodes without reading the text. Across 10
cross-family pairs (3B to 32B) the receiver keeps over 90% of its own score. Cross-tokenizer positions are aligned
through character spans (their eq. 12). Behaviours seen in the sender, such as vision or Chinese ability, partly
carry over though training used English text only. Limits they state: one mapper per ordered pair, one handoff
rather than repeated ones, and the transferred cache "may contain information beyond what is explicit in the
text", with auditability left as future work. The repo has no license and ships no results for its
`scripts/interp_security/` experiment, which compares refusal behaviour for the sender, the receiver and the
receiver reading the transferred cache.

| What SwaT offers | Where it fits here | Limits |
|---|---|---|
| A real cross-family latent channel to measure | **E6** below: tests the threat row "latent channels bypass text-level checks" instead of only listing it | Needs a GPU to train a mapper and open weights on both ends; this engine is CPU-only and has been run on GPT-2 and a Llama-style layout, and SwaT was not shown on Gemma-2 2B |
| Cross-tokenizer alignment by character spans | A starting point for **E3** across families | It maps KV-caches, not the residual-stream directions a `SteerSpec` edits, so it does not replace the linear direction map |
| A refusal-direction analysis across sender, receiver and rendered cache (code only, no published results) | Method reference for **E5** and for guard classes: does a latent channel carry or strip a safety direction | Unverified until someone runs it |
| A mapper is valid for exactly one pair of weights | Pin it by both weights-manifest hashes, as the `theseus-lab` result in section 2b requires | None |

Cheaper first step: section 2b notes `mesh-intelligence` already has a CPU LatentMAS sidecar. Run E6 there first;
use SwaT only if cross-family or no-prefill transfer is needed. The repo has no license, so we would ask the
authors for one or reimplement from the paper; do not copy its code.

## 3. Architecture (as built)

```
member agent ──A2A──▶ host ──▶ engine container (CPU, no network, weights read-only)
   │  signed SteerSpec            │  forward hooks on a hosted model
   │  + direction file            ▼
   │  + prompt set          steered and clean text, gate results, KL, probe projections
   ◀──── output + SteerReceipt ───┘  (receipt signed by the host key)
        │
        └─▶ orchestrator (LangGraph): E1 chain, arms, statistics
```

Reused as is: invites, Biscuit grants, the `Runner` trait, the A2A task and artifact model, the audit log, the
admission broker. New: `Op::Steer`, `SteerSpec` and `SteerReceipt` (core), `SteerCaps` grant facts, `SteerHost`
(weights store, guards, budgets), read-only mounts and image digests on runners, the Python engine in `steer/`,
the `playbox_steer` MCP tool, `playbox up --steer-store`, and the E1 harness in `orchestrator/`.

### 3.1 SteerSpec (`crates/playbox-core/src/steer.rs`)

A signed record (`playbox.steer-spec.v1`) of exactly one intervention: model label, **weights manifest hash**,
site, layers, **direction hash**, recipe label, mode, strength, token scope, **prompt-set hash**, seed. The digest
ignores who signed it. Strength is an integer in thousandths of a norm (`coef_milli`), so each spec has one text
form. The sites are `resid_post` (the block output), `attn_out` and `mlp_out`, all in the residual basis.

What the numbers mean (implemented in `steer/playbox_steer/engine.py`, and tested):

- **Unit.** 1.0 is the mean L2 norm of the site's activations over the prompt set's non-first token positions in a
  clean pass, per layer. (Neuronary's unit is different; see section 2c.)
- **Modes.** `add`: `h += s·d`. `clamp`: the component along `d` becomes `s`. `ablate`: the component along `d` is
  removed (strength must be 0). `d` is a unit vector; the file must be unit norm within 1e-3 or it is refused.
- **Scope.** `prompt` edits the prompt's positions during the prefill pass; `generated` edits every position after
  it, one decode step at a time; `all` does both. (`generated` leaves the first generated token's own logits
  alone, because those come from the prompt pass.)
- **Decoding.** Greedy unless the prompt set sets a temperature; sampling is seeded from the spec seed plus the
  prompt index. Prompts run one at a time with no padding, one thread, deterministic algorithms: the same request
  gives the same bytes (tested, including inside the Docker image).

### 3.2 Grants (`crates/playbox-net/src/grants.rs`)

Steering rights are Biscuit facts and checks in the member's grant, absent unless the host issues them.
`SteerCaps`: allowed weights hashes, layer range, largest strength (by magnitude), allowed modes, optional
direction allow-list, tokens per run, guard classes the member may steer along, whether probing is allowed, and a
run and total-token budget. Over-limit requests are refused, not clamped.

- **Enforced by the grant:** model, layer range, strength, mode, direction, tokens per run, the right to probe.
  Every check is scoped to steering operations, so a steering-only narrowing leaves `echo`, `pull`, `run` alone.
- **Enforced by the host, from what it issued:** the run and total-token budget. A member's own narrowing of the
  budget is not enforced (it cannot be counted in a token); the host's ceiling is.
- **Narrowing offline:** `grants::steer_narrowing(layer_max, max_coef, modes, max_tokens)` produces the block.
  Lean proves narrowing cannot widen (`lean/Playbox/Steer.lean`, section 6).
- **Defaults** (CLI and MCP): modes `add` and `clamp` only, so `ablate` is off unless the host adds it; strength
  up to 4 norms; no guard classes.
- **Guards.** A host can list known safety directions per model in `<store>/guards/<weights hash>/guards.json`
  (class, site, layer, vector file). A spec at that site and layer whose direction has |cosine| ≥ 0.5 with a guard
  is refused with exit code 3 unless the member's grant names that class. Probing is not guarded (it edits
  nothing). This catches directions near a known one; it does not recognize a novel safety direction.
- **Startup check.** The grant's task caps come from the host's `Limits`. A steering host whose needs exceed them
  would answer every request "not authorized", so it refuses to start instead.

### 3.3 Runner

The engine runs in the pinned image (`steer/Dockerfile`, CPU-only torch, `transformers==4.46.3`) under the Docker
runner: no network, dropped capabilities, read-only root, memory/CPU/time limits, and the weights (and guards)
bind-mounted **read-only** at fixed paths the host chooses. Mounts are never part of a peer's `TaskSpec`, so a
member cannot name a host path. The host recomputes every weights directory's hash when it opens the store and
refuses to start on a mismatch. The image must be on the runner's allow-list; the receipt names its content digest
(`docker image inspect` ID). **There is no GPU path.** The Docker image is CPU-only; GPU passthrough and GPU
confidential computing are not tested here.

### 3.4 Receipts

`SteerReceipt` (`playbox.steer-receipt.v1`) is signed by the host's endpoint key and binds the spec digest, weights,
prompt set, image digest, a request hash (the spec plus `kind run|probe`), the output hash, a time and a **stated
ceiling** (`host_claim`, `local_internal_exploratory`, `attested`). The client refuses a receipt that is not signed
by the host it is connected to or does not match its spec and the exact output bytes. The ceiling is whatever the
operator configured; the CLI and MCP default to `local_internal_exploratory`. Nothing yet raises it to `attested`
automatically, so no result should be quoted above its stated ceiling.

### 3.5 Evaluator and lineage (the "RSI" loop)

The search-over-configurations loop is **not wired up yet**. What exists: the per-run outputs it would score (text,
KL to the clean model, probe projections), signed receipts to cite as evidence, and the E1 harness. The existing
`evaluator` and `Lineage` types in `playbox-core` are unchanged and untried on steering specs. Honest label stays:
this would be automated configuration search with a fixed evaluator, and becomes self-improvement only if the loop
also updates something persistent.

### 3.6 Taint and memory

Model output is untrusted text. The MCP tool registers it with the taint tracker as untrusted. `playbox_steer` is a
`Write`-effect action needing `Reviewed` text: prompts copied from another member's output need a person's
approval before they are run. Recipes pulled from a public site (section 2c) are not wired in; when they are, they
enter shared memory quarantined.

### 3.7 Surfaces

- **Agents:** the `playbox_steer` MCP tool (steer or probe) and `steer_store`, `steer_image`, `steer_mem_mib`,
  `steer_timeout_secs`, `steer_caps` and `grant_ttl_secs` on `playbox_create`.
- **Humans:** `playbox up --runtime docker --steer-store DIR [--steer-image IMG] [--steer-mem-mib N]`.
- **Making directions:** `python -m playbox_steer extract|random|manifest` (see `steer/playbox_steer/__main__.py`).
  Extraction is difference-of-means of last-token states between two prompt groups.
- **Experiments:** `orchestrator/playbox_orchestrator/steer_chain.py` (LangGraph chain, arms, statistics) driving a
  sidecar through `mcp.py`; `orchestrator/experiments/e1_gpt2.py` runs it on real GPT-2 through Docker.

## 4. Experiments

Each reports a paired difference against a clean arm, a prompting-only baseline, random-direction controls, and a
side-effect metric. Status first, then the one result so far.

| | Status |
|---|---|
| **E1** contagion across a chain | Harness built; run on real GPT-2 at two strengths with 64 seeds and 20 random-direction controls (below). The steered direction beats all controls and the prompting baseline on its own projection measure, and an independent classifier and a pairwise judge agree it reads as more positive (strongest at strength 0.2). A second trait (formality) shows the same pattern against random directions but not against prompting. |
| **E2** detectability | The read-only probe operation exists; no detector or baseline classifier is built. |
| **E3** transfer across models | Not built. |
| **E4** config-search lineage | Not built (section 3.5). |
| **E5** safety-direction handling | The guard and default-deny behavior are built and tested (section 5); no research claim is made. |

### E1: does a steered trait spread to agents that only read its text?

**Setup (what ran).** GPT-2 small (124M) hosted in a Docker steering room; a sentiment direction from
difference-of-means over 12 positive and 12 negative sentences at layer 6 (`resid_post`); **64** neutral seed
prompts (a fixed shuffle of 8 frames × 8 subjects); 24 new tokens, greedy; three hops (A steered, B reads A's text
and continues it, C reads B's). Arms: `clean`; `steered` (the sentiment direction at strength s); `prompted` (a
"write something very positive" prefix on hop 0, no steering); and **20 `null` arms** (random unit directions,
seeds 100-119, same strength). Strengths 0.2 and 0.4 norms. The measure is the projection of each hop's text onto
the sentiment direction at layer 6, in units of the layer's activation norm, as a paired difference against `clean`
with a 95% bootstrap interval over seeds. KL is the mean per-token KL(steered ‖ clean) along the steered text. The
runs were split across 6 parallel shards and merged; the merge checks that every shard's `clean` arm is identical,
which it was, including a shard that had to be rerun. Every run carries a host receipt digest. Reports with every
text: [strength 0.2](e1-gpt2-64seeds-coef0.2.json), [strength 0.4](e1-gpt2-64seeds-coef0.4.json). Reproduce with
`orchestrator/experiments/e1_gpt2.py --shard K/6` and `merge_e1.py`.

| Effect vs clean (hop 0 → hop 2) | strength 0.2 | strength 0.4 |
|---|---|---|
| **steered** | +0.034 → +0.027 [hop 0: +0.026, +0.043] | +0.040 → +0.035 [hop 0: +0.031, +0.049] |
| KL of the steering edit (nats/token) | 0.15 | 0.53 |
| **prompted** (same in both columns) | +0.020 → +0.015 [hop 0: +0.013, +0.027] | |
| 20 random directions: mean ± sd | +0.003 ± 0.006 (range −0.010 to +0.017) | +0.003 ± 0.009 (range −0.014 to +0.019) |
| steered vs the 20 nulls, hop 0 / 1 / 2 | above all 20; z = +5.5 / +4.8 / +5.1 | above all 20; z = +4.1 / +4.1 / +4.3 |
| steered − prompted, paired, hop 0 | +0.014 [+0.006, +0.022] | +0.020 [+0.011, +0.029] |
| steered − prompted, paired, hop 2 | +0.012 [+0.003, +0.021] | +0.020 [+0.009, +0.031] |
| share of hop-0 effect still present at hop 2: steered / prompted | 78% / 74% | 87% / 74% |

**What this shows (on this model, trait, layer and extraction set).**
1. A steered sentiment direction raises the sentiment projection of the generated text, and does so more than any of
   20 random directions of the same strength; the 0.4 run's z of about +4 and the 0.2 run's of about +5 hold at all
   three hops.
2. It is also measurably larger than the prompting baseline, by a paired difference whose interval excludes zero at
   both strengths and at hop 2. This is a different reading from the first, smaller run (16 seeds, 3 nulls), which
   could not separate them; that run was too small to see it.
3. About 80-90% of the hop-0 effect is still there two hops later in agents that only read the text and are not
   steered. The prompting baseline keeps about three quarters of its smaller effect.

**What it does not show.**
- **That the projection alone means the text got more positive.** The projection is onto the direction that was
  used to steer, so on its own it could rise without the text changing in a way a reader sees. The behavioral check
  below is what addresses this.
- **That persistence is "contagion" rather than copying.** GPT-2 continuing a text tends to repeat it (see the
  texts in the reports). The prompted arm persists too. What the data support is that more of the steered effect
  survives than of the prompted effect, not why.
- **Anything about other traits, layers, models or extraction sets.** One trait, one layer (6), GPT-2 small, one
  set of 24 sentences, one run per setting. No correction was made for two strengths and several comparisons.
- **That steering is cheap in side effects.** The KL at 0.4 (0.53 nats/token) is large; random directions at the
  same strength have a mean KL of 0.24, steered 0.53.
- **That random directions are inert.** 7 of 20 had a hop-0 interval excluding zero at 0.2 and 12 of 20 at 0.4,
  in both signs; that spread is why the null population, not a single null, is the right comparison.

**What would extend it.** Other traits and layers; several extraction sets for the same trait; a larger model that
does not loop; and measuring at a layer other than the one the direction was extracted from.

#### Behavioral check: do the texts read as more positive?

Two measures that are not the steering direction, both checked first on 24 held-out gold pairs (more versus less
positive sentences, none from the extraction set): both scored 24/24 (the script refuses to run below 95% for the
classifier and 90% for the judge).

- **Classifier:** `siebert/sentiment-roberta-large-english`; score is its positive-minus-negative logit, so +4 is a
  large shift on a scale that saturates near ±5.
- **Pairwise judge:** `Qwen/Qwen3-1.7B-Base`, asked which of two texts is more positive (two worked examples, both
  orders averaged so position bias cancels; score is the log-odds that the steered text wins).

Each steered, prompted or null text is compared with the `clean` text for the same seed and hop. Reports with
per-seed values: [0.2](e1-gpt2-64seeds-coef0.2.judged.json), [0.4](e1-gpt2-64seeds-coef0.4.judged.json); run
`judge/experiments/judge_e1.py REPORT OUT`.

| Effect vs clean, hop 0 → hop 2 | strength 0.2 | strength 0.4 |
|---|---|---|
| classifier, **steered** | +4.05 → +3.06 (hop 0 [+2.70, +5.40]) | +3.99 → +4.03 (hop 0 [+2.67, +5.38]) |
| classifier, prompted | +1.62 → −0.02 (hop 0 [−0.02, +3.24]) | same arm |
| classifier, 20 random directions | mean +0.05 ± 1.5 | mean −0.09 ± 2.4 |
| classifier, steered vs nulls (z, hop 0 / 2) | +2.7 / +3.0, above all 20 | +1.7 / +2.5, above all 20 |
| judge log-odds, **steered** | +0.90 → +0.40 (hop 0 [+0.67, +1.13]) | +0.83 → +0.40 (hop 0 [+0.61, +1.05]) |
| judge, prompted | +0.56 → +0.21 | same arm |
| judge, 20 random directions | mean +0.06 ± 0.23 | mean +0.02 ± 0.35 |
| judge, steered vs nulls (z, hop 0 / 2) | +3.7 / +2.6, above all 20 | +2.3 / +2.4, above 95-100% of nulls |
| steered − prompted, paired, classifier, hop 0 / 2 | +2.4 [+1.3, +3.7] / +3.1 [+1.4, +4.8] | +2.4 [+1.1, +3.6] / +4.1 [+2.5, +5.6] |
| steered − prompted, paired, judge, hop 0 / 2 | +0.34 [+0.08, +0.60] / +0.18 [−0.08, +0.45] | +0.27 [−0.01, +0.54] / +0.19 [−0.10, +0.48] |
| texts where steered beats clean, classifier (hop 0 / 2) | 88% / 73% | 89% / 80% |

Agreement between the two measures across all 4,000+ texts per report: Spearman 0.69 (0.2) and 0.70 (0.4).
Across the 22 arms, the projection used earlier tracks both behavioral measures: Spearman 0.74 to 0.95 against the
classifier and 0.90 to 0.96 against the judge, at every hop and both strengths. So the projection was a fair stand-in
for behavior in this experiment, and random directions that happened to raise it also raised the behavioral
measures.

**What the behavioral check adds, and where it is thin.**
- An independent classifier and a separate judge both rate steered text as more positive than clean text, and more
  than the texts from every one of the 20 random directions (classifier at both strengths; judge at 0.2, and 19 of
  20 at hops 1 and 2 at 0.4), with the effect still present two hops on. The projection result was not a quirk of
  measuring with the steering direction.
- The z-scores are lower than the projection's (about +2 to +3.7 against +4 to +5.5) and the random directions are
  noisier on these measures, especially at 0.4. The evidence is strongest at 0.2.
- Against the prompting baseline, the classifier separates them clearly at every hop and strength. The judge
  separates them only at hop 0 for 0.2; its other intervals include zero. The prompted arm's classifier effect is
  itself not distinguishable from zero.
- The judge is a 1.7B base model and the classifier was trained on review-style text; both did perfectly on
  24 clean pairs but GPT-2's text is stranger than that. Their 0.69 to 0.70 agreement says they are measuring
  related things, not that either is a ground truth.
- Still one model, one trait, one layer, one extraction set. No human rated anything.

#### A second trait: formality

Same design, a different trait, chosen to check that the sentiment result is not specific to sentiment: a formality
direction (difference of means over 12 formal and 12 informal sentences, layer 6, `resid_post`), the same 64 seeds and
20 random directions, one strength (0.3, chosen before seeing results, not swept), prompting baseline "write in a very
formal, professional and polite register". Behavior is scored by `s-nlp/roberta-base-formality-ranker` and by the same
local judge asked "which is more formal"; both scored 24/24 on 24 held-out gold pairs, and the sentiment classifier
does not rank those pairs (a test asserts it), so the measures are about their own trait. Reports:
[projection](e1-gpt2-formality-64seeds-coef0.3.json), [behavior](e1-gpt2-formality-64seeds-coef0.3.judged.json).

| Effect vs clean, hop 0 / 1 / 2 | steered | prompted | 20 random directions (mean ± sd) | steered vs nulls |
|---|---|---|---|---|
| projection onto the trait direction | +0.025 / +0.024 / +0.025 | +0.005 / +0.002 / +0.003 (intervals include 0) | −0.003 ± 0.007 / +0.002 ± 0.019 / +0.002 ± 0.006 | above 20, 19, 20 of 20; z +3.9 / +1.2 / +3.9 (hop 1's spread is inflated by one random direction at +0.076) |
| classifier (formal − informal logit) | +1.66 / +1.73 / +1.66 | +1.71 / +1.26 / +1.43 | +0.01 ± 0.41 / +0.15 ± 0.33 / +0.19 ± 0.34 | above all 20; z +4.0 / +4.8 / +4.3 |
| pairwise judge (log-odds more formal) | +0.10 / +0.12 / +0.11 | +0.13 / +0.05 / −0.01 | −0.01 ± 0.08 / +0.02 ± 0.06 / +0.01 ± 0.06 | above 19, 18, 18 of 20; z +1.5 / +1.7 / +1.6 |
| steered − prompted, classifier, paired | −0.05 [−0.74, +0.66] / +0.47 [−0.24, +1.18] / +0.23 [−0.46, +0.93] | | | |
| steered − prompted, judge, paired | −0.03 [−0.16, +0.11] / +0.07 [−0.03, +0.17] / +0.13 [+0.03, +0.23] | | | |

**What this shows.**
1. The classifier agrees the steered text is more formal than clean text, by more than any of 20 random directions
   (z about +4 to +5), and the effect is the same size two hops later in agents that only read the text.
2. This second trait does not reproduce the sentiment result on one point: **steering does not beat prompting here.**
   The prompted prefix raises the classifier score as much as steering does (paired differences include zero at every
   hop), while for sentiment steering clearly beat prompting on the classifier.
3. The projection measure missed the prompted arm's effect: projection says prompting did nothing (+0.003), the
   classifier says it did as much as steering. So a projection onto the extracted direction is not a sufficient
   stand-in for behavior. Across arms its rank correlation with the classifier is 0.34 to 0.63 for formality
   (0.74 to 0.95 for sentiment), and with the judge 0.28 to 0.54 (0.90 to 0.96).

**Where it is thin.** The judge is a weak instrument for formality: effects of about +0.1 log-odds, steered above
only 18 to 19 of 20 random directions, and agreement with the classifier of Spearman 0.25 per text (0.69 to 0.70 for
sentiment). The classifier was trained on a particular notion of formality. One strength, so no read on how the
effect scales. The gold pairs were clean contrasts; GPT-2's continuations are not. Everything from the sentiment
section's caveats still applies: one model, one layer, one extraction set per trait, no human ratings.

**What it means for the claim.** Across two traits, an edit to one agent's activations produces text that the
independent measures rate as more of the trait than any of 20 random edits do, and that difference is still present
two hops downstream in agents that only read the text. Whether steering is better than simply asking for the trait
depends on the trait (better for sentiment, no better for formality in this setup). The chain result rests on
GPT-2's tendency to carry text forward; it is not evidence about instruction-following models.

**Running it took three tries, and two of the failures are worth knowing.** Six parallel shards on a machine that
was also running other jobs timed out at the 600 s steering limit; a rerun with a 30-minute limit then hit the
member's grant expiring after its default hour, because the first hop took longer than that under load. Both fail
closed and are correct behavior. `playbox_create` now takes `grant_ttl_secs`, and the experiment sets six hours.

**A fix this run forced.** Under six concurrent shards, one request was refused with "Reached Datalog execution
limits": the Biscuit library's default time budget for evaluating a grant is one millisecond. A refusal fails
closed, but it was wrong. The authorizer now allows 250 ms and keeps the library's default caps on facts and
iterations (`grants.rs`).

**E2.** Can a peer with `probe_only` rights detect that a transcript came from a steered agent, using the
difference-of-means direction, against a baseline classifier? The probe operation is the building block.

**E3.** Fit a linear map between two models' residual spaces and test whether one's direction steers the other.
Needs the spec's `scale` field and a second hosted model.

**E4.** Agents search layer, strength and recipe space; compare the best lineage score to a human sweep and to
prompting. Needs evaluator and lineage wired to steering specs.

**E5.** Confirm that the grant refuses ablation of a guarded direction and the audit log records the attempt. The
refusal is tested; the audit record for `steer_denied` is written for authorization failures and was not asserted in
a test.

**E6. Hidden steering through a latent channel.** Agent A runs a steering spec with scope `prompt`, then hands B
its cache instead of its text (LatentMAS on CPU first; SwaT for cross-family later). B sees no steering text. Also
run the arm where the text is identical but A is unsteered. Metric: projection of B's activations on the trait
direction and the E1 judge, against the E1 text-channel result, with the same null-direction percentile controls.
If B shifts while the text is identical, the effect travels where the broker, taint tracker and receipts cannot
see it. Report it as a measured gap with its size. Then test the guards: does a refusal direction survive the
transfer, or does a guarded direction get through unflagged? The cross-family version needs a mapper trained per
model pair and a GPU, and is not built.

*Built for the same-base case* (no mapper needed): `steer/playbox_steer/latent.py` and `python -m playbox_steer.e6`
(4 tests on a tiny model). A's context is read under a steering spec with scope `prompt`; B continues from that
cache with its own query; every arm shows B identical text. B's behaviour is scored by the log-mass of positive
minus negative next-token words (no judge) and the KL from the clean arm. A check that the clean handoff equals B
reading the text itself passes (max logit difference 9e-5).

*Results* (real models on CPU; 64 neutral contexts, 32 seeded random null directions; steering at one layer
with `tokens: prompt`; raw numbers in `steer/results/`; stated ceiling `local_internal_exploratory`). "Specific" is
the steered effect minus the mean effect of the random directions, per context, with a 95% bootstrap interval.
"Nulls as far" is the share of the 32 random directions that push at least as far the same way. A first, smaller
GPT-2 run (32 contexts, 8 nulls, strengths 0.5 to 4) is kept as `e6_gpt2_first_run.json`; it agrees with the
GPT-2 rows below.

| model, trait | strength | latent effect | specific [95% CI] | nulls as far | KL (null mean) | text channel |
|---|---|---|---|---|---|---|
| GPT-2 (layer 4), sentiment | 0.25 | +0.84 | +0.80 [+0.70, +0.92] | 0.00 | 0.037 (0.015) | +0.90 |
| | 0.5 | +0.51 | +0.48 [+0.33, +0.64] | 0.00 | 0.078 (0.065) | +0.66 |
| | 1 | -0.73 | -0.75 [-0.90, -0.58] | 0.94 | 0.286 (0.275) | +0.45 |
| GPT-2, fear | 0.25 | +0.39 | +0.46 [+0.40, +0.53] | 0.00 | 0.013 (0.012) | +0.03 |
| | 0.5 | +0.34 | +0.50 [+0.41, +0.58] | 0.03 | 0.036 (0.055) | +0.17 |
| | 1 | +0.04 | +0.29 [+0.19, +0.39] | 0.38 | 0.093 (0.204) | -0.06 |
| SmolLM2-135M (layer 10), sentiment | 0.25 | +0.92 | +0.90 [+0.81, +1.00] | 0.00 | 0.039 (0.038) | +0.90 |
| | 0.5 | +1.24 | +1.16 [+1.07, +1.26] | 0.00 | 0.100 (0.145) | +1.24 |
| | 1 | +1.44 | +1.29 [+1.23, +1.36] | 0.03 | 0.218 (0.329) | +0.80 |
| SmolLM2-135M, fear | 0.25 | +1.00 | +0.99 [+0.90, +1.09] | 0.00 | 0.042 (0.035) | +0.09 |
| | 0.5 | +1.40 | +1.37 [+1.27, +1.48] | 0.00 | 0.121 (0.134) | +0.11 |
| | 1 | +1.62 | +1.48 [+1.34, +1.61] | 0.00 | 0.260 (0.285) | +0.01 |

What this supports. With identical text, an edit made to A's context moves B in the intended direction. In 11 of
12 cells the trait-specific part is positive with an interval above zero. In SmolLM2 it grows with strength and
almost no random direction matches it, and the steered KL is at or below a random direction's, so it is not just
general disruption. For fear, the text channel carries almost nothing (+0.01 to +0.17) while the cache carries
+0.3 to +1.6: this is the case the planned checks cannot see. For sentiment the text channel is comparable, so
there a latent channel adds less. What it does not support. GPT-2 sentiment at strength 1 reverses (-0.73, and
94% of random directions push further positive), and GPT-2 fear at 1 is weak; both match the first run, and
the reversal is unexplained. The score is B's immediate lean at one next-token position, not later behaviour.
The nulls are random directions, not other meaningful ones. Two small models, one steering layer each, two traits,
one query per trait: a reason to test a frontier-scale open model and more traits, not a general claim.

### E6 round 2: the four limits, a larger model, a defence, and a faster path

Run 2026-10-29; raw numbers in `steer/results/round2/`; stated ceiling `local_internal_exploratory`. Changes from
round 1: B's greedy continuation is scored too (a word lexicon over 12 generated tokens), besides the next-token
score; nulls are now also four unrelated contrasts (formality, tense, topic, question versus statement) at the same
layer and strength; two layers and three queries per trait; and Qwen2.5 1.5B and 3B were added. GPT-2 and SmolLM2
ran on CPU with 16 contexts and 8 random nulls; the Qwen runs used 32 contexts and 16 random nulls on the Mac GPU in
float32. Each model has 8 cells (2 layers, 2 traits, strengths 0.25 and 0.5). A sample is one (context, query).

| model | samples per cell | next-token effect range | cells where no unrelated null pushes as far | generation effect range | cells with generation interval above 0 | ... and above the unrelated nulls' mean | KL above a random direction's |
|---|---|---|---|---|---|---|---|
| GPT-2 (124M) | 48 | +0.52 to +1.90 | 8 of 8 | -0.002 to +0.014 | 0 of 8 | 2 of 8 | 7 of 8 |
| SmolLM2-135M | 48 | +1.15 to +2.07 | 8 of 8 | +0.006 to +0.049 | 5 of 8 | 7 of 8 | 6 of 8 |
| Qwen2.5-1.5B | 96 | +0.53 to +1.58 | 8 of 8 | +0.009 to +0.030 | 8 of 8 | 8 of 8 | 8 of 8 |
| Qwen2.5-3B | 96 | +0.20 to +1.11 | 8 of 8 | +0.001 to +0.022 | 2 of 8 | 3 of 8 | 8 of 8 |

What this supports. The next-token effect is specific to the trait direction: in all 32 cells none of the unrelated
contrast directions pushed as far, and its interval is above zero. So round 1's worry that random directions were
too easy a null does not explain it. What it supports less well. The effect on B's later text is real in some setups
(Qwen2.5-1.5B in every cell, SmolLM2 in most) and absent in others (GPT-2 in none, Qwen2.5-3B in six of eight, and
near zero for fear on 3B). It does not grow with model size: the 3B model is weaker than the 1.5B one, and the
models differ in family, depth and the layers chosen, so this is not a clean test of scale. The generation score is a
short lexicon count (effects of about 1 to 5 words per hundred), which is a weak instrument. I would not claim the
effect reaches later behaviour in general; it reaches it for some models. (Round 3 below replaces this reading: the word count undercounted, and independent judges find the effect in nearly every model.)

*Defence* (`cache_audit.py`, `audit_e6.py`, 5 tests; GPT-2 and SmolLM2, 32 contexts each). The sender signs
`playbox.cache-handoff.v1`: weights hash, sha256 of the text and of the exact cache bytes. The receiver checks the
signature, that the cache matches the record, and then recomputes the cache from the text and compares. Honest
handoffs: 0 false positives at tolerance 0 and at 1e-3. Steered caches signed by the sender itself: caught in every
case down to strength 0.001, on both models; the first two checks pass such a cache, so only the recompute catches it.
Honest numerical noise when the auditor uses 4 threads instead of 1 was exactly 0 on this machine, which says
nothing about other hardware or precisions. The recompute costs a prefill (about the same time as B reading its
query here, because the contexts are short), so it is an audit of a sample: with probability `p` per handoff a
sender who cheats `n` times is caught with probability `1 - (1 - p)^n`.

*Faster path* (`fast.py`, 5 tests). Every arm and context goes through the model in one batch, with a steering
vector per row (zero for the clean arm), left padding with positions from the mask, queries and generation on the
whole batch, and the context cache rewound with `crop` rather than copied. On the same device and precision it
reproduces the one-at-a-time path (tiny GPT-2 and Llama-style models, and real GPT-2: next-token +1.05, generation
+0.0139 with an identical interval, KL 0.033 on CPU and on the Mac GPU in float32). One cell takes 15 s instead of
187 s on GPT-2 (12x) and 22 s instead of 228 s on SmolLM2 (10x) on CPU. Qwen2.5-1.5B takes 30 to 90 s per cell on
the GPU; the CPU one-at-a-time path took roughly 25 minutes for the first cell of a smaller grid (12 contexts, 6 nulls) before I stopped it, so the speedup is at least 20x but I did not time the same cell both ways. Half precision is not
free: float16 matched to the second decimal, bfloat16 moved the generation score and the KL (KL 0.059 against 0.033),
so any half-precision result should be reported as its own setting.

vLLM. I first thought vLLM had no Mac GPU route. It does, through the `vllm-metal` plugin (MLX-based, paged
attention marked experimental; [docs](https://docs.vllm.ai/projects/vllm-metal)). I did not try it: E6 needs a
per-row edit inside the model and direct access to the cache, which a managed server does not offer. The
practical route for a vLLM-scale run is the CUDA steering fork named in section 2c on a GPU machine.

### E6 round 3: independent judges, and a 7B model

Run 2026-10-29. Reports and every generated text are in `steer/results/round3/`; judged results and the summary are
in `judge/results/e6/`. Stated ceiling `local_internal_exploratory`. The generated text is now scored by two
measures that never see the steering direction, as well as the word count: a trained classifier (sentiment:
`siebert/sentiment-roberta-large-english`; fear: `j-hartmann/emotion-english-distilroberta-base`, fear against joy)
and a pairwise judge (`Qwen/Qwen3-1.7B-Base`, asked which of two texts shows more of the trait, in both orders so a
position bias cancels). Both reached 24 of 24 on held-out gold pairs for each trait before scoring (the fear pairs
are new, `judge/playbox_judge/fear.py`). Every steered text is compared with the clean arm's text for the same
(context, query), and the effect is set against 16 random nulls and 4 unrelated-contrast nulls (the judge on the
steered arm, the 4 unrelated nulls and 8 random ones). Settings are the same for every model: 32 contexts, 3 queries
per trait, 12 generated tokens, strengths 0.25 and 0.5, two layers, two traits (8 cells per model, 96 samples per
cell). The 1.5B and 3B models ran in float32 on the Mac GPU; Qwen2.5-1.5B was also run in bfloat16 as a precision
control, and Qwen2.5-7B only in bfloat16.

| model | word count above 0 | classifier above 0 | judge above 0 | no unrelated null as far (classifier / judge) | steered text identical to clean | classifier effect range | judge effect range |
|---|---|---|---|---|---|---|---|
| GPT-2 (124M) | 2 of 8 | 5 of 8 | 7 of 8 | 6 / 7 | 34% | +0.01 to +1.39 | -0.10 to +0.47 |
| SmolLM2-135M | 6 of 8 | 8 of 8 | 8 of 8 | 8 / 8 | 14% | +1.16 to +3.29 | +0.36 to +1.45 |
| Qwen2.5-1.5B, float32 | 8 of 8 | 8 of 8 | 8 of 8 | 8 / 8 | 42% | +0.93 to +2.26 | +0.14 to +0.97 |
| Qwen2.5-1.5B, bfloat16 | 7 of 8 | 8 of 8 | 8 of 8 | 8 / 8 | 37% | +1.10 to +2.42 | +0.16 to +1.00 |
| Qwen2.5-3B, float32 | 2 of 8 | 5 of 8 | 7 of 8 | 8 / 8 | 42% | +0.37 to +2.91 | +0.07 to +0.78 |
| Qwen2.5-7B, bfloat16 | 5 of 8 | 8 of 8 | 8 of 8 | 7 / 8 | 5% | +2.04 to +4.40 | +0.50 to +1.20 |

"Above 0" counts cells whose 95% bootstrap interval is above zero. All three measures have the same sign in 7 or 8
cells per model. The next-token effect is specific in every cell of every model (no unrelated-contrast null pushed
as far), as before; the 7B model's is the largest (+1.95 to +3.50).

What this supports, and a correction. Round 2 read the word count as saying the effect on later text was absent for
GPT-2 and weak for Qwen2.5-3B, and that it did not grow with size. The independent measures disagree: the word count
undercounts (GPT-2 2 of 8 cells by word count, 7 of 8 by the judge; the same for the 3B model). With the better
instruments an edit made to A's context shifts what B goes on to write, in nearly every model and cell, and no
random or unrelated-contrast direction does as much. The 7B model shows the largest effects, and its steered text
differs from the clean text in 95% of samples against 58% to 86% for the others. Size is still not a clean story: the
3B model's effects are smaller than the 1.5B model's, the models differ in family, depth and the layers chosen, and
the 7B model was run in bfloat16. The precision control is reassuring: Qwen2.5-1.5B in bfloat16 agrees with float32
on 8 of 8 cells for both independent measures (and 7 of 8 for the word count).

What it does not support. The effects are averages over all samples, including those where the text did not change
(14% to 42% for most models), so the effect on texts that changed is larger. Texts are 12 tokens. Each classifier
was trained on other kinds of text (reviews; short social posts). The pairwise judge is a Qwen model, as are four
of the six runs (all the Qwen2.5 ones), so a shared bias cannot be ruled out. Strengths stop at 0.5, because round 1 showed that
larger ones mostly break the model. Two traits, three families, one size above 3B. This is an exploratory
measurement and not a general result.

Practical reading for rooms: a cache handed to another agent can carry a behavioural edit that no text check sees,
and an independent judge on the receiver's output can see it. That supports pairing a latent handoff with the audit
from round 2 and a judge on a sample of outputs.

## 5. Threat model additions

| Threat | Mitigation (built, with its test) | Remaining gap |
|---|---|---|
| A guest removes a hosted model's refusal behavior | `ablate` is off by default; guards refuse a direction within cosine 0.5 of a host-listed safety direction at that site and layer unless the grant names the class (`a_guarded_direction_is_refused...`, and again inside Docker) | Only directions the host listed are recognized. A novel direction, or a combination of small steps, is not. |
| A member steers with a spec someone else signed, or replays one | The spec must be signed by the sender's own key (`a_spec_signed_by_someone_else_is_refused`) | A member can still run their own spec as often as the budget allows. |
| A member sends files that differ from what the spec names | Direction and prompt set are hashed by the host before anything runs (`files_must_hash_to_what_the_spec_names`) | None known. |
| A member exceeds model, layer, strength, mode, direction or token limits | Grant checks; refused, not clamped (`the_grant_bounds...`, `a_model_or_direction_outside_the_grant...`) | None known. Boundary values are tested. |
| A member runs forever or burns the host's compute | Run and token budget counted before the run, per member (`the_budget_is_counted_per_member`); at most two tasks at once per host (a semaphore); memory, CPU and time limits; scratch quota; cancelled if the member is revoked | The budget is in memory: a host restart resets it. The budget is not narrowed by a member's own attenuation. |
| The engine container reaches the network or writes outside its scratch | No network, read-only root, read-only mounts (`docker_engine_image`); host reads `out.json` without following symlinks and bounded (`output_reader_refuses_symlinks...`) | Container isolation is weaker than a microVM; the libkrun runner has no mounts yet and refuses steering. |
| Weights on disk differ from their name | The host recomputes every hash at startup and refuses a mismatch (`a_mislabeled_weights_store_is_refused`); Rust and Python agree on the hash (cross-check test) | Weights are trusted once hashed; a later swap on disk is only caught at the next start. |
| A steered peer manipulates the reading agent | Output registered as untrusted; `playbox_steer` needs `Reviewed` prompts (taint test) | The broker is lexical. Persuasion by style is not caught. |
| Receipt forgery or misuse | Host signature; the client checks host key, spec, and exact output bytes; the ceiling is printed on the receipt | An un-attested host can lie about what it ran. Nothing raises the ceiling to `attested` yet. |
| An imported public recipe carries an unexpected edit | Not built (section 2c): imports are meant to be untrusted and quarantined | Until built, do not import recipes automatically. |
| Overfitting a sealed holdout | Not built (section 3.5) | Inherent once it is. |
| Latent channels between agents bypass text-level checks | Measured, not closed. **E6** shows an edit made to A's context moves B through the cache with identical text (section 4). The defence is built as a library and tested (`steer/playbox_steer/cache_audit.py`: a signed record of the text and the exact cache, plus an audit that recomputes the cache from the text); it is not an A2A operation yet | A lying sender can sign its own steered cache, so only the recompute catches it, and that costs a prefill: audit a sampled fraction. It needs the same model on both ends, so it cannot cover a cross-family mapper. Tolerance across hardware or precision is untested. |

## 6. Status and milestones

Built and tested (`cargo test -p playbox-core -p playbox-runner -p playbox-net -p playbox-mcp`, `steer/` pytest,
`orchestrator/` pytest, `lake build`):

- **S1** spec and receipt types (7 tests).
- **S2** Python engine with gates, probe, KL, guards, diff-of-means and random directions (31 tests, two
  architectures, all three sites); Docker image; `Op::Steer` on the host with 14 integration tests against the real
  engine, plus a Docker test (`cargo test -p playbox-net --test steer -- --ignored`).
- **S3** grant facts, narrowing, guards, budgets (above); Lean: narrowing cannot widen, chains stay inside the
  root (`lean/Playbox/Steer.lean`, no `sorry`).
- **S4 (part)** MCP tool and CLI flags (MCP end-to-end test drives two real sidecars with Docker); E1 harness with
  arms and statistics (11 orchestrator tests with a fake backend, including shard merging); a real GPT-2 run at 64 seeds (section 4).

Not built: the evaluator and lineage wiring for steering specs (E4); the transfer experiment (E3); a trained
detector for E2; recipe import, the `scale` field and `SteerPlan` from section 2c; a latent-score (cardinal-harness style) judge, though a paired pairwise judge exists; a GPU
path and anything attested above `local_internal_exploratory`; a libkrun mount; persistent budgets.

Out of scope for v1: fine-tuning, weight export, latent channels between agents (E6 only measures one, it does not add one to rooms), frontier models.

## 7. Open questions

- Does moving to Qwen3.5-4B (what neuronary and neuron-explorer use) work with this engine? It needs a newer
  `transformers` and its block layout checked; unverified.
- GPU confidential computing for receipts: is attested GPU inference available on a platform you will use?
- How should "direction class" be defined so grants stay enforceable without a central registry? Today it is the
  host's guard list.
- Is a 1.7B local judge enough for traits other than sentiment, or do those need a stronger model or human ratings?
  Sentiment has a trained classifier to check against; most traits do not.
- E6 needs a GPU and a trained mapper per pair. Is the CPU LatentMAS sidecar enough to show the effect, or does it need SwaT-style cross-family transfer? And will the SwaT authors license the code?
- Whether to publish findings to a public dictionary such as neuronary or keep rooms private; publishing is an
  outward action that should go through a person's approval.
