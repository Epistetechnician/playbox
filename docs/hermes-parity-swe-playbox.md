# Hermes on SWE-bench through a Playbox room (arm P)

Arm P of the SWE parity harness (`tools/bench/hermes-parity/run_swe_playbox.py`): Hermes runs on the host with the `playbox` terminal backend,
so every terminal command is a signed `playbox_delegate` into an OpenShell microVM (image `pb-swe-django:arm64`, non-root, no network, warm
sandbox per member). It is compared with arms L (local Hermes) and S (Hermes in an OpenShell VM over ssh), which are in
`swe-results/` and `swe-results2/` (branch `claude/hermes-openshell-parity-056f23`).

Same model (`claude-haiku-4-5-20251001`, provider `anthropic`), toolset (`terminal,file`), `--yolo` and issue prompt as L and S. Only the repo
path (`/sandbox`) differs. Scoring is `swe/score.py`: agent patch minus `tests/`, plus the hidden test patch, instance tests in a no-network
container; resolved means all FAIL_TO_PASS and PASS_TO_PASS pass. It is our own scorer, not the official SWE-bench harness.

## Results (n = 5 per cell, one run at a time)

| instance | L resolved | S resolved | P resolved |
|---|---|---|---|
| django-11848 | 4 / 5 | 5 / 5 | 5 / 5 |
| django-11422 | 4 / 5 | 3 / 5 | 1 / 5 |
| django-11905 | 0 / 5 | 0 / 5 | 0 / 5 |
| **total** | **8 / 15** | **8 / 15** | **6 / 15** |

| per run, mean (Hermes cost estimate) | L | S | P |
|---|---|---|---|
| django-11848 cost / wall | $0.115 / 95 s | $0.115 / 90 s | $0.148 / 428 s |
| django-11422 cost / wall | $0.198 / 143 s | $0.255 / 175 s | $0.279 / 812 s |
| django-11905 cost / wall | $0.341 / 190 s | $0.306 / 151 s | $0.272 / 1145 s |
| arm total cost, 15 runs | $3.27 | $3.38 | $3.49 |

Arm P facts, 15 runs: 1028 delegates, 526 of them changed the tree; audit log verified in 15 of 15; 756 taint advisories (advisory mode,
`PLAYBOX_APPROVERS` unset, so none blocked anything); 3 refused delegates (0.3%), all "uploading the workspace failed" with macOS tar xattr warnings; the model retried each. No run needed a
retry for a missing model call (`attempts` is 1 in all 15).

## Reading it

- Cost is within about 7% of L and S in total. Wall-clock is 4 to 6 times longer: each command ships the whole Django checkout (~30 MB) in and
  out and replays earlier edit commands, about 7 s per command warm.
- django-11848: parity. django-11905: no arm resolves it in any of 5 runs, so it cannot separate arms.
- django-11422 is the gap: P resolved 1 of 5 against 4 of 5 (L) and 3 of 5 (S). With n = 5 that difference is not significant (Fisher exact,
  P vs L, p about 0.2), but it is the only cell where P is clearly below the other arms and it is not shown to be noise either. The
  total 6/15 vs 8/15 is likewise inside what 5 runs per cell can tell apart. Two things in P's runs plausibly cost it: Hermes's file tool
  verifies a write by reading it back, and in a fresh-checkout-per-command sandbox that read-back sometimes disagrees ("Post-write
  verification failed"), which sends the model into extra turns; and the 6x slower loop. This is a hypothesis from reading logs, not tested.
- The agent's patch can carry stray `.hermes-tmp.*` files from the file tool; they are harmless to scoring.

## Design

- **Host repo.** Per instance, a single-commit git repo of the Django tree at the instance's base commit (`git archive` into a fresh repo on
  branch `main`; cached under `~/.playbox-hermes/parity-playbox/swe-repos/`, copied per run). `playbox_delegate` checks out `main`.
- **Warm vs keep.** Plain `warm` was enough once the runner could carry the workspace (below); the keep mode was not needed.
- **State.** The backend replays edit commands before each new command (unchanged, see `docs/hermes-playbox.md`); this is the source of the
  taint advisories.
- **Patch.** The final delegate patch (`delegates.final.json`), minus `tests/`. Never truncated in these runs.
- **Credential.** Arm P ran on Hermes' pooled OAuth credential `anthropic-oauth-3`; L and S's original runs used the account's earlier
  credential. Same model and provider; OAuth stayed on the host. The first P attempt on the original credential hit its usage limit.
- **Daemons.** The driver kills stale Hermes gateway daemons for its own HERMES_HOME before each run and retries a run that made no model call.

## What had to be fixed first (separate PR)

The runner could not carry a Django workspace at all: `openshell sandbox exec` rejects more than 4 MiB on stdin and any single argument over
32 KiB. The first stalled every delegate on a workspace above ~5 MB (shown as "timed out"); the second made every edit that
carried a large file fail (three P runs came back with empty patches and were discarded). Both are fixed in PR #45 (chunked upload; long scripts
staged as a file). Arm P was run with that binary.

## Caveats

- n = 5 per cell, three instances, one model, one scorer of our own. Not a benchmark claim.
- Arms were not run interleaved: L and S ran earlier (and on another credential), P later, so API latency and load differ. Wall-clock
  comparisons are indicative; the roughly 5x gap is far larger than that noise.
- Playbox adds a signed grant, a verifying audit log and a microVM per command; it did not stop any command here. Taint advisories are advisory.
- Raw data: `tools/bench/hermes-parity/swe-results-playbox/` (JSON and patch per run).

Reproduce: `PLAYBOX_BIN=<playbox with the PR #45 fix> tools/bench/hermes-parity/run_swe_playbox_batch.sh` (outside any sandbox; needs Docker for scoring).
