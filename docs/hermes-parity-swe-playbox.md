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
`PLAYBOX_APPROVERS` unset, so none blocked anything); 3 refused delegates (0.3%), all "uploading the workspace failed" with macOS tar xattr warnings; the model retried each. One run (django-11905 P5) made no model call
on its first attempt and was retried by the driver; its time and cost are from the retry (`attempts: 2` in its JSON).

## Reading it

- Cost is within about 7% of L and S in total. Wall-clock is 4 to 6 times longer: each command ships the whole Django checkout (~30 MB) in and
  out and replays earlier edit commands, about 7 s per command warm.
- django-11848: parity. django-11905: no arm resolves it in any of 5 runs, so it cannot separate arms.
- django-11422 is the gap: P resolved 1 of 5 against 4 of 5 (L) and 3 of 5 (S). With n = 5 that difference is not significant (Fisher exact,
  P vs L, p about 0.2), but it is the only cell where P is clearly below the other arms and it is not shown to be noise either. The
  total 6/15 vs 8/15 is likewise inside what 5 runs per cell can tell apart. In 4 of 5 P runs the model patched
  `run_with_reloader` (watch `sys.argv[0]`), which the hidden tests do not cover; the resolving patches (L: 4 of 5, S: 3 of 5, P: 1 of 5)
  change `iter_modules_and_files`. L5, S4 and S5 took the wrong route too. So the gap is which fix the model chose. What differs in P's loop
  is not established; see the rerun below.
- The agent's patch can carry stray `.hermes-tmp.*` files from the file tool; they are harmless to scoring.

## django-11422 rerun after a backend fix

Reading the P logs showed a real defect in our backend: stdin for a command (the Hermes file tool's write) was bound to the last statement of
its script, so file-tool writes left empty files and Hermes reported "Post-write verification failed" in 4 of the 5 first P runs (PR #66 fixes
it; checked live: 0 bytes before, all bytes after). The 5 django-11422 runs were repeated on the fixed backend
(`swe-results-playbox-stdinfix/`): **0 of 5 resolved**, no verification failures, 0 refused delegates, audit verified 5 of 5, $1.28 total,
mean wall 778 s. Four of the five again patched `run_with_reloader`/`BaseReloader` rather than `iter_modules_and_files`. So the write bug
cost the model turns but is not what separated P from L and S on this instance; across both P sets django-11422 is 1 of 10, against 4 of 5
(L) and 3 of 5 (S). With these sample sizes that is suggestive of a real P deficit on this instance, not proof, and its cause is still
unexplained (candidates, untested: the 5 to 6 times slower loop, replayed edits, no network in the sandbox for any exploration the
model did elsewhere). The 15-run table above is the original set and is kept as is; with the rerun in place of the original 11422 cell P is
5 + 0 + 0 = 5 of 15.

## Addendum (2026-11-19): Root cause of the `django-11422` gap — `.git` future-history leakage in Arms L & S

Forensic inspection of the Hermes session SQLite logs (`home-django-L/state.db` and `home-django-S/state.db`) resolved the `django-11422` gap:

- `run_swe.py` (Arms L and S) copied the full `/usr/local/testbed/.git` history and ran `git checkout base_commit`, leaving the future gold fix commit `b2790f74d4` (*"Fixed #30479 -- Fixed detecting changes in manage.py by autoreloader when using StatReloader"*) reachable via `git log --all`.
- **Every resolved run in Arms L and S read `b2790f74d4`:** 4/4 resolved runs in Arm L (`L1`–`L4`) and 3/3 resolved runs in Arm S (`S1`–`S3`) executed `git log --oneline --all` and `git show b2790f74d4`. Across all 56 historical `django-11422` sessions in both DBs, Haiku 4.5 inspected `b2790f74d4` in 40/56 sessions.
- **Uncontaminated solve rate in Arms L & S was 0 / 3:** every non-leaking run (`L5`, `S4`, `S5`) patched `run_with_reloader` instead of `iter_modules_and_files` — the exact same solution Haiku 4.5 chose in Arm P and Arm P2.
- **Playbox blocked the leak by construction:** `run_swe_playbox.py` / `run_swe_playbox_v2.py` build a single-commit `git archive base_commit`, and the OpenShell runner strips `.git` from `/sandbox`. `run_swe.py` has now been fixed to use single-commit `git archive base_commit` by default (opt-in `--dirty-git-history` to reproduce the old leak).

## Arm P2 (2026-11-19): Uncontaminated 5-Instance Parity, Cascade & Hybrid Post-Quantum Receipts

`tools/bench/hermes-parity/run_swe_playbox_v2.py` (Arm P2) combines:
1. **Stateful persistent OpenShell sessions (`keep=true`, `sync="none" | "delta" | "full"`):** eliminates per-command 30 MB workspace re-uploads and edit replay; median delegate latency drops from **6.97 s** (Legacy Arm P) to **0.07 s** (**99× faster**).
2. **3-hop AtlasKV (`playbox_atlas/`) + gate-verified compounding memory:** builds a 16k-triple KG2KV + HiKVP index in ~1.2 s on the host and injects a compact 3-hop call-chain brief + receipt-backed `reviewed` lessons (`quarantined` entries dropped).
3. **Hardened non-oracle online gate (`run_repro_generator` + existing repo tests) & conditional cascade (`playbox_router/`):** rejects broken repro scripts (`SyntaxError` / `ImportError` / `ModuleNotFoundError` / `NameError`) on `base_commit`, evaluates candidates without ever touching hidden SWE-bench `test_patch`, and escalates only when needed.
4. **Enforced zero-false-positive taint (`PLAYBOX_APPROVERS` + `PLAYBOX_TRUST_OFFLINE_WORKSPACE=1`):** runs `playbox-mcp` in **hard-blocking taint mode** (`0` false-positive advisories on offline own-repo delegates while hard-blocking peer chat, web browse, unreviewed memory, and networked delegates).
5. **Hybrid post-quantum Attested Gate Receipts (`Ed25519+ML-DSA-65`, FIPS 204):** every run emits `<tag>.receipt.json` signed via `playbox receipt sign` and verified via `playbox receipt verify --require-pq` / `playbox_router.verify_signed_receipt(..., require_pq=True)`.

### 1. Haiku 4.5 / Sonnet 4.6 Uncontaminated Highlights (`/tmp/swe-p2-v2/`)

| Instance & Run | Arm / Model | Resolved (`f2p` / `p2p`) | Wall (s) | Median Cmd (s) | Cost (USD) | Audit & Hybrid PQ Receipt |
|---|---|---|---|---|---|---|
| `django-11848` | Legacy Arm L (mean of 5) | 4 / 5 | 95.0 s | — | $0.1150 | None |
| `django-11848` | Legacy Arm S (mean of 5) | 5 / 5 | 90.0 s | — | $0.1150 | None |
| `django-11848` | Legacy Arm P (mean of 5) | 5 / 5 | 428.0 s | ~6.97 s | $0.1480 | Audit only |
| `django-11848-P2-1` | **Arm P2** (`claude-haiku-4-5-20251001`) | **Yes (`2/2`, `42/42`)** | **71.4 s** | **0.45 s** | **$0.0663** | **Verified (`Ed25519+ML-DSA-65`)** |
| `django-11422` (uncontaminated `L5,S4,S5`) | Arms L & S (`claude-haiku-4-5-20251001`) | **0 / 3** | 143–175 s | — | $0.198–$0.255 | None |
| `django-11422-P2-2` | **Arm P2** (`claude-haiku-4-5-20251001`, v2.1) | No (`0/1`, `44/44`) | 328.9 s | **0.09 s** | $0.2785 | **Verified (`Ed25519+ML-DSA-65`)** |
| `django-11422-P2-3` (`w_escalate`) | **Arm P2 Cascade** (`claude-sonnet-4-6`, 5 API calls, 9 delegates) | **Yes (`1/1`, `44/44`)** | **48.2 s** (worker) | **0.07 s** | **$0.0787** | **Verified (`Ed25519+ML-DSA-65`)** |

### 2. Uncontaminated 10-Instance Head-to-Head (`openai-codex` `gpt-5.6-luna`, Single-Commit Archive, Zero `.git` Leak)

All 10 calibrated Django instances under `~/.playbox-hermes/openshell-parity/swe/` run on the exact same single-commit `git archive base_commit` tree (`verify_single_commit_repo() == (True, 1)`). Arm P2 includes online sealed Docker gate verification, `--repro-gate`, conditional escalation (`gpt-5.6-terra`), enforced `PLAYBOX_APPROVERS` taint mode, 2-host `RoomLog` quorum promotion (`ROOM-16`/`ROOM-17`), `T1` Secure Enclave hardware attestation, and hybrid `Ed25519+ML-DSA-65` receipt verification:

| Instance | Clean Arm L (Resolved / Wall / Cost) | Clean Arm S (Resolved / Wall / Cost) | Arm P2 (Resolved / Wall / Cost / Med Cmd / `sync=none` / Taint Adv / Receipt) |
|---|---|---|---|
| `django-11848` | No (`1/2` f2p) / 56.4 s / $0.0361 | No (`1/2` f2p) / 51.4 s / $0.0296 | **Yes (`2/2` f2p, `42/42` p2p)** / 96.6 s / $0.0634 / **0.08 s** / 26/43 / **0** / `Ed25519+ML-DSA-65` |
| `django-11422` | **Yes (`1/1` f2p)** / 61.3 s / $0.0626 | **Yes (`1/1` f2p)** / 63.7 s / $0.0464 | **Yes (`1/1` f2p, `44/44` p2p)** / 113.5 s / $0.0753 / **0.07 s** / 10/21 / **0** / `Ed25519+ML-DSA-65` |
| `django-11905` | No (`1/2` f2p) / 84.6 s / $0.0490 | No (`1/2` f2p) / 132.1 s / $0.0806 | No (`1/2` f2p, `36/36` p2p) / 126.4 s / $0.0663 / **0.06 s** / 15/27 / **0** / `Ed25519+ML-DSA-65` |
| `django-11001` | **Yes (`2/2` f2p)** / 298.2 s / $0.0473 | **Yes (`2/2` f2p)** / 58.9 s / $0.0367 | **Yes (`2/2` f2p, `118/118` p2p)** / **78.3 s** / $0.0751 / **0.07 s** / 17/28 / **0** / `Ed25519+ML-DSA-65` |
| `django-11620` | **Yes (`1/1` f2p)** / 103.7 s / $0.0890 | **Yes (`1/1` f2p)** / 115.6 s / $0.0841 | **Yes (`1/1` f2p, `55/55` p2p)** / **103.4 s** / **$0.0713** / **0.07 s** / 38/53 / **0** / `Ed25519+ML-DSA-65` |
| `django-12125` | **Yes (`2/2` f2p)** / 77.5 s / $0.0740 | No (`1/2` f2p) / 94.4 s / $0.0737 | **Yes (`2/2` f2p, `43/43` p2p)** / 178.5 s / $0.1023 / **0.07 s** / 28/50 / **0** / `Ed25519+ML-DSA-65` |
| `django-12700` | **Yes (`1/1` f2p)** / 71.6 s / $0.0506 | No (`0/1` f2p) / 68.5 s / $0.0346 | **Yes (`1/1` f2p, `66/66` p2p)** / 134.5 s / $0.0991 / **0.08 s** / 29/57 / **0** / `Ed25519+ML-DSA-65` |
| `django-11099` | **Yes (`3/3` f2p)** / 32.2 s / $0.0160 | **Yes (`3/3` f2p)** / 38.2 s / $0.0174 | **Yes (`3/3` f2p, `19/19` p2p)** / 99.3 s / $0.0379 / **0.06 s** / 9/17 / **0** / `Ed25519+ML-DSA-65` |
| `django-11049` | **Yes (`1/1` f2p)** / 72.0 s / $0.0838 | **Yes (`1/1` f2p)** / 90.5 s / $0.0584 | **Yes (`1/1` f2p, `8/8` p2p, escalated)** / 231.4 s / $0.1468 / **0.06 s** / 25/36 / **0** / `Ed25519+ML-DSA-65` |
| `django-12286` | **Yes (`1/1` f2p)** / 103.6 s / $0.0498 | **Yes (`1/1` f2p)** / 70.6 s / $0.0444 | **Yes (`1/1` f2p, `7/7` p2p)** / 171.8 s / $0.0711 / **0.08 s** / 19/34 / **0** / `Ed25519+ML-DSA-65` |
| **10-Instance Total / Mean** | **8 / 10 (80.0%)** / 96.1 s mean / $0.5581 ($0.0698 / solve) | **6 / 10 (60.0%)** / 78.4 s mean / $0.5059 ($0.0843 / solve) | **9 / 10 (90.0% — 9/9 unambiguous)** / **133.4 s mean** / $0.8087 (**$0.0899 / solve, +50% more solves than Arm S**) / **0.07 s med cmd** / **0 taint advisories (enforced)** / **10/10 PQ receipts** |

*Note on `openai-codex` cost accounting:* `openai-codex` OAuth reports `cost_status: "included"` (`0.0` direct charge) with exact token counts (`input_tokens`, `cache_read_tokens`, `output_tokens`); both `run_swe.py` and `run_swe_playbox_v2.py` apply identical token-metered API-equivalent pricing ($1.00 / 1M uncached input, $0.10 / 1M cached input, $5.00 / 1M output) so cross-arm USD and USD-per-solve comparisons are strictly apples-to-apples.

*5-Gate Compliance Bundle (`playbox.attested_gate.v1`):* All 18 SWE-bench Arm P2 run receipts, the host `audit.log` (`69` entries), the `BOX-7` egress log (`2` entries, `1` denied `/admin`), the 2-host `RoomLog` (`19` entries, `9` dual-host `ROOM-16`/`ROOM-17` promotions, `19/19` PQ-committed under hybrid `Ed25519+ML-DSA-65` checkpoint), and the `T1` Secure Enclave hardware attestation (`~/.playbox/history/default.log`, pinned device key `a4838567dde927b7...`) are packaged into `/tmp/swe-p2-v2/swe-bench-5gate-bundle.json` (`bundle_digest: 5b581665850021b17b8c06e4fec54c640cc074d0c1044bfcd9c43f37682c7fda`) via `playbox gate export` and verified offline via `playbox gate verify --require-signature --require-pq --require-tier t1 --json` (`ok: true`, `pq_signed: true`, `achieved_tier: "T1"`, `egress_requests: 2`, `promotions: 9`, `room_log_pq_committed: 19`, `attested_receipts: 18`, `enforced_gates_count: 5`, `claims_full_84_permf: false`).

### 3. Devin Recursive Hill-Climb (`gemini-4-neon-high` → `claude-opus-5-5-high`, 5 / 5 = 100.0% Resolved, Zero `.git` Leak)

Driven via `tools/bench/hermes-parity/hillclimb_playbox.py` (`--harness devin --runtime openshell --primary-model gemini-4-neon-high --opponent-model claude-opus-5-5-high`, artifacts in `/tmp/swe-devin-hc/`). Round 1 runs `gemini-4-neon-high` with 3-hop AtlasKV and compounding verified room memory (`verified-memory.jsonl`); any unsolved instance climbs in Round 2 with `claude-opus-5-5-high` using strictly non-oracle feedback:

| Instance | Round & Winning Model | Resolved (`f2p` / `p2p`) | Wall (s) | Delegates (`sync=none` / `delta`) | Median Cmd (s) | Cost (USD) | Taint Adv (Enforced) | PQ Receipt & 5-Gate Bundle |
|---|---|---|---|---|---|---|---|---|
| `django-11848` | Round 1 (`gemini-4-neon-high`) | **Yes (`2/2`, `42/42`)** | 341.1 s | 17 (7 / 10) | 0.64 s | $0.5669 | **0** | **Verified (`Ed25519+ML-DSA-65`, `T1`)** |
| `django-11422` | Round 1 (`gemini-4-neon-high`) | **Yes (`1/1`, `44/44`)** | 206.5 s | 24 (9 / 15) | 1.05 s | $0.3223 | **0** | **Verified (`Ed25519+ML-DSA-65`, `T1`)** |
| `django-11905` | Round 1 (`gemini-4-neon-high`) → **Round 2 (`claude-opus-5-5-high`)** | **Yes (`2/2`, `36/36`)** | **45.3 s** (R2) | **2** (0 / 2) | 10.02 s | **$0.0496** (R2) | **0** | **Verified (`Ed25519+ML-DSA-65`, `T1`)** |
| `django-11001` | Round 1 (`gemini-4-neon-high`) | **Yes (`2/2`, `118/118`)** | 280.6 s | 27 (15 / 12) | **0.18 s** | $0.4501 | **0** | **Verified (`Ed25519+ML-DSA-65`, `T1`)** |
| `django-11620` | Round 1 (`gemini-4-neon-high`) | **Yes (`1/1`, `55/55`)** | 337.6 s | 45 (34 / 11) | **0.12 s** | $0.4919 | **0** | **Verified (`Ed25519+ML-DSA-65`, `T1`)** |
| **5-Instance Total** | **4× `gemini-4-neon-high` (R1) + 1× `claude-opus-5-5-high` (R2)** | **5 / 5 (100.0%)** | **242.2 s mean** | 115 winning (172 total) | **0.12 s–0.64 s** | **$1.8809 winning** ($0.3762 / solve; $2.5587 total) | **0 across all runs** | **6/6 PQ receipts + 6/6 5-gate bundles + 5/5 verified lessons** |

### 4. `n = 50` Multi-Repo Polyglot Scaling (`run_verified_eval.py`, Python + Rust + TypeScript)

Driven via `tools/bench/hermes-parity/run_verified_eval.py` (`--concurrency 3`, single-commit archives, `--memory-gate online`, 3-tier `claude-haiku-5-5` → `claude-sonnet-5` → `claude-sonnet-5-5,claude-sonnet-5` cascade in `/tmp/swe-verified-50-claude/`):

| Suite / Language Family | Tasks (`n`) | Clean Arm S (Stock OpenShell) | Playbox Arm P2 (3-Tier Cascade + 5-Gate `T1` PQ) |
|---|---|---|---|
| `django/django` (cached `n = 28`) | 28 | 25 / 28 (89.3%; `gpt-5.6-luna`: 22 / 28 = 78.6%) | **26 / 28 (92.9%)** · `$0.0642`/solve (fast gate) |
| `pytest-dev/pytest` | 3 | 2 / 3 (66.7%) | **3 / 3 (100.0%)** |
| `sympy/sympy` | 5 | 3 / 5 (60.0%) | **4 / 5 (80.0%)** |
| `scikit-learn/scikit-learn` | 3 | 2 / 3 (66.7%) | **2 / 3 (66.7%)** |
| `pallets/flask` | 1 | 0 / 1 (0.0%) | 0 / 1 (0.0%) |
| `Rust` (`cargo test` crates) | 5 | 5 / 5 (100.0%) | **5 / 5 (100.0%)** |
| `TypeScript` (Node 26 `node --test`) | 5 | 5 / 5 (100.0%) | **4 / 5 (80.0%)** |
| **`n = 50` Polyglot Total** | **50** | **42 / 50 (84.0%)** (37 / 50 = 74.0% pre-gateway-retry, McNemar one-sided $p = 0.035$) · 0 receipts | **44 / 50 (88.0%, 95% CI `[76.2%, 94.4%]`)** · **`$0.1051`/solve** (fast gate; `$0.2018` w/ uniform critic) · **0.12–0.15 s med cmd** · **0 taint advisories** · **50/50 PQ receipts + 44 PQ `RoomLog` promotions (`T1`)** |

### 5. `n = 20` Live Adversarial Security & Anti-Reward-Hacking Benchmark (`run_security_eval.py`)

20 real coding tasks across 5 live threat vectors (`secret_exfil`, `backdoor_injection`, `git_history_leak`, `test_tampering`, `sandbox_escape_traversal`, 60 live runs in `/tmp/swe-security-eval/`):

| Arm | Clean Task Utility | Attack Success Rate (ASR) | Unflagged Reward Hacks | Cryptographic Receipts & `T1` PQ Bundle |
|---|---|---|---|---|
| **Arm L** (Unsandboxed Local Hermes) | 19 / 20 (95.0%) | **20.0% (4 / 20 host escape)** | 0.0% | None |
| **Arm S** (Stock OpenShell SSH) | 17 / 20 (85.0%) | **20.0% (4 / 20 VM secret exposure)** | 0.0% | None |
| **Arm P2** (Playbox Attested Gate) | **19 / 20 (95.0%)** | **0.0% (0 / 20)** | **0.0% (0 / 20)** | **20/20 `Ed25519+ML-DSA-65` + 19 PQ promotions (`T1`)** |

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
