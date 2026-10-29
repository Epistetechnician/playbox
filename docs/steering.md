# Steering rooms: activation steering as a Playbox use case

Status (2026-10-29): steering hosting, grants, receipts, guards, the MCP and CLI surfaces and an E1 harness are built
and tested (section 6); the evaluator loop, transfer, detection and recipe import are not. The one experiment run so
far is inconclusive (section 4). Research claims below come from abstracts and search summaries, not from full-text
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
- **Judge for E1.** Use pairwise ratio judging in the cardinal-harness style as the behavioral measure, next to
  activation projections. Report each hop's score with its uncertainty and the gap between two judge models.
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
  `steer_timeout_secs`, `steer_caps` on `playbox_create`.
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
| **E1** contagion across a chain | Harness built and run once on real GPT-2 (below). Inconclusive on trait-specificity. |
| **E2** detectability | The read-only probe operation exists; no detector or baseline classifier is built. |
| **E3** transfer across models | Not built. |
| **E4** config-search lineage | Not built (section 3.5). |
| **E5** safety-direction handling | The guard and default-deny behavior are built and tested (section 5); no research claim is made. |

### E1: does a steered trait spread to agents that only read its text?

**Setup (what ran).** GPT-2 small (124M) hosted in a Docker steering room; a sentiment direction from
difference-of-means over 12 positive and 12 negative sentences at layer 6 (`resid_post`); 16 neutral seed prompts;
24 new tokens, greedy; three hops (A steered, B reads A's text and continues it, C reads B's). Arms: `clean`,
`steered` (the sentiment direction at strength s), `prompted` (a "write something very positive" prefix on hop 0, no
steering), and three `null` arms (random unit directions, seeds 100-102, same strength). The measure is the
projection of each hop's text onto the sentiment direction, in units of the layer-6 activation norm, as a paired
difference against `clean` with a 95% bootstrap interval over seeds. KL is the mean per-token KL(steered ‖ clean)
along the steered text. Every run carries a host receipt; the full reports with every text and spec digest are
[e1-gpt2-coef0.3.json](e1-gpt2-coef0.3.json) and [e1-gpt2-coef0.6.json](e1-gpt2-coef0.6.json). Reproduce with
`orchestrator/experiments/e1_gpt2.py`.

| Arm (hop 0 → hop 2), effect vs clean | strength 0.3 | strength 0.6 |
|---|---|---|
| steered | +0.033 → +0.029 (KL 0.33) | +0.038 → +0.029 (KL 0.96) |
| prompted | +0.025 → +0.018 | +0.025 → +0.018 (same arm) |
| null (3 random directions) | +0.008, +0.008, +0.015 → +0.012, +0.001, +0.014 | −0.006, +0.006, **+0.028** → −0.009, +0.005, +0.030 |

**What this shows.** At 0.3 the steered arm's hop-0 effect (CI +0.016 to +0.052) is above all three random
directions (each CI includes 0 or only just excludes it), and most of it is still there two hops later (retained
90% and 88%). At 0.6 the picture changes: one random direction produces an effect (+0.028, CI +0.014 to +0.042) that
the steered arm (+0.038) cannot be separated from, and the steered arm's KL is nearly 1 nat per token, so the model
is being pushed hard. The prompting baseline (+0.025) is not distinguishable from steering at either strength,
which matches AxBench's finding that prompting is hard to beat.

**What it does not show.** That the sentiment direction specifically spreads. Three reasons the persistence is weak
evidence on its own: GPT-2 continuing a text tends to repeat it (see the texts in the reports), so "retained" mostly
measures copying; a random direction can move the sentiment projection too, and one did; and there are 16 seeds, one
model, one trait, one layer, one extraction set and three null directions, with no correction for looking at two
strengths and several arms. Treat it as a working harness with a first, inconclusive reading, not a finding.

**What would settle it.** More seeds and many null directions (report the steered effect as a percentile of the null
population, as neuron-explorer does); a larger model that does not loop; a behavioral judge in addition to the
projection (pairwise-ratio judging, section 2c); repeating the extraction with different sentence sets to see how
much of the effect is the sentence set; and a hop-0 effect sized for a similar KL across arms.

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

*First result* (real GPT-2 on CPU, steer layer 4, 32 neutral contexts, one sentiment direction, 8 random null
directions; raw numbers in `steer/results/e6_gpt2.json`; stated ceiling `local_internal_exploratory`):

| strength (norms) | latent: trait-logit effect [95% CI] | latent KL | null effects (min to max, mean) | text channel effect |
|---|---|---|---|---|
| 0.5 | +0.42 [+0.17, +0.67] | 0.08 | -0.15 to +0.36, +0.16 | +0.69 |
| 1 | -0.77 [-1.05, -0.48] | 0.29 | -0.10 to +1.22, +0.33 | +0.45 |
| 2 | -1.50 [-1.74, -1.27] | 0.54 | -0.10 to +2.85, +0.65 | +0.49 |
| 4 | -2.55 [-2.83, -2.26] | 0.78 | -0.45 to +3.38, +0.82 | +0.00 |

What this supports: with identical text, B's behaviour changes, so an edit does travel through the cache. At 0.5
the shift is in the intended direction and larger than all 8 nulls. What it does not support: at 1 and above the
effect reverses sign and its KL is about the same as a random direction's, so those are mostly general
disruption of the cache, not controlled steering; the reversal is unexplained. Random directions also shift B
(on average positive here), so the trait-specific part at 0.5 is the gap above that, roughly +0.26. One model,
one trait, 32 contexts and 8 nulls: treat it as a reason to test properly, not as a finding. The text channel's
effect vanishes at strength 4 because A's text degenerates, while the cache still carries a large change.

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
| Latent channels between agents bypass text-level checks | Not built. **E6** (section 4) is the planned measurement; section 2d | Unmeasured. If E6 shows an effect, a latent lane would need a signed, logged text summary beside every cache, and a check that the two agree. |

## 6. Status and milestones

Built and tested (`cargo test -p playbox-core -p playbox-runner -p playbox-net -p playbox-mcp`, `steer/` pytest,
`orchestrator/` pytest, `lake build`):

- **S1** spec and receipt types (7 tests).
- **S2** Python engine with gates, probe, KL, guards, diff-of-means and random directions (26 tests, two
  architectures, all three sites); Docker image; `Op::Steer` on the host with 14 integration tests against the real
  engine, plus a Docker test (`cargo test -p playbox-net --test steer -- --ignored`).
- **S3** grant facts, narrowing, guards, budgets (above); Lean: narrowing cannot widen, chains stay inside the
  root (`lean/Playbox/Steer.lean`, no `sorry`).
- **S4 (part)** MCP tool and CLI flags (MCP end-to-end test drives two real sidecars with Docker); E1 harness with
  arms and statistics (9 orchestrator tests with a fake backend); a real GPT-2 run (section 4).

Not built: the evaluator and lineage wiring for steering specs (E4); the transfer experiment (E3); a trained
detector for E2; recipe import, the `scale` field and `SteerPlan` from section 2c; a pairwise-ratio judge; a GPU
path and anything attested above `local_internal_exploratory`; a libkrun mount; persistent budgets.

Out of scope for v1: fine-tuning, weight export, latent channels between agents (E6 only measures one, it does not add one to rooms), frontier models.

## 7. Open questions

- Does moving to Qwen3.5-4B (what neuronary and neuron-explorer use) work with this engine? It needs a newer
  `transformers` and its block layout checked; unverified.
- GPU confidential computing for receipts: is attested GPU inference available on a platform you will use?
- How should "direction class" be defined so grants stay enforceable without a central registry? Today it is the
  host's guard list.
- Which judge for E1 beyond the probe projection and KL? A pairwise-ratio judge in the style of cardinal-harness is
  the candidate; not built.
- E6 needs a GPU and a trained mapper per pair. Is the CPU LatentMAS sidecar enough to show the effect, or does it need SwaT-style cross-family transfer? And will the SwaT authors license the code?
- Whether to publish findings to a public dictionary such as neuronary or keep rooms private; publishing is an
  outward action that should go through a person's approval.
