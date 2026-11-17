# hermes-parity
`run.py ARM N OUTDIR` runs one Hermes agent on the Tripwire task (`../tripwire/task.py`), arm `L` (terminal.backend local) or `S` (ssh into an OpenShell microVM). `analyze.py` summarises. `results/` holds the raw per-run JSON from the 2026-11-16 run.
Setup for S: a sandbox named `parity-s` from `hermes-agent-nonroot:latest`, `ssh_config.example` and the `bin/ssh`,`bin/scp` wrappers copied to `~/.playbox-hermes/openshell-parity/` (the wrappers force `-F` that config, whose ProxyCommand is `openshell ssh-proxy`).
