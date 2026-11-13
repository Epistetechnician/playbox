# Cached full context vs RAG vs retrieval-then-cache (hybrid), 128K corpus

Code: `steer/playbox_steer/kv_hybrid.py` (corpus, BM25, sessions, streaming client, summary; stdlib only, `steer/tests/test_kv_hybrid.py`),
`steer/tools/kv_hybrid_modal.py` (Modal, vLLM 0.25.1, one H100 per model), `steer/tools/kv_hybrid_report.py` (tables below from the result files).
Raw results: `steer/results/kv_hybrid/kv_4b.json`, `kv_30b.json` (`kv_4b_v1.json` is the first 4B run, same conclusions, smaller question set).

## Status: the first throughput claim did not survive a control

The first version of this benchmark (sections "What was tested" to "What this does not show" below) ran its arms one after another on one server with the prefix cache on and no separation between arms, so an arm could reuse blocks an earlier arm had cached (the hybrid prompts overlap each other and the RAG preamble). Its throughput, first-token and cost columns, and the statement that the hybrid beats plain RAG on throughput and cost, are therefore **not reliable and are withdrawn**. Its accuracy columns are not affected by caching and stand.

The corrected run ("Held-out K, harder questions, salted cache" below) puts a different run id at the very start of every arm's prompt, so no cache block is shared between arms (agents inside one arm still share), picks K on a separate corpus and separate questions, and adds three question kinds. What it shows: the retrieval-then-cache hybrid does **not** beat plain RAG on throughput (3,053 vs 4,526 turns per minute on the 4B model, 2,862 vs 3,854 on the 30B), and it loses a little accuracy on lookups (96.1% vs 100%, 4B). Its one advantage is first-token latency on later turns (0.12 s vs 0.22 s), at the price of a much slower first turn (1.07 s vs 0.23 s). With a fixed per-session working set it fails on questions about two projects (2%) and on topic drift (48%); the adaptive variant restores those (94 to 99.6%, against RAG's 96 to 100%) but not the throughput.

## What was tested

A synthetic 111K-token corpus (640 chunks, 80 projects x 8 chunks). Each chunk states four facts about one project among filler; the same 32
attributes recur for every project with random values, so a lookup needs the right project and the right attribute and a near-miss project is a
real distractor. Agents ask "What is the <attribute> for Project <name>?", scored by exact substring. Prompt = shared preamble + context + question.

- `full`: the whole corpus is the context (111K tokens), with the vLLM prefix cache on (after one cold request) or off.
- `rag`: per question, BM25 top-5 chunks (about 940 tokens); the context changes every turn.
- `hybrid`: per session, BM25 top-K chunks for the project name, retrieved once, in document order, so the prefix is identical on every turn and across agents on the same project. K = 24 (4.2K tokens) or K = 10 (the project's 8 chunks plus 2; 1.8K tokens, `hybrid10`).
- Scenarios: `indep` = 32 agents on 32 different projects, 4 turns; `team` = 64 agents on 8 projects, 16 turns. All agents run at once. `full` without the cache runs 1 turn (each request prefills 111K and only about 3 fit in memory at once).

## Results, first version (throughput columns withdrawn, see Status)


### Qwen/Qwen3-4B-Instruct-2507 (H100, corpus 111,316 tokens, 640 chunks; retrieval recall {'indep/rag': 1.0, 'indep/hybrid': 1.0, 'indep/hybrid10': 1.0, 'team/rag': 1.0, 'team/hybrid': 1.0, 'team/hybrid10': 1.0})

| scenario / arm | n | accuracy (95% CI) | first token, turn 1 | first token, later turns | turns/min | cached | prompt tokens | $ per 1k correct |
|---|---|---|---|---|---|---|---|---|
| indep/full+cache | 128 | 70.3% (62-78) | 3.02 s | 5.57 s | 338 | 100% | 111,342 | 0.277 |
| indep/rag+cache | 128 | 100.0% (97-100) | 0.36 s | 0.24 s | 4423 | 9% | 936 | 0.015 |
| indep/hybrid+cache | 128 | 97.7% (93-99) | 1.05 s | 0.16 s | 2958 | 76% | 4,195 | 0.023 |
| indep/hybrid10+cache | 128 | 100.0% (97-100) | 0.37 s | 0.12 s | 6017 | 79% | 1,793 | 0.011 |
| team/rag+cache | 1024 | 100.0% (100-100) | 0.37 s | 0.18 s | 11589 | 83% | 940 | 0.006 |
| team/hybrid+cache | 1024 | 96.5% (95-97) | 0.21 s | 0.40 s | 8412 | 100% | 4,205 | 0.008 |
| team/hybrid10+cache | 1024 | 100.0% (100-100) | 0.21 s | 0.22 s | 13588 | 100% | 1,803 | 0.005 |
| indep/full-nocache | 32 | 68.8% (51-82) | 127.00 s | - | 8 | 0% | 111,342 | 11.913 |
| indep/rag-nocache | 128 | 100.0% (97-100) | 0.36 s | 0.26 s | 4064 | 0% | 936 | 0.016 |
| indep/hybrid-nocache | 128 | 97.7% (93-99) | 1.18 s | 1.14 s | 918 | 0% | 4,195 | 0.073 |
| indep/hybrid10-nocache | 128 | 100.0% (97-100) | 0.47 s | 0.41 s | 2273 | 0% | 1,793 | 0.029 |
| team/rag-nocache | 1024 | 100.0% (100-100) | 0.53 s | 0.26 s | 4508 | 0% | 940 | 0.015 |
| team/hybrid10-nocache | 1024 | 100.0% (100-100) | 0.91 s | 0.82 s | 2316 | 0% | 1,803 | 0.028 |

### Qwen/Qwen3-30B-A3B-Instruct-2507-FP8 (H100, corpus 111,316 tokens, 640 chunks; retrieval recall {'indep/rag': 1.0, 'indep/hybrid': 1.0, 'indep/hybrid10': 1.0, 'team/rag': 1.0, 'team/hybrid': 1.0, 'team/hybrid10': 1.0})

| scenario / arm | n | accuracy (95% CI) | first token, turn 1 | first token, later turns | turns/min | cached | prompt tokens | $ per 1k correct |
|---|---|---|---|---|---|---|---|---|
| indep/full+cache | 128 | 91.4% (85-95) | 2.47 s | 5.27 s | 365 | 100% | 111,342 | 0.197 |
| indep/rag+cache | 128 | 100.0% (97-100) | 1.12 s | 0.29 s | 2635 | 9% | 936 | 0.025 |
| indep/hybrid+cache | 128 | 99.2% (96-100) | 1.21 s | 0.10 s | 2636 | 76% | 4,195 | 0.025 |
| indep/hybrid10+cache | 128 | 100.0% (97-100) | 0.46 s | 0.13 s | 4793 | 79% | 1,793 | 0.014 |
| team/rag+cache | 1024 | 100.0% (100-100) | 0.44 s | 0.17 s | 10018 | 83% | 940 | 0.007 |
| team/hybrid+cache | 1024 | 100.0% (100-100) | 0.23 s | 0.33 s | 9064 | 100% | 4,205 | 0.007 |
| team/hybrid10+cache | 1024 | 99.5% (99-100) | 0.23 s | 0.17 s | 14413 | 100% | 1,803 | 0.005 |
| indep/full-nocache | 32 | 90.6% (76-97) | 161.94 s | - | 6 | 0% | 111,342 | 11.521 |
| indep/rag-nocache | 128 | 100.0% (97-100) | 1.10 s | 0.34 s | 2425 | 0% | 936 | 0.027 |
| indep/hybrid-nocache | 128 | 99.2% (96-100) | 1.33 s | 1.27 s | 818 | 0% | 4,195 | 0.081 |
| indep/hybrid10-nocache | 128 | 100.0% (97-100) | 0.56 s | 0.38 s | 1922 | 0% | 1,793 | 0.034 |
| team/rag-nocache | 1024 | 100.0% (100-100) | 0.63 s | 0.36 s | 3346 | 0% | 940 | 0.02 |
| team/hybrid10-nocache | 1024 | 99.7% (99-100) | 1.05 s | 0.90 s | 2092 | 0% | 1,803 | 0.032 |


Cold first request for the full corpus with the cache on: 7.7 s (4B) and 9.8 s (30B) with nothing else running.

## Reading, first version (the throughput, latency and cost claims below are withdrawn, see Status)

- **Your friend's point holds for full context.** Even with 100% of the prompt served from cache, 32 agents on a 111K context wait about 5 s for the first token (3 s on turn 1), and accuracy is 70% (4B) and 91% (30B) against 100% for retrieval. Without the cache, the first token takes 127 s (4B) and 162 s (30B) and 6 to 8 turns per minute. The cache removes prefill; it does not remove attention over 111K tokens on every decode step.
- **Plain RAG is the strong baseline.** On this task it is 100% accurate at about 0.2 to 0.4 s per first token and thousands of turns per minute, with or without the cache (the cache only helps its shared preamble, 9% of the prompt in `indep`, 83% in `team` because agents repeat the same retrieval).
- **Retrieval-then-cache (`hybrid10`) beats plain RAG here, but not on every column.** Against `rag` with the cache on, same models, same questions: accuracy equal (100% in `indep` on both models; 100% and 99.5% against 100% in `team`); turns per minute 6,017 vs 4,423 (4B) and 4,793 vs 2,635 (30B) in `indep`, 13,588 vs 11,589 (4B) and 14,413 vs 10,018 (30B) in `team`; dollars per 1,000 correct answers 0.011 vs 0.015 and 0.014 vs 0.025 in `indep`, 0.005 vs 0.006 and 0.005 vs 0.007 in `team`. First token on later turns is lower in `indep` (0.12 vs 0.24 s on 4B, 0.13 vs 0.29 s on 30B) and level or higher in `team` (0.22 vs 0.18 s on 4B, 0.17 vs 0.17 s on 30B).
- **The gain is the cache, and it needs a big enough stable prefix.** The same `hybrid10` without the cache is slower than RAG (2,273 vs 4,064 turns per minute on 4B `indep`; 2,316 vs 4,508 `team`), because its prompt is twice as long. K matters: `hybrid` (K = 24) loses 2 to 3 points of accuracy on the 4B model (97.7% and 96.5%) and is slower than RAG in `team` on both models; trimming to the project's own chunks fixed that.
- **What was fixed.** The recommendation was to use both: retrieval decides what goes in context, the cache makes it cheap to reuse. On this benchmark that gives the accuracy of RAG, the reuse of a cache, and a higher throughput and lower cost per correct answer than either alone, at both model sizes.

## What this does not show (read before quoting any of it)

- **Not SOTA.** There is no comparison with published systems or standard benchmarks here, only with the two baselines in this harness. Nothing in these numbers supports a state-of-the-art claim.
- **Synthetic, easy for retrieval.** BM25 recall was 1.0 for every arm, so RAG's 100% is a ceiling; the benchmark cannot show a case where retrieval misses and full context would win (multi-hop questions, aggregation, answers spread across many chunks, vague queries). Those are where full context is expected to do better, and they are untested.
- **K = 10 was chosen after K = 24 underperformed on the same questions** and uses the fact that a project has 8 chunks. It is a tuned setting, not a held-out one. A fair claim needs K picked on separate questions.
- **Sessions are topic-structured by construction** (every turn is about the same project). Hybrid only helps when later questions fall inside the first retrieval; a session that wanders would need re-retrieval, which breaks the cached prefix.
- **One seed, one run per arm, one GPU, two Qwen3 models, greedy decoding.** Throughput arms last seconds (128 to 1,024 requests), so turns per minute is a burst figure, not steady state. Differences of a few points in accuracy are within the confidence intervals shown.

## Held-out K, harder questions, salted cache

Code: `steer/tools/kv_hybrid_run.py` (run on a gman H100 with vLLM 0.33; results `steer/results/kv_hybrid/kv_v4_<model>.json`; `kv_v3_*.json` is the same run without the cache salt and is superseded). Phase 1 picks K for the hybrid on a different corpus (seed 100) and different questions (lookup plus paraphrase); phase 2 freezes it and tests on the original corpus and a second one (seeds 0 and 1), 32 agents x 4 turns each, cache on, a different cache salt per arm. Question kinds: `lookup` (the attribute by name), `para` (the attribute in other words: "the day it goes live"), `cross` (two projects in one question, both values must be right), `drift` (the session's second half asks about another project). `hybrid` keeps its working set (K chunks for the session's project); `adaptive` keys the working set on the question's project (one project: its cached working set; two projects: retrieve for the question; none: RAG). Numbers pool both test corpora (n = 256 per row).

**Qwen3-4B-Instruct-2507**, K chosen on the dev corpus = 24. Dev sweep (accuracy / turns per minute): K=4: 27% / 14000, K=6: 44% / 11163, K=8: 55% / 9204, K=10: 56% / 8060, K=12: 57% / 7269, K=16: 57% / 5764, K=24: 57% / 3978

| questions | arm | n | accuracy (95% CI) | first token, turn 1 | first token, later turns | turns/min | prompt tokens |
|---|---|---|---|---|---|---|---|
| lookup | full | 256 | 53.9% (48-60) | 8.79 s | 5.04 s | 310 | 111,216 |
| lookup | rag | 256 | 100.0% (99-100) | 0.23 s | 0.22 s | 4526 | 945 |
| lookup | hybrid | 256 | 96.1% (93-98) | 1.07 s | 0.12 s | 3053 | 4,190 |
| lookup | adaptive | 256 | 96.5% (93-98) | 1.06 s | 0.13 s | 3063 | 4,190 |
| para | rag | 256 | 12.9% (9-18) | 0.23 s | 0.09 s | 8302 | 939 |
| para | hybrid | 256 | 16.0% (12-21) | 1.07 s | 0.16 s | 3148 | 4,192 |
| para | adaptive | 256 | 16.4% (12-21) | 1.06 s | 0.16 s | 3127 | 4,192 |
| cross | rag | 256 | 95.7% (92-98) | 0.23 s | 0.09 s | 3749 | 954 |
| cross | hybrid | 256 | 2.0% (1-4) | 1.06 s | 0.07 s | 2683 | 4,200 |
| cross | adaptive | 256 | 94.1% (91-96) | 0.45 s | 0.13 s | 2149 | 1,824 |
| drift | rag | 256 | 100.0% (99-100) | 0.23 s | 0.21 s | 4490 | 948 |
| drift | hybrid | 256 | 48.0% (42-54) | 1.08 s | 0.14 s | 3089 | 4,190 |
| drift | adaptive | 256 | 95.3% (92-97) | 1.06 s | 0.17 s | 2529 | 4,204 |

**Qwen3-30B-A3B-Instruct-2507-FP8**, K chosen on the dev corpus = 24. Dev sweep (accuracy / turns per minute): K=4: 29% / 7366, K=6: 49% / 9011, K=8: 65% / 7022, K=10: 66% / 6019, K=12: 70% / 4752, K=16: 70% / 5269, K=24: 75% / 3644

| questions | arm | n | accuracy (95% CI) | first token, turn 1 | first token, later turns | turns/min | prompt tokens |
|---|---|---|---|---|---|---|---|
| lookup | full | 256 | 85.2% (80-89) | 10.90 s | 5.12 s | 286 | 111,218 |
| lookup | rag | 256 | 100.0% (99-100) | 0.29 s | 0.22 s | 3854 | 946 |
| lookup | hybrid | 256 | 99.6% (98-100) | 1.14 s | 0.10 s | 2862 | 4,192 |
| lookup | adaptive | 256 | 100.0% (99-100) | 1.15 s | 0.11 s | 2835 | 4,192 |
| para | rag | 256 | 22.3% (18-28) | 0.29 s | 0.17 s | 5081 | 940 |
| para | hybrid | 256 | 50.8% (45-57) | 1.15 s | 0.14 s | 2765 | 4,194 |
| para | adaptive | 256 | 50.8% (45-57) | 1.15 s | 0.14 s | 2893 | 4,194 |
| cross | rag | 256 | 100.0% (99-100) | 0.29 s | 0.21 s | 3310 | 955 |
| cross | hybrid | 256 | 2.0% (1-4) | 1.16 s | 0.10 s | 2327 | 4,202 |
| cross | adaptive | 256 | 97.3% (94-99) | 0.53 s | 0.32 s | 2072 | 1,825 |
| drift | rag | 256 | 100.0% (99-100) | 0.29 s | 0.25 s | 3857 | 949 |
| drift | hybrid | 256 | 50.4% (44-56) | 1.16 s | 0.13 s | 2891 | 4,192 |
| drift | adaptive | 256 | 99.6% (98-100) | 1.15 s | 0.20 s | 2300 | 4,204 |

Reading:

- **Cached full context loses on every metric**: 54% (4B) and 85% (30B) on lookups, 8.8 to 10.9 s to the first request's first token and about 5 s on later turns, 286 to 310 turns per minute.
- **Plain RAG wins accuracy and throughput on lookups** (100%, 4,526 and 3,854 turns per minute). The hybrid's first-token time on later turns is lower (0.12 vs 0.22 s on 4B, 0.10 vs 0.22 s on 30B) because 77% of its 4.2K-token prompt comes from cache, but every turn still reads 4.2K tokens against RAG's 0.9K, and its first turn costs 1.07 to 1.15 s.
- **Dev-set choice of K was 24 for both models** (best dev accuracy; the dev set includes paraphrased questions, which improve with more chunks). The earlier K = 10 result was tuned on the test questions and is not supported by this run.
- **A fixed working set breaks when the question leaves it**: `hybrid` scores 2% on two-project questions and 48 to 50% on drift. `adaptive` repairs both (cross 94.1% on 4B, 97.3% on 30B; drift 95.3% and 99.6%), RAG gets 95.7 to 100%, so adaptive matches RAG here but is not faster than it.
- **Paraphrased questions are mostly a model problem**: with the right chunks in context (hybrid and adaptive recall is 100%), the 4B model answers 16% and the 30B model 51%; RAG's retrieval misses the right chunk (recall 64%) and gets 13% and 22%. The hybrid is ahead of RAG there, by model strength, at low absolute accuracy.
- **Where this leaves the combined idea**: a cache-friendly working set buys later-turn latency, not throughput or accuracy, on this task; it needs a fallback when the question leaves the working set; and plain RAG is the baseline to beat on a task where retrieval is easy. Not tested: a workload with a much larger shared working set (where caching 4K tokens per agent is a bigger saving than here), a public long-context benchmark, and more than one seed per question kind.
