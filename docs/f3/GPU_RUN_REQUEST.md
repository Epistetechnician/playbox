# F3 GPU run: go/no-go request

Decision wanted from the user. Nothing here has been started: no gman node was woken, started, stopped or touched, and no GPU hours were spent. All hours and dollars below are **estimates**, not measurements, except the rate ($5.04/h, gman H100, as measured by the user).

## Proposal

**Conditional GO for a pilot, NO-GO for the full run until the substrate adapter exists and the pilot passes.** The harness, verifier and negative tests work on a CPU toy ([dry_run](dry_run/README.md): 4 promotions, 5 rejections for gain, 1 holdout-caught injected candidate, all re-derived and replayed by the verifier). What is not built is the GPU substrate: `challenge/f3/task.py` generates synthetic data and trains a 2-layer model; it has no adapter for the autoresearch `prepare.py`/`train.py` (climbmix shards, 8192-token BPE, bits per byte). That adapter is about a day of CPU-only work and must be dry-run on the Mac (autoresearch supports MPS) before any GPU time is bought.

## What the real run is

Target: autoresearch GPT (about 50M parameters) on climbmix shards; metric = harness-measured training seconds to a fixed dev bpb `L*`, evaluation excluded, ratio against the frontier on the same host ([PROTOCOL.md](PROTOCOL.md) sections 2, 4, 6). Dev = pinned val shard 6542 (agents see it). Holdout = a shard agents never get, committed by Merkle root in genesis, revealed after the last candidate. Candidates run under `playbox-runner` (no network, no custodian directory), not as plain subprocesses.

## Node plan (user to approve; I will not touch gman)

| Role | Node | Notes |
|---|---|---|
| Primary | one new H100 node | Not `playbox-tp3` (a command that is not ours is running there), not `playbox-swarm3`, not `jspace-*`. User names the node or approves creating one. gman needs the sandbox bypass, which I did not use: the user must run or authorize the gman commands. |
| Second host | a different H100 node, separately provisioned, same GPU type | Runs replay only, from a clean checkout at the genesis commit, never given the harness state or `custodian/`. Up only after the round ends. |

## Commands (primary), in order

`<run>` is `~/f3/run1`. Steps 1 to 3 use a spec file for the real substrate that **does not exist yet** (`task_spec_gpu.json`, written with the adapter); the commands are otherwise those already in the repo.

```sh
# 0. on the node: clean checkout at the pinned commit, data prep (one time, about 2 min per autoresearch README)
git clone <repo> && git checkout <GENESIS_COMMIT> && uv run prepare.py
# 1. calibration + genesis receipt (baseline x13 runs; pins protocol/code hashes and the holdout root)
python3 challenge/f3/harness.py init --work <run>
cp <run>/host.pub <out-of-band location>        # pin the key before any candidate runs
# 2. each candidate (agents push files; this is the only entry point)
python3 challenge/f3/harness.py submit --work <run> CAND.py --author NAME
# 3. end of round
python3 challenge/f3/harness.py reveal --work <run>
# 4. verify without the harness
python3 challenge/f3/verify.py --work <run> --host-pubkey "$(cat host.pub)" --replay none
```

Second host, after copying only `receipts.jsonl`, `candidates/` and `host.pub`:

```sh
python3 challenge/f3/verify.py --work <copy> --host-pubkey "$(cat host.pub)" --replay all
```

## Hours and cost (estimates)

Assumes baseline time-to-target T0 of about 5 min on an H100 (to be measured in step 1; this is the biggest uncertainty), 3 seeds, frontier re-run with each candidate.

| Phase | Estimate | GPU-h | Cost at $5.04/h |
|---|---|---|---|
| Setup, data prep, smoke test | 0.5 h | 0.5 | 2.5 |
| Calibration (13 baseline runs) | 1.1 h | 1.1 | 5.5 |
| Round: Q = 20 candidates x about 26 min (6 runs of about 4 min plus evals) | 8.7 h | 8.7 | 43.9 |
| Second-host replay: up to 5 promotions x 9 runs x about 5 min | 3.8 h | 3.8 | 19.1 |
| **Total** | **14.1 h** | **14.1** | **about 71** |
| With 25% contingency | | 17.6 | **about 89** |

**Pilot (recommended first step):** Q = 8 candidates, calibration only plus 3.5 h of round, replay of at most 2 promotions: about 6.5 GPU-h, **about 33 USD**. The pilot's job is to measure T0, noise and the real holdout gap, and to confirm the numbers above before committing to Q = 20.

## Stop conditions

Stop and report at the first of: Q candidates used; 8 consecutive rejects; cumulative gain of 2x; spend reaching the **hard ceiling of 90 USD** (pilot: 40 USD); any primary-versus-replay disagreement on a promotion (do not continue, report it); a candidate process observed reading outside its working directory; baseline T0 above 10 min or noise cv above 0.10 at calibration (the target is then not usable).

## Replay plan

Second host replays every promotion on its recorded seeds (steps to target within one eval interval, holdout within tolerance) and on the revealed fresh seeds (gates pass, gain at least theta/2). Wall-clock is only compared within a host. A promotion that fails replay is reported as a false promotion; no retry with different seeds.

## Claims it supports, and does not

Supports: promotions re-derive from signed receipts; a candidate that overfits visible dev is stopped by the sealed holdout; promotions reproduce on a second same-type GPU host. Does not support: that agents beat random search or a tuned recipe (needs a comparator arm; the dry run used scripted patches); anything about other scales or data; honesty of a host that holds the signing key (attestation is self-reported; both hosts are ours); the absence of adaptive holdout overfitting (bounded by Q, not excluded). Details: [PROTOCOL.md](PROTOCOL.md) section 13.

## Needed from the user

1. Approve building and Mac-dry-running the GPU substrate adapter (no spend).
2. Pick or approve the two nodes and run, or authorize, the gman commands (sandbox bypass is yours to grant).
3. Choose pilot (about 33 USD) or full (about 71 to 89 USD), and confirm the 40 and 90 USD ceilings.
4. Agents for the round: scripted stand-ins prove nothing about agents; say whether real agents run it and whether a random-search comparator arm should use the same budget.
