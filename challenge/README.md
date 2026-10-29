# challenge/: the Commons v0 harness

Runnable v0 of [docs/challenge.md](../docs/challenge.md). Needs the steer venv (`steer/.venv`, torch + transformers) and SmolLM2-135M in the local Hugging Face cache.

```sh
./run_demo.sh            # full demo, ~10 min on an M4 Max; writes state/ (gitignored)
../steer/.venv/bin/python gate.py status | credit | verify
```

- `gate.py`: merge gate. `gate.py a SCHEME.json --author X` (Tier A), `gate.py b CANDIDATE.py --author X` (Tier B). Writes hash-chained, Ed25519-signed receipts to `state/receipts.jsonl` and moves `state/frontier.json` on a verified win.
- `tier_a_matmul/`: exact checker for matrix-multiplication schemes (rank = number of multiplications), plus a second independent checker in the gate. `problem.json` (2x2, closes at 7), `matmul3.json` (open target, best known 23).
- `tier_b_lm/`: lossless speedup of greedy decoding for SmolLM2-135M on CPU. `bench.py` is the harness (owns the clock, samples sealed prompts, checks token equality). `baseline/` is the pinned no-KV-cache baseline. `candidates/` has example submissions, including cheats.
- `results/`: the recorded demo run and its receipts.

Candidate API (Tier B): a file defining `Model(model_dir)` with `generate(ids: list[int], n: int) -> list[int]` (greedy). Imports are whitelisted in `tier_b_lm/problem.json`.

Known limits: the "replica" is a second sealed seed on the same machine, not a second host; the hardware fingerprint is self-reported (no attestation); candidates run as ordinary subprocesses with the import whitelist, not in a no-network container (the real room uses `playbox-runner`). See the doc.
