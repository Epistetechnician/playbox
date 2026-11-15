# Metered benchmark: text, RAG, cached working set, latent vector and routed swarms on one task

Code: `steer/playbox_steer/metered_bench.py` (task, arms, meter), `metered_hf.py` (receiver wrapper and runner),
`steer/tools/metered_bench_report.py` (tables). Results: `steer/results/metered/metered_{small,big}.json`; the first run, with variable decode
length, is kept as `*_variable_decode.json`. Tests: `steer/tests/test_metered_bench.py` (5 pass, including a check that a cached-prefix answer
equals the uncached one and that reusing the cache does not drift).

## The task and the arms

Forty sessions per pair. Each session is one project of a shared corpus (360 chunks, 58,656 tokens, 32 facts per project, the
[kv-hybrid](kv-hybrid.md) corpus) with one **topic turn** ("which field is Project P about?", answered from a short overview chunk) and seven **fact
turns** ("what is the badge code for Project P?"). Same sessions, same receiver, same scoring for every arm. Topic turn: the generation names the
project's field (lexicon decoder). Fact turn: the exact value appears in the generation.

| arm | how the sender's knowledge reaches the receiver |
|---|---|
| none | nothing (the floor) |
| text_word | the topic as one word of text ("Topic: aviation.") |
| text_rag | BM25 top-5 chunks for the turn's own question, prefilled fresh every turn |
| hybrid | the project's working set (BM25 top-10) prefilled once per session into a KV cache, reused every turn |
| latent_q, latent_free | the sender's thought mapped into the receiver and injected at layer 0.45 (the E7 channel); the receiver answers the question prompt (q) or continues an open prompt (free) |
| swarm | latent_free on the topic turn, hybrid on fact turns |
| text_swarm | the same routing with the topic as a text word: what the latent route has to beat |

Pairs: SmolLM2-135M to Qwen3-1.7B-Base (float32) and OLMo-2-7B to Qwen3-8B-Base (bfloat16), one H100, gman rate 3.765 USD per GPU-hour.

**Meter.** Every region that touches a model is timed with a synchronised clock, batch size 1, one request at a time, so GPU-seconds equal wall time.
Phases: sender thinking, the one cache prefill per session, receiver generation. Every call decodes exactly 16 tokens (end-of-sequence suppressed;
see the first run below for why). The mapper fit (400 s small, 520 s big, measured) is amortised over 1,000 sessions and charged to the latent arms only.

## Results (fixed decode, 40 sessions, one seed)

Small receiver (Qwen3-1.7B; topic n = 40, fact n = 280 per arm):

| arm | topic % (95% CI) | fact % (95% CI) | context tokens/turn | GPU-s/session | USD per 1000 sessions | correct answers per USD |
|---|---|---|---|---|---|---|
| none | 2 (0-13) | 0 (0-1) | 17 | 2.37 | 2.483 | 10 |
| text_word | 72 (57-84) | 0 (0-1) | 18 | 2.37 | 2.479 | 292 |
| text_rag | 5 (1-16) | 82 (78-86) | 917 | 2.83 | 2.960 | 1968 |
| hybrid | 0 (0-9) | 78 (73-83) | 1698 | 2.57 | 2.685 | 2039 |
| latent_q | 55 (40-69) | 0 (0-1) | 17 | 2.94 | 3.076 | 179 |
| latent_free | 60 (45-74) | 0 (0-1) | 15 | 2.93 | 3.069 | 196 |
| swarm | 60 (45-74) | 78 (73-83) | 1486 | 3.09 | 3.236 | 1878 |
| text_swarm | 72 (57-84) | 78 (73-83) | 1488 | 2.53 | 2.644 | **2345** |

Big receiver (Qwen3-8B):

| arm | topic % (95% CI) | fact % (95% CI) | context tokens/turn | GPU-s/session | USD per 1000 sessions | correct answers per USD |
|---|---|---|---|---|---|---|
| none | 0 (0-9) | 0 (0-1) | 17 | 2.87 | 2.998 | 0 |
| text_word | 55 (40-69) | 0 (0-1) | 18 | 2.86 | 2.992 | 184 |
| text_rag | 8 (3-20) | 98 (95-99) | 917 | 3.19 | 3.336 | 2076 |
| hybrid | 5 (1-16) | 97 (94-98) | 1698 | 3.20 | 3.349 | 2046 |
| latent_q | 70 (55-82) | 0 (0-1) | 17 | 3.59 | 3.752 | 187 |
| latent_free | 57 (42-72) | 0 (0-1) | 15 | 3.61 | 3.776 | 152 |
| swarm | 57 (42-72) | 97 (94-98) | 1486 | 3.69 | 3.860 | 1910 |
| text_swarm | 55 (40-69) | 97 (94-98) | 1488 | 2.96 | 3.094 | **2375** |

Context tokens for hybrid are the cached prefix plus the question; the prefix is read from the cache, not recomputed.

## What it shows

- **Only the document channels carry facts.** RAG and the cached working set answer 78 to 98% of fact turns; the vector and the topic word answer none.
  A steering vector carries a concept, not a badge code, and the table says so rather than dropping the turn type.
- **The latent vector buys no topic accuracy over one word of text and costs more.** Topic accuracy of latent_free against text_word is 60 against 72
  (small) and 57 against 55 (big), inside each other's intervals. It costs 0.56 to 0.75 GPU-seconds more per session (small 2.93 against 2.37; big
  3.61 against 2.86): the sender's forward passes, the steered generation, and the fit share. The fit share shrinks with more sessions
  (400 s over 1,000 sessions is 0.4 s per session), but the per-session sender and steering cost does not, so there is no session count at which
  the vector is cheaper than the word on this task. Its other properties (no text in the channel, nothing added to the receiver's context: 15 tokens
  against 18 here) are real but are not a cost advantage.
- **Routing by turn type beats any single channel per dollar, and the text route beats the latent route.** text_swarm (word for the topic, cached
  working set for facts) is the best arm per dollar on both pairs (2,345 and 2,375 correct answers per USD), 18 to 20% cheaper than the latent swarm
  at the same fact accuracy and the same topic accuracy within intervals.
- **Caching the working set does not beat RAG here.** Small receiver: hybrid is 9% cheaper per session (2.57 against 2.83 GPU-s) with a 4-point lower
  fact score (78 against 82, intervals overlap). Big receiver: equal cost (3.20 against 3.19) and equal accuracy. This agrees with the vLLM result in
  [kv-hybrid.md](kv-hybrid.md). Each session here has its own project, so no cache is shared between sessions; a cache pays when many agents read
  the same working set, which this task does not exercise.

## First run and the confound it exposed

The first run let generation stop at end-of-sequence. Then GPU-seconds mostly followed how early an arm's short answer ended the generation, not
how the context arrived: the empty-context arm cost 2.06 s per session on the small pair against 1.62 for RAG, which is the wrong way round. Forcing
16 tokens on every call removed that (none and text_word now agree to the hundredth, 2.37 and 2.37). The variable-decode files are kept so the size
of the confound is visible; do not quote their cost figures.

## Caveats

- Forty sessions: the topic turn has 40 trials per arm (interval about 28 points wide), the fact turns 280. One seed, one corpus.
- Batch size 1 on Hugging Face, not a batched server; absolute GPU-seconds would be much lower under vLLM for every arm, and the ratios could change.
  Decode is a fixed 16 tokens, which a real system would cut short.
- The document arms score near zero on the topic turn because a base model asked "which field" over a page of records does not answer with the
  field; that is a prompt weakness of the task, not a verdict on those channels. The latent_free arm uses an open prompt, the others the question prompt.
- The text arms' sender cost is not counted (the topic word is the true topic, an oracle), so their cost is a lower bound; a text sender would
  read the documents once per session, as the latent sender does.
- Amortised fit over 1,000 sessions is an assumption (a stated parameter, `--amortise`); the mapper is per ordered pair, as in the rest of this project.
