# Smoke test of the compile stand-in (H100, 2026-11-14)

Question: does `h100-1` (`torch.compile` on the training forward, [arms.py](../../../challenge/f3/arms.py)) clear theta = 0.06 at the pilot step budget? **Yes, by a wide margin: gain 0.61 (0.55 above theta).**

Setup: resumed my own node `f3-pilot-a` (disk intact, data and venv from pilot 1; no new node, so no setup cost), `task_spec_gpu.json` (depth 8, 2048-token rows, 2**19 tokens per step), run through `task_climbmix.run_candidate`, which is the harness's own scrubbed subprocess (`PATH=/usr/bin:/bin`, `HOME` a temp dir, `-I`), so compile worked under the scrubbed environment. 240 steps = 960 micro-batches on seeds 1 to 3, training wall excludes evaluation, compile time is inside the wall. Not the full decision procedure: no target stop, no holdout, no interleaved frontier runs; it is the same wall measure on the same steps.

| run | seed | wall (s) | dev loss at 240 steps |
|---|---|---|---|
| baseline (eager) | 1 | 245.8 | 1.1789 |
| compile, cold cache | 1 | 114.3 | 1.1782 |
| compile, warm cache | 1 | 96.0 | 1.1774 |
| baseline | 2 | 245.4 | 1.1823 |
| compile | 2 | 96.3 | 1.1782 |
| baseline | 3 | 245.4 | 1.1793 |
| compile | 3 | 96.3 | 1.1787 |

Median over 3 seeds: baseline 245.4 s, compile 96.3 s, **gain = 1 - 96.3/245.4 = 0.608** (theta 0.06, margin 0.548). Cold-cache run alone: gain 0.535. Compile costs only about 18 s on a cold cache (114.3 vs 96.0 s). Dev loss at step 240 is the same (1.177 to 1.179 vs 1.179 to 1.182; slightly lower compiled, within seed noise), so the candidate reaches the target L* = 1.18 at about the same step, and the gain is throughput, not quality. The baseline ran 2.55 times slower than compiled, so the eager baseline reached about 14% of peak or less, as estimated.

Caveats: the real decision measures time to the target dev loss with interleaved frontier runs and a holdout gate (the holdout gap on this setup was +0.005 at pilot 1, eps 0.015; compile is numerically near-identical so the gate is expected to pass, but this smoke did not measure it). Raw numbers: `smoke_results.jsonl`.

Spend: node f3-pilot-a 285.73 to 303.17 min = 17.4 min, **1.17 USD** (`gman usage`, 19.20 to 20.37 USD cumulative for the node, ceiling for this smoke was 5 USD). Node state: **stopped (disk intact)**, confirmed with `gman node ls`; no other node was touched. Auto-stop: a local guard script (stop at smoke_done or 40 min) stopped it; `tools/gman-run-stop.sh` named in the request does not exist, so I wrote that guard in the scratchpad (not committed).
