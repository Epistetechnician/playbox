# Proof status

What was re-checked, how, and what still rests on stored results or on the authors' word. Checked 2026-11-13 on a Mac
(Apple Silicon, git 2.55.0) unless noted. Levels: **re-run** (the check executed again here), **recomputed** (numbers rebuilt
from the raw result files by `scripts/verify_claims.py`; the experiment itself was not repeated), **stored** (only the
authors' documents and result files, nothing re-derived), **open**.

## Software

| Claim | Level | How |
|---|---|---|
| Rust workspace tests pass | re-run | `cargo test --workspace --offline`: 29 test binaries, 0 failed |
| Bundle import refuses a `.git/hooks` tree on old git | re-run (git behaviour), code read | On git 2.43.0 (`ubuntu:24.04`) `git fetch` from a bundle ignores `transfer.fsckObjects` and imports the tree; `fsck --strict` with `hasDotgit=error` rejects it. `bundle_import` runs that fsck (#11). The Rust test itself was not run on 2.43 after the fix; it passes on 2.55.0 |
| Black-box conformance | re-run | `conformance/run.py`: 6 pass, 1 skipped (`version_skew` needs an older release) |
| Grant theorems (Lean) | re-run | `lake build` succeeds |
| Hill-climb gate receipts | re-run + recomputed | `challenge/gate.py verify`: 6 receipts verified; 4 reject, 2 promote |
| Python suite `steer` | re-run | `uv run --offline --group dev pytest tests`: 128 passed in 16 min |
| Python suites `judge`, `world` | open | not yet run |
| Fuzz targets | stored | not re-run this session |
| Two-machine, India, Pi, T1 Secure Enclave, T3 TDX runs | stored | [tested-on.md](tested-on.md); needs the original hardware |

## Research numbers (recomputed from raw files)

`python3 scripts/verify_claims.py` rebuilds these and exits non-zero on any mismatch (currently all match):

- Qwen3-8B ↔ OLMo-2-7B, probe-gated, two seeds: 140/160 topics, 71/72 controls, Wilson CI 81 to 92%.
- Qwen3-8B → DeepSeek-7B at receiver layer 0.9: 139/160 topics, 68/72 controls.
- Swarm, 400 trials: novelty filter 55.7% recall, one agent 44.5%, all summed 46.7%, rare concepts 54.2% against 11.5%,
  all-k recovered 17% against 66% for oracle text.
- Hub (E8, five small models, k=64): hub 0.47, per-pair map 0.51, receiver's own direction 0.81, random 0.03; 64 wire bytes.
- KV hybrid, corrected salted run (4B): hybrid 3,053 turns/min against RAG 4,526, lookup accuracy 96.1% against 100%.
  The first throughput claim stays withdrawn.

## Reading the numbers fairly

- The swarm's p = 2e-9 compares the novelty filter with the plain sum. Against random agents the pooled p is 1e-5, and
  in the Qwen→OLMo direction alone it is 0.057 (not significant); the OLMo→Qwen direction carries most of the gap.
- Latent vectors lose to text on accuracy and bytes whenever the reader can use text (§6e, §6j of
  [latent-channels.md](latent-channels.md)). The channel's case is no readable text and an untouched context.
- Everything in `latent-channels.md` is `local_internal_exploratory`; most cells are one or two seeds.

## Not proven

- Re-running any GPU experiment (needs Modal or an H100); only the stored outputs were recomputed.
- An independent audit; a hardware TPM; libkrun on Linux/KVM; a real closed-model reader of concept cards.
- The 3×3 matmul target (best known 23) and a second host for the Commons replica.
