# F3 second H100 pilot: go/no-go request

Decision wanted from the user. **Nothing here has been started**: no gman node, Modal or paid API was touched, no GPU hours spent. Everything below is prepared offline (Mac, no network spend). Hours and dollars are **estimates** built from the pilot-1 measurements; the rate is $5.04 per node-hour (gman H100). Rules: [PROTOCOL.md](PROTOCOL.md) section 15, amendment A4.

## Why a second pilot

Pilot 1 ([pilot/README.md](pilot/README.md)) measured the H100 setup and showed the sealed holdout catching the injected dev memorizer, but it had **0 promotions**, so the replay rule was never exercised on real hardware. The Mac-tuned stand-ins (learning rates, Muon steps) do not move time-to-target at this scale; the best honest gains were 0.040 and 0.039 against a threshold of 0.06. Pilot 2 aims at at least one promotion, replayed on a second node, plus a no-skill comparator at the same budget.

## Smoke result (measured, 1.17 USD): compile gain 0.61

A gated smoke test ran on my own resumed node `f3-pilot-a` ([smoke_p2/README.md](smoke_p2/README.md)): at 240 steps, baseline 245.4 s vs compile 96.3 s (median of 3 seeds), **gain 0.608 against theta 0.06**, cold-cache run alone 0.535, dev loss the same. Compile works under the harness's scrubbed environment. The smoke gate below is therefore passed; `h100-1` should promote, and the later stand-ins are then compared to a compiled frontier (about 96 s per run).

## What is new (all offline, committed, tested)

1. **Five H100 stand-ins** (`challenge/f3/arms.py`, arm `h100`; scripted patches, not agents). Expectations are predictions, not measurements:

| # | Stand-in | Why it might clear theta 0.06 | Risk |
|---|---|---|---|
| 1 | `torch.compile(model)` for the training forward | Baseline runs eager at about 1.04 s/step, which I estimate at about 14% of H100 peak; fusing the logit softcap, norms and rotary removes memory-bound passes. The metric is training seconds, so speed counts directly. | Measured by the smoke: works under the scrubbed env, gain 0.61, cold-cache cost about 18 s. The remaining uncertainty is the holdout gate (expected to pass) and the real target-stop decision. |
| 2 | weight decay 0 + grad clip 1.0 | Each alone gave gain 0.040 at pilot 1 (one eval interval = 0.042); together two intervals = 0.083 | Each alone was within noise (cv 0.018); the sum is unmeasured |
| 3 | anneal LR to step 240 (plan 360 to 240) | The target is the loss at step 240, but the baseline's LR has only decayed to 0.67 there | Gain is quantized in 10-step intervals (0.042): needs 2 intervals |
| 4 | injected dev memorizer | Repeats the sealed-holdout test against the frontier of this run | Expected reject (`holdout_failed`) |
| 5 | comment-only null control (reserve, not run at Q = 4) | Expected gain about 0 | |

2. **Random-search comparator** (`arms.random_proposals`, fixed seed 20261114, 9 hyperparameters, 1 or 2 changed per proposal, applied to the arm's frontier). First four: MATRIX_LR 0.0731; GRAD_CLIP 1.0 + WEIGHT_DECAY 0.00982; WARMDOWN_RATIO 0.88; EMBEDDING_LR 1.24. It cannot propose code (no compile), so the comparison is "hyperparameter search vs code-level proposals under one rule". n = 4 and one seed: descriptive only, no claim about agents.
3. **Two arms, one genesis.** After `init` the work directory is copied; each arm continues its own chain from the shared, signed genesis. This saves a second calibration (about 55 min). Tested: `test_forked_arms_share_one_genesis_and_both_verify`.
4. **Smoke gate** (below) that can abort the whole pilot for about 3.5 USD before calibration.
5. Amendment A4 (spec `task_spec_gpu_p2.json`, same scale as pilot 1, arms, compile on the clock, smoke gate, stop conditions). `pilot.py --arm {h100,random}`.

## Plan and commands (primary node, new)

```sh
export F3_SUBSTRATE=climbmix F3_SPEC=task_spec_gpu_p2.json F3_DATA_DIR=~/f3/data F3_HOLDOUT_DIR=/srv/f3-custodian/holdout
# 0. node: clean checkout committed with fixed author/date; venv with system site packages; pyarrow tiktoken requests rustbpe numpy cryptography
# 1. data + tokenizer (about 1.1 GB): python3 challenge/f3/task_climbmix.py prepare --data --holdout --tokens
# 2. SMOKE: DONE (see smoke_p2); on a fresh node repeat only if wanted. Original commands:
python3 challenge/f3/arms.py write --arm h100 --n 1 --out $S/compile.py
python3 challenge/f3/task_climbmix.py run --candidate challenge/f3/baseline_climbmix.py --seed 1 --out $S/base.json    --stop-batches 960
python3 challenge/f3/task_climbmix.py run --candidate $S/compile.py                     --seed 1 --out $S/cold.json    --stop-batches 960
python3 challenge/f3/task_climbmix.py run --candidate $S/compile.py                     --seed 1 --out $S/warm.json    --stop-batches 960
# GO if warm wall_s <= 0.90 x base wall_s. Else stop the node and report.
# 3. init (13 baseline runs), pin the key off the node, then fork the calibrated genesis into two arms
python3 challenge/f3/harness.py init --work ~/f3/p2 ; cp -a ~/f3/p2 ~/f3/p2_random
# 4. arm h100, then arm random, Q = 4 each (resumable, one decision about 12 to 27 min)
python3 challenge/f3/pilot.py --work ~/f3/p2        --arm h100   --max 4
python3 challenge/f3/pilot.py --work ~/f3/p2_random --arm random --max 4
# 5. reveal both, verify both without the harness
python3 challenge/f3/harness.py reveal --work ~/f3/p2 ; python3 challenge/f3/harness.py reveal --work ~/f3/p2_random
python3 challenge/f3/verify.py --work ~/f3/p2 --host-pubkey "$(cat host.pub)" --replay none      # and the same for p2_random
```

Second node (new, created only after step 5): same tree and fixed-date commit as A3, primary's `tokenizer/` copied in, then `verify.py --work <copy of receipts.jsonl, candidates/, host.pub> --host-pubkey ... --replay all` for at most 2 promotions of arm `h100`.

## Cost estimate (ceiling 40 USD)

Basis, **measured at pilot 1**: a full decision takes about 26.5 min on the eager baseline (6 runs of about 250 s plus evaluation); an early-stopped one (memorizer) 15.4 min; calibration (13 runs) about 55 min; non-decision overhead (setup, data, calibration, verify) was 87 min of the node's 281.7 min. **Measured at the smoke:** a compiled run takes 96 s (114 s with a cold cache). **Assumed:** evaluation about 90 s per decision as at pilot 1; replay is 9 runs per promotion, about 15 min for a compiled promotion.

| Item | Hours | USD |
|---|---|---|
| Primary: setup and data on a new node (0.1 h instead if my pilot-1 node `f3-pilot-a`, disk intact, is resumed) | 0.5 | 2.5 |
| Primary: calibration (eager baseline, 13 runs) | 0.9 | 4.5 |
| Primary: arm `h100`, Q = 4 (compile 19 min with an eager frontier, wd/clip 11, anneal 11, memorizer 8) | 0.8 | 4.1 |
| Primary: arm `random`, Q = 4 (4 x 26.5 min, eager) | 1.8 | 8.9 |
| Primary: reveal, verify | 0.2 | 1.0 |
| Second node: setup 0.5, replay of 2 promotions 0.5, verify 0.1 | 1.1 | 5.5 |
| **Total** | **5.3** | **about 26.5** |
| If a random proposal hits the 737 s wall cap in 2 decisions, or any stand-in underperforms | +0.5 | up to about 29 |

Q = 5 per arm would add about 0.6 h (3 USD), to about 30 USD, so Q = 5 now fits comfortably; Q = 4 stays the default and the reserves (null control, `random-5`) are the first thing to add. **Hard ceiling 40 USD; stop and ask if projected spend before creating the second node exceeds 36 USD.**

## Stop conditions

First of: smoke gate fails (about 3.5 USD spent); T0 above 10 min, cv above 0.10 or theta above 0.15 at calibration; none of `h100-1` to `h100-3` promotes (stop, report; no second node, arm `random` not run); a promotion that fails replay (stop, report as a false promotion, no retry); a candidate process reading outside its directory; projected spend above 36 USD before the second node; the 40 USD ceiling.

## What it supports, and does not

Supports (if it goes as planned): a promotion that re-derives from signed receipts and reproduces on a second same-type host on recorded and fresh seeds; the sealed holdout rejecting the injected memorizer again; a null-ish comparator stream under the same rule. Does not support: that agents beat anything (stand-ins are scripted; the random arm is n = 4 on one seed); "agents never see the holdout" (same Unix user for candidates and evaluator, A3); an independent second host (same operator); any scale other than this one. Promotion is not guaranteed: if compile does not pay as predicted the smoke gate says so cheaply, and that is a result too.

## Needed from the user

1. Go or no-go for pilot 2 (about 27 USD expected, ceiling 40).
2. The same node authorization as pilot 1: two new H100 nodes, gman with the sandbox bypass for gman only, nothing else touched (`playbox-tp3` and others are not ours).
3. Whether to keep Q = 4 per arm (recommended) or Q = 5 (about 30 USD).
