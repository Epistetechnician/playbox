# F3 local dry run (CPU, tiny GPT, scripted stand-ins)

Not agents: ten scripted patches to the baseline, one of them an injected bad candidate (`dev_memorizer_INJECTED_BAD`: trains on the dev blocks it can see). Ceiling label: `local_internal_exploratory`. Hardware attestation is self-reported (`attested: false`).

Measured on one Apple-silicon Mac, CPU, 4 threads, about 5 minutes from genesis to reveal (306 s by receipt timestamps).

| seq | stand-in | verdict | reason | gain vs frontier |
|---|---|---|---|---|
| 1 | lr 3e-3 | promote | | 0.501 |
| 2 | lr 3e-2 | reject | gain_below_threshold | -0.277 |
| 3 | warmup 50 | reject | gain_below_threshold | -0.054 |
| 4 | wd 1e-4 | reject | gain_below_threshold | -0.001 |
| 5 | **injected dev memorizer** | reject | **holdout_failed** (caught false promotion) | 0.685 on dev; holdout loss 4.19 to 4.25 vs limit 2.510 |
| 6 | lr 6e-3 | promote | | 0.275 |
| 7 | 1 layer | promote | | 0.234 |
| 8 | batch 16 | reject | gain_below_threshold | -0.119 |
| 9 | width 128 | promote | | 0.137 |
| 10 | clip 1.0 | reject | gain_below_threshold | -0.045 |

Calibration (frozen in the genesis receipt): target dev loss L* = 2.486 (oracle floor 2.013), holdout tolerance 0.0243, timing noise cv 0.037, minimum gain theta 0.12.

Files: `receipts.jsonl` (signed chain), `candidates/` (sources by sha256), `feed.jsonl` (the redacted feed agents would see), `harness.log`, `verify.txt` (chain, genesis, lineage, rule re-derivation, reveal, leak scan), `verify_replay.txt` (the same plus replay of all 4 promotions on recorded seeds and on the revealed fresh seeds, and re-running the caught candidate). `host.pub` is the pinned public key. The private key and the custodian directory are not committed.

```sh
python3 challenge/f3/verify.py --work docs/f3/dry_run --host-pubkey "$(cat docs/f3/dry_run/host.pub)"
python3 challenge/f3/verify.py --work docs/f3/dry_run --host-pubkey "$(cat docs/f3/dry_run/host.pub)" --replay all   # about 3 min on CPU
```

The genesis binds the git commit `c5cba09` of the harness; the verifier checks the file hashes in this repo against it. The verifier would fail if `challenge/f3/task.py`, `baseline.py`, `harness.py`, `task_spec.json` or `docs/f3/PROTOCOL.md` were edited after this run.
