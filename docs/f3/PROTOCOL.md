# F3 protocol: a verified hill-climb on a small pretraining target

Status: **preregistration**. This file is committed before any F3 run (dry or real). Its sha256 is pinned in the genesis receipt of each run, and the verifier refuses a run whose genesis names a different hash. Where a number below says "set by calibration", the *rule* that produces it is fixed here and the value is written into the genesis receipt before the first candidate is evaluated.

Scope: Phase 1 (design, harness, local CPU dry run). The GPU run is a separate go/no-go ([GPU_RUN_REQUEST.md](GPU_RUN_REQUEST.md)). Nothing here has been run on a GPU.

## 1. What F3 is meant to show

Agents in a playbox room hill-climb a small public pretraining target. Every promotion is a signed receipt. A sealed holdout the agents never see gates the promotion. A second host replays it. The point is not the loss number. The point is that an outsider holding only the receipts and the repo can re-derive every promotion and catch one that should not have happened.

## 2. Target and fixed metric

**Metric (primary, lower is better):** `T` = harness-measured training wall-clock seconds until the model first reaches a fixed **dev loss target** `L*`, with evaluation time excluded from the clock. The clock is owned by the harness (`time.perf_counter`, paused inside the harness's dev evaluation). On GPU the harness synchronizes the device before reading the clock. A candidate cannot report its own time.

**Why time-to-loss and not loss-at-fixed-time.** The autoresearch substrate (`autoresearch-macos`, read-only reference) scores `val_bpb` after a fixed 300 s budget, and its learning-rate schedule is a function of elapsed wall-clock. Loss at a fixed time depends on the GPU, so a replay on a different GPU is not comparable, and the schedule is tied to the budget, so a target-loss stop is unnatural there. The nanoGPT speedrun convention (time to a fixed validation loss) compares ratios on the same host and is what a second host can reproduce. F3 uses that convention. A candidate's gain is always a **ratio against the current frontier measured on the same host in the same session**, never an absolute time across hosts.

**Secondary metrics (recorded, not gated):** harness-counted training batches and tokens served to the candidate until the target; hardware-independent up to numerical nondeterminism.

**Candidate surface.** A single Python file defining `build_model(vocab_size, block_size) -> nn.Module` (forward `x[B,T] -> logits[B,T,V]`) and `train(ctx, model)`. Architecture, optimizer, schedule, batch size, precision, and data order within the harness-served stream are all editable. The harness-owned pieces (tokenization or data generation, dev set, target, holdout, clock, stop rule) are not in the candidate's surface. `ctx.next_batch(B)` is the only way to get training data; `ctx.eval_dev(model)` is the only way to read dev loss and is where the target is detected.

**Substrates.**

| | Dry run (this phase) | Real run (proposed, needs go/no-go) |
|---|---|---|
| Data | Synthetic order-2 source over a 32-symbol vocabulary. The grammar is public. Train and dev samples come from public seeds. The holdout comes from a custodian-held seed. | `karpathy/climbmix-400b-shuffle` shards as prepared by autoresearch's `prepare.py` (8192-token BPE). Train: shards given to agents. Dev: the pinned val shard 6542 (agents see it). Holdout: a shard the agents are never given (custodian downloads it). |
| Model | 2-layer, 64-wide GPT, CPU | autoresearch `train.py` GPT (about 50M params at depth 8), one H100 |
| Dev/holdout metric | cross-entropy in nats per token | bits per byte on dev and holdout (vocabulary independent) |

The real-run substrate changes only what is behind the same harness contract. The calibration rule (section 4), promotion rule (section 6), receipts (section 9) and replay rule (section 10) are identical.

## 3. Data roles

- **train**: served only through `ctx.next_batch`.
- **dev**: visible to agents (a candidate can read `ctx.dev_blocks`). The target `L*` is defined on dev. This is deliberate: dev is what agents hill-climb, and it is how the injected overfit candidate is possible.
- **holdout**: never given to agents, never passed to the candidate process, never in the repo, never in `ctx`. Used only by the evaluator process, after training, on the saved weights.
- **replay seeds**: training seeds the agents do not know, used only by the verifier and the second host.

## 4. Calibration (sets the numbers; runs before any candidate)

Run the pinned baseline, then freeze:

1. **Target `L*`.** Median dev loss of the baseline after `S_cal` fixed steps over the training seeds, rounded up to 3 decimals. (Dry run: `S_cal = 300`, seeds 1 to 3.)
2. **Holdout tolerance `eps`.** For 5 baseline runs on distinct calibration seeds (not training seeds), stopped at `L*`: gap `g = holdout_loss - dev_loss`. `eps = max(eps_floor, 3 * sd(g))`, plus the absolute value of the mean gap.
3. **Timing noise `cv`.** 5 repeats of the baseline on one fixed seed (the step count is identical, so the spread is timing noise). `theta = max(theta_floor, ceil(3 * cv, 2 decimals))`, where `theta` is the minimum gain.
4. **Caps.** Wall cap = `wall_cap_mult` times the baseline median `T`. Token cap = `token_cap_mult` times the baseline median tokens. A run that hits a cap has not reached the target.

The values `L*`, `eps`, `theta`, `cv`, caps, the baseline hash, the spec hash and the code hashes go into the **genesis receipt**.

## 5. Static checks (before any compute)

A candidate is rejected without being run if:
- its imports are outside the whitelist (`torch`, `numpy`, `math`, `time`, `dataclasses`, `typing`, `functools`, `itertools`, `collections`);
- its source contains the holdout secret seed or salt, or any 24-byte window of a holdout item (checked against the custodian's holdout; this check also runs again in the verifier after the reveal);
- the patch to the frontier does not apply (scripted proposers) or the file does not parse.

## 6. Promotion rule

Evaluated on the **training seeds** `{1,2,3}`. For each seed the candidate trains once and the current frontier trains once, order randomized per seed, in fresh subprocesses.

A candidate is **promoted** iff all of the following hold:

1. **Target reached.** On every seed the harness-side dev loss reached `L*` within the caps.
2. **Dev recomputed.** On every seed the evaluator process, loading only the saved weights, recomputes dev loss `<= L* + 1e-4`. This catches a candidate that fakes its own crossing.
3. **Holdout.** On every seed the evaluator's holdout loss `<= L* + eps`.
4. **Gain.** `ratio = median_seeds(T_cand) / median_seeds(T_frontier)` and `gain = 1 - ratio >= theta`.
5. **Static checks clean** (section 5).

Otherwise it is **rejected**, with the first failing reason in the order above (`target_not_reached`, `dev_mismatch`, `holdout_failed`, `gain_below_threshold`, `static_check`). The promoted candidate becomes the frontier. Every decision, promote or reject, gets a receipt.

## 7. False promotions

- **False promotion (the thing the system must not do).** A promotion in the final chain that, on the replay of section 10, fails the holdout gate, or has replay gain below `theta/2`, or whose source leaks the holdout, or whose receipt does not re-derive from its own recorded measurements. The reported rate is `false promotions / promotions`, with the count of both. With a handful of promotions the rate has a wide interval and the report says so.
- **Caught false promotion.** A candidate that passes steps 1 and 4 of the rule on dev (reached the target and cleared the gain threshold) but fails the holdout (3), so it would have been promoted without the holdout. It is recorded as a reject with `caught_false_promotion: true`. The count of these is a gate-efficacy statistic. It is not a rate over all candidates.
- **Missed false promotion.** Any false promotion by the first definition that the gate promoted. The verifier reports these; the demo's claim is "zero found by the pre-registered replay", never "zero exist".

## 8. Holdout design

- **Custodian.** The harness's evaluator role holds a secret seed and a 32-byte salt in a directory the candidate process is not given. The holdout is `N` blocks of `block+1` tokens.
- **Commitment.** Before any candidate runs, the genesis receipt carries a salted Merkle root over the holdout blocks (same construction as `crates/playbox-core/src/evaluator.rs`: per-item blind from the salt, domain-separated leaf and node hashes, odd node carried up). The Python port uses SHA-256 where the Rust uses BLAKE3, so the two are the same design but not byte-compatible.
- **Sealing.** The candidate subprocess gets a scrubbed environment and no holdout path. The evaluator process receives the holdout tokens on stdin, after importing the candidate module.
- **Redacted feed.** During the round, agents see a feed with verdict, reason category, and the dev numbers. They see only a pass/fail bit for the holdout, never the holdout loss. The full signed receipts are published at the end of the round.
- **Reveal.** After the last candidate, the custodian signs a `reveal` receipt with the seed and salt (and the replay seeds). The verifier regenerates the holdout, checks it against the committed root, then checks that the reveal is the last receipt and that no candidate source contains the secret.
- **Budget.** The holdout answers at most one query per candidate and at most `Q` candidates per round (real run: set in the GPU request). The pass/fail bit leaks at most `Q` bits about the holdout over a round; this bounds, but does not remove, adaptive overfitting.

## 9. Receipt contents

Receipts use `challenge/common.py` (the Commons v0 mechanism): a hash-chained log, body hashed over canonical JSON, Ed25519 signature over the id. Each body records the self-reported hardware fingerprint (`attested: false`), `ts`, `host` (public key) and `prev`.

- **genesis**: protocol sha256, task spec sha256, code sha256 of `task.py`, `baseline.py`, `harness.py`, the git commit, the holdout root, a commitment to the replay seeds, calibration values (`L*`, `eps`, `theta`, `cv`, caps, seeds), and the baseline candidate hash.
- **decision** (one per candidate): genesis id, sequence number, author, parent frontier id, candidate sha256, per-seed measurements for candidate and frontier (`T`, batches, tokens, harness dev loss, evaluator dev loss, evaluator holdout loss), medians, ratio, gain, which gates passed, verdict, reason, `caught_false_promotion`.
- **reveal**: genesis id, holdout seed and salt, replay seeds and nonce.

Candidate sources are archived by sha256 next to the log so any decision can be re-run.

## 10. Verification and the second-host replay rule

**The verifier** (`challenge/f3/verify.py`) takes the receipts, the archived candidate files and the repo state. It does not call the harness driver. It checks, in order:

1. Chain: `prev` links, body hash, Ed25519 signature against a **pinned host public key** supplied out of band (committed before the run), no duplicate ids or sequence numbers, every non-genesis body bound to the genesis id.
2. Genesis: the protocol hash equals the committed protocol file; the code hashes equal the files at the recorded git commit; the baseline hash matches.
3. Lineage: each decision's parent is the frontier at that point; each candidate hash matches the archived file.
4. Rule re-application: it recomputes medians, ratio, gates and verdict from the recorded measurements with its own implementation of section 6, and requires the recorded verdict to match.
5. Holdout: the reveal opens the committed root; the reveal is last; no candidate source leaks the secret.
6. Replay (optional, costs compute): re-run each promoted candidate and its parent.
   - Recorded seeds: deterministic quantities must match the receipt (steps to target within one eval interval, evaluator losses within 0.02).
   - Revealed replay seeds: the promoted candidate must pass gates 1 to 3 and clear `gain >= theta/2` against its parent, measured on the replaying host.

**Second-host rule (real run).** A second machine, run from a clean checkout at the genesis commit, not given the harness state or the custodian directory, executes step 6 on the revealed replay seeds. A promotion survives only if the second host's replay passes. Wall-clock times are never compared across hosts; only the within-host candidate/parent ratio, the gates and the step counts are. For the real run both hosts are the same GPU type.

## 11. Stop conditions

A round ends at the first of: the candidate quota `Q` is used, the wall-clock budget is spent, `K` consecutive rejects (real run: set in the GPU request), the frontier reaches the stretch target (cumulative gain of 2x), or any abort condition in the GPU request (cost ceiling, anomalous disagreement between primary and replay). All decisions up to the stop are reported, including rejects.

## 12. What is reported, whatever happens

Number of candidates, promotions, rejects by reason, caught false promotions, missed false promotions found by replay, per-promotion replay result, cumulative speedup with an interval from the replay seeds, and total compute. A round with no promotions, or with replay failures, is reported as such.

## 13. Claims

**The demo can support:**
- Every promotion in the chain re-derives from signed receipts and archived sources, and the verifier rejects forged, replayed, tampered, and leaked variants (tested).
- A candidate that overfits the visible dev set can clear a dev-only gate and be stopped by a sealed holdout (shown by injection).
- Under this rule on this task, promotions reproduce on fresh seeds on a second host (real run only).

**The demo cannot support:**
- That agents are better than random search, humans, or an existing tuned recipe. That needs a comparator arm with the same budget. The scripted stand-ins in the dry run are not agents and prove nothing about agents.
- That the climb generalizes to larger models, other data, or a different compute budget.
- That the evaluator host is honest. Hardware attestation is self-reported (`attested: false`). A compromised host with the signing key can issue consistent receipts; the replay on an independent host is the only defense, and for the real run both hosts are operated by the same party.
- That the candidate cannot reach the holdout. In the dry run, isolation is process separation, a scrubbed environment and stdin delivery. It is not an OS sandbox. The real run must run candidates under `playbox-runner` (no network, no access to the custodian directory) before the holdout statement can be made.
- That adaptive overfitting through the pass/fail bit is absent. It is bounded by `Q`, not excluded.
- That a zero false-promotion count means zero false promotions. It means none found by this replay with this many seeds.
- Anything about the Rust holdout code's byte format: the Python port is the same design with SHA-256.

## 14. Preregistration mechanics

1. Commit this file. 2. Commit the harness, verifier, tests and archived scripted stand-ins. 3. Generate the host key; publish `host.pub`. 4. Run `init` (calibration, genesis receipt). 5. Run rounds. 6. `reveal`. 7. Verify, then replay. Deviations after step 1 are recorded in the run notes with the reason, never by silently editing this file after its hash is pinned.

## 15. Amendment log

Amendments are appended here with the date and are committed before the run they apply to. None changes the rules in sections 4 (calibration), 6 (promotion rule), 7 (false promotions) or 10 (verification and replay).

**A1, 2026-11-13, before any climbmix run (Phase 1.5: GPU substrate adapter, Mac dry run).**
- *Substrate contract.* The harness and verifier now load a substrate module named in the genesis receipt (`substrate.task`, one of `task` or `task_climbmix`) with its spec file and baseline. The genesis also pins the substrate's data helper (`climbmix_data.py`) and, for climbmix, a `data_manifest` of sha256 hashes of the tokenizer, token caches and shards. A replaying host must hold byte-identical artifacts or the verifier fails before replaying. The earlier synthetic substrate is unchanged in behavior; its dry run was regenerated because `harness.py` and `task.py` changed.
- *Climbmix details that section 2 left open.* Loss is bits per byte (bpb) on dev and holdout rows. Dev rows are the first `dev_rows` rows of the pinned val shard (6542), packed sequentially from BOS-prefixed documents (upstream autoresearch uses best-fit packing, so its `val_bpb` is not comparable). The holdout is a separate custodian shard (6541): documents taken in the order of a permutation seeded by the secret seed, packed the same way; the Merkle items are the selected documents' UTF-8 bytes. Train rows are random windows of a pre-tokenized stream of the train shards. The leak scan checks 64-character windows of holdout documents (stride 16) and the seed and salt. The baseline is autoresearch's GPT with Muon+AdamW ported to the candidate contract, with LR and weight-decay schedules over a planned step count, fp32 embeddings, and no `torch.compile` (a candidate that adds it pays the compile time on the clock).
- *Tokenizer.* Trained by `rustbpe` on the train shards only (never dev or holdout), as upstream does. Its hash is in the data manifest.
- *Dry-run scale.* The Mac dry run uses a smaller spec (`task_spec_gpu_mac.json`: 256-token blocks, 2 layers, 3 calibration seeds). `task_spec_gpu.json` is the H100 spec and its scale numbers are estimates until the pilot calibration; they are frozen in the genesis receipt before any candidate runs.
- *Verifier.* `verify.py` fails if the pinned code differs from the genesis git commit, unless `--allow-dirty` is passed (tests only).

**A2, 2026-11-13, before the second Mac dry run (found by the first one).**
- *What happened.* The first climbmix Mac run (8 stand-ins, discarded; receipts kept in the session notes, not in the repo) evaluated dev every 20 optimizer steps while the calibration target sat at about 60 steps, so one eval interval was a third of the run. Every candidate that was not a no-op showed a gain of about -0.32 (it crossed the target one interval later), and the measured timing noise (cv 0.004, one seed repeated) said nothing about it. The injected dev memorizer was still caught by the holdout.
- *Amendment to section 4(3).* `theta = max(theta_floor, ceil(3 * cv, 2 decimals), ceil(eval_granularity, 2 decimals))`, with `eval_granularity = eval_every / S_cal` (both in optimizer steps). The genesis records `eval_granularity`. The synthetic substrate's theta is unchanged by this (0.01 against 0.12).
- *Unit fix.* Calibration steps are now converted to micro-batches with the substrate's `batches_per_step` before the stop rule is applied (before, `steps` was compared with micro-batches, so a climbmix baseline with 2 micro-batches per step was calibrated at half the intended steps).
- *Mac spec.* `eval_every` 4 steps, 256 dev and 256 holdout rows, 100 calibration steps.

**A3, 2026-11-14, before the H100 pilot.** Scope and conditions of the pilot run; no rule in sections 4, 6, 7 or 10 changes.
- *Scope.* One round of Q = 8 scripted stand-ins (the first 8 of `STAND_INS_CLIMBMIX`, which include the injected dev memorizer), on one new H100 node, then replay of at most 2 promotions on a second new H100 node brought up after `reveal`. Hard cost ceiling 40 USD at 5.04 USD per node-hour. Stop conditions as in GPU_RUN_REQUEST.md (replay disagreement, candidate reading outside its directory, T0 above 10 min, cv above 0.10, theta above 0.15, ceiling).
- *Spec numbers.* `steps`, `plan_steps`, `eval_every` and `device_batch` in `task_spec_gpu.json` are set from a smoke measurement of the baseline on the node and committed before `init`; after `init` they are frozen by the genesis spec hash.
- *Key.* The host public key printed by `init` is copied off the node and recorded locally before the first candidate is submitted.
- *Same-user custody.* The holdout shard sits in a directory outside the candidate's environment, but the candidates and the evaluator run as the same Unix user, so this is not an OS-level seal. The stand-ins are scripted and cannot read files (static check), which is the only protection. The statement "agents never see the holdout" is not supported by this pilot for real agents.
- *Second host.* Created new, with the same code tree committed with fixed author and date so the genesis commit hash is identical, and with the primary's tokenizer copied in.
- *Driver.* `pilot.py` submits stand-ins one by one through `harness.submit`; it is not pinned by the genesis because it only calls the pinned harness.

**A4, 2026-11-14, before any second-pilot run (offline preparation; nothing was run on a GPU).** Scope and conditions of the second H100 pilot; no rule in sections 4, 6, 7 or 10 changes.
- *Purpose.* Pilot 1 produced 0 promotions, so the replay rule (section 10) was never exercised on real hardware. Pilot 2 needs at least one promotion, replayed on a second node, and a no-skill comparator at the same budget.
- *Spec.* `task_spec_gpu_p2.json`: the same scale as pilot 1 (240 calibration steps, plan 360, eval every 10, 2**19 tokens per step, no compile in the baseline), new `id`. Calibration is re-run and frozen in a new genesis; pilot-1 numbers (T0 245.7 s, cv 0.018, theta 0.06, offset +0.005) are expectations only.
- *Two arms from one genesis.* After `init` the work directory is copied (`cp -a`); each copy is one arm and has its own chain from the shared genesis receipt. Arm `h100` is scripted stand-ins (`arms.py`: compile the model; weight decay 0 with grad clip 1.0; anneal to step 240; the injected dev memorizer; a fifth, a comment-only null control, is a reserve). Arm `random` is hyperparameter proposals from `arms.random_proposals` (fixed seed 20261114, fixed space, 1 or 2 constants each, applied to the arm's current frontier; the fifth is a reserve). Both lists are fixed and committed before the run. **Both arms have Q = 4** (`pilot.py --max 4`); the reserves are not run unless the user raises the budget, and then in both arms equally. Both arms have the same spec, the same genesis, and run one after the other on the same node. `reveal` is called on both only after both are done; the holdout is shared between them. The stand-ins are not agents and the random arm cannot propose code changes (no compile); the comparison is of two proposal streams under one rule, with n = 5 and one random seed, and is descriptive only.
- *Compile on the clock.* A candidate that calls `torch.compile` pays its compile time inside the measured training seconds (the clock only stops for evaluation). Each decision runs 3 seeds and uses the median, so the first (cold-cache) seed of a new graph is the slowest and is not the median; seeds 2 and 3 load the inductor cache from `/tmp`. The evaluator and the dev/holdout evaluation use the uncompiled module. The scrubbed candidate environment keeps `PATH=/usr/bin:/bin`; whether compile works under it is checked by the smoke run below, not assumed.
- *Smoke gate (before `init`, may only abort, never change a stand-in).* Run the baseline and the `h100-1` compile patch for 240 steps (960 micro-batches) on seed 1, the compile patch twice (cold, then warm). Abort the pilot and stop the node if the compile run fails, or if the warm compiled wall is not at most 0.90 times the baseline wall (gain at least 0.10, above theta with margin). Everything else in the stand-in list stays as committed.
- *Stop conditions.* As in A3, plus: none of `h100-1` to `h100-3` promotes (stop, report; no second node, random arm not run); projected cumulative spend above 36 USD before the second node is created (stop, ask); hard ceiling 40 USD at 5.04 USD per node-hour.
- *Second node.* Created only after both arms are revealed, from the same committed tree (fixed author and date so the genesis commit hash matches) with the primary's tokenizer copied in; it replays at most 2 promotions from arm `h100` (`verify.py --replay all`, as in A3). A promotion from arm `random` is reported but not replayed unless the budget gate allows.
- *Custody.* Unchanged from A3: same Unix user for candidate and evaluator, so the statement that agents never see the holdout is not supported by this pilot.
- *Verifier.* `verify.py` now accepts a genesis whose pinned `protocol_sha256` is the PROTOCOL.md text at the genesis git commit when the current file only extends it (amendments append, section 15); an in-place edit of the pinned text still fails (tested). Before this, every amendment broke the verification of all earlier committed runs. The committed dry runs and pilot-1 receipts verify against this repo again without being regenerated.
- *Smoke result, 2026-11-14 (gated step run before the second pilot, 1.17 USD, node stopped).* The compile stand-in `h100-1` was run for 240 steps on seeds 1 to 3 through the harness's scrubbed subprocess on my pilot-1 node (resumed, disk intact): baseline 245.4 s, compile 96.3 s (median), gain 0.608 against theta 0.06; cold-cache compile 114 s. The smoke gate is passed. No stand-in or rule was changed after seeing it. Record: `docs/f3/smoke_p2/`.

**A5, 2026-11-14, after the H100 pilot (found when the synthetic dry run was re-verified from a clean clone).** A verifier fix; no rule in sections 4, 6, 7 or 10 changes.
- *What happened.* Check 2 of section 10 compared the genesis receipt's protocol hash with this file as it is today. Appending A3 changed the file, so the synthetic dry run, whose genesis pins the version before A3, stopped verifying, although nothing in it had changed. Any amendment would do the same to every earlier run.
- *Fix.* When the file today does not match, `verify.py` looks through this file's git history and accepts the pinned hash only if it is a committed version whose commit time is not after the genesis receipt. It reports that commit. A hash that was never committed, or was committed after the run began, still fails. In a clone without history the check fails, as it should: the verifier cannot show the version was committed first.
