# Hermes parity: local terminal vs OpenShell microVM

**Result: no capability loss measured.** On the Tripwire task (6 seeded bugs, 24 hidden tests), Hermes (claude-haiku-4-5, Anthropic OAuth) scored 24/24 in 10 of 10 runs with terminal commands local (arm L) and 10 of 10 with them in an OpenShell microVM (arm S).

| | L (local) | S (OpenShell VM) |
|---|---|---|
| runs / reached 24 | 10 / 10 | 10 / 10 |
| hidden tests, mean | 24.00 | 24.00 |
| cost per run (est., `--usage-file`) | $0.041 | $0.034 |
| tokens per run | 110k | 113k |
| model calls per run | 8.9 | 10.9 |
| wall-clock per run | 33.0 s | 30.2 s |

Totals: L $0.41, S $0.34. No 429 or usage-limit message seen.

## Sandbox overhead
Command round trip through `openshell sandbox exec`: about 0.03 s. Per-run setup (upload repo, git init in VM): 0.3 s. First sandbox create: about 65 s (image export, rootfs); a repeat create with the image cached: 1.1 s. Over SSH, the VM added no measurable wall-clock (S 30 s vs L 33 s; run-to-run noise is larger than any difference).

## Parity statement
Margin: S no more than 2 tests (of 24) below L on average. Observed difference is 0.00 with zero variance, so a bootstrap is degenerate. A cruder, honest bound: 10/10 in each arm gives a 95% lower bound of about 0.69 on the per-run chance of a perfect score (Clopper-Pearson), so a rare failure mode cannot be excluded. What n=10 shows: S is not meaningfully worse on a task this easy. What it cannot show: parity on harder tasks (the task has a ceiling effect; both arms saturate), different models, or rare failures. The harder SWE-bench Flask/Django set was not run.

## How the S arm works
Model call and OAuth credential stay on the host. Hermes' `ssh` terminal backend points at `openshell-parity-s.default`; a wrapper `ssh`/`scp` on PATH adds `-F` a config whose ProxyCommand is `openshell ssh-proxy`. The agent's repo lives in the VM (`/sandbox/pb/...`); I download it and score with the hidden tests in Docker on the host.

## Method notes and caveats
- `terminal.*` in config.yaml overrides `TERMINAL_*` env vars, and `--ignore-user-config` forced local. So each run gets its own `HERMES_HOME` of symlinks to the real one (no dotfiles, so no `.env`) with a rewritten `config.yaml`. `~/.hermes/config.yaml` was never edited; `diff` against the backup is clean.
- The ssh backend syncs `~/.hermes` skills, cache and attachments into the VM. The first attempt synced about 2,200 host files (cached web pages, PDFs). Fixed by giving both arms empty skills/cache/attachments dirs. Consequence: both arms ran without the user's skills.
- Credential check after the runs: no auth.json, .env or credentials file in the VM, `credentials/` empty, no token-like env vars. Not a proof of absence, but the OAuth material was never sent by the backend.
- Model usage: 20 recorded runs plus 5 earlier calls (a first S run that silently ran locally, an S run that fixed the bugs but had no usage file, 3 probes), about 25 of the 30 cap. Start attempts that failed before any model call are not counted.
- The `hermes` on PATH inside the harness subprocess was an older install; the harness uses `~/.local/bin/hermes` explicitly.
- Hermes leaves a daemon per HERMES_HOME; the harness kills them after each run.
- S1 was rescored after Docker Desktop was found stopped; Docker was restarted.
- Cost is Hermes' own estimate from the provider models API, not a bill.

Raw data: `tools/bench/hermes-parity/results/`, and `~/.playbox-hermes/openshell-parity/`.
