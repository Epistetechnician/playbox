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
