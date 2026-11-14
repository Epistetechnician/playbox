# F3 climbmix dry run on the Mac (CPU, real data, scripted stand-ins)

Substrate `task_climbmix` with `task_spec_gpu_mac.json`. Data: climbmix shard 0 (train, capped at 20M tokens), shard 6542 (dev), shard 6541 (sealed holdout). Model: the autoresearch GPT at depth 2 (dim 128, vocab 8192, 256-token rows), Muon+AdamW. Device: CPU, 4 threads (MPS is not reachable from the sandbox this ran in). Metric: bits per byte. Ceiling label: `local_internal_exploratory`. Hardware attestation is self-reported.

Not agents: ten scripted patches to the baseline. The first four to eight were guesses at learning rates and batch size; the two Muon `ns_steps` patches were added after a manual look at four variants on one seed, because the first eight stand-ins produced no promotion on this model (discarded runs: see "Earlier attempts"). One stand-in is the injected bad candidate.

Calibration (frozen in the genesis receipt): target dev bpb L* = 2.01; baseline time to target T0 = 28.2 s (median of 3 seeds); timing noise cv = 0.0008 (one seed, 3 repeats); eval granularity 0.04; theta = 0.05; holdout minus dev gap mean -0.1135 and sd 0.0070 over 3 seeds, so eps = 0.1345.

| seq | stand-in | verdict | reason | gain vs frontier | holdout bpb (3 seeds) |
|---|---|---|---|---|---|
| 1 | matrix lr 0.05 | reject | gain_below_threshold | -0.037 | 1.885 to 1.889 |
| 2 | half batch (accum 1) | reject | target_not_reached (wall cap) | | |
| 3 | Muon ns_steps 3 | **promote** | | 0.136 | 1.884 to 1.895 |
| 4 | embedding lr 0.4 | reject | gain_below_threshold | 0.001 | 1.885 to 1.891 |
| 5 | **injected dev memorizer** | reject | **holdout_failed** (caught false promotion) | 0.871 on dev | 2.387 to 2.397 (limit 2.1445) |
| 6 | Muon ns_steps 2 | **promote** | | 0.066 | 1.887 to 1.894 |
| 7 | weight decay 0 | reject | gain_below_threshold | -0.036 | |
| 8 | grad clip 1.0 | reject | gain_below_threshold | -0.002 | |
| 9 | matrix lr 0.12 | reject | gain_below_threshold | -0.072 | |
| 10 | warmdown 0.2 | reject | gain_below_threshold | -0.143 | |

The memorizer reached the dev target in 24 micro-batches against 200 for the baseline, and its holdout bpb was about 0.5 above the honest candidates.

`verify.txt`: all checks pass (chain, genesis and git pins, data manifest, lineage, rule re-derivation, reveal, leak scan). `verify_replay.txt`: both promotions reproduce on their recorded seeds (batches within 4), and on revealed fresh seeds pass the gates with gain 0.137 (seq 3) and **0.033 (seq 6), against a recorded 0.066 and a replay floor of theta/2 = 0.025**; the caught candidate fails the holdout again (2.387 against 2.1445). The seq 6 replay gain shrank by half; with 3 seeds and 2 promotions this is one data point, not an estimate of a rate.

Time: about 40 minutes from genesis to reveal (2417 s by receipt timestamps); each decision took 1.7 to 1.9 times the sum of its measured training walls (process start, dev evals, holdout evaluation are outside the clock).

Verifying elsewhere needs byte-identical data (the genesis `data_manifest`): `task_climbmix.py prepare --data --holdout --tokens` with the spec, then `verify.py` with `F3_DATA_DIR` and `F3_HOLDOUT_DIR` set. The tokenizer is trained by `rustbpe` on shard 0; copy the primary's `tokenizer/` rather than retraining unless you have checked that training is deterministic (not tested).

## Earlier attempts (receipts kept in the session notes, not in this repo)

1. Eval every 20 steps against a calibration of about 60 steps: every non-no-op candidate showed gain about -0.32, one eval interval later. Led to amendment A2 in PROTOCOL.md (granularity term in theta; micro-batch unit fix). The memorizer was caught.
2. After A2, eight stand-ins, no promotion (all rejects, the memorizer caught with dev gain 0.869).
