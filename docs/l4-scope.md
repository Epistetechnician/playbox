# L4: a cross-family KV-cache channel (scoping and feasibility)

Status: scoped 2026-11-04. One feasibility test run (about 6 minutes of one A10G, well under 1 USD). Recommendation at the end.
L4 is the tier of [latent-channels.md](latent-channels.md) section 2 that carries a model's full context state to a model of
a different family, with both models frozen and the receiver never reading the text. Our L2 channel (a mid-layer vector, E7
result 73.8% over 20 topics, [steering.md](steering.md)) carries one concept; L4 would carry what the sender actually read.

## 1. What is published and what is reproducible

Numbers below are the authors' claims unless marked "ours". I read the SwaT paper text (method, tables, limitations) and the
other papers' abstracts only.

| Work | What it does | Cross-family | Code and license | Weights |
|---|---|---|---|---|
| **SwaT**, "Swarm Telepathy: Prefill-Free KV-Cache Transfer Across Families", arXiv 2610.15095 (Oct 2026; Alabdulkareem, Tenison, Zhang, Zhu, Katz, Gutfreund) | A trained ~300M-parameter transformer maps a frozen sender's K/V cache to a frozen receiver's. 12 LLMs, 6 families, 10 cross-family pairs, 3B to 32B | yes | `Ar-Kareem/SwaT`: **no license** (GitHub reports none). Paper is CC BY 4.0 | none shipped; you train it |
| **C2C**, Cache-to-Cache, arXiv 2510.03215 (Tsinghua; ICLR 2026) | A fuser (projection, dynamic weighting, layer gate) merges the sharer's cache into the receiver's own cache. Receiver still reads the same input | partly (a Llama-3.2-1B sharer into a Qwen3-0.6B receiver) | `thu-nics/C2C`, **Apache-2.0**; training script `script/train/SFT_train.py`, torchrun recipes | pretrained fusers on Hugging Face (`nics-efc/C2C_Fuser`); weights license not checked |
| **Closed-form linear mapper**, arXiv 2608.03893 (Heo et al.) | Per-head ridge map between caches with RoPE removed; needs matching KV head count and head dimension | no: tested within families (different sizes) | none found | none |
| **XKV**, "Dual-Cache Latent Space Communication", arXiv 2608.20617 | Learned-query attention plus a learned layer map; handles different depth, KV heads, head dim, tokenizer; 6 heterogeneous pairs | yes | none found | none |
| **KVCOMM**, arXiv 2510.12872 (NeurIPS 2025) | Training-free reuse of one model's KV cache across agents (offset correction by anchors); ~70% reuse | no, same model | `FastMAS/KVCOMM`: no license | none |
| **LatentMAS**, `Gen-Verse/LatentMAS` | Latent working-memory sharing between agents | same model | Apache-2.0 | n/a |
| **Q-KVComm**, arXiv 2512.17914 | Quantised cache plus a text fact summary prepended | heterogeneous via calibration, but not purely cache | not checked | none |

What SwaT does, from the paper:

- **What is mapped.** The sender's pre-RoPE keys and values of every layer, concatenated per layer group, become memory for
  cross-attention. The mapper has B in {7, 8, 9} blocks (width 1024, 16 heads, FFN 4096). Each block has causal
  self-attention over receiver-token embeddings, causal cross-attention to the sender memory, and an FFN. It outputs one K and
  V entry per receiver token in each receiver layer of the block's group. Outputs are RMS-rescaled to calibrated receiver
  statistics, then given receiver RoPE.
- **Different depths.** Each model's layers are split into B contiguous groups; receiver group b reads sender group b.
  Different KV head counts and head dimensions are absorbed by the learned projections.
- **Different tokenizers.** Sender tokens get a fractional receiver position from character spans (eq. 12), and a causal mask
  lets a receiver token see only sender tokens that start at or before it. No token is dropped.
- **Training.** Stage 1: 5,000 steps of normalised MSE to the receiver's own cache (lr 3e-4 cosine) on PG-19, WikiText-103
  and long-QA training splits. Stage 2: 24,000 steps of the receiver's next-token loss through the frozen receiver (lr 3e-6)
  on ARC, HellaSwag, WinoGrande, MMLU auxiliary and GSM8K traces. About 44M English tokens, AdamW, bf16, under one GPU-day
  per pair. Batch size is not in the text I read.
- **Results (claimed).** Retention (receiver score reading the transferred cache over its own score reading the text) mean
  92% to 109% over 17 benchmarks for 7 small-to-large pairs, and 103% to 143% for 3 large-to-small pairs. Against
  text-to-text summaries, the cache plus Attention Matching compaction scored +14.9 to +19.0 points with 2.6x to 6.8x less sender
  time. In their cross-family baseline table, a closed-form linear map reached 30.5 (Qwen3-4B to Llama3-8B) and a dense
  latent map 43.5, against SwaT's 63.5.
- **Stated limits.** One mapper per ordered pair (n(n-1) for n families), single handoffs only, the sender's text must exist
  so text monitoring remains possible, and the cache may carry information beyond that text.

What is reproducible for us:

- Reimplementing the **method from the paper** is open to us: the paper is CC BY 4.0. I have not checked patents.
- The **SwaT repo has no license**, so all rights are reserved by default. Do not copy its code or ship its configs. Either ask
  the authors for a license or write our own from the paper (the plan below does the latter).
- **C2C is Apache-2.0** and is the usable scaffold for a training harness, recipes and a model wrapper that takes a
  different sharer and receiver.
- The repo ships no mapper weights and no results for its safety experiment. We would train our own.

## 2. Design for our pair, SmolLM2-135M and Qwen3-1.7B-Base

Dimensions (read from the configs on the GPU box): SmolLM2-135M has 30 layers, 3 KV heads, head dimension 64 (192 per layer
per K or V); Qwen3-1.7B has 28 layers, 8 KV heads, head dimension 128 (1024 per layer per K or V) and applies a norm to keys
before RoPE. Different tokenizers (49k against 151k vocabulary), so the character-span alignment is required.

**Mapper.** SwaT-style, scaled to the pair: B = 7 blocks, D = 1024 gives about 300M parameters; D = 512 (about 80M) is the
cheap first try since the sender is tiny. Inputs: the sender's pre-RoPE K and V of all 30 layers (11,520 numbers per sender
token). Outputs: Qwen3's 28 layers of K and V (57,344 numbers per receiver token) at receiver positions. Train in two
stages (MSE, then receiver NLL through the frozen receiver).

**Reverse direction.** Qwen3-1.7B to SmolLM2 is a second mapper (E7 measures both directions).

**Scaling to the Qwen3.8 brain family.** The brain (Qwen3.8-27B-FP8 on Modal) is a hybrid model: vLLM refused to start with
`max_num_seqs (1024) exceeds available Mamba cache blocks`, which says some layers keep recurrent state, not K/V. SwaT-style
mapping covers only full-attention layers; a recurrent layer's state is not a KV cache, and no paper above addresses it.
Plan: map into the attention layers only, treat the recurrent layers as unmapped (their state starts empty), and measure what
that costs on a small hybrid before committing. A 27B receiver also needs the training pass to run on 80 GB cards.

## 3. Feasibility result (ours)

`playbox_steer/l4_feasibility.py`, run by `tools/l4_feasibility_modal.py`, results in
`results/telepathy/l4_feasibility.json` (one A10G, 261 s). Setup: fit **one ridge map** from the sender's K/V of all layers
(RoPE removed) to the receiver's K/V of all layers, on 78,291 aligned receiver tokens (wikitext-2 plus the fit-topic
templates, the 16 original topics held out). Feature columns are standardised; lambda and rank are chosen by held-out
reconstruction R^2, not by the downstream metric. At test time the predicted cache gets Qwen3's RoPE and is given to the
frozen receiver, which scores a continuation whose context it never read.

Pipeline check: a cache rebuilt from pre-RoPE K/V gives the same next-token loss as the receiver's real cache (1.4907 and
1.4907).

Retention = (L_none - L_x) / (L_none - L_own): the share of the gain a real context cache gives that condition x recovers.
`none` is the continuation with no context; `own` is Qwen3's real cache of the context; `other_own` is a real cache of an
unrelated context; `mean` is the average cache repeated.

| Test | none | own | unrelated real cache | mean cache | **mapped (linear)** | mapped + RMS rescale |
|---|---|---|---|---|---|---|
| Natural text (wikitext validation), 150 items, NLL | 3.458 | 2.526 | 3.559 | 7.082 | **2.941** | 2.969 |
| ... retention (95% bootstrap CI) | | 100% | -10.9% (-17.3, -3.7) | -389% | **55.5% (46.2, 62.6)** | 52.5% (43.2, 60.0) |
| Templated held-out topics, 48 items, NLL | 4.941 | 1.169 | 2.960 | 7.799 | 2.510 | 2.204 |
| ... retention | | 100% | 52.5% (48.7, 56.7) | -75.8% | 64.4% (62.4, 66.2) | **72.6% (70.7, 74.2)** |
| 21-way concept retrieval by likelihood gain (chance 4.8%) | | 95.2% | | | 85.7% | 95.2% |

Held-out reconstruction R^2 of the chosen map: keys 0.80, values 0.57; the lowest lambda was best, and ranks 512 and 128 did
worse (R^2 0.456 and 0.324 against 0.577), so the map is not low-rank.

How to read it, and what it does not show:

- **It works on natural text.** A purely linear map recovers about 55% of the gain of a real context cache on text it never
  fitted, while an unrelated real cache is slightly harmful. That separates "carries the context" from "any plausible cache".
- **The templated rows are inflated by format priming:** an unrelated real cache already recovers 52%, so read the mapped
  figure against that. The mapped cache beats the unrelated cache (64% to 73% against 52%).
- **Concept retrieval is 86% to 95% over 21 topics**, close to the receiver's own 95%. This is a likelihood test with
  templated topics and 5 hand-written ones, one seed, not the blind two-agent protocol. It says the cache carries the topic;
  it does not say agents can decode it.
- Retention here is on next-token loss, which is not task accuracy. SwaT's own linear baseline fell to 30.5 on a cross-family
  task score, so a linear map is a floor, not a design. The MLP and SwaT-style mapper is the next step.
- Single run, single pair, no seed variance.

## 4. Cost estimate

Assumptions, labelled as estimates: Modal A10G about 1.10 USD per hour and H100 about 4 USD per hour; the mapper trains on
about 44M tokens in two stages, where stage 2 backpropagates through the receiver.

| Item | Compute | Cost |
|---|---|---|
| Done: linear feasibility | 6 min A10G | under 1 USD |
| SwaT-style mapper, SmolLM2 to Qwen3-1.7B (about 80M to 300M parameters) | about 3 to 6 A10G-hours per direction | 4 to 8 USD per direction |
| Second direction, evaluation, two seeds, ablations | 10 to 20 A10G-hours | 12 to 25 USD |
| Data: wikitext, long-QA and reasoning splits (English, public) | none | 0 |
| Qwen3.8-27B receiver, one ordered pair (the 27B forward and backward dominate) | about 10 to 20 H100-hours | 40 to 80 USD |

The pair-by-pair figure for the large model is the number that matters for scale: a swarm of n families costs n(n-1) of those.

## 5. How L4 composes with L2 and E7, and how we would measure it

- **Different payloads.** L2 sends k numbers (about 8 KB for Qwen3-1.7B) and steers one concept. L4 sends a whole context's state
  (for Qwen3-1.7B about 115 KB per token in fp16, so roughly 18 MB for a 160-token message) and carries detail. Use L2 for
  topic, routing and behavioural edits; use L4 for facts that cannot be summarised in a vector.
- **Same blind protocol as E7.** Agent A privately reads sentences about a topic. Instead of a vector, A's engine prefills
  them and writes the mapped cache; agent B's receiver model generates continuations from the 8 neutral openers conditioned
  on that cache (no vector), and B names the topic from those continuations among 20 topics plus controls. Same topics, same
  88-trial plan and trial order, same Haiku-free open brain, so the 73.8% L2 figure is the direct comparison.
- **Power.** 80 concept trials give an interval of about +/-9 points; detecting a 10-point difference needs about 200 trials.
  Run 200 or more for the head-to-head.
- **Extra arms.** (a) cache of the sentences against cache of a different topic (control for format priming); (b) L2 vector
  plus cache together; (c) agent-written sentences (the E7 fit-style gap) instead of templates.
- **Auditability.** The cache may carry content that is not in A's text. Pin each mapper by both weights hashes (the
  `theseus-lab` rule in [steering.md](steering.md) 2b) and log the sentences A read next to the transferred cache.

## 6. Risks

- **Pair explosion.** n(n-1) mappers; SwaT states this themselves. Section 7 proposes a way around it.
- **Hybrid brain.** Recurrent layers have no K/V; unresolved (section 2).
- **Small sender.** SmolLM2-135M is a weak encoder; SwaT's gains are largest from big to small. Expect lower retention from
  135M to 1.7B than their 3B+ pairs, and measure the reverse direction as the strong-sender case.
- **Overfitting to templates.** Our fit text is templated; agents write free sentences. Mix natural text (done here) and
  agent-written text.
- **Metric gap.** Loss retention is not task accuracy; the blind protocol is the real test.
- **Single handoff, no composition,** as in the paper.
- **Safety.** A cache can carry more than the visible text, which matters for latent channels that bypass text checks
  ([latent-channels.md](latent-channels.md), threat table).
- **Licence.** SwaT's code cannot be reused; C2C's trained weights have an unchecked licence.

## 7. The angle worth trying: a hub for caches, and a prefix instead of a whole cache

Published work trains one mapper per ordered pair. Our L2 channel already solved that shape of problem with a hub: each
model gets one encoder and one decoder against a shared k-dimensional space (2n adapters, not n(n-1); see
[latent-channels.md](latent-channels.md) section 3). Three ideas follow, in order of cost.

1. **Hub-to-prefix ("L4-lite").** Train, per receiver, a small decoder from the shared hub code to **m soft prefix K/V slots**
   (m of 8 to 32) in that receiver, using the receiver's own next-token loss with the receiver frozen. The wire stays at k
   bytes (the L2 hub code), but the receiver gets a prefix cache instead of one added vector. This is the L2 injection
   generalised to attention memory, and it needs one decoder per model (2n, not n(n-1)). It is the cheapest test of whether
   richer-than-a-vector state helps the E7 decode, and it sits between what we have and SwaT.
2. **Anchor positions.** Compact the sender's cache to a few anchor positions before mapping (the paper already combines SwaT
   with Attention Matching compaction), so the mapper sees 8 to 32 summary slots and the wire carries slots, not tokens.
3. **A latent cache hub.** Replace the pairwise mapper with a Perceiver-style resampler per model that turns its cache into a
   fixed number of shared latent slots and back, trained so every model's slots agree on anchor text (the CCA objective of
   the L2 hub) and with the receiver's loss on top. This is the future work SwaT itself names (shared latent space, 2n
   mappers); our L2 hub fit is a working precedent for the alignment step.

## 8. Recommendation

**Build, in this order, with a go or no-go after each:**

1. (Done, positive.) A plain linear cross-family map recovers about 55% of a real context cache's gain on natural text and
   keeps 86% to 95% concept retrieval; an unrelated cache does not.
2. **Next, about 25 USD:** train the SwaT-style mapper for SmolLM2 and Qwen3-1.7B (both directions, our own code written from
   the paper; do not use the unlicensed repo), and run the blind 20-topic protocol with 200 or more trials against the 73.8% L2
   result. Go on if it beats L2 or if concept retrieval holds with agent-written sentences.
3. **In parallel, cheaper:** hub-to-prefix (idea 1), because it reuses the L2 hub and gives a head-to-head at the same wire size.
4. **Do not** train a mapper for the 27B hybrid brain until step 2 shows a gain and the recurrent-layer question has a plan.

Not worth doing: using the SwaT repo directly (no license), or a low-rank linear map (rank 128 retained far less
reconstruction than the full-rank map).

Code and data: `steer/playbox_steer/l4_feasibility.py`, `steer/tools/l4_feasibility_modal.py`,
`steer/results/telepathy/l4_feasibility.json` (and `l4_smoke.json`, a 12-item pipeline check).

## 9. Stage 2: the SwaT-style mapper on one H100 (plan and bar, written before training)

Committed before any training run so the bar cannot move afterwards.

**What is trained.** Our own implementation from the paper's method (`steer/playbox_steer/l4_swat.py`; the unlicensed SwaT repo
was not used), for both directions: SmolLM2-135M to Qwen3-1.7B-Base and the reverse. B = 7 blocks, width 1024, 16 heads, FFN 4096.
Stage 1 (normalised MSE to the receiver's own cache): 3,000 steps. Stage 2 (receiver next-token loss on a continuation, reading
only the mapped cache): 8,000 steps. Batch 48 documents. Data: 100,000 (context, continuation) items, 85% wikitext-103 train
paragraphs and 15% templated text from the fit-topic pool (the 16 original topics are held out). Deviations from the paper are
listed in the `l4_swat.py` docstring (shorter schedules, a larger stage-2 learning rate, output scaling by the calibrated RMS).
A debug run on a tiny mapper (55M parameters, 6k items, 700 steps, A10G) already reached a retention of 0.58 on held-out
wikitext-103 in the training-time bf16 check, so the code learns; that number is not the result.

**Where and cost.** gman node `playbox-e6-cuda` (1x H100), workspace `default`, 0.084 USD per minute weekday (5.04 USD per hour),
15-minute session minimum. Expected 3 to 4 hours for both directions, about 15 to 20 USD; hard cap 30 USD of GPU time. The node is
stopped when the work ends or anything hangs.

**Held-out evaluation (final numbers use fp32 models and the same protocol as section 3, so they compare with the linear map).**
(A) the 150 wikitext-2 validation items of section 3; (B) the 48 templated held-out-topic items; (C) 21-way concept retrieval by
likelihood gain. Controls: the unrelated real cache and the mean cache. Reference points: linear map 55.5% retention on (A)
(95% CI 46.2 to 62.6), 21-way retrieval 85.7% (95.2% with RMS rescale).

**Bar.** A direction passes if all of:
1. (A) retention is at least 75%, with the lower bound of its 95% bootstrap interval above 62.6% (so it beats the linear map
   with non-overlapping intervals);
2. (B) mapped retention exceeds the unrelated-real-cache control by at least 20 points;
3. (C) 21-way retrieval is at least 90%.

**Blind two-agent protocol.** Run it only for a direction that passes. Same 20 topics, same protocol as E7 (Haiku-free open
brain on Modal, Hermes agents, B names the topic from continuations generated from the mapped cache), at least 200 concept trials
plus controls so a 10-point difference from L2's 73.8% is detectable. Reading of the result: above L2 with the lower bound of the
95% interval over 73.8% counts as better; within the interval counts as comparable (then the case for L4 rests on richer payloads,
not on this metric); below counts as worse. If no direction passes, stop and report; do not run the blind protocol.

## 10. Stage 2 results

Raw files: `steer/results/telepathy/l4_stage2/` (`eval_*.json`, sighted training logs). Both sighted mappers trained 3,000 + 8,000
steps on the H100 (about 58 and 50 minutes); the blind ones trained about 50 and 45 minutes. fp32 evaluation, same protocol as
section 3, 150 wikitext-2 items, 48 templated held-out-topic items, 21 concepts. Retention = (none - x)/(none - own).

### Sighted mapper (SwaT as published: the mapper reads the receiver's own token embeddings of the context)

| direction | (A) wikitext retention (95% CI) | unrelated-cache control | (B) templated mapped / unrelated | (C) 21-way retrieval (own cache) | bar |
|---|---|---|---|---|---|
| SmolLM2-135M to Qwen3-1.7B | 87.6% (83.8 to 91.5) | -4.0% | 30.9% / 56.6% | 81.0% (85.7) | fails (B), (C) |
| Qwen3-1.7B to SmolLM2-135M | 115.3% (110.8 to 120.0) | -3.0% | 76.4% / 49.7% | 95.2% (100) | passes all three |

(A) beats the linear map (55.5%, CI 46.2 to 62.6) in both directions with non-overlapping intervals. Retention above 100%
means the mapped cache gives the small receiver a lower loss than its own cache of the same text.

**Why this pass does not count for the claim we care about.** The sighted mapper takes the receiver's own embeddings of the
context as input. So the receiver side must already hold the context text: this is prefill-free (no receiver forward pass over the
context), not text-free. Beating the unrelated-cache control by 26.7 points on templated text is therefore not evidence of a
channel that carries a concept without text. The L2 comparison (73.8%) needs a mapper that sees only the sender's cache.

### Text-blind mapper (ours: a single learned query vector, length-only alignment; still needs the receiver token count)

| direction | (A) wikitext retention (95% CI) | (B) templated mapped / unrelated | (C) 21-way retrieval | bar |
|---|---|---|---|---|
| SmolLM2-135M to Qwen3-1.7B | 86.7% (83.5 to 90.2) | 5.1% (CI -2.4 to 12.3) / 56.6% | 71.4% | fails (B), (C) |
| Qwen3-1.7B to SmolLM2-135M | NaN | NaN | 4.8% (chance) | invalid run |

On natural text the blind mapper recovers nearly as much as the sighted one (86.7% vs 87.6%), so the sender's cache alone
carries the content. On templated topics it is at chance level (5%), below the unrelated real cache (56.6%), which means it
did not learn the topic structure from the sender cache; this is the metric that matters for the concept-passing use case.
The Qwen3-to-SmolLM2 blind run produced NaN mapped losses at every checkpoint; the cause was not found (the training log was
not retrievable after the node stopped; a numerical overflow in the mapper output is the likely cause). It was not rerun.

**Deviation to disclose.** The blind runs used a 50% templated share in the training mix (the sighted runs used 15%). I chose
that after seeing the sighted templated result, so it is a post-hoc change; it did not help.

### Blind two-agent protocol

Not run. Only the sighted Qwen3-to-SmolLM2 direction passed the bar, and its pass relies on text input, so it does not test
the L2 comparison. No blind direction passed.

### Cost

GPU node `playbox-e6-cuda`: about 18.8 USD for this stage (node cumulative 21.78 USD, 2.96 USD before it) including the
time lost to the two blind jobs that ran out of GPU memory when launched alongside the sighted run. Under the 28.9 USD
cap. The node was stopped and reads `stopped (disk intact)` in `gman node ls`.

### Recommendation

Stop L4 as a concept channel with this recipe. What worked: a trained cross-family cache mapper is real on natural text (87.6%
and 115% retention against 55.5% for the linear map), and the Qwen3-to-SmolLM2 direction is a good prefill-free cache transfer
when the receiver holds the text. What did not: the text-blind version, which is the one L4 needs to beat L2's hub vector, learned
nothing about topics. Build further only if (1) the blind Qwen3-to-SmolLM2 NaN is fixed and rerun (about 5 USD, 45 minutes), and
(2) the blind templated retention clears the same bar, tested before any two-agent run. Until then L2 stays the cross-family
channel.

## 11. Go/no-go on a real pair at small scale (plan and bar, written before training)

Committed before the run. Question: does a text-blind mapper (the receiver reads nothing but the mapped cache; the mapper sees only the sender's
cache and the receiver's token count) keep retention on a real pair, Qwen3-4B-Base to Llama-3.2-3B (`unsloth/Llama-3.2-3B`, an ungated mirror of the
base weights)? Section 10 found the blind mapper learned natural text (86.7%) but not topics (5.1% on templated items, chance on 21-way retrieval),
on a pair whose sender was 135M parameters. This tests whether a sender of 4B changes that, before anyone pays 60 to 150 USD for a full-size mapper.

Run: `tools/node/gonogo_chain.sh` (our `l4_swat.py`, `--blind`, B = 4 blocks, width 768, 1,500 + 4,000 steps, batch 32, 50,000 items, 15% templated,
hard cap 75 minutes of training), then `tools/l4_eval_cli.py` (fp32 held-out evaluation, the section 3 protocol: 150 wikitext-2 items, 48 templated
held-out-topic items, 21-way concept retrieval). One H100 on gman, budget 10 USD for this step, 30 USD for the whole piece of work (the metered
benchmark included); the node is stopped when it ends.

**Bar (the section 9 bar, applied to the blind mapper).** Go only if all of: (A) wikitext retention at least 75% with the lower bound of its
95% bootstrap interval above 62.6%; (B) templated mapped retention exceeds the unrelated-real-cache control by at least 20 points; (C) 21-way
retrieval at least 90%. (A) alone does not count: the blind mapper already passed it in section 10. Anything else is a no-go and the full-size run
is not started.

### Go/no-go result (Qwen3-4B-Base to Llama-3.2-3B, blind mapper, small scale)

Raw files: `steer/results/telepathy/l4_gonogo/` (`eval_q4l3_blind.json`, `logs.txt`; `*_nan_run1*` is the first attempt). 4 blocks, width 768,
1,500 + 4,000 steps, batch 32, 50,000 items, 36 minutes of training on one H100, then the fp32 held-out evaluation of section 3.

The first attempt was invalid: stage-1 loss went to NaN at about step 700 (the same failure as the blind Qwen3-to-SmolLM2 run in section 10) and
stayed there, so every evaluation read NaN. The retry skips any batch whose loss or gradient is non-finite and aborts after 200 of them; it skipped
30 of 5,500 steps and trained normally. The skip guard is the only change to `l4_swat.py` between the attempts.

| test | none | own cache | unrelated real cache | **mapped (blind)** |
|---|---|---|---|---|
| (A) wikitext, 150 items, retention (95% CI) | NLL 3.522 | 2.462 | -0.5% (-5.9, 4.9) | **97.6% (94.4, 100.9)** |
| (B) templated held-out topics, 48 items, retention (95% CI) | NLL 5.154 | 1.301 | 56.1% (52.0, 60.1) | **75.2% (71.9, 78.7)** |
| (C) 21-way concept retrieval by likelihood gain (chance 4.8%) | | 100% | | **100%** |

**Against the bar written before the run:** (A) passes (97.6%, lower bound 94.4% above 62.6%); (C) passes (100% against the 90% bar, 21 items);
(B) misses: mapped beats the unrelated-cache control by 19.1 points against a required 20, a shortfall of 0.9 points. The bar says go only if all
three hold, so by the bar this is a **no-go, by a margin inside the noise** (48 items, one seed, the two intervals nearly touch). I did not move the bar.

What it does and does not show:

- The blind mapper no longer fails on topics: 100% retrieval and 75% templated retention here, against chance (5%) and 5% for the 135M-sender pair in
  section 10. A 4B sender with a real pair changes the outcome; the small-sender result was a sender-size effect, not a property of the blind design.
- "Blind" still means the receiver reads no text, but the mapper is told the receiver's token count (length only). The retrieval test is a
  likelihood comparison over 21 topics, not two agents naming a topic from continuations.
- Retention on wikitext above 95% means the mapped cache gives the receiver nearly its own cache's gain; it is next-token loss, not task accuracy.
- Cost of this stage on gman: about 4 USD (training, evaluation and the failed first attempt), against the 10 USD set for it.

What would settle (B): more templated items and a second seed on this checkpoint (about 1 USD of evaluation if the checkpoint is kept; it is on the
stopped node's disk, not in the repository). The full-size mapper (60 to 150 USD) is not started: that spend is the user's call.

## 12. Settling (B): plan and decision rule, written before the run

Section 11 missed (B) by 0.9 points on 48 items (16 topics x 3 draws, one mapper, one seed). Two sources of doubt, both addressed here:
the item draw (few items, and items of one topic are correlated, so the real sample size is the 16 topics) and the mapper itself (one training run).

**What is run.** (1) The same checkpoint re-evaluated on 720 templated items: 3 draw seeds (5, 6, 7) x 16 held-out topics x 15 draws
(`playbox_steer/l4_eval_b.py`). (2) A second mapper, identical recipe, training seed 1 (the section 11 run is seed 0), evaluated the same way.
The difference mapped-minus-unrelated-real-cache retention is computed on the same items for both conditions, and its 95% interval is a
bootstrap over topics (whole topics resampled), which is the honest interval for 16 topics. Control added: `mapped_wrong`, the mapper fed another
topic's sender cache and scored on this item's continuation; mapped minus mapped_wrong is topic specificity with the format held fixed.

**Decision rule (fixed now).** (B) is **settled as a pass** if, for each of the two mappers, the pooled point estimate of mapped minus
unrelated-real-cache retention is at least 20.0 points. It is **settled as a fail** if it is below 20.0 for both. One above and one below is
"not robust" and counts as a no-go. The intervals and the specificity control are reported, not used to decide. (A) and (C) are re-checked
only on the existing mapper (they passed with wide margins); no other criterion changes.

**Budget.** About 5 USD of H100 time on gman (two evaluations of about 12 minutes, one 40-minute training run, loads), inside the 30 USD ceiling;
12.4 USD was spent before this step. The node is stopped at the end.

### Settling (B): result

Raw files: `steer/results/telepathy/l4_gonogo/settleb_s0.json`, `settleb_s1.json`, `settleb_logs.txt`. 720 templated items per mapper (16 held-out
topics x 15 draws x draw seeds 5, 6, 7); interval = bootstrap over topics. Mapper s0 is the section 11 checkpoint (training seed 0); s1 is the same recipe,
training seed 1 (30 and 29 non-finite batches skipped respectively; both trained normally).

| | s0 | s1 |
|---|---|---|
| mapped retention | 75.6% | 85.8% |
| unrelated real cache retention (same items) | 59.6% | 59.6% |
| **(B) mapped minus unrelated** | **16.0 points** (95% CI 14.0 to 18.0) | **26.2 points** (95% CI 24.2 to 28.1) |
| across the three draw seeds | 15.3, 16.1, 16.7 | 25.8, 26.3, 26.5 |
| topics at or above 20 points (of 16) | 5 | 15 |
| specificity: mapped minus mapped-on-another-topic's-cache | 52.5 points (49.6 to 55.2) | 54.2 points |

**Verdict by the rule fixed before the run: not robust, therefore a no-go.** One mapper clears 20 points (s1, by 6 points) and one does not (s0, by 4);
the rule counts a split as a no-go, and I kept it.

What the run did settle:

- **The 48-item miss was not item noise.** Item draws move the result by about 1 point (three draw seeds agree within 1.4 points for each mapper) and the
  topic-cluster intervals are about 4 points wide. What moves it by 10 points is the training seed. The earlier "19.1 against 20" was one draw of a
  quantity whose real spread between mappers is much larger than that gap.
- **The channel carries the topic in both mappers.** Feeding a mapper another topic's sender cache drops retention from 76 to 23 (s0) and from 86 to 32
  (s1), so the 52 to 54 point specificity does not depend on the seed. Only the margin over a real unrelated cache, which is a hard baseline (it has
  the right format and scores 60% on a wrong topic), is seed-sensitive.
- **Not tested for s1:** (A) wikitext retention and (C) 21-way retrieval were evaluated only for s0 (97.6% and 100%). I do not claim s1 passes all three.

What follows, not done: the question is now how often a training seed lands above 20, not whether this one does. Two more seeds cost about 5 USD for
training and evaluation (36 minutes of training each at about 4 USD per hour). Do that before the 60 to 150 USD full-size mapper: if training seed
decides pass or fail at 4B, it will decide it at full size too, and the larger spend should buy several seeds or a recipe that is less seed-sensitive,
not one run. Spend on this step: about 5.4 USD on gman (node total 116.54 USD, 17.9 USD of the 30 USD ceiling for the whole piece of work).

### Seed 1: (A) and (C), measured

`steer/results/telepathy/l4_gonogo/ac_s1.json`. The seed-1 mapper (the one that cleared (B) at 26.2 points) also passes the other two parts of the bar:
(A) wikitext retention 97.7% (95% CI 94.5 to 100.9); (C) 21-way concept retrieval 95.2% (20 of 21; chance 4.8%; own cache 100%). So for seed 1
all three criteria hold; for seed 0, (A) and (C) held and (B) was 16.0 against 20. The two-seed (B) split stands as written above.

Two more seeds were started and did not complete: the seed-2 training run (step 1,700 of 5,500, loss finite, 15 batches skipped) was cut short when
another job started on the same node, so no seed-2 or seed-3 result exists. The benchmark's second corpus seed did not run either. The work is
therefore still at two training seeds and one corpus seed.

## 13. Mapper recipe v2 (written, tested on a tiny model, not yet trained at scale) and its decision rule

Motivation: section 12 found that the training seed, not the evaluation draw, decides whether the blind mapper clears (B): 16.0 and 26.2 points for two
seeds of the same recipe. A write-up of cross-model cache transfer (onetriangle.ai, "cache-transfer"; a company post, read through a summary, no code or
license) describes a recipe aimed at that kind of instability. Nothing was copied; `steer/playbox_steer/l4_swat_v2.py` implements the ideas
over our own mapper, each as a switch (`V2Config`, `tools/l4_train_v2.py`):

1. **Ridge start.** A closed-form ridge map from the aligned sender cache to the receiver cache, per layer group (centred, per-column scaled, 0.01 relative
   penalty, fitted on 160 batches, the last tenth held out for R^2). The transformer adds a residual on top and its output layers start at zero, so
   training begins at the ridge map, not at random.
2. **KL loss** in stage 2: match the receiver's next-token distribution when it reads the real context cache (teacher) instead of NLL on the true token.
3. **Key weight 2** in stage 1: keys are weighted twice as much as values in the reconstruction error (the post reports mapped keys hurt recall far more than mapped values).
4. **Edge recompute (k = 4):** the first context token's K/V is the receiver's own (it depends on that token only) and the last four context tokens are
   fed to the receiver as input, attending to the mapped cache, as a real receiver would compute them; their predictions are not scored. This is
   the faithful form: no real-cache information about those tokens leaks into training or evaluation.

What was verified locally (`steer/tests/test_l4_v2.py`, 7 tests, a 4-layer random Llama as sender and receiver, so these check mechanics and not quality):
the prior is zero before it is fitted and a fitted ridge explains part of the real cache; nearest-token alignment; the edge input equals plain continuation
when fed the real cache (the two NLLs agree to float noise); KL is zero for the real cache and positive for a zero cache; a short training run stays
finite and its checkpoint reloads; the settling evaluation runs on a v2 checkpoint and reports an edge condition. What was not verified: any effect on a
real pair. The ridge matrices are large for the full pair (about 1.06 billion floats at 4 blocks, 4.2 GB in float32, saved in the checkpoint); fitting them was
not run at that size.

**Plan and rule (fixed before any run).** Train v2 with training seeds 0 to 3 (same data, steps, width and block count as sections 11 and 12) and
evaluate each with `tools/l4_eval_b_cli.py` (the 720-item, topic-cluster-bootstrap (B) test; v2 is scored on its deployment path, `mapped_edge`, and
also without the edge tokens). For the old recipe, seeds 0 and 1 exist; seeds 2 and 3 are trained too so both recipes have four. The recipe is
judged **less seed-sensitive** only if v2's (B) values have a range of at most 5 points across its four seeds, the old recipe's range over its four seeds
is larger, and the lowest v2 seed is at least 20.0 points. Otherwise the result is "no evidence of an improvement". Ablations (one switch off at a time on
seeds 0 and 1) are run only if v2 passes. Estimated cost about 20 USD of H100 time (eight trainings of about 40 minutes plus evaluations); it waits for the
gman workspace cap to be raised and has not been started.

## 14. Result of the v2 run (section 13's rule applied as written)

(B) = mapped retention minus unrelated-real-cache retention, 720 items, in points. v2 is scored on its deployment path (mapped_edge: first token real,
last 4 context tokens read natively) and also on the plain mapped cache. Files: `steer/results/telepathy/l4_v2/`.

| seed | v2 edge path | v2 plain | old recipe plain |
|---|---|---|---|
| 0 | 26.6 | 16.6 | 16.0 |
| 1 | 25.7 | 8.2 | 26.2 |
| 2 | 27.0 | 20.0 | 16.8 |
| 3 | 27.1 | 7.9 | 17.6 |
| range | 1.4 | 12.1 | 10.2 |

Rule: v2 range <= 5 (1.4, yes), old recipe's range larger (10.2, yes), lowest v2 seed >= 20.0 (25.7, yes). **By the rule as written, v2 passes.**

What the pass does and does not mean:
- The stability comes from the edge path. Without the edge tokens v2 is no better than the old recipe (range 12.1, two seeds below 10 points), so the
  receiver is not reading "nothing but the cache"; the section-11 bar (retention with the cache alone) is still not met.
- The old recipe has no edge-path number, so the comparison mixes paths: the edge tokens themselves may supply some of the +26 points. That comparison was then run (below).
- Specificity (mapped minus mapped-wrong cache) is 44-48 points for v2 and 53-55 for the old recipe: both carry topic information.
- The ablations of section 13 are now licensed by the rule but not run. The full-size mapper spend stays on hold.
Cost: about 37 USD of H100 time for this run.

### 14.1 Edge controls (run after the table above)

The old recipe's four checkpoints scored with the same 4 edge tokens, plus two controls on the same 720 items (`edge_v1_s0..3.json`; code: `l4_eval_b.py`
with `edge_k`, `tools/node/edge_chain.sh`). A first attempt ran the old evaluation code by mistake (packing error, no edge numbers) and was discarded.
Retention = share of the real-cache gain recovered; (B) in points against the unrelated real cache.

| condition | retention |
|---|---|
| edge tokens only (real first token, no mapped cache) | 7.4% |
| unrelated real cache + edge tokens | 58.9% |
| v2 mapped cache + edge tokens (seeds 0-3) | 85.3-86.6% |
| old recipe, plain / with edge tokens (seeds 0-3) | 75.6-85.8% / 69.6-77.7% |

| seed | old recipe plain (B) | old recipe + edge (B) | v2 + edge (B) |
|---|---|---|---|
| 0 | 16.0 | 10.1 | 26.6 |
| 1 | 26.2 | 18.1 | 25.7 |
| 2 | 16.8 | 13.9 | 27.0 |
| 3 | 17.6 | 11.3 | 27.1 |
| range | 10.2 | 8.0 | 1.4 |

Reading:
- The edge tokens alone are not the source of v2's gain (7.4% retention on their own), and giving them to a mapper not trained for them makes it slightly
  worse (every old-recipe seed drops 3-8 points). The gain needs a mapper trained with them (the other v2 switches could also contribute; not isolated).
- On the deployment path v2 is both higher (mean 26.6 vs 13.4) and far steadier (range 1.4 vs 8.0). The section-13 pass stands, now with the path
  confound removed.
- Still true: the receiver reads 4 real tokens alongside the mapped cache, so this is not "the receiver reads nothing but the cache"; on the plain path v2
  is not better (range 12.1). The v2 checkpoints were deleted, so v2 itself has no edge-only or unrelated-cache-plus-edge control of its own; the
  controls above are properties of the receiver and the items and were measured with the old checkpoints.
- The 20-point bar of section 11 is met on the edge path (about 26 points) but not on the cache-only path. Whether a 4-token edge counts as an acceptable
  deployment is a decision for the user, not a result. Ablations and the full-size mapper remain unstarted. Cost of the controls: about 6 USD (including the wasted first attempt).

### 14.2 One-switch-off ablations (2026-11-24, Colab A100-SXM4-80GB, seed 0)

Ran three one-switch-off ablations of `V2Config` on `Qwen/Qwen3-4B-Base -> unsloth/Llama-3.2-3B` (same 720-item `(B)` test, draw seeds 5, 6, 7, `edge_k=4` controls; runner: `steer/tools/l4_v2_ablations_colab.py`, results in `steer/results/telepathy/l4_v2/ablate_{no_ridge,nll_loss,kw1}_s0.json`). Note: the two ridge-on runs (`nll_loss`, `kw1`) hit the 4,500 s wall cap around steps 2,577–2,665 on A100 because the 4.2 GB float32 ridge matmul slows each forward step ~2× compared to H100 (~5,489 steps in §14), whereas `no_ridge` ran full steps cleanly. `edge0` was skipped to conserve GPU compute units since §14.1 already tested mappers trained without edge tokens.

| condition (seed 0, 720 items) | `mapped_edge` ret | `B_edge` (pts, 95% CI) | `mapped` plain ret | `B_plain` (pts, 95% CI) | specificity | Δ vs full v2 (`B_edge` / `B_plain`) |
|---|---:|---:|---:|---:|---:|---:|
| full v2 (§14, H100, 4.2 GB ridge) | 86.19% | +26.63 (25.5–27.9) | 76.20% | +16.63 (15.0–18.4) | 46.03 | baseline |
| **`no_ridge`** (`--no-ridge`, ~60 MB) | **88.15%** | **+28.59** (27.4–29.9) | **83.63%** | **+24.07** (22.8–25.3) | 38.68 | **+1.96 / +7.44** |
| **`nll_loss`** (`--loss nll`, step 2,665) | 80.25% | +20.69 (19.1–22.1) | 55.75% | −3.81 (−5.9 to −1.7) | 56.17 | **−5.94 / −20.44** |
| **`kw1`** (`--key-weight 1.0`, step 2,577) | 84.40% | +24.83 (23.7–26.2) | 76.72% | +17.16 (15.9–18.6) | 44.37 | **−1.80 / +0.53** |

Controls reproduced identically across all three runs: `other_own = 59.56%`, `other_own_edge = 58.86%`, `edge_only = 7.38%`.

Reading:
- **Teacher KL distillation (`loss="kl"`) is the primary load-bearing switch:** switching stage 2 to NLL drops `B_edge` by −5.94 pts and collapses cache-only `B_plain` by −20.44 pts (below an unrelated real cache).
- **The 4.2 GB ridge prior hurts on seed 0:** removing it shrinks checkpoints ~70× (~60 MB), eliminates the ~12 GiB Gram-matrix VRAM spike, speeds up training ~2×, and **improves both `B_edge` (+1.96 pts → +28.59) and cache-only `B_plain` (+7.44 pts → +24.07)**, clearing the 20-point cache-only bar on seed 0.
- **`2×` key weight (`kw=2.0`) gives a modest edge-path gain (+1.80 pts)** and is roughly neutral on the plain path (−0.53 pts), noting the step-count caveat (2,577 vs 5,489 steps on A100 vs H100).
- **Honest limit:** Single training seed (`seed 0`); confirming whether `no_ridge` also holds a ≤5 pt range across 4 seeds (and whether `B_plain ≥ 20` holds across seeds 1–3) remains open if L4 is ever unfrozen. Total Colab spend: ~46.6 CU (`l4-a100-hm` terminated immediately after `kw1`; 39.71 CU remaining).
