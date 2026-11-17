# hermes-playbox-backend

Hermes terminal backend (`terminal.backend: playbox`): each command is a `playbox_delegate` task run by a Playbox host in an OpenShell
microVM. Install by linking `playbox_terminal/` into `~/.hermes/plugins/playbox-terminal` and adding `playbox-terminal` to
`plugins.enabled`. Design, results and gotchas: `docs/hermes-playbox.md`. Harness: `tools/bench/hermes-parity-playbox/`.
