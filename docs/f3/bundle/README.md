# F3 replay bundle (for outside verification)

**Status: owner-run, unreviewed (`LocalDevelopmentUnreviewed`). Nothing in this bundle has been reviewed by anyone outside the authors.** This bundle exists so that an outside party can check what was claimed without the authors' harness, machines or accounts. It contains no private key, no custodian directory and no holdout data.

## What this is
F3 is a small demo of a *verified hill-climb*: candidate training scripts for a ~50M-parameter GPT (the autoresearch GPT on the climbmix text data) are scored by one number, **training seconds on one H100 until a fixed dev loss (1.18 bits per byte) is reached**, as a ratio to the current frontier script. A candidate is *promoted* only if it is faster by at least a calibrated threshold (theta) **and** it also reaches the target on a sealed holdout shard that the candidate never sees. Every decision is an Ed25519-signed, hash-chained receipt. The rules are in `protocol/PROTOCOL.current.md` (sections 4, 6, 7, 10; amendments in 15).

Two real H100 runs are included, both with **scripted stand-ins** (fixed patches written beforehand, not agents):
- **pilot 1** (`runs/pilot1`): 8 candidates, 0 promotions, 1 caught false promotion (an injected dev-set memorizer).
- **pilot 2** (`runs/pilot2`): 4 candidates, 2 promotions (`compile_model`, gain 0.61; `anneal_to_step_240`, gain 0.12), 1 rejection under the threshold, 1 caught false promotion (the memorizer again). Replayed once on a second H100 node (`runs/pilot2/replay2.txt`).

## What is claimed (exactly)
1. **The signed chain verifies.** For each run, every receipt's body hashes to its id, links to the previous id, carries a valid Ed25519 signature under the pinned host key, no id repeats, sequence numbers are contiguous, and all receipts are bound to the genesis. *(Tier A)*
2. **The genesis pins the code and protocol.** The genesis receipt's hashes of `PROTOCOL.md`, the task spec and the four code files equal the files in this bundle. *(Tier A)*
3. **Promotions re-derive from recorded measurements.** An independent re-implementation of the promotion rule (`verify.py: rederive`, which does not import the harness) recomputes medians, ratio, gates and verdict from the measurements stored in each receipt and reaches the recorded verdict, reason and ratio for every decision. Each candidate source hashes to the hash in its receipt, and each candidate's parent is the frontier at that point. *(Tier A)*
4. **The holdout commitment is last-revealed and consistent.** The reveal is the final receipt (so the holdout secret was not published during the round); the replay seeds it reveals match the commitment signed in the genesis; no archived candidate contains the revealed seed or salt. *(Tier A)* **The Merkle opening of the holdout root against the real holdout documents, and the scan of candidate sources for 64-character windows of holdout documents, need the holdout shard and are Tier B/not run here** (they were run by the authors; outputs in `runs/*/verify*.txt`).
5. **No candidate source leaks the holdout** — in the narrow sense of the two checks above (secret and, for the authors' own runs, document windows).
6. **Replay on a second node reproduced.** On a second H100 node, `verify.py --replay all` re-ran both promoted candidates: recorded seeds reproduced (steps to target within 10, holdout loss within 0.02), fresh seeds [1508, 1513, 1852] passed the gates with gain 0.615 and 0.129 (>= theta/2), and the caught false promotion failed the holdout again (2.249 vs limit 1.2225). Output: `runs/pilot2/replay2.txt`. **You cannot confirm this from Tier A; it is the authors' recorded output.**

## What is NOT claimed
- **Not real agents.** All candidates are scripted stand-ins (`challenge/f3/arms.py`). Nothing is said about agent skill, or about agents vs random search (the random-search arm was planned and **not run**).
- **The second host is not independent.** Same operator, same GPU type, same code tarball. It shows the result is not an accident of one machine, not that two parties agree.
- **No OS-level custody.** Candidates and the evaluator ran as the **same Unix user** (protocol A3). "Agents never see the holdout" is not shown; the only protection was that scripted stand-ins cannot read files (static check) and a scrubbed environment.
- **The compile gain is a speed-up.** `compile_model` is faster per step, at the same loss per step (smoke: dev loss at step 240 equal within noise). It is not a better model.
- **Hardware is self-reported** (`attested: false`); a host holding the signing key could issue consistent receipts. The replay is the only defense and it is same-operator.
- **Tiny n.** Two promotions, one caught false promotion, one run per pilot. Descriptive only. A zero missed-false-promotion count means none found by this replay.
- **Owner-run.** Run, calibrated, pinned and reported by the same person; no preregistration was witnessed by anyone else.
- Wall-clock seconds are never compared across hosts.

## Pins
| | pilot 1 | pilot 2 |
|---|---|---|
| Host public key (Ed25519, hex) | `9b2fdb975930638e802e83fc7045e2af30b3d2c813240943bb0ad446c796a3f5` | `e4b0b7b959914f2e3332fe7037fe4e63854a44bc87671cb53a86621055dcce5d` |
| How the key was pinned | recorded locally 2026-11-14T16:16:26Z before any candidate (`runs/pilot1/README.md`; the key itself is only in `host.pub` and the genesis) | `runs/pilot2/PINNED.txt`: pinned locally 2026-11-15T00:58Z, printed on the node 00:52Z, before any candidate |
| Genesis git commit (the authors' node tree) | `83eba5ad16e6fae0f91f65895398b451c59d8460` | `fa3bba87b47cf23a0c701c1ac576ac10ee47a0b0` |
| Genesis receipt timestamp (unix) | 1794672956 | 1794704230 |
| Pinned `PROTOCOL.md` sha256 | `583fabc7caaa9d97defa6da8cb3b2baeb8497be9f5950cc56762325de1c0a69e` (file `protocol/PROTOCOL.pilot1.md`) | `e7c3871a022dbde4258895f246aae44594a09bd5122991a0f42cc4fc166739c8` (file `protocol/PROTOCOL.pilot2.md`) |
| Task spec | `challenge/f3/task_spec_gpu.json` `1c298a1b…` | `challenge/f3/task_spec_gpu_p2.json` `5cf50b51…` |
| Calibrated | target 1.18 bpb, theta 0.06, eps 0.0151 | target 1.18 bpb, theta 0.05, eps 0.0425 |

Both runs pin the same four code files: `task_climbmix.py ab1166aa…`, `baseline_climbmix.py b32a304c…`, `harness.py f892414e…`, `climbmix_data.py 0b4c6586…`.

**The genesis commit hashes cannot be looked up from this bundle.** They are commits of a tree the authors rebuilt on each GPU node with a fixed author and date; they are not commits of the public repository (see "Known awkward facts"). The binding is therefore the file hashes: the genesis code hashes equal the files in `challenge/f3/` here, byte for byte. The `PROTOCOL.md` text pinned by each genesis is supplied as a file (`--protocol-file`) because the verifier's normal route uses git history, which this bundle does not have (amendment A5 explains the git route). What this loses: the check "PROTOCOL.md was committed before the genesis receipt" is a git-history check and is **not run** here; in the public repo the two pinned versions are commits `0908479` (A3) and `0bb8968` (smoke result added to A4), both earlier than the corresponding genesis timestamps. `make_bundle.sh` asserts that those commits hash to the pins.

The hash of **every file** in the bundle is in `SHA256SUMS` (and with origin in `MANIFEST.md`). The tarball hash is published outside the tarball (it cannot contain its own).

## Tools needed (Tier A)
`bash`, `python3` >= 3.10, Python packages `cryptography` (tested 48.0.0) and `numpy` (tested 2.4.5), and `shasum` or `sha256sum`. No GPU, no dataset, no network, no git, no PyTorch. `check.sh` sets `PYTHONDONTWRITEBYTECODE` and writes nothing into the bundle.

```bash
tar xzf f3-bundle.tar.gz && ./f3-bundle/check.sh
```

## Tier A: what it checks, and what it cannot
`check.sh` (1) checks every file against `SHA256SUMS`; (2) for each pilot, compares `host.pub` with the key pinned in the script and runs `challenge/f3/verify.py --no-data --protocol-file protocol/PROTOCOL.<pilot>.md`, which performs the checks of claims 1 to 4 above.

**Tier A cannot check:**
- that any candidate actually runs, or that its recorded wall seconds, step counts or losses are real (the measurements are signed by the host key, so Tier A shows they were not edited afterwards, **not** that they are true);
- the data manifest against real shards (tokenizer, token caches, dev shard, holdout shard hashes in the genesis);
- the holdout Merkle opening against the real holdout documents, and the document-window leak scan (both shown as `SKIP` in the output below);
- that the genesis git commit exists, or that PROTOCOL.md was committed before the genesis;
- replay, anything about the second host, or whether it was independent;
- whether the host key really is the key of a machine that ran the runs (it is pinned by the authors; Tier A takes it as given).

**Search done for this bundle:** the bundle's final tree was searched for private-key material (PEM `BEGIN ... PRIVATE KEY`, 64-hex values other than the two public keys, files named `*.key`, `*.pem`, `*priv*`, `custodian*`, `.env`), and for the holdout (any `holdout` shard file, `.parquet`, `.npy`, `.pt`, `.pkl`; and the 64-character windows of the holdout documents could not be searched because the documents are not here). Result is at the end of this file. Note the reveal receipt contains the holdout *seed and salt* by design (they are what is revealed at the end of a run); that is not a private key and not the holdout data.

## Tier B: re-run the replay on an H100 with the data (documented, not run)
Not run for this bundle: it needs a GPU and a paid node, which were not authorized for this task. Reference for what the authors did: `runs/pilot2/README.md`, `runs/pilot2/replay2.txt`, `docs/f3/GPU_RUN_REQUEST_P2.md` in the repo, and `challenge/f3/climbmix_data.py`.

1. **Hardware and software to match.** One H100 80GB; Linux x86_64 (authors' kernel 5.15, glibc 2.39), **Python 3.12.3**, the `pytorch-2.13-cuda12.9` image (PyTorch 2.13, CUDA 12.9), plus `pyarrow tiktoken requests rustbpe numpy cryptography`. Exact pip versions of the authors' nodes are **not recorded in the repo**; record yours in `REVIEW_TEMPLATE.md`.
2. **Code.** Use this bundle's `challenge/f3/` as the tree (its hashes equal the genesis pins). Do not use a different `harness.py`, `task_climbmix.py`, `baseline_climbmix.py` or `climbmix_data.py`.
3. **Data (about 1.1 GB).** climbmix shards from `https://huggingface.co/datasets/karpathy/climbmix-400b-shuffle` (see `climbmix_data.py`): train shards 0 to 9, dev shard 6542, holdout shard 6541. Set `F3_SPEC=task_spec_gpu_p2.json` (pilot 2) or `task_spec_gpu.json` (pilot 1), `F3_DATA_DIR=<dir>`, `F3_HOLDOUT_DIR=<dir outside the candidate's reach>`, then `python3 challenge/f3/task_climbmix.py prepare --data --holdout --tokens`. This trains a tokenizer (`rustbpe`) and builds token caches, and prints a data manifest; **it must equal the `data_manifest` in the genesis receipt** (`runs/pilotN/receipts.jsonl`, first line, `body.data_manifest`). The tokenizer hash is the one that may not reproduce on a different `rustbpe`/platform; the authors copied the primary node's tokenizer to the second node rather than retraining. If your manifest differs, say so in the review; replay then does not test the same artifacts.
4. **Verify with data, then replay.**
```bash
export F3_SPEC=task_spec_gpu_p2.json F3_DATA_DIR=... F3_HOLDOUT_DIR=...
python3 -I challenge/f3/verify.py --work runs/pilot2 --host-pubkey e4b0b7b959914f2e3332fe7037fe4e63854a44bc87671cb53a86621055dcce5d \
   --repo-root . --protocol-file protocol/PROTOCOL.pilot2.md --replay none      # now also opens the holdout Merkle root and scans documents
python3 -I challenge/f3/verify.py ... --replay all                             # about 0.5 h on one H100 for pilot 2 (9 runs per promotion, compiled about 96 s each)
```
(`--repo-root .` is the bundle root; the genesis git commit is not found and is reported as a soft check.)
5. **Expected outputs and tolerances** (from `runs/pilot2/replay2.txt`, `verify.py` section 6): for each promotion, "recorded seeds reproduce" means for every recorded seed the candidate still reaches the target, **steps to target within 10 steps (one eval interval) of the recorded value**, and **holdout loss within 0.02 bpb** of the recorded one. "Fresh replay seeds [1508, 1513, 1852]" means all gates pass (dev <= target + `dev_recompute_tol`, holdout <= 1.18 + 0.0425 = 1.2225) and the candidate/parent median wall ratio on **your** host gives gain >= theta/2 = 0.025. The authors saw 0.615 (compile) and 0.129 (anneal); anything near these, same sign, is the expected shape, but **absolute seconds are not compared across hosts**. The caught memorizer (`783880e3…`) must fail the holdout again (authors: 2.249 bpb vs limit 1.2225). Expected final line: `RESULT: VERIFIED {"receipts": 6, "promotions": 2, "rejections": 2, "caught_false_promotions": 1, ...}` with 0 FAIL lines (`replay2.txt` has 38 lines). A replay failure is a finding: write it down, do not retry until it passes.
6. **Pilot 1** has no promotion to replay; with data, only the holdout opening and the caught memorizer's holdout failure (2.204 vs limit 1.195, `verify_replay.txt`, same node) can be re-checked.

## Layout
`README.md` this file · `REVIEW_TEMPLATE.md` · `check.sh` Tier A · `make_bundle.sh` rebuilds the tarball · `SHA256SUMS`, `MANIFEST.md` · `challenge/f3/` verifier and pinned code (copy of the repo's, tests omitted) · `protocol/` the two pinned PROTOCOL.md versions and the current one · `runs/pilot1`, `runs/pilot2` receipts, candidate sources, `host.pub`, authors' verify/replay outputs and logs (pilot 2 files were extracted unchanged from `docs/f3/pilot2/p2out.tgz`, hash in `runs/pilot2/P2OUT_TGZ_SHA256.txt`) · `runs/smoke_p2` the compile smoke measurement · `TIER_A_OUTPUT.txt` the output below.

## Known awkward facts (so you do not have to find them)
- **Node B was built from a tarball, not from the genesis commit.** Both pilots' genesis commit hashes (`83eba5ad…`, `fa3bba87…`) are of trees committed on the GPU nodes with a fixed author and date, not commits of the public repository, where the same files have different commit hashes. The authors' own `verify*.txt` passed the "file at git commit" checks only because they ran on those node trees. Pilot 1's README says this itself. Second-host replay used the same tarball approach with the primary's tokenizer copied in, so "same code" is shown by file hashes, not by a clean clone of a public commit.
- **The git-based protocol-order check in `runs/pilot2/verify2.txt` compares against a rewritten date**: it prints "protocol committed 1794614400", but the public commit `ca9ad41` that first added `PROTOCOL.md` is dated 1794608258. Both are earlier than the genesis (1794704230), so the conclusion stands; the printed number is the node tree's fixed date.
- **Pilot 2 cost exceeded the A4 hard ceiling.** A4 and `docs/f3/GPU_RUN_REQUEST_P2.md` state a 40 USD hard ceiling (estimate 26.5 USD); `runs/pilot2/README.md` reports 41.25 USD spent and calls the ceiling "$50". The "random-search" arm and the null control, both in A4's plan for budget-matched comparison, were not run; A4 describes two arms and Q = 4 each, the run has one arm.
- **The calibrations of the two pilots differ in ways the README does not explain.** Same spec scale and data, but the holdout-minus-dev gap was +0.0051 (eps 0.0151) in pilot 1 and +0.0325 (eps 0.0425) in pilot 2, and timing noise cv was 0.0179 versus 0.0004 (one seed repeated, `calibration.noise_cv` in each genesis). The holdout tolerance in pilot 2 is therefore about 2.8 times larger. Both are what the calibration rule produced and are signed before any candidate; they are still a question a reviewer will ask.
- **The anneal decision was lost once with no receipt and rerun** (`runs/pilot2/arm.log` shows `RESUME2` twice before seq 3, and a `RESUME` before seq 2; the README mentions it once). Only signed receipts count, and the chain is contiguous.
- **Stale statements in the repo.** `protocol/PROTOCOL.current.md` still says "Phase 1 ... Nothing here has been run on a GPU." (written before the pilots). `runs/pilot1/README.md` says `candidates/` holds "the 10 archived sources"; it holds 9 (the baseline and 8 candidates). `runs/pilot2/README.md` says "~0.05" for theta (the genesis says 0.05; the A4 text and the request say the pilot-1 value 0.06 as an expectation).
- **The data manifest includes the holdout shard hash** (`data_manifest.holdout_shard`), and the holdout shard is a public climbmix shard (6541) that the custodian merely kept apart. So the holdout is "unseen by candidates" only by custody, and reproducible by anyone with the dataset. Any claim that depends on the sealed shard resting on secrecy alone is weak; see "No OS-level custody".
- **`verify.py`** gained `--no-data` and `--protocol-file` for this bundle (tested in `challenge/f3/tests/test_f3.py`); the pilots' own `verify*.txt` outputs were produced by the earlier version, whose checks are otherwise identical.

## Key-material and holdout search (done on the final extracted tree, 57 files)
- No file named `host.key` (the harness writes the signing key there as a PEM, `harness.py:_key`), `*.pem`, `*.key`, `*priv*`, `custodian*`, `.env`, `*.parquet`, `*.npy`, `*.pt`, `*.pkl` or `*.pyc`. No PEM/OpenSSH/PGP private-key header in any file (the only text match for "PRIVATE KEY" is this README describing the search). `make_bundle.sh` copies an explicit list of paths; it never reads a work directory.
- The 64-hex values in the bundle are the two public keys (24 occurrences) and 260 others that are receipt ids, candidate/code/spec hashes, Merkle/commitment values and the reveal's holdout salt. A regex cannot tell a hash from a raw 32-byte secret; what was checked is that the receipts' fields are only the verifier's known fields (the reveal has `holdout_seed`, `holdout_salt`, `replay`, no key field) and that no log or README mentions a private key or secret. The holdout seed and salt are in the two reveal receipts by design.
- No holdout document text or shard is in the bundle (no data files at all). Whether candidate sources contain holdout document windows could only be checked with the shard; the authors' runs did (their `verify*.txt`), and Tier B repeats it.
- No GPU job, gman, Modal or paid API was used to build or check this bundle.

## Tier A output (real run, from a clean extraction of the tarball into a new temp dir; no `.git`, no repo, no network, no data; Python 3.14.5, cryptography 48.0.0, numpy 2.4.5, macOS; exit 0). Same text is in `TIER_A_OUTPUT.txt`
```
== 0. bundle file hashes (SHA256SUMS)
[PASS] every file matches SHA256SUMS
[PASS] no file outside SHA256SUMS

== pilot1: host key 9b2fdb975930638e802e83fc7045e2af30b3d2c813240943bb0ad446c796a3f5
[PASS] runs/pilot1/host.pub equals the pinned key
[PASS] chain: hash links, body hashes, Ed25519 signatures under the pinned key, no duplicates, sequence: 10 receipts
[PASS] genesis: protocol hash equals the committed PROTOCOL.md
[PASS] genesis: task spec hash: task_spec_gpu.json
[PASS] genesis: task_climbmix.py hash equals the file in the repo state
[PASS] genesis: baseline_climbmix.py hash equals the file in the repo state
[PASS] genesis: harness.py hash equals the file in the repo state
[PASS] genesis: climbmix_data.py hash equals the file in the repo state
[PASS] genesis: git commit lookup: commit not present in this clone; file hashes above are the binding (soft)
[SKIP] genesis: local data artifacts match the data manifest (tokenizer, token caches, shards): NOT CHECKED (no-data mode)
[PASS] lineage: baseline archived and hashed
[PASS] decision seq 1 (a8c77fab8b): archived candidate source matches its hash
[PASS] decision seq 1 (a8c77fab8b): parent is the frontier at that point: parent b32a304cb3, frontier b32a304cb3
[PASS] decision seq 1 (a8c77fab8b): verdict re-derived from recorded measurements: recorded reject/gain_below_threshold ratio=0.9988; re-derived reject/gain_below_threshold ratio=0.9988
[PASS] decision seq 2 (c38b570872): archived candidate source matches its hash
[PASS] decision seq 2 (c38b570872): parent is the frontier at that point: parent b32a304cb3, frontier b32a304cb3
[PASS] decision seq 2 (c38b570872): verdict re-derived from recorded measurements: recorded reject/target_not_reached ratio=None; re-derived reject/target_not_reached ratio=None
[PASS] decision seq 3 (556f1dd615): archived candidate source matches its hash
[PASS] decision seq 3 (556f1dd615): parent is the frontier at that point: parent b32a304cb3, frontier b32a304cb3
[PASS] decision seq 3 (556f1dd615): verdict re-derived from recorded measurements: recorded reject/gain_below_threshold ratio=1.0811; re-derived reject/gain_below_threshold ratio=1.0811
[PASS] decision seq 4 (314fc7977d): archived candidate source matches its hash
[PASS] decision seq 4 (314fc7977d): parent is the frontier at that point: parent b32a304cb3, frontier b32a304cb3
[PASS] decision seq 4 (314fc7977d): verdict re-derived from recorded measurements: recorded reject/gain_below_threshold ratio=1.0001; re-derived reject/gain_below_threshold ratio=1.0001
[PASS] decision seq 5 (39079932c0): archived candidate source matches its hash
[PASS] decision seq 5 (39079932c0): parent is the frontier at that point: parent b32a304cb3, frontier b32a304cb3
[PASS] decision seq 5 (39079932c0): verdict re-derived from recorded measurements: recorded reject/holdout_failed ratio=0.1345; re-derived reject/holdout_failed ratio=0.1345
[PASS] decision seq 6 (1b3df86124): archived candidate source matches its hash
[PASS] decision seq 6 (1b3df86124): parent is the frontier at that point: parent b32a304cb3, frontier b32a304cb3
[PASS] decision seq 6 (1b3df86124): verdict re-derived from recorded measurements: recorded reject/gain_below_threshold ratio=1.1579; re-derived reject/gain_below_threshold ratio=1.1579
[PASS] decision seq 7 (dcdacc9ecd): archived candidate source matches its hash
[PASS] decision seq 7 (dcdacc9ecd): parent is the frontier at that point: parent b32a304cb3, frontier b32a304cb3
[PASS] decision seq 7 (dcdacc9ecd): verdict re-derived from recorded measurements: recorded reject/gain_below_threshold ratio=0.9597; re-derived reject/gain_below_threshold ratio=0.9597
[PASS] decision seq 8 (6a07d1d15a): archived candidate source matches its hash
[PASS] decision seq 8 (6a07d1d15a): parent is the frontier at that point: parent b32a304cb3, frontier b32a304cb3
[PASS] decision seq 8 (6a07d1d15a): verdict re-derived from recorded measurements: recorded reject/gain_below_threshold ratio=0.9611; re-derived reject/gain_below_threshold ratio=0.9611
[PASS] holdout: exactly one reveal and it is the last receipt: a reveal before the last decision means the holdout leaked mid-round
[SKIP] holdout: revealed seed and salt open the root committed in genesis: NOT CHECKED (no-data mode: needs the holdout shard)
[PASS] holdout: replay seeds match the commitment in genesis
[PASS] holdout: no archived candidate source contains the revealed seed or salt
[SKIP] holdout: no archived candidate source contains a window of a holdout document: NOT CHECKED (no-data mode: needs the holdout shard)

RESULT: VERIFIED  {"receipts": 10, "promotions": 0, "rejections": 8, "caught_false_promotions": 1, "reject_reasons": {"gain_below_threshold": 6, "holdout_failed": 1, "target_not_reached": 1}}

== pilot2: host key e4b0b7b959914f2e3332fe7037fe4e63854a44bc87671cb53a86621055dcce5d
[PASS] runs/pilot2/host.pub equals the pinned key
[PASS] chain: hash links, body hashes, Ed25519 signatures under the pinned key, no duplicates, sequence: 6 receipts
[PASS] genesis: protocol hash equals the committed PROTOCOL.md
[PASS] genesis: task spec hash: task_spec_gpu_p2.json
[PASS] genesis: task_climbmix.py hash equals the file in the repo state
[PASS] genesis: baseline_climbmix.py hash equals the file in the repo state
[PASS] genesis: harness.py hash equals the file in the repo state
[PASS] genesis: climbmix_data.py hash equals the file in the repo state
[PASS] genesis: git commit lookup: commit not present in this clone; file hashes above are the binding (soft)
[SKIP] genesis: local data artifacts match the data manifest (tokenizer, token caches, shards): NOT CHECKED (no-data mode)
[PASS] lineage: baseline archived and hashed
[PASS] decision seq 1 (1cf676d369): archived candidate source matches its hash
[PASS] decision seq 1 (1cf676d369): parent is the frontier at that point: parent b32a304cb3, frontier b32a304cb3
[PASS] decision seq 1 (1cf676d369): verdict re-derived from recorded measurements: recorded promote/promoted ratio=0.3904; re-derived promote/promoted ratio=0.3904
[PASS] decision seq 2 (28bbf440e3): archived candidate source matches its hash
[PASS] decision seq 2 (28bbf440e3): parent is the frontier at that point: parent 1cf676d369, frontier 1cf676d369
[PASS] decision seq 2 (28bbf440e3): verdict re-derived from recorded measurements: recorded reject/gain_below_threshold ratio=0.9629; re-derived reject/gain_below_threshold ratio=0.9629
[PASS] decision seq 3 (9f9365321d): archived candidate source matches its hash
[PASS] decision seq 3 (9f9365321d): parent is the frontier at that point: parent 1cf676d369, frontier 1cf676d369
[PASS] decision seq 3 (9f9365321d): verdict re-derived from recorded measurements: recorded promote/promoted ratio=0.8822; re-derived promote/promoted ratio=0.8822
[PASS] decision seq 4 (783880e37d): archived candidate source matches its hash
[PASS] decision seq 4 (783880e37d): parent is the frontier at that point: parent 9f9365321d, frontier 9f9365321d
[PASS] decision seq 4 (783880e37d): verdict re-derived from recorded measurements: recorded reject/holdout_failed ratio=0.1927; re-derived reject/holdout_failed ratio=0.1927
[PASS] holdout: exactly one reveal and it is the last receipt: a reveal before the last decision means the holdout leaked mid-round
[SKIP] holdout: revealed seed and salt open the root committed in genesis: NOT CHECKED (no-data mode: needs the holdout shard)
[PASS] holdout: replay seeds match the commitment in genesis
[PASS] holdout: no archived candidate source contains the revealed seed or salt
[SKIP] holdout: no archived candidate source contains a window of a holdout document: NOT CHECKED (no-data mode: needs the holdout shard)

RESULT: VERIFIED  {"receipts": 6, "promotions": 2, "rejections": 2, "caught_false_promotions": 1, "reject_reasons": {"gain_below_threshold": 1, "holdout_failed": 1}}

TIER A: ALL CHECKS PASSED (see README: what this does not cover)
```
