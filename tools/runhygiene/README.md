# runhygiene

Deterministic checks that make experiment scripts reproducible. Stdlib only. Contract mirrors codehygiene
(findings with kind/line/probability, `--check`, `--json`).

```bash
python3 tools/runhygiene/runhygiene.py challenge steer tools --exclude '*/hermes_*/*' --check
python3 tools/runhygiene/runhygiene.py challenge --frozen tools/runhygiene/frozen.txt --base origin/main --check
```

| kind | flags |
|---|---|
| `unseeded_rng` | `default_rng()`, `random.Random()`, `RandomState()` with no seed |
| `global_rng` | numpy/random/torch module-level random calls with no seed in the file (`generator=` counts as seeded; tests downgraded) |
| `swallowed_error` | broad `except` whose body is `pass`/`continue` (best-effort cleanup like `kill`/`close` is ignored) |
| `frozen_edit` | files matching a glob in a frozen list that changed vs `--base` |

Tested against 23 real jspace-pilot files (0 false positives) and this repo's python (about 15 findings, see PR).
Not wired into CI as a gate: the findings in `tools/bench` and `challenge` need triage first.
