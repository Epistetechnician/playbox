# Beyond text: latent channels across model families

Status (2026-11-03). Built: the hub (`steer/playbox_steer/hub.py`), its wire message `PBL1`, and the E8 runner
(`hub_run.py`), with 7 tests on synthetic models and on tiny random models of three architectures. Real runs:
section 6a (five families, one H100, 2026-11-03), 6b (k sweep and join test, one A10G on Modal) 6c (anchor-count
sweep), 6d (more anchors), 6e (concept cards for a text-only reader), 6f (the hub in a room) 6g (an end-to-end run through a room) 6h (commit-and-challenge audit), 6i (models joining one at a time) 6j (token cost against text), 6k (natural-text anchors, twenty concepts) 6l (room hardening), 6m (audit threshold transfer) and 6n (fuzzing the parsers) and 6o (a shared-memory spike on Hindsight) and 6r (near-miss liars). Builds on E6/E7 in [steering.md](steering.md).

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
| L1 concept card | nothing (closed models fit here) | a hub vector rendered as nearest anchor topics and words, with a hash of the source | built without caption or hash; weak, see 6e |
| L2 hub latent | read and write of one mid-layer residual stream (open weights, any family) | a k-dimensional concept or behavioural vector, k bytes | built; measured on five families (6a to 6d); signed room transport (6f) |
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
3. **Commit and challenge (built, section 6h).** The sender commits to the hash of the anchor-style sentences it read.
   On audit it reveals them, and the auditor encodes them through *its own* spoke and compares with the claimed hub
   vector (cosine above a calibrated threshold). This works across families, which the E6 recompute audit cannot: that
   one needs identical weights. It audits what the sender said it was sending, not whether the sentences are honest.
4. **Signed envelope.** `PBL1` carries a body hash only. `PBL2` adds an Ed25519 signature and a room operation (built,
   section 6f), in the style of `Op::Handoff` (E6 round 6).

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
  sentences (done, section 6j).
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

## 6c. Anchor-count sweep (2026-11-03)

`steer/results/e8/e8_anchors.json`, from `steer/tools/e8_modal.py --what anchors` (Modal, A10G). Same models, concepts,
prompts and strength as 6a. The hub is fitted on n randomly chosen anchors out of the 426 (three different subsets per
n), k=128, and the whitening rank scales with n (`min(256, 0.6 n)`; 256 for all 426, the main run's setting). Held-out
sentences and concepts are unchanged. One run.

| anchors n | accuracy, 16 not into TinyLlama (three subsets, mean) | held-out cosine (three subsets) | pairs within 10 points of the pair map (of 16) |
|---|---|---|---|
| 100 (k capped at 100) | 0.48, 0.38, 0.44 (0.43) | 0.84, 0.84, 0.83 | 7, 2, 6 |
| 200 | 0.50, 0.51, 0.52 (0.51) | 0.88, 0.88, 0.88 | 10, 7, 10 |
| 300 | 0.53, 0.55, 0.59 (0.55) | 0.89, 0.89, 0.89 | 12, 11, 14 |
| 426 (all) | 0.57 | 0.90 | 13 |

- **Monotonic and stable now.** Held-out cosine barely moves between subsets (under 1 point) and rises with n; accuracy
  rises with n and its subset spread (up to 11 points at n=100, 8 at n=300) is about the size of one cell's noise.
- **The rule (13 of 16 within 10 points) is met by all 426, and by one of three subsets at 300 (14); the other two at
  300 are 12 and 11.** So the pre-set rule needs close to the full 426 anchors at k=128, and more anchors are likely
  to help (no data beyond 426).
- **200 anchors costs about 6 points of accuracy and 2 of cosine against 426; 100 costs about 13 and 6.**
- **The first attempt was confounded.** It kept every whitened direction for n up to 257 and gave a non-monotonic
  curve (200 worse than 100). Its raw output is kept as `e8_anchors_v1.json`; do not use it.
- **Not tested:** more than 426 anchors; k=64 with the scaled rank; the choice of the 0.6 factor itself; other families.

## 6d. More anchors than 426 (2026-11-03)

`steer/results/e8/e8_more_anchors.json`, from `steer/tools/e8_modal.py --what more` (Modal, A10G). The 426 anchors plus
the sentences of 8 or 16 new topics (`anchors_extra.py`, same templates, no word from a test lexicon, 574 sentences
after dropping duplicates). Held-out sentences, test concepts, models and strength unchanged. Whitening rank
`0.6 n`. One run, one anchor set per size.

| anchors | k | accuracy, 16 not into TinyLlama | accuracy, all 20 | held-out cosine |
|---|---|---|---|---|
| 426 | 128 | 0.57 | 0.50 | 0.90 |
| 426 | 256 | 0.55 | 0.47 | 0.92 |
| 713 | 128 | 0.67 | 0.57 | 0.89 |
| 713 | 256 | 0.65 | 0.56 | 0.91 |
| 1000 | 128 | 0.66 | 0.57 | 0.88 |
| 1000 | 256 | 0.72 | 0.61 | 0.90 |

- **More anchors help, but the pair map gains just as much (control below).** The hub went from 0.57 to 0.72 into the
  16 working receivers. Read it with the control, not alone: the hub did not overtake the pair map.
- **Control: the per-pair ridge map fitted on the same anchors** (`steer/results/e8/e8_pair_control.json`, same setup;
  its 426-anchor arm reproduces the main run's pair numbers in all 20 pairs, so the setup is the same).

| anchors | pair map, 16 pairs | hub k=128 | pairs within 10 points of the pair map (of 16) | hub k=256 | pairs within 10 points (of 16) |
|---|---|---|---|---|---|
| 426 | 0.59 | 0.57 | 13 | 0.55 | 11 |
| 713 | 0.69 | 0.67 | 13 | 0.65 | 15 |
| 1000 | 0.72 | 0.66 | 13 | 0.72 | 15 |

  The hub matches the pair map at 1000 anchors with k=256 (0.717 against 0.716) and trails it by 2 to 6 points
  elsewhere, with the rule (13 of 16) met in every row but k=256 at 426. So the benefit is the one in 6a: 10 adapters
  and 64 to 256 bytes instead of 20 maps, at no accuracy cost once k and the anchors are large enough. The earlier
  reading that the hub beat the pair map was an artefact of fitting only the pair map on 426 anchors.
- **Accuracy and held-out cosine disagree.** Cosine is flat or slightly down with the new anchors (0.90 to 0.88 at
  k=128) while accuracy goes up. The held-out sentences come from the original 16 topics, so cosine measures fit to
  that distribution; accuracy is on the five test concepts, which are outside both sets. More topic breadth helps the
  concepts the anchors never covered, which is what the channel is for. I did not test this explanation.
- **The extra anchors are template text from the same generator.** A further gain may come from more varied text; this
  run shows only that more of this kind helps. 713 to 1000 is mixed at k=128 (0.67 to 0.66), so expect the gain from
  this kind of anchor to flatten around 700 to 1000; k=256 only pays off at 1000.
- **Not tested:** natural (non-template) anchor text, a second subset per size, more
  than 1000 anchors, other families.

## 6e. Concept cards for a text-only reader (2026-11-03)

The closed-model arm. A model we cannot hook can only read text, so the hub vector is turned into a card of the nearest
anchor topics and words (`hub_run.cards`, no sentence from the sender), and a text-only reader is asked to use it.
Sender side: `steer/results/e8/e8_cards.json` (hub: 1000 anchors, k=256, rank 0.6 n; 5 senders x 5 concepts = 25 cards,
plus 25 cards from random vectors). Reader: `steer/tools/e8_card_reader.py` on one H100,
`Qwen/Qwen3.8-27B-FP8` with text only (the Hermes brain of E7; an open model standing in for a closed one, not a
vendor's model), temperature 0, results in `steer/results/e8/e8_card_reader.json`. Per item, one context:
`sentences` (the 8 sentences the sender read), `text` (6 key words chosen from them by tf-idf, the same word budget as
the card), `card`, `none`, `null` (card from a random vector). Task `choose`: name the topic from five (chance 20%,
25 items per arm). Task `continue`: finish one of 8 prompts, scored with the lexicon decoder (200 per arm).
One run. Ceiling `local_internal_exploratory`.

| context | tokens | choose the topic (of 25) | continue on topic (of 200) |
|---|---|---|---|
| plain sentences | 87 | 25 | 158 (79%) |
| 6 key words, written by the sender | 19 | 25 | 171 (86%) |
| hub card | 30 | 15 (60%) | 57 (29%) |
| card from a random vector | 30 | 7 (28%) | 12 (6%) |
| no context | 0 | | 1 (0.5%) |

- **The card carries something, and much less than six words of text.** It beats the random-vector card (60% against
  28% choosing, 29% against 6% continuing) and chance, and costs 11 more tokens than the key-word summary that scores
  100% and 86%. For a concept that can be named in text, text is cheaper and better. The wire message (256 bytes) is
  not the reader's cost; the card text is.
- **It works for three concepts and fails for two.** Choosing: music 5 of 5, sport 5 of 5, medicine 5 of 5, ocean 0
  of 5, space 0 of 5. The cards for ocean and space name castles, workshops, farms and towers: the anchors (garden,
  kitchen, hospital, circus, theater...) have nothing near the sea or space, which is the limit in section 7 seen from
  the receiving side. Random-vector cards were read as medicine 12 times of 25, sport 6, music 5, space 2, ocean 0, so
  part of the 15 may be the reader's default for a card with no matching words. I did not separate that.
- **Plain sentences did not reach 100% when continuing** (79%, lower than the 6 key words): the lexicon decoder counts
  only listed words, and a fluent continuation about "the storm on the horizon" misses it. Treat the continue column as
  a coarse score.
- **This does not show the card is useful.** A closed agent gets its value from the hub only if the concept has no short
  text form, or the text must not be shared. This arm did not test either. What it shows: today's card, built only from
  the nearest anchors, loses to a short text summary on accuracy and token cost.
- **Not tested:** a real closed model; a card with a sender-written caption (the design's embassy); more concepts than
  five, which the hub's anchors do not span; second run; the card with the sender's word summary appended.

## 6f. The hub in a room (2026-11-04)

What is built (`crates/playbox-net/src/latent.rs`, `steer/playbox_steer/hub.py`): a room can carry hub vectors between
members, signed.

- **`PBL2` envelope.** `PBL2` | u32 header length | JSON header | k int8 bytes. The header holds the hub id, a sender
  label, `k`, the quantization scale (hex of the big-endian float32, so no float text has to round-trip between Python
  and Rust), the blake3 of the body, the signer's public key and an Ed25519 signature over a fixed text of those fields.
  Python signs (`pack_signed`, `unpack_signed`); Rust verifies (`latent::parse`) and can also sign (`latent::pack`).
- **Two room operations**, both membership-level (like `say`), so a stranger or a revoked member gets nothing:
  `latent` posts one envelope; `latent_pull {hub, since}` returns up to 32 envelopes for a hub after a cursor.
- **What the host checks on `latent`:** the magic, header size, hub id and sender label shape, `0 < k <= 4096`, body
  length equals `k`, body hash, the signature, and that the signer is the sending member's own key (a message signed by
  someone else is refused). It files the envelope, posts a one-line notice to the feed (hub, sender label, k, seq; no
  vector) and returns a receipt signed with the host key, which the client verifies against the exact bytes it sent
  (`Client::latent_send`).
- **Reading needs no trust in the host.** Every envelope is self-verifying, so `Client::latent_pull` re-checks each one
  and a reader can check the signer against the keys it trusts (`unpack_signed(..., trusted_signers=...)`). The host
  can withhold or replay messages but cannot forge one.
- **Bounds.** The mailbox keeps 256 envelopes in all and 32 per member (oldest dropped). Section 6l adds persistence,
  a post rate limit, a hub allowlist, replay refusal and expiry.
- **The host never decodes a vector.** It holds no spoke, so it cannot read what a vector means. Decoding, the strength
  cap and the guards all stay with the receiving member (section 5, items 1 and 2).

Tests: `crates/playbox-net/tests/latent.rs` (posting and reading between two members, a forged or cross-signed or
truncated or flipped message, the per-member bound, a stranger) and a fixed-key envelope produced by the Python sender
that the Rust packer must reproduce byte for byte; `steer/tests/test_hub.py` (round trip, tampering, an untrusted signer,
another hub's message).

Built later: persistence, a rate limit and a hub allowlist (6l); the Python side reaches a room through
`playbox latent session` (6g); the guard check on the receiving member is `latent_room.guard_blocks` (6g).

## 6g. End to end through a room (2026-11-04)

`steer/results/e8/e8_room.json`, from `steer/tools/e8_room_demo.py`. Everything below ran on one Mac (CPU) in one
session, with the transport real: a `playbox up --local` host, one `playbox latent session` member per model (new
command, `crates/playbox-cli/src/latent.rs`), each with its own identity key, and `steer/playbox_steer/latent_room.py` for
the Python side. Models: SmolLM2-135M, Pythia-410m, GPT-2 (three architectures). Hub: 1000 anchors, k=256. Strength 0.5,
5 concepts x 8 prompts per message. One run.

Flow: each model reads a concept, encodes it into the hub and posts a signed `PBL2` envelope (628 to 631 bytes with the
header and signature, 256 of them the vector) to the room. Each other model pulls, verifies the signature against the
sender's key, decodes with its own spoke, runs the guard check, applies the vector and generates. The concept label never
travels; the script keeps it to score.

| | result |
|---|---|
| messages delivered (3 senders x 5 concepts x 2 receivers) | 30 |
| accuracy through the room, all 30 (240 texts, chance 20%) | 70% |
| the receiver's own direction (upper bound) | 89% |
| medicine blocked by the receiver's guard (a direction in the receiver's own space, class `medicine`, cosine >= 0.5) | 6 of 6; no other concept blocked |
| accuracy over the 24 delivered | 69% |
| a post signed by pythia's key but sent by smollm2's member | refused by the host: "the message must be signed by the member who sends it" |
| feed notices (one per message, no vector in them) | 15 |
| text identical to the in-process path (no int8, no room) | 202 of 240; the rest differ only by int8 rounding of the 256 values |

By receiver: into SmolLM2 26 and 31 of 40 from GPT-2 and Pythia (its own direction: 34); into GPT-2 25 and 27 (own: 39);
into Pythia 33 and 26 (own: 34). By concept: space 43 of 48, medicine 35, music 35, ocean 33, sport 22 (sport is weakest,
as in the earlier E8 runs).

- **What this shows:** the whole path works as designed across three architectures, over a real host: a signed vector
  goes in, a differently built model decodes it and says something on topic about 7 times in 10, a forged sender is
  refused by the host, and a guarded direction is stopped by the receiver without the host ever decoding anything.
- **What it does not show:** the models are small (the hub's numbers in 6a to 6d are for larger ones and this run's
  accuracy is lower than the 5-family 1000-anchor run); the receiver's spoke came from the same process as the sender's
  (a real deployment ships a spoke, not a hub fit, to each member); the guard here is a single direction I built from
  the receiver's own medicine concept, so it shows the mechanism, not that unknown unsafe directions are caught; one run,
  one subset of concepts; the host is local and honest.

## 6h. Commit-and-challenge audit (2026-11-04)

Built (`crates/playbox-net/src/latent.rs`, `steer/playbox_steer/latent_audit.py`, `hub.py`) and run through a real room
(`steer/tools/e8_audit_demo.py`, results in `steer/results/e8/e8_audit.json`).

**Protocol.** The sender's signed `PBL2` envelope may carry a `commit`: blake3 over a fixed prefix, a random salt and the
sentences it read. The commitment is inside the signed text, so it cannot be swapped or stripped afterwards. Any member
can challenge a message (a feed line `[challenge] seq=N`; the room has no special operation for it). The sender answers
with `latent_reveal {seq}`: the salt and the sentences as JSON. The host accepts a reveal only from the message's sender,
only if its hash equals the signed commitment, only once, and within bounds (64 sentences, 512 bytes each, 16 KiB, no
newlines); it never reads the sentences. Readers get the reveal with the message. An auditor then encodes the revealed
sentences through its own model and spoke and compares the result with the vector that was sent
(`latent_audit.audit`). This works across families, which the E6 recompute audit cannot.

**Run.** The same three models and hub as 6g (1000 anchors, k=256), each posting 12 messages with commitments: 5 honest,
5 "swap" (commits to concept A's sentences, sends concept B's vector), 1 random vector, 1 silent (honest vector, never
answers). Each message audited by the two other models: 72 audits. One run, one hub.

| cosine of the auditor's vector for the revealed sentences with the vector sent | n | min | median | max |
|---|---|---|---|---|
| honest | 30 | 0.40 | 0.51 | 0.58 |
| swap (right sentences, wrong concept's vector) | 30 | | 0.29 | 0.42 |
| random vector | 6 | | 0.03 | 0.11 |

- **Separation is good, not perfect:** the chance that an honest cosine is above a lying one is 0.994. The ranges touch
  (lowest honest 0.40, highest swap 0.42).
- **A threshold I guessed beforehand did badly: at 0.5, 36 of 36 liars were caught but only 16 of 30 honest messages
  passed.** Honest cross-family cosines are about 0.5, not near 1, because two different models encode the same
  sentences differently. At 0.437, chosen to separate these same 66 numbers (so in-sample and optimistic), 36 of 36
  liars are caught and 28 of 30 honest pass. A real room needs a threshold set from honest runs of its own models and a
  second look (a person or a stronger check) for messages near the line, not an automatic quarantine.
- **A silent sender is detectable:** the 6 audits of silent messages all came back "the sender did not reveal".
- **A sender cannot change its story:** 18 attempts by swapping or random senders to reveal different sentences were
  all refused by the host, because they do not match the signed commitment.
- **What a pass means:** the vector is consistent with sentences the sender committed to before the challenge. It does
  not mean the sentences are harmless (they are text, so the auditor or a person can read them), and a sender that
  commits to sentences that really produce its payload passes. It also does not tell which of sender or auditor is wrong
  when they disagree beyond the threshold; the room decides what to do (nothing here quarantines automatically).
- **Not tested:** more than three small models, a larger or natural-text concept set, a sender that picks sentences to
  game the audit, and the host acting on verdicts. (A threshold set on one run and used on another: 6m.)

## 6i. Models joining one at a time (2026-11-04)

`steer/results/e8/e8_chain.json`, from `steer/tools/e8_modal.py --what chain` (Modal, A10G). The same five models and
1000 anchors as 6d, k=256. `all`: one hub fitted on all five. `chain`: fitted on SmolLM2 and Qwen3 only, then Pythia,
GPT-2-medium and TinyLlama join one after another with their own adapters, nobody refitted. Two join orders. Accuracy is the
mean over ordered pairs not into TinyLlama (16), 40 texts per pair, one run.

| | pairs | accuracy, all-five hub | accuracy, chain | held-out cosine, all five / chain |
|---|---|---|---|---|
| base to base | 2 | 0.63 | 0.61 | 0.905 / 0.900 |
| base to joiner | 10 | 0.72 | 0.64 | 0.908 / 0.899 |
| joiner to joiner | 4 | 0.77 | 0.64 | 0.912 / 0.898 |
| all 16 | 16 | 0.72 | 0.64 | 0.908 / 0.899 |

- **Join order changes nothing** (the two orders gave identical numbers). By construction each joiner is fitted against the
  same shared coordinates, so an earlier model never changes when a later one joins and the order is irrelevant. That part
  of "drift" is zero.
- **The cost is in the shared coordinates, which come from the models there at the start.** A hub grown from two models
  to five is 8 points of accuracy below one fitted on all five (0.64 against 0.72; roughly 3 standard errors on 640
  texts) and 1 point of cosine. The loss is largest for the pairs between late joiners (0.77 down to 0.64). In 6b, one
  model joining a hub of four cost 3 points; three late joiners cost 8, so the cost grows with how much of the group
  joined late.
- **Practical rule:** adding a model is cheap and needs no refit of the others, but a hub should be refitted from all its
  members' anchor states when more than about one model in four joined after the fit. Refitting changes the shared
  coordinates, so every member then takes a new adapter pair; the room's `hub id` changes, so mixed hubs cannot be
  confused (`unpack_signed` refuses another hub's message).
- **Not tested:** a longer chain, joiners from architectures far from the base models, a refit schedule, and whether a
  more diverse base pair would shrink the gap.

## 6j. Token cost against a text exchange (2026-11-04)

`steer/results/e8/e8_token_cost.json`, from `steer/tools/e8_token_cost.py` (local CPU). Receivers: SmolLM2-135M, Pythia-410m,
GPT-2 (base models). The same 5 concepts, 8 prompts, 24 greedy tokens and lexicon scorer as every E8 run. Text arms put the
text in front of the prompt; the latent and direct arms are the room run of 6g (30 messages, ungated). One run.

| how the concept is sent | tokens added to the receiver's context | bytes sent | accuracy (texts) |
|---|---|---|---|
| nothing | 0 | 0 | 1% (120) |
| 6 key words, as text | 21 | 89 | 59% (120) |
| the 8 sentences the sender read, as text | 88 | 429 | 57% (120) |
| hub vector through a room | 0 | 630 | 70% (240) |
| the receiver's own direction | 0 | 0 | 89% (240) |

- **The vector costs the receiver no context tokens and scored higher than the text arms** (70% against 59% and 57%).
  The 11-point gap over the key words is about 1.7 standard errors on these sample sizes: suggestive, not established.
- **It is not cheaper on the wire:** 630 bytes (a 256-byte vector plus a header and a signature) against 89 bytes for six key
  words. What it saves is the receiver's context window and the model's reading of the text, not bytes.
- **These receivers are small base models that make poor use of text context.** The 27B model of 6e reached 86% from the same
  six words. For a capable reader, text is both cheaper and more accurate (6e); the vector's case rests on receivers that
  can be steered but read poorly, and on keeping the concept out of the text channel where injection checks look.
- **Latent accuracy differs by receiver** (SmolLM2 71%, GPT-2 65%, Pythia 74%), as the text arms do (50 to 70%).
- **Not tested:** latency or compute (the vector adds one forward hook and no context tokens, but I did not time it), a
  larger base model as receiver, instruction-tuned receivers, and text in other formats.

## 6k. Natural-text anchors and twenty concepts (2026-11-04)

`steer/results/e8/e8_natural.json`, from `steer/tools/e8_modal.py --what natural` (Modal, A10G). Everything before this used
template anchors and five concepts. Here: fifteen more concepts in natural untemplated sentences (`concepts_natural.py`:
volcano, cooking, finance, law, war, religion, computing, aviation, painting, farming, weather, railways, chemistry,
elections, architecture; eight sentences and about 30 lexicon words each), so twenty in all, chance 5%. Natural anchors come
from Wikipedia (the WikiText-2 training text), sentences of 6 to 30 words with every sentence containing any concept's
lexicon word removed, so no test concept is in any anchor set. Held-out cosine is on 150 natural sentences that no hub saw.
The same five models, k=256, whitening rank 0.6 n, strength 0.5; 4 prompts per concept, so 80 texts per pair. One run.

| hub fitted on | anchors | accuracy, 16 pairs not into TinyLlama | held-out cosine (natural sentences) |
|---|---|---|---|
| template sentences | 1000 | 0.52 | 0.36 |
| natural sentences | 1000 | 0.49 | 0.72 |
| natural sentences | 2000 | 0.55 | 0.75 |
| template and natural | 2000 | **0.59** | 0.72 |
| *the receiver's own direction (upper bound)* | | *0.75* | |

- **The hub carries about 80% of what a receiver gets from reading the concept itself on twenty concepts** (0.59 against
  0.75), with 5% chance. On the original five concepts the earlier runs were at about 0.72 against 0.89, a similar
  fraction. So the result is not specific to five easy concepts or to template anchors.
- **A hub fitted on template text works on natural concepts, and measures badly on natural text.** Its accuracy (0.52) is
  close to the natural-anchor hub with the same number of anchors (0.49), while its held-out cosine on natural sentences is
  0.36 against 0.72. Cosine and accuracy have disagreed before (6d); this is a clearer case, and it means held-out cosine
  is not a safe stand-in for how well a concept transfers.
- **More text and a mixture help.** 2000 natural sentences beat 1000 (0.55 against 0.49); template plus natural, 2000 in all,
  is best (0.59). The 1000-sentence differences are inside what one run of this size can tell apart; the 0.59 against 0.49
  gap is about 3 standard errors.
- **Concepts differ a lot.** With the mixed hub: sport 0.84, space 0.80, music 0.77, ocean 0.72, medicine 0.75, chemistry
  0.72, law 0.69, cooking 0.62, weather 0.55, aviation 0.36, volcano 0.20. Some low values are the receiver's own limit:
  the direct arm reaches only 0.31 for volcano and 0.44 for cooking, so the lexicon decoder or the base models are the
  weak part there, not the channel. Others are the channel: aviation is 0.75 for the receiver itself and 0.08 to 0.53
  through the hubs, and chemistry swings from 0.20 (template hub) to 0.86 (2000 natural).
- **Not tested:** natural anchors from other domains, a larger natural set, instruction-tuned receivers, concepts chosen by
  someone else, and a decoder other than a word list.

## 6l. Room hardening (2026-11-06)

The mailbox from 6f was a demo: in memory, no rate limit, any hub id, and a restarted host lost every message and reused
sequence numbers. Built (`crates/playbox-net/src/latent.rs`, `LatentPolicy` in `HostConfig`, flags on `playbox up`):

- **Persistence.** A host opened with `--state DIR` keeps the mailbox in `DIR/latent.json`, written to a temp file and
  renamed after every post and reveal. On start each stored message is checked again: the envelope must parse (hash and
  signature), the signer must be the member recorded as sender, it must be within the age limit, and a stored reveal must
  still open the signed commitment. Anything else is dropped; a file that is not JSON gives an empty mailbox and a note on
  stderr. Sequence numbers continue where they stopped. A host without `--state` is unchanged (memory only).
- **Rate limit.** `--latent-posts-per-minute N` (default 30), per member, same sliding window as `say`. A member over it is
  told to slow down; other members are not affected. The window is in memory, so it resets when the host restarts.
- **Hub allowlist.** `--latent-hub HUB`, repeatable. Without it any hub id is accepted; with it a post for any other id is
  refused before it is filed. (The host cannot tell a real hub from a made-up id, since it never decodes; this only keeps
  the room to the hubs its members actually use.)
- **Replay and expiry.** The host remembers the blake3 of every envelope it filed within the age limit (up to 2048) and
  refuses the same bytes again. A message older than `--latent-max-age-secs` (default 3600) is no longer served, and its
  digest is forgotten. Age is the host's own arrival time.
- **Cursor.** A reader whose cursor is ahead of the host (a host that restarted without a store) starts over from zero, so
  it does not skip new messages.

Checks. `crates/playbox-net/tests/latent.rs` grew from 9 to 14 tests: allowlist and rate limit, replay and expiry, a cursor
ahead of the host, a restart that keeps messages, reveals, numbering and replay memory, and a damaged or edited store.
`steer/tools/e8_room_hardening.py` runs the real `playbox up --state` host and `playbox latent session` members with
Python-signed envelopes on a synthetic hub (`results/e8/e8_room_hardening.json`): 15 messages posted; a forged post, a
replay and an unlisted hub id refused; the host stopped and started again on the same directory; then all 15 come back with
the same sequence numbers, the same bytes, verified signers and vectors that decode to what was sent; the replay is still
refused and the next post is number 16; a member is refused after exactly 30 posts while another still posts; and a message
past a 3 s age limit is not served. Existing host suites (`chat`, `persist`, `security`, `abuse`, `fuzz_host`, `room`,
`collab`) still pass.

Limits.
- **Not rerun with language models.** The three-family generation demo (6g) was not repeated against a restarted host: this
  machine's Hugging Face cache sits on an external drive that is not mounted. The restart only touches the mailbox, and the
  bytes that come back are identical to the ones sent, so the receiver's path is the one 6g measured; but that is an
  argument, not a rerun.
- **Replay is refused by the host only.** An envelope carries no timestamp, so a member can post its own old envelope again
  once the age limit has passed (it is that member's own message), and a malicious host could serve an old one to a reader.
  A reader that wants to refuse repeats should keep the hashes it has seen; the CLI session does not do that yet.
- **Snapshot file.** The whole mailbox (at most 256 envelopes) is rewritten on each post: fine at 30 posts a minute per
  member in a small room, wasteful beyond that; an append-only log is the next step if rooms grow.
- **Built with Rust 1.99.1**, not the pinned 1.93.1 (its home is on the same unmounted drive); CI builds with the pin.

## 6m. Does an audit threshold transfer? (2026-11-07)

6h picked its cut on the same 66 numbers it then scored, so it said nothing about a cut chosen on one run and used on
another. `steer/tools/e8_audit_transfer.py` (results in `steer/results/e8/e8_audit_transfer.json`) sets a threshold on a
calibration run and applies it to fresh runs. Same three models. Calibration: hub A (1000 anchors, k=256), the five template
concepts, 90 audits (30 honest). Test 1: hub A again with fifteen natural concepts the calibration never saw (270 audits: 90
honest, 180 lying). Test 2: hub B, fitted on a different anchor set (the 426 base anchors, k=128), all twenty concepts (360
audits: 120 honest, 240 lying). Lying means 6h's swap (commits to concept A's sentences, sends the next concept's vector) and
a random vector, one of each per concept and sender. Messages go through the real `PBL2` packer (signed, int8) and
`latent_audit.audit`; the room and reveal operation are not repeated (6h did that), so this changes only the threshold.

The calibration run reproduces 6h: honest cosines 0.40 / 0.51 / 0.58 (min, median, max). A threshold from labelled liars is
not available to a real room, so three rules that use only honest messages are compared with it and with the best cut on the
test data itself (the oracle, an upper bound).

| calibrate on, test on | cut | honest passed | liars caught |
|---|---|---|---|
| hub A template (30 honest) to hub A natural (90 / 180) | oracle 0.439 | 90/90 | 178/180 |
| | lowest honest, 0.400 | 90/90 | 167/180 |
| | mean minus 2 sd, 0.417 | 90/90 | 176/180 |
| same calibration to hub B, all twenty concepts (120 / 240) | oracle 0.474 | 120/120 | 231/240 |
| | lowest honest, 0.400 | 120/120 | 197/240 |
| | mean minus 2 sd, 0.417 | 120/120 | 206/240 |
| reverse: hub A natural (90 honest) to hub A template (30 / 60) | oracle 0.400 | 30/30 | 57/60 |
| | lowest honest, 0.439 | 26/30 | 60/60 |
| | mean minus 2 sd, 0.435 | 28/30 | 60/60 |

- **Same hub, new concepts: yes, with small losses.** A cut from honest messages only kept every honest message in test 1
  (90/90, 95% interval 0.96 to 1.00) and caught 93% (lowest honest) to 98% (mean minus 2 sd) of liars, against 99% for the
  oracle. Honest cosines barely moved (median 0.512 to 0.504); lying ones reached higher on natural concepts (up to 0.445,
  against 0.424 before), which is where the missed liars are.
- **A different hub: no.** On hub B honest cosines are higher (median 0.605) and so are lying ones (up to 0.57). The
  calibration cut still passed every honest message but caught only 82% to 86% of liars; the oracle for hub B is 0.474 and
  catches 96%. A threshold belongs to a hub (its anchors and k); refit it when either changes, and when members' models change.
- **The lowest honest cosine is fragile.** From 30 honest messages it was 0.400; from 90 it was 0.439, and carried back to
  the 30 it flagged 4 of them (13% false alarms). Mean minus 2 sd was the steadiest of the three (0 / 0 / 2 false alarms in
  240 honest messages across the three tests) but it is a rule of thumb, not a guarantee: honest cosines have no reason to be
  normal.
- **Per-pair thresholds overfit.** A cut per (sender, auditor) pair from 5 honest messages each gave 10 false alarms in 90
  honest messages on test 1; the global cut gave none. Pairs would need many more calibration messages.
- **Separation is stable.** The chance that an honest cosine is above a lying one was 0.997 (calibration), 0.9996 (test 1)
  and 0.996 (test 2). The gap that moves between hubs is the position of the cut, not whether one exists.

What this supports: calibrate on a batch of honest committed messages from the room's own members under the room's own
hub (at least about 90, from several concepts), use mean minus 2 sd or something more conservative, expect about 2 in 100
liars like these to slip through on the same hub (test 1; hub B showed more, above), and route messages near the cut to a second check. It does not support carrying one cut
between hubs.

Not tested here: liars close to the honest vector (done in 6r: the audit misses payloads up to 25%), a sender that picks sentences to fit its payload, more than three
small models, instruction-tuned auditors, and a calibration batch that itself contains a liar.

## 6n. Fuzzing the parsers (2026-11-07)

Everything in 6f to 6m reads bytes that another member (or a host relaying them) controls, and none of it had been
mutation-tested. What ran: the eight existing libFuzzer targets and two new ones (`latent_envelope`: the `PBL2` envelope, the
reveal and the receipt; `latent_store`: the saved mailbox read back at start), 60 s each on nightly with the seeded corpora,
70 000 to 450 000 executions per target; the 15 000-request host mutation test, now also sending `say`, `feed`, signed `latent`
posts, reveals and pulls, then restarting the host on the mailbox the barrage left; and seeded mutation tests of the Python
parsers (`steer/tests/test_hub_fuzz.py`, `test_handoff_fuzz.py`, 20 000 handoffs and 33 000 hub messages in the exploratory
runs). The Rust side found nothing: no crash in any target, and the host stayed up and kept a valid store.

The Python side found real bugs, each reachable by one hostile member and each now a refusal:

- **`unpack` and `unpack_signed` raised `AttributeError`, `TypeError`, `KeyError` and `struct.error`** for a header that is not a
  JSON object, a `scale` of `null`, a signer that is a list, and so on. The worst case needs no forgery: a trusted member signs
  a header whose `scale_f32` is not 8 hex characters, the signature verifies, and the receiver dies on `struct.unpack`. A
  receiver that handled only `ValueError` (the documented failure) crashed on any of these. Headers are now checked field by
  field and every failure is a `ValueError`; deeply nested JSON is one too.
- **A handoff record with a list for its signer crashed `Receiver.accept`** at the first set lookup, and tensors of an odd
  shape raised from inside `DynamicCache`. Every record field must now be text and a cache that cannot be built is a refusal.
  20 000 mutated handoffs: none accepted, no exception.
- **The audit trusted the shape of a reveal.** A reveal with a non-text sentence raised `TypeError`, and nothing bounded the
  sentences an auditor would run through its model if a relaying host served more than the sender's host allowed. It now
  applies the host's bounds (64 sentences, 512 bytes each, 16 KiB, no newline, a hex salt) and treats the rest as a failed audit.
- **A prompt file nested thousands deep** raised `RecursionError` out of `read_prompts`, past the entry point's `ValueError`
  handler; it is now a `ValueError`.

Each new test fails on the old code (checked) and passes on the new.

Wiring found on the way: `playbox_create` (the MCP tool agents, the Hermes plugin and the cockpit use) had no way to set the room
limits that `playbox up` takes (`max_members`, `max_concurrent`, `max_tasks_per_minute`, `task_budget`) or the latent policy
(`latent_hubs`, `latent_posts_per_minute`, `latent_max_age_secs`); rooms made through it always ran on defaults. It takes them
now. Four things had tests that CI never ran and now do: `world/` (a `dev` group and `pythonpath` so it follows the same
`uv run --group dev pytest` as the other projects), the Hermes witness plugin (plain `unittest`; pytest cannot collect it),
and the cockpit's end-to-end script (its own job).

Limits. Sixty-second runs find shallow bugs, not deep ones; the signature-checking path in `latent_envelope` is only reached by
mutating valid seeds, which almost always break the signature. The Python mutations are structure-aware but hand-written, so
they cover the shapes I thought of. The CLI session's JSON handler and the host's reply to a hostile reveal were exercised only
through the host test. Not fuzzed: the sender-side code (it takes tensors from the user's own models), and `hub_run`'s data
handling.

## 6o. Shared room memory on Hindsight: a spike (2026-11-11)

Question: could each room keep a shared memory in an unmodified [Hindsight](https://github.com/vectorize-io/hindsight) server (a bank per
room), with Playbox's trust metadata riding along, or would it need a fork? `steer/tools/hindsight_spike.py` (results in
`steer/results/hindsight_spike.json` and `hindsight_observations.json`) ran the published image against a local model (Ollama `phi4`;
no key, nothing left the machine). Two authors retained four entries, two honest and two a planted contradiction, each with the
fields Playbox keeps for a memory entry (author key, signature, evidence reference, review status) as `metadata` and `tags`.

| Check | Result |
|---|---|
| Metadata and tags come back on recall, unchanged | 4 of 4 |
| Recall filtered by `author:` tag returns only that author's entries | yes, both authors |
| Recall filtered to `status:accepted` hides the planted claim (`tags_match: all`) | yes; unfiltered recall does surface it |
| Retaining the same `document_id` again does not duplicate | 4 units before and after |
| Deleting a revoked author's documents removes them from recall | yes |
| Consolidation (observations) mixes authors | not shown: 0 mixed in 20 observations over two scope modes |

What it supports: **run it unmodified as a sidecar, no fork.** The bridge Playbox would write is thin, because the host can stamp
author, room and status as tags and metadata, filter recall on them so unreviewed entries stay out of what an agent is shown, retry
safely by `document_id` (the same idea as the request id in `a2a-binding.md`), and revoke by deleting an author's documents.

What it does not show. The consolidation check is inconclusive: the local model kept the contradictory claims apart even in
`combined` mode, which is meant to merge, so this run cannot tell whether a stronger model would blend a hostile author's claim
into an "observation" the room then trusts. In `per_tag` mode Hindsight also makes one observation per tag value, including a
room-wide one (`room:spike`), so a shared tag is a path by which authors could be merged. The safe design is to keep unreviewed
entries out of consolidation or give each author their own scope, and to test that with a strong model and many overlapping
entries before relying on it. Also unmeasured: recall quality, cost, and what derived observations do after a deletion (the
deleted documents left recall, but observations built from them were not checked). Retaining four entries took 48 s on a local
20 B-class model, so a real room would pay for an LLM call per retain; that cost and the disclosure that memory text reaches the
configured model are the host's to decide. Hindsight is a Python server with Postgres, so it stays an opt-in extra, not part of
the single binary.

## 6p. The room's shared memory (2026-11-12)

6o said to run Hindsight as a sidecar and not fork it. This is the bridge: `crates/playbox-net/src/shared_memory.rs`, wired into the host, the client, the MCP server and the CLI.

How it works. The host is the only client of the memory server, so a member never holds a key to it or a way to name a bank. A member sends `remember` (text, optional `key`) or `recall` (query, optional author) as ordinary room operations that need only membership. The host:

- stamps the author from the authenticated key, never from the text, and tags every entry `room:<id>`, `author:<member>`, `status:<state>`;
- keeps its own ledger (`memory.jsonl`, one line per change, damaged lines skipped on load) as the record, and treats the server as an index: `reindex` rebuilds it after the server loses its data;
- names each document by a hash of author and key (or text), so a retried post replaces itself instead of duplicating, and a retry costs no post budget;
- recalls with `status:accepted` and the room tag, then keeps only hits whose document the ledger holds as accepted, whose author is still a member, and whose returned tags agree with the ledger. Hits with no document id and derived "observations" are dropped, because 6o left open whether consolidation can be kept apart by author;
- on revoking a member, deletes their documents in the background; every recall first sweeps authors who are no longer members, so a deletion that failed, or a revocation that reached this host as a notice, still completes, and the entries are hidden at once either way;
- with `--memory-review` (or `memory_review`), holds posts as `pending` in the ledger and never sends them to the server until the host accepts them, so unreviewed text cannot be consolidated with anything. `quarantine` takes an entry out of the index and keeps the record, and the member cannot post it back;
- caps an entry at 4096 bytes, a member at 30 posts a minute and 1000 entries, the room at 10 000.

Use. `playbox up --memory-url http://127.0.0.1:8888/v1/default [--memory-bank NAME] [--memory-review]`; in an MCP host, `memory_url` and `memory_review` on `playbox_create`; members call `playbox_room_remember` and `playbox_room_recall`. What recall returns is registered as untrusted text, like the feed.

Tested: ten unit tests and four room tests against an in-process index (author stamping, retry, key replacement, revocation, review, a lying index, restart with a damaged ledger, limits, the HTTP client against a throwaway server, and a room without memory). The host mutation test now sends `remember` and `recall` too. One test (`a_real_hindsight_server_serves_the_room`, `--ignored`, needs `HINDSIGHT_URL`) runs the same room against a real server; it passed on 2026-11-12 against the unmodified server with local `phi4` (30 s): two members posted contradictory deployment claims, both were recalled with their authors, and after `revoke_member` the server logged one `delete_document`, kept one fact (the other member's) and recall returned only that member's entry.

Consolidation, rerun with a stronger model (`gpt-oss:20b`, 2026-11-12, `steer/results/hindsight_observations_gptoss.json`). In `combined` mode every observation kept one author's tags (6 of 6, none mixed). In `per_tag` mode none carried both authors (0 of 10), but the observations sit under single-tag scopes: a `status:accepted` observation would blend every accepted author's claims, and an `author:` filter would not return it. The test had only one accepted author, so it could not show that blending; the `per_tag` run also did not finish within 15 minutes on this model. Result: the one-author-per-observation property held, the cross-author case is still untested, and the bridge's choice not to recall observations stands.

Not done: consolidation isolation with two accepted authors is unproven (this design avoids depending on it); recall quality and cost at scale; the bank is not deleted when a room ends; a replica host does not share a ledger with its primary.

## 6q. Browsing through the host, behind an allowlist (2026-11-13)

A member can read a web page through a browser the host runs, and only pages the host allows. `crates/playbox-net/src/browse.rs` drives any browser that speaks the Chrome DevTools Protocol (headless Chrome, Kitesurf, a hosted CDP service) over a `ws://` endpoint on a trusted address, through a small WebSocket client (`ws.rs`). The operation is `browse` (a new right in member grants): `{"op":"browse","url":...,"mode":"text"|"html"|"screenshot","wait_ms":...}`. It answers with the text, HTML or an image, the final URL, title and status, what was blocked, and how many requests the page made.

What keeps the browser on the list:

- the URL is checked before anything starts: http or https, no credentials, no empty host labels, and a host and port the allowlist names (`example.com`, `*.example.com`, `host:port`; a pattern without a port allows 80 and 443; `*.com` and `*` are refused);
- a host name is resolved here and refused if any address is private, loopback, link-local (cloud metadata), carrier-grade NAT, NAT64 or reserved, unless `allow_private` is set;
- each visit gets its own browser context (no cookies or cache shared across visits or members), disposed when the connection closes;
- every request the page makes is paused at the browser and checked against the same list: subresources, redirects, frames, workers. Frames in other processes and popups are attached before they run and get the same interception; a target the browser cannot intercept is closed. Interception is switched on before the page is asked to load;
- page scripts are off unless the operator sets `javascript`; with them on, `WebSocket`, `WebTransport` and WebRTC are removed, because interception does not see them;
- downloads are denied, a visit has a time limit (30 s), a request limit (300), size limits (256 KiB of text, 2 MiB of image), two visits at once, six per member per minute. The audit log records the URL without its query string.

Use. `playbox up --browse-endpoint http://127.0.0.1:9222 --browse-allow example.com --browse-allow '*.docs.rs' [--browse-js]`, with Chrome started as `chrome --headless=new --remote-debugging-port=9222 --user-data-dir=/tmp/profile`; in an MCP host, `browse_endpoint`, `browse_allow` and `browse_javascript` on `playbox_create`; members call `playbox_browse`. The page's text is registered as untrusted.

Tested. Unit tests cover the allowlist (host, port and subdomain matching, credentials, other schemes, look-alike and encoded hosts), the address classes, and a seeded property test over 200 000 generated URLs (an accepted URL is always http or https, has no credentials, and is on a listed host and port); that test found one bug while it was being written, an empty leading label (`https://%2edocs.rs`) matching `*.docs.rs`, fixed. A scripted CDP server checks the sequence (interception and script switch-off before the page loads, an off-list request failed, an allowed one continued). The host mutation test sends `browse` too.

Against a real browser (Chrome 157, headless, on 2026-11-13): a page on the allowed host that loaded a stylesheet, a preload, a prefetch, an image, two frames (one on a different site, so in another process), a form, and, with scripts on, a beacon, a `fetch`, a `WebSocket` and a popup, all aimed at a forbidden server, plus a 302 and a meta refresh to it. **No request reached the forbidden server, with scripts off or on**; the redirect was refused and the page reported what was blocked. Text, HTML and a screenshot came back. The page's script did not run with scripts off, and `WebSocket` was gone with them on.

Not closed. The forbidden server still saw bare TCP connections (3 with scripts off, 5 with them on) and no bytes: the browser opens speculative connections for `preconnect` hints and navigations before the request is paused. Nothing is sent, but a closed or open port on an internal host can be probed blindly; keep the browser on a network that reaches only what it should. DNS is resolved here and again by the browser, so a name that changes between the two can still land on a private address; the same advice holds. The endpoint is plain `ws://`, so the browser must be local or on a private link. Kitesurf and other CDP servers were not tried; only Chrome was.

## 6r. Near-miss liars: a small payload slips past the audit (2026-11-13)

6h and 6m only tested liars that were a different topic or noise. A sender that wants a payload through would stay close to its
committed sentences. `steer/tools/e8_audit_nearmiss.py`, `e8_guard_nearmiss.py` and `e8_payload_effect.py` (results in
`steer/results/e8/e8_audit_nearmiss.json`, `e8_guard_nearmiss.json`, `e8_payload_effect.json`; local CPU, SmolLM2, GPT-2, Pythia-410m,
the 6g hub of 1000 anchors and k=256) test three things. One run each, fixed seeds, ceiling `local_internal_exploratory`.

**1. The audit.** Every liar commits to concept C's sentences and sends a vector that is mostly C's: `(1-a) C + a X`, where X is the
next concept or the guarded `medicine`; the nearest other concept in the sender's space; or C plus random noise of relative size e.
Thresholds come from honest messages only (the five template concepts, 30 messages), and are applied to a fresh run with fifteen natural
concepts (90 honest, 90 per liar type). The honest messages all passed under every rule (90/90).

| liar | median cosine | AUC against honest | caught, mean minus 2 sd (cut 0.417) |
|---|---|---|---|
| honest | 0.504 | | 0 of 90 false alarms |
| medicine mixed in at 10% | 0.505 | 0.51 | 0 of 90 |
| medicine mixed in at 25% | 0.496 | 0.59 | 1 of 90 |
| medicine mixed in at 50% | 0.454 | 0.87 | 15 of 90 |
| medicine mixed in at 75% | 0.387 | 1.00 | 71 of 90 |
| next concept mixed in at 25% / 50% / 75% | 0.498 / 0.463 / 0.415 | 0.57 / 0.79 / 0.96 | 0 / 10 / 46 of 90 |
| nearest other concept, swapped in | 0.396 | 0.99 | 56 of 90 |
| noise of 25% / 50% / 100% of the norm | 0.487 / 0.447 / 0.356 | 0.64 / 0.86 / 1.00 | 4 / 24 / 79 of 90 |

The audit separates a vector that is mostly a different concept (AUC 0.96 or more from 75% mixing) and misses a vector that is
mostly the committed one: up to 25% of a payload is indistinguishable from honest cross-family disagreement, whose own spread is
about 0.4 to 0.6. This is the gap 6m listed as untested, and it is the audit's limit, not a tuning problem: the honest cosines
are already low.

**2. The receiver's guard (6g).** For every sender, receiver and carrier concept (114 messages per row), the decoded vector's cosine
with the receiver's own `medicine` direction, against the 0.5 threshold:

| medicine mixed in | blocked | median cosine |
|---|---|---|
| 0% (honest) | 3 of 114 (2.6% false blocks) | 0.34 |
| 10% | 10 of 114 | 0.38 |
| 25% | 29 of 114 | 0.44 |
| 50% | 96 of 114 | 0.53 |
| 75% / 100% | 114 of 114 | 0.59 / 0.62 |

The guard is the stronger of the two controls: it stops most 50% mixes and all of 75% and up, but it also lets three quarters of
25% mixes through.

**3. Does the payload steer?** The receiver decodes `(1-a) C + a medicine`, scales it to 0.5 receiver norms and generates on the eight
standard prompts (4 carriers x 6 ordered pairs x 8 prompts = 192 generations per row; the lexicon decoder reads each text):

| medicine mixed in | read as the carrier | read as medicine |
|---|---|---|
| 0% | 68% | 0% |
| 25% | 60% | **0 of 192** |
| 50% | 32% | 21 of 192 (11%) |
| 100% | 0% | 75% |

At the strength the room uses, a 25% payload did not show up in a single generation; it appears at 50%, where the guard blocks
84% of messages and the audit 17%. So in this setup the mixes that evade both controls (25% and below) do not steer, and the ones
that steer (50% and up) are mostly stopped by the guard. The honest reading is narrower than "safe": it was measured with one
strength, three small models, a word-list decoder, one guarded concept and a sender that mixes linearly. A receiver run at a higher
strength, a larger or instruction-tuned receiver, or a payload chosen to be more potent per unit of cosine could move the picture,
and the audit alone gives no protection against a payload that is not a different topic. The guard should stay in front of any
receiver; the audit supplies provenance, not safety.

Not tested: stronger receiver strengths, larger receivers, an adaptive sender that optimises the payload against the audit's
cosine, guards for concepts other than `medicine`, and a calibration batch that contains a liar.

## 7. Not covered

Concepts outside the anchors; arbitrary thoughts (this is a concept channel); multi-turn exchanges; models smaller than
the anchor corpus can resolve; non-transformer architectures; adversarial senders; any claim above
`local_internal_exploratory`.
