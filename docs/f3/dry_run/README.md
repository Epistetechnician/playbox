# F3 local dry run (CPU, tiny GPT, scripted stand-ins)

Not agents: ten scripted patches to the baseline, one of them an injected bad candidate (`dev_memorizer_INJECTED_BAD`: trains on the dev blocks it can see). Ceiling label: `local_internal_exploratory`. Hardware attestation is self-reported (`attested: false`).

Measured on one Apple-silicon Mac, CPU, 4 threads, 268 s from genesis to reveal (receipt timestamps). Regenerated on 2026-11-14 after the substrate-aware harness and amendment A2 changed `harness.py` and `task.py` (the first run, commit `22b7a1d`, pinned the older files and no longer verifies against this repo state; its numbers differed slightly).

Calibration (frozen in the genesis receipt): target dev loss L* = 2.486 (oracle floor 2.013), baseline time to target 6.5 s, timing noise cv 0.0027, eval granularity 0.0083, minimum gain theta 0.05, holdout tolerance 0.0206.

| seq | stand-in | verdict | reason | gain vs frontier | holdout loss (3 seeds) |
|---|---|---|---|---|---|
| 1 | lr 3e-3 | promote | | 0.506 | 2.466 to 2.484 |
| 2 | lr 3e-2 | reject | gain_below_threshold | -0.356 | |
| 3 | warmup 50 | reject | gain_below_threshold | -0.057 | |
| 4 | wd 1e-4 | reject | gain_below_threshold | -0.002 | |
| 5 | **injected dev memorizer** | reject | **holdout_failed** (caught false promotion) | 0.675 on dev | 4.211 to 4.252 (limit 2.507) |
| 6 | lr 6e-3 | promote | | 0.227 | 2.465 to 2.477 |
| 7 | 1 layer | promote | | 0.256 | 2.471 to 2.482 |
| 8 | batch 16 | reject | gain_below_threshold | -0.154 | |
| 9 | width 128 | promote | | 0.080 | 2.459 to 2.482 |
| 10 | clip 1.0 | reject | gain_below_threshold | -0.024 | |

The stand-ins were chosen after a manual look at nine variants on two seeds; the dry run measures the gate, not the proposals.

`verify.txt`: all checks pass (re-verified 2026-11-14 from a fresh clone on a second machine; since amendment A3 the protocol check reads "a PROTOCOL.md committed before the genesis receipt", see A5). `verify_replay.txt`: all 4 promotions reproduce on their recorded seeds, and on the revealed fresh seeds pass the gates with gains 0.507, 0.227, 0.248 and 0.101 (floor theta/2 = 0.025); the caught candidate fails the holdout again (4.217 against 2.507).

Files: `receipts.jsonl` (signed chain), `candidates/` (sources by sha256), `feed.jsonl` (the redacted feed agents would see), `harness.log`, `verify.txt` (chain, genesis, lineage, rule re-derivation, reveal, leak scan), `verify_replay.txt` (the same plus replay of all 4 promotions on recorded seeds and on the revealed fresh seeds, and re-running the caught candidate). `host.pub` is the pinned public key. The private key and the custodian directory are not committed.

```sh
python3 challenge/f3/verify.py --work docs/f3/dry_run --host-pubkey "$(cat docs/f3/dry_run/host.pub)"
python3 challenge/f3/verify.py --work docs/f3/dry_run --host-pubkey "$(cat docs/f3/dry_run/host.pub)" --replay all   # about 3 min on CPU
```

The genesis binds git commit `9a251e8`; the verifier checks the file hashes in this repo against it. The verifier would fail if `challenge/f3/task.py`, `baseline.py`, `harness.py`, `task_spec.json`, `harness.py` were edited after this run, or if the text of `docs/f3/PROTOCOL.md` that was pinned were edited in place (appended amendments are fine).
