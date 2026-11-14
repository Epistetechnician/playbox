# F3 GPU run: go/no-go request

Decision wanted from the user. Nothing here has been started: no gman node, Modal or paid API was touched, and no GPU hours were spent. All hours and dollars below are **estimates**, except the rate ($5.04/h, gman H100, as measured by the user) and the Mac numbers, which are marked as measured.

## Proposal

**Go for the pilot, then decide on the full run from the pilot's numbers.** The GPU substrate now exists and has been dry-run on a Mac CPU with real climbmix data: the autoresearch GPT (Muon+AdamW) behind the same harness, bits per byte, a sealed holdout shard, signed receipts, independent verifier with replay. What has not been run is anything on an H100: the H100 spec (`task_spec_gpu.json`) is an estimate until the pilot calibrates it.

Mac dry run, measured ([dry_run_climbmix](dry_run_climbmix/README.md)): 10 scripted candidates gave 2 promotions, 7 rejections and 1 injected dev-memorizer that reached the dev target 8 times faster than the baseline (dev gain 0.87) and was rejected by the holdout (holdout 2.39 bpb against a limit of 2.14; honest candidates 1.88 to 1.90). The verifier passed every check, both promotions reproduced on recorded seeds, and on fresh seeds one kept its gain (0.137) while the other shrank from 0.066 to 0.033 (floor 0.025). Baseline time to target on the Mac CPU was 28.2 s, timing noise cv 0.0008 to 0.0044 across runs, holdout-minus-dev offset -0.05 to -0.11 bpb (depends on which holdout documents the secret seed picks). Each candidate decision took 1.7 to 1.9 times the sum of its training walls. Two things the dry run found were fixed by amendment before the final run (PROTOCOL A2): a too-coarse eval interval made every candidate look slower, and calibration steps were counted in micro-batches.

## What the real run is

Target: autoresearch GPT (depth 8, about 50M parameters, 2048-token rows) on climbmix shards 0 to 9; metric = harness-measured training seconds to a fixed dev bpb `L*`, evaluation excluded, as a ratio to the frontier on the same host ([PROTOCOL.md](PROTOCOL.md) sections 2, 4, 6, 15). Dev = shard 6542 (agents see it). Holdout = shard 6541, downloaded only into a custodian directory the candidate processes never receive; committed by Merkle root in genesis, revealed after the last candidate. For the real run candidates must run under `playbox-runner` (no network, no custodian directory); the Mac run used a scrubbed subprocess only.

## Node plan (user to approve; I will not touch gman)

| Role | Node | Notes |
|---|---|---|
| Primary | one H100 node | Not `playbox-tp3` (a command that is not ours is running there). The user names the node or approves creating one; I have not looked at the state of `playbox-swarm3` or `jspace-*`. gman needs the sandbox bypass, which I did not use: the user runs or authorizes those commands. |
| Second host | a different H100 node, same GPU type | Replay only, from a clean checkout at the genesis commit, never given the harness state or `custodian/`. Up only after the round ends. Same operator as the primary, so this is a second host, not an independent party. |

## Commands (primary), in order

`<run>` is `~/f3/run1`. The genesis commit must be the commit that contains the code; the verifier fails otherwise.

```sh
# 0. node: clean checkout at the genesis commit; torch with CUDA and these packages (versions to pin at the pilot): pyarrow tiktoken requests rustbpe numpy cryptography
export F3_SUBSTRATE=climbmix F3_SPEC=task_spec_gpu.json F3_DATA_DIR=~/f3/data F3_HOLDOUT_DIR=/srv/f3-custodian/holdout
# 1. data: 12 shards (10 train, 1 dev, 1 holdout), about 92 MB each = about 1.1 GB; tokenizer is trained on the 10 train shards
python3 challenge/f3/task_climbmix.py prepare --data --holdout --tokens        # prints the data manifest
# 2. calibration + genesis receipt (13 baseline runs; pins protocol, code, data manifest, holdout root)
python3 challenge/f3/harness.py init --work <run>
cp <run>/host.pub <out-of-band location>        # pin the key before any candidate
# 3. each candidate (agents push files; the only entry point)
python3 challenge/f3/harness.py submit --work <run> CAND.py --author NAME
# 4. end of round
python3 challenge/f3/harness.py reveal --work <run>
# 5. verify without the harness
python3 challenge/f3/verify.py --work <run> --host-pubkey "$(cat host.pub)" --replay none
```

Second host: same steps 0 and 1 with the primary's `tokenizer/` copied in (BPE training determinism across hosts is not tested), then:

```sh
python3 challenge/f3/verify.py --work <copy of receipts.jsonl, candidates/, host.pub> --host-pubkey "$(cat host.pub)" --replay all
```

## Hours and cost (estimates)

Basis: autoresearch's README example reports 953 steps in 300 s on an H100 at depth 8 (not measured by us), so a 480-step baseline run is about 150 s of training; the Mac showed per-decision overhead of 1.7 to 1.9x the training wall, assumed to carry over (unverified on H100), giving about 4.5 min per run. T0 and the overhead are the largest uncertainties and are what the pilot measures.

| Phase | Estimate | GPU-h | Cost at $5.04/h |
|---|---|---|---|
| Setup, 1.1 GB data prep, smoke test | 0.5 h | 0.5 | 2.5 |
| Calibration (13 baseline runs) | 1.0 h | 1.0 | 5.0 |
| Round: Q = 20 candidates, 6 runs each, about 27 min | 9 h | 9 | 45.4 |
| Second-host replay: up to 5 promotions x 9 runs | 3.4 h | 3.4 | 17.1 |
| **Total** | **13.9 h** | **13.9** | **about 70** |
| With 25% contingency | | 17.4 | **about 88** |

**Pilot (first step):** Q = 8 candidates, calibration plus 3.6 h of round, replay of at most 2 promotions: about 6.5 GPU-h, **about 33 USD**. Its job: measure T0, noise, overhead and the real dev/holdout offset, and re-set the spec numbers (`steps`, `plan_steps`, `device_batch`) before the full run. Any change goes in a PROTOCOL amendment first.

## Stop conditions

First of: Q candidates used; 8 consecutive rejects; cumulative gain 2x; spend at the **hard ceiling of 90 USD** (pilot: 40 USD); a promotion that fails replay (stop, report); a candidate process observed reading outside its working directory; at calibration, T0 above 10 min, noise cv above 0.10, or theta above 0.15 (the target is then not usable).

## Replay plan

The second host replays every promotion on its recorded seeds (batches to target within one eval interval, holdout within 0.02) and on the revealed fresh seeds (gates pass, gain at least theta/2). Wall-clock is only compared within a host. A promotion that fails replay is reported as a false promotion; no retry with other seeds. Expect marginal promotions to fail: on the Mac, one of two promotions kept only half its gain on fresh seeds.

## Claims it supports, and does not

Supports: promotions re-derive from signed receipts and sources; an overfit-to-dev candidate is stopped by the sealed holdout; promotions reproduce on a second same-type GPU host. Does not support: that agents beat random search or a tuned recipe (the dry runs used scripted patches; a comparator arm needs its own budget); anything about other scales or data; honesty of a host holding the signing key (attestation is self-reported); no adaptive overfitting through the pass/fail bit (bounded by Q only). [PROTOCOL.md](PROTOCOL.md) section 13.

## Known risks found by the dry run

- Small holdouts shift: the holdout-minus-dev offset moved between -0.05 and -0.11 bpb across rounds on 65k-token holdouts. The calibrated tolerance absorbs it, but a large offset loosens the gate. The real holdout is 8x larger per row count.
- `eps` adds the absolute mean offset even when the holdout is easier than dev; this is the preregistered rule and is looser than needed in that case.
- The injected memorizer here was easy to catch (holdout 0.5 bpb worse). A subtler overfit would be a better test and is not part of this request.
- pip and uv could not verify TLS inside the Mac sandbox (curl worked); the node setup should not depend on that.

## Needed from the user

1. Pilot go or no-go (about 33 USD, ceiling 40).
2. The two nodes, and who runs or authorizes the gman commands.
3. Whether real agents run the round and whether a random-search comparator arm gets the same budget.
