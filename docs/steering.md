# Steering rooms: activation steering as a Playbox use case

Status: design. Nothing here is built. Written 2026-10-29. Research claims below come from abstracts and
search summaries, not from full-text reads, unless marked "read". Verify before citing.

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
experiment in section 6.

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

## 3. Architecture

```
member agent ──A2A──▶ host ──▶ steer task (GPU sandbox)
   │  SteerSpec                    │  model + hooks
   │  (signed, in grant bounds)    ▼
   │                         text + activations summary
   ◀──── SteerReceipt ─────────────┘   (signed by host, binds spec, weights, prompts, output)
        │
        ├─▶ evaluator (custodian-signed, sealed holdout) ─▶ Variant in Lineage
        └─▶ shared memory (findings with provenance, quarantined until reviewed)
```

Reused as is: invites, grants (Biscuit), `Runner`, A2A task/artifact model, history log, memory, taint broker.
New: a `steer` task kind, a spec type, a grant fact family, a receipt extension, and a GPU runner image.

### 3.1 SteerSpec

A canonical, hashable record (same `record` format as the other signed text):

```
playbox-steer-spec v1
model:        sha256 of weights manifest (+ human name, e.g. gemma-2-2b-it)
site:         resid_post | attn_out | mlp_out
layers:       [12, 13, 14]
direction:    sha256 of vector file (or sae:<release>/<id>/<feature>)
recipe:       diffmean | probe | sae_feature | reft_r1 | ablate (project out)
mode:         add | clamp | ablate
coef:         6.0              # in units of the residual norm at that layer, see below
tokens:       prompt | generated | all
prompts:      sha256 of prompt-set manifest
seed:         u64
```

Coefficients are expressed relative to the mean residual norm at the site, so a bound means the same thing
across models. The geometry work above suggests angle and norm behave differently; the spec therefore
stores direction and norm separately and both are bounded.

### 3.2 Grants

New facts, attenuable only downward, enforced by the host (over-limit is refused, not clamped, as today):

- `steer_models`: allowed weight manifests.
- `steer_layers`: allowed range. `steer_max_coef`: max relative norm.
- `steer_modes`: subset of `add`, `clamp`, `ablate`.
- `steer_directions`: allow-list of vector hashes, or classes (`persona`, `topic`, `style`).
- `steer_deny`: classes never allowed. **Default deny: `refusal`, `honesty`/`deception`-linked directions,
  and `ablate` on any safety direction.** A guest who wants those needs an explicit root-signed grant.
- `probe_only`: read activations and compute projections, no writes. Cheapest capability to hand out.
- `steer_budget`: total generated tokens and number of runs.

Lean: extend the attenuation model so `steer_*` bounds never widen across delegation. This is the same
proof shape as the existing cpu/memory/timeout limits, so it is a small addition.

### 3.3 Runner

Needs a GPU. libkrun does not give one today and I have not checked Docker GPU passthrough on your hosts.
Plan: a `DockerRunner` variant with `--gpus`, a pinned image that contains the model-hook code, and the image
digest on the attested allow-list (the policy already carries image digests). CPU-only is viable for Gemma-2 2B
for a demo. No network, as for other tasks; weights are mounted read-only and checked against the manifest hash.

### 3.4 Receipts

Extend `Receipt` with `steer_spec_digest`, `weights_digest`, `prompt_set_digest`, `image_digest`, and
`output_digest`. The host signs it; the member's session history records it. With a TEE-attested host
(Confidential Space is already supported for T3; GPU confidential computing is a separate question I have not
checked) the claim "this output came from this model with exactly this intervention" becomes verifiable, not
reported. Without attestation it is a signed host claim, which is still useful for reproduction.

### 3.5 Evaluator and lineage (the "RSI" loop)

The "recursive self-improvement" here is **search over steering configs and enrichment protocols**, not weight
updates:

1. An agent proposes a `Variant`: a new SteerSpec (and optionally a new prompt protocol) on a parent.
2. The custodian-signed evaluator runs it against a sealed holdout of behavior probes and a capability guard.
3. `Lineage` records the score with a lower confidence bound; noise is not improvement.
4. Accepted variants are written to shared memory as findings, citing receipts as evidence.

Honest label: this is automated configuration search with a fixed evaluator. It becomes true self-improvement
only if the loop also updates something persistent (fine-tuning, a trained steering adapter). Do not claim that
in v1. Known limit from `trust-layers.md` carries over: a fixed holdout is eventually overfit; `total_cost`
records queries spent.

### 3.6 Taint and memory

Steered model output is untrusted text to every other member's agent. It already flows through the admission
broker. Steering makes the injection risk sharper: a steered "persuasive" or "sycophantic" peer is a more
effective attacker on a reading agent. Findings written to memory are quarantined until a pinned reviewer
accepts them, as today.

## 4. Experiments (ranked by novelty over cost)

Each reports per-input distributions, a prompting-only baseline, a coherence/capability guard, and receipts.

**E1. Contagion across a chain (headline).** Agents A→B→C all run the same base model. Only A is steered
toward trait T (use persona-vector traits, plus a harmless topic like "Golden Gate"). B and C see only text.
Metric: projection of B's and C's own activations on T's direction, plus a behavioral judge, per hop.
Controls: no steering; A prompted toward T instead; B and C on a *different* base model. Hypothesis: text-only
contagion dampens fast for style/topic and persists for in-context persona adoption; unknown for sycophancy.
Novel because I found no direct measurement.

**E2. Detectability.** Can a peer with `probe_only` access to a *reference* model detect that a transcript
came from a steered agent, using the diff-of-means direction versus a baseline classifier? AxBench suggests
supervised detection is strong. Gives an audit signal that works without trusting the author's claim.

**E3. Transfer.** Fit the linear map between two models (Gemma-2 2B and 9B first, then a different family) and
test whether A's vector steers B. Reproduces the ACL result in a setting where the vector arrives via signed
memory, then tests the architecture-agnostic claim.

**E4. Config-search lineage.** Agents search layer/coef/recipe space; compare lineage's best score to a human
sweep and to prompting. Tests whether the search finds off-axis configs that avoid the brittleness Tan et al.
report.

**E5. Safety-direction handling.** Confirm grants refuse refusal-direction ablation and that the audit log
records the attempt. This is a product-safety test, not a research result.

## 5. Threat model additions

| Threat | Mitigation | Remaining gap |
|---|---|---|
| Guest removes a model's refusal behavior on a shared host | Default-deny directions; `steer_deny`; refused, not clamped | Direction classes are by hash/label; a novel direction is not recognized as safety-relevant. A cosine check against known safety directions would catch some, not all. |
| Steered peer manipulates the reading agent | Taint broker; memory quarantine; E2 detectors | Lexical broker does not catch persuasion by style. |
| Receipt forgery | Host signature; TEE attestation where available | Un-attested host can lie about what it ran. |
| Overfitting the sealed holdout | `total_cost`; rotation by custodian | Inherent. |
| Dual-use release of "uncensored" weights via a rank-one edit | Tasks return text and summaries, not weights; no weight export grant in v1 | Weight-edit export would need its own grant and review. |
| Latent channels between agents bypass text-level checks | Not built | Needs its own design if added. |

## 6. Milestones

- **S0 (this doc):** agree on scope and the E1 protocol.
- **S1 (done, `playbox-core/src/steer.rs`, 7 tests):** `SteerSpec` type, canonical signed record, signer-independent digest, `SteerReceipt` with a stated `Ceiling`. The permutation-invariance test from `theseus-lab` is not a test here: it needs a real model, so it moves to S2. S1 instead restricts sites to the residual-stream basis (`resid_post`, `attn_out`, `mlp_out`), which MLP hidden-unit permutations don't change. The receipt is a separate record from `receipt::Receipt`, not an extension of it.
- **S2:** `steer` task in a CPU Docker image (pattern from `mesh-intelligence`'s LatentMAS sidecar) with Gemma-2 2B and a diff-of-means vector; pre-flight gates from `interpretability-lab-sandbox` (parity, no-op, reach); one end-to-end run over A2A with a receipt.
- **S3:** grant facts and host enforcement; Lean attenuation extension.
- **S4:** orchestrator graph for E1 (extends the existing LangGraph fan-out); evaluator wiring for E1/E4.
- **S5:** GPU image, 9B model, transfer experiment E3.

Out of scope for v1: fine-tuning, weight export, latent (activation) channels between agents, frontier models.

## 7. Open questions

- GPU confidential computing for receipts: is attested GPU inference available on a platform you will use?
- Which behavior judge for E1 (a pinned open model vs a probe)? A probe is cheaper and consistent; a judge is more legible.
- How should "direction class" be defined so grants stay enforceable without a central registry?
- Whether to publish findings to Neuronpedia-style public pages or keep rooms private.
