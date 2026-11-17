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

## Second task set: SWE-bench (flask-4045, django-12700)
5 runs per arm per instance (20 runs, same model and harness shape). Scored by our own harness (`swe/score.py`): the agent's source patch plus the hidden `test_patch`; all FAIL_TO_PASS and PASS_TO_PASS tests must pass. Validated: the gold patch resolves both instances and the unfixed tree fails both.

| Instance | Arm | Resolved | Cost/run | Calls/run | Wall/run |
|---|---|---|---|---|---|
| django-12700 | L | 5/5 | $0.172 | 25 | 136 s |
| django-12700 | S | 5/5 | $0.170 | 30 | 158 s |
| flask-4045 | L | 0/5 | $0.303 | 46 | 140 s |
| flask-4045 | S | 0/5 | $0.213 | 38 | 106 s |

Read: **again no difference, but neither instance can separate the arms.** django is at the ceiling (all resolved), flask at the floor (0/10: Haiku adds an `assert` where the tests expect a `ValueError`, and misses the endpoint checks; the issue text doesn't say which). Solving, cost and time look alike within noise. That is evidence of no large capability loss on these tasks, not measured equality near the model's threshold. A middle-difficulty instance set would be needed for that.

Caveats for this set:
- The official SWE-bench images are linux/amd64 and run as root; this Mac and OpenShell are arm64 and refuse root images. So I built arm64 images (`swe/images/`): repo at the base commit plus pinned dependencies (Python 3.9 for flask, 3.8 for django instead of 3.6). Both arms use the same repo and pins (L natively on the host, S in the VM). Results are not comparable to published SWE-bench numbers.
- OpenShell's Landlock policy lets the sandbox user read only `/usr /bin /lib /etc` and write only `/tmp` and `/sandbox`; `/opt` is unreadable, so the toolchain lives in `/usr/local`.
- Issue text and tests come from the public `princeton-nlp/SWE-bench` dataset. django PASS_TO_PASS is the 66 of 77 tests my log parser sees passing under the gold patch.
- Model usage: 20 more runs, about 45 in total, beyond the original 30-run cap with the owner's explicit approval. No 429 or usage-limit message.

Raw data: `tools/bench/hermes-parity/swe-results/`.

## Third task set: mid-difficulty Django (11848, 11422, 11905)
The first SWE-bench pair sat at ceiling and floor. I calibrated 5 small Django 3.x SWE-bench Lite instances with 2 local runs each, dropped the two that were always solved (11001, 11620) and kept three that varied. Then 5 runs per arm per instance, same arm64 image as above.

| Instance | Arm | Resolved | Target tests fixed | Cost/run | Calls/run | Wall/run |
|---|---|---|---|---|---|---|
| django-11848 | L | 4/5 | 9/10 | $0.115 | 20 | 95 s |
| django-11848 | S | 5/5 | 10/10 | $0.115 | 21 | 91 s |
| django-11422 | L | 4/5 | 4/5 | $0.198 | 31 | 143 s |
| django-11422 | S | 3/5 | 3/5 | $0.255 | 41 | 176 s |
| django-11905 | L | 0/5 | 5/10 | $0.341 | 62 | 190 s |
| django-11905 | S | 0/5 | 5/10 | $0.306 | 56 | 151 s |
| **all** | **L** | **8/15** | | **$0.218** | | **143 s** |
| **all** | **S** | **8/15** | | **$0.225** | | **139 s** |

**Result: S and L resolve at the same rate (53% vs 53%) on tasks near Haiku's threshold.** Per-instance rate difference (S minus L), averaged over instances: 0.00, bootstrap 95% CI [-0.20, +0.20]. So at n=15 per arm the data rule out a drop of more than about 20 percentage points in resolve rate, not smaller ones; the 2-test margin used for Tripwire does not translate here. The per-instance differences (+1, -1, 0) go in both directions, which is what noise looks like. Cost and time are the same within noise.

Honest gaps:
- **One Anthropic 429 mid-batch.** The first attempt at django-11905 S5 hit "usage limit resets in ~93h" after 3 retries and was discarded. It was rerun later on a second pooled OAuth credential (label `anthropic-oauth-3`, added by the owner), so every cell now has 5 valid runs. All other runs used the original credential, so the two credentials are not separable in the data.
- 15 L / 15 S valid runs; invalid runs (8 S runs that failed before any model call because the shared OpenShell gateway dropped or a stale Hermes daemon was still up, 1 L run with a gateway disconnect, and the 429 run) are excluded and not included in `swe-results2/`. They never reached the model, so excluding them cannot favour either arm. The one exception is the 429 run, which also never completed.
- 5 runs per cell is thin; one instance flips by a single run.
- Calibration runs (2 per instance, local only) were used to pick instances, so L is not blind to the selection. Only the post-selection runs are in the table, except L2 of the three kept instances, which were calibration runs and also counted. This could slightly favour L's resolve rate by selection but is small.
- Model usage: this set used about 38 Hermes runs (11 calibration, 12 more local, 15 sandbox including the 429 one), within the 60 you approved, for about 83 across the whole investigation. The 429 is the account's own usage limit, not the run cap; it resets in about 93 hours.
