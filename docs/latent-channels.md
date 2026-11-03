# Beyond text: latent channels across model families

Status (2026-11-03). Built: the hub (`steer/playbox_steer/hub.py`), its wire message `PBL1`, and the E8 runner
(`hub_run.py`), with 7 tests on synthetic models and on tiny random models of three architectures. Real runs:
section 6a (five families, one H100, 2026-11-03) and 6b (k sweep and join test, one A10G on Modal). The token-cost
comparison, the anchor-count sweep and the closed-model arm in section 6 are still plans. Builds on E6/E7 in [steering.md](steering.md).

## 1. The problem

Text is the only channel every model shares, and it is a poor one: it is lossy (a concept is squeezed into tokens and
re-derived), expensive (tokens in and out at both ends), and it is where prompt injection lives. E6 and E7 showed that
open-weights models can pass a concept or a behavioural edit without text. But `telepathy.py` fits a ridge map per
ordered pair: N models need N(N-1) maps, a new model needs 2N more, and nothing covers models whose activations we
cannot touch (every API-only model).

## 2. What a model exposes decides the channel

| Tier | Needs from the model | Carries | Status |
|---|---|---|---|
| L0 text | nothing | anything, lossily | exists |
| L1 concept card | nothing (closed models fit here) | a hub vector rendered as nearest anchor concepts plus a short caption, with a hash of the source | design only |
| L2 hub latent | read and write of one mid-layer residual stream (open weights, any family) | a k-dimensional concept or behavioural vector, k bytes | built, synthetic and tiny-model tests |
| L3 KV-cache handoff | identical weights on both ends | full context state | built (E6 rounds 4 and 6) |
| L4 trained cache mapper (SwaT-like) | open weights, a GPU-day per pair | cross-family cache | not built; license unclear |

A closed model gets L0 and L1 only. Nothing here lets a vector be injected into a model we cannot hook, and the doc
does not pretend otherwise.

## 3. The hub (L2)

Every model gets one encoder and one decoder against one shared k-dimensional space, instead of a map per pair:

```
sender state diff d_i  --enc_i-->  h (k floats, int8 on the wire)  --dec_j-->  receiver direction in R^{d_j}
```

- **Fit.** All models read the same anchor texts (the 16 fit topics of `telepathy_data.py`, 576 sentences, 150 held out). Each
  model's centred mid-layer states are whitened by an SVD; the whitened scores are stacked and the top-k left singular
  vectors are the shared coordinates (MAXVAR generalized CCA). Each spoke is a ridge fit to and from those coordinates.
- **Join.** A new model reads the same anchors and fits only its own two adapters. No existing spoke changes
  (`test_a_new_model_joins_without_refitting_the_others`). Cost per new model is linear, not quadratic.
- **Wire.** `PBL1` = magic, length, JSON header (hub id, sender, k, scale, sha256 of the body), k int8 values. For k=64
  that is 64 bytes of payload against 2,304 to 8,192 bytes of float32 for a raw vector, and no text. `unpack` refuses
  anything malformed, oversized, for another hub, or whose body does not match its header.
- **Subspace.** The hub keeps what the models' states have in common on the anchors; a direction only one model has
  does not cross (`test_private_directions_do_not_cross`). That is the intended behaviour, and it is also the
  limit: a concept the anchors do not span will not transfer.

What the tests show, and no more: on synthetic models whose states are one shared concept space seen through a
different linear lens each, with private directions and noise, every ordered pair of four models (widths 64 to 224)
transfers a held-out concept at cosine about 0.996, random vectors through the hub score about 0.2, and k below the
true rank loses accuracy (k=10 of 20: 0.72). Real residual streams are not exactly linear images of each other, so
this validates the algebra and the plumbing, not the claim. That needs E8.

## 4. Closed and private models

- **Read-only spoke from an embedding endpoint.** A text-embedding API is an encoder. Fit `enc` from its vectors to
  the hub on the anchors and a closed vendor's model can *send* into the hub with no activation access. It cannot
  receive. (Not built. Needs the vendor's terms checked for storing embeddings.)
- **Embassy.** A small local open model joins the hub and speaks for a closed agent. Closed to hub: the agent writes a
  few sentences describing the concept, the embassy encodes them. Hub to closed: the embassy decodes the vector against
  the anchor corpus and hands the agent a concept card (nearest concepts, a caption, the source hash). The closed model
  reads text, but short, ranked, and with provenance. The gain is compression, routing and de-duplication across
  families, not thought transfer.
- **What stays impossible.** Steering or reading a closed model's activations. Receipts for a closed model's behaviour
  remain host claims.

## 5. Safety of a channel that bypasses text checks

A latent channel is an injection channel the admission broker cannot read. Controls, in order of how much exists:

1. **Strength cap in the receiver's norm units.** Exists for steering specs; the receiver applies the decoded vector at
   a strength the host caps (E7: stronger is also worse, 0.5 best).
2. **Guards on the decoded vector.** The existing guard check (|cosine| ≥ 0.5 with a listed safety direction at that
   site and layer) applies unchanged to the receiver-space vector. Same limit as today: only known directions.
3. **Commit and challenge (proposed, not built).** The sender commits to the hash of the anchor-style sentences it read.
   On audit it reveals them, and the auditor encodes them through *its own* spoke and compares with the claimed hub
   vector (cosine above a calibrated threshold). This works across families, which the E6 recompute audit cannot: that
   one needs identical weights. It audits what the sender said it was sending, not whether the sentences are honest.
4. **Signed envelope.** `PBL1` carries a body hash only. Signing reuses the `playbox.cache-handoff.v1` record format and
   a host operation like `Op::Handoff` (E6 round 6). Not wired.

## 6. E8: the experiment to run next

`python -m playbox_steer.hub_run --model smollm2=P --model qwen3=P --model llama=P --model gemma=P --model gpt2=P --out e8.json`

For each ordered pair and each held-out concept it reports id accuracy for `hub` (int8 on the wire), `pair` (the
ridge map for that exact pair, the baseline), `direct` (the receiver's own direction, the upper bound) and `null`
(random vectors through the hub), plus held-out cosine of hub versus pair maps and the bytes per thought.

Decision rules, set before running:

- The hub is worth keeping if `hub` is within 10 points of `pair` in at least 80% of pairs and above every null.
- k sweep 16, 32, 64, 128 and anchor-count sweep 100 to 426; pick the smallest k within 5 points of the best.
- Families: SmolLM2, Qwen3, a Llama, a Gemma, a Phi, and GPT-2 or Pythia for the other block layouts. Then one 7B to
  show width scaling.
- Join test: fit on four, add the fifth without refitting, and compare its accuracy with a refit-all hub.
- Cost: tokens used by text versus bytes by hub for the same concept, with a text-only arm that gets the same
  sentences.
- Closed arm: a concept card read by a frontier model (the E7 Hermes setup) against the plain sentences, scored the
  same way. Expect no accuracy gain; the question is tokens saved.

What would count as failure: the hub only matches `pair` for the family pairs the lexicon decoder already liked, or
needs k near the model width.

## 6a. First E8 result (2026-11-03)

`steer/results/e8/e8.json`, from `steer/tools/e8_node.py` on a gman H100 (node `playbox-e6-cuda`). fp32, layer at 45%
of depth, k=64, strength 0.5 receiver norms, 24 tokens. Models: SmolLM2-135M (width 576), Qwen3-1.7B-Base (2048),
Pythia-410m (1024), GPT-2 medium (1024), TinyLlama-1.1B (2048). 426 anchor sentences, 150 held out, 5 test concepts x
8 prompts per cell (chance 20%). One cell is 40 texts: a step is 2.5 points and one cell's noise is roughly 8 points.
One run. Ceiling `local_internal_exploratory`.

| mean over ordered pairs | id accuracy, all 20 | id accuracy, 16 not into TinyLlama | held-out cosine, all 20 |
|---|---|---|---|
| hub (64 int8 bytes) | 0.47 | 0.53 | 0.87 |
| pair map for that exact pair | 0.51 | 0.59 | 0.92 |
| receiver's own direction (upper bound) | 0.81 | 0.88 | |
| random vectors through the hub | 0.03 | 0.03 | |

- **The pre-set rule is met only because of TinyLlama.** Hub within 10 points of the pair map in 16 of 20 pairs (rule:
  80%) and above its null in 20 of 20. But TinyLlama is a poor receiver at this layer and strength (its own direction
  reaches 0.53, every pair into it is 0.10 to 0.28), so those four pairs sit near the floor and pass trivially. Among
  the 16 pairs into a receiver that works, the hub is within 10 points in 12 (75%): a miss.
- **The hub costs about 4 to 6 points of accuracy and 5 points of held-out cosine** against a dedicated map, for 10
  adapters instead of 20 maps and 64 bytes instead of 2,304 to 8,192. It beat or tied the pair map in 10 of 20 pairs,
  so the gap is not uniform (it is largest into SmolLM2 from Pythia: 0.38 against 0.73).
- **Both are well below the receiver's own direction** (0.47 and 0.51 against 0.81): the channel carries about 60% of
  what the receiver gets from having read the concept itself.
- **Not tested:** other layers or strengths for TinyLlama, k, anchor count, the join test, concepts other than five,
  seeds. Do not read the ranking of single pairs.

## 6b. k sweep and join test (2026-11-03)

`steer/results/e8/e8_sweep.json`, from `steer/tools/e8_modal.py` on Modal (A10G, fp32). Same models, anchors, concepts,
prompts and strength as 6a; hub arm only, so k=64 below is the 6a hub column. Same noise as 6a: one cell is 40 texts
and one run. The pair map's mean accuracy is 0.51 (0.59 into the 16 receivers other than TinyLlama).

| hub size k | wire bytes | accuracy, all 20 | accuracy, 16 not into TinyLlama | held-out cosine | pairs within 10 points of the pair map (of 20 / of 16) |
|---|---|---|---|---|---|
| 16 | 16 | 0.23 | 0.26 | 0.80 | 4 / 0 |
| 32 | 32 | 0.33 | 0.37 | 0.84 | 7 / 3 |
| 64 | 64 | 0.47 | 0.53 | 0.87 | 16 / 12 |
| 128 | 128 | 0.50 | 0.57 | 0.90 | 17 / 13 |

- **k matters a lot below 64.** At k=16 the channel is close to chance (0.20); k=32 recovers about a third of the way.
  Accuracy follows held-out cosine, which keeps rising with k.
- **Gains flatten from 64 to 128** (+3 points of accuracy, +3 of cosine, better than or equal in 13 of 20 pairs). k=128
  also meets the rule from section 6 where k=64 missed: 13 of 16 (81%) pairs into a working receiver are within 10
  points of the pair map. It is still 128 bytes. Only 426 anchors fit it, so more anchors may be needed before
  going higher; not tested.
- **Joining works.** Fitting the hub on four models and giving the fifth only its own two adapters gave mean accuracy
  0.44 against 0.47 when all five were fitted together, over the 40 ordered pairs that involve the newcomer (8 per
  model), and held-out cosine 0.865 against 0.869. By model, the joiner is lower than the all-five hub for four of five
  (by 1 to 5 points) and higher for TinyLlama (0.37 against 0.35). The accuracy differences are inside the noise of these
  cells; the cosine difference is under half a point. So a new model costs two adapters, not a refit.
- **Not tested:** anchor count; whether joining several models in a row drifts; k above 128; other layers or strengths;
  concepts outside the five.

## 7. Not covered

Concepts outside the anchors; arbitrary thoughts (this is a concept channel); multi-turn exchanges; models smaller than
the anchor corpus can resolve; non-transformer architectures; adversarial senders; any claim above
`local_internal_exploratory`.
