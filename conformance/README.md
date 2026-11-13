# Conformance

Black-box scenarios that start the built `playbox` and `playbox-mcp` binaries and check what a user or an agent can observe. They
import none of the project's code, so they keep passing across refactors and fail when a promise to a user breaks.

```bash
cargo build -p playbox-cli -p playbox-mcp
python3 conformance/run.py                  # fast tier: seconds, no docker
python3 conformance/run.py --tier full      # also scenarios that need docker
python3 conformance/run.py capabilities     # one scenario
PLAYBOX_BIN=/usr/local/bin python3 conformance/run.py   # installed binaries
```

`CONFORMANCE_REQUIRE_ALL=1` turns a skipped scenario into a failure (CI sets it).

| Scenario | Tier | Promise |
|---|---|---|
| `cli_contract` | fast | version, completions with typed values, `doctor --json` and its exit status, parse errors exit 2, readable errors, quiet pipes |
| `room_basics` | fast | join, say and feed, the host's audit log, revocation refuses the next call |
| `capabilities` | fast | a call runs once however often it is sent, bounds refuse bad arguments, the result hash is in the audit log |
| `conversations` | fast | roster-only access, order, signatures, unread marks, invites |
| `live_delivery` | fast | hello, nothing before the handshake, then only new messages, framed as untrusted |
| `secrets_docker` | full | a task reads a granted secret at `/run/secrets`; an ungranted one is refused; the log has the name, not the value |

To add one: a file in `scenarios/` with `TIER` and `run(ctx)`; raise `AssertionError` to fail and `harness.Skip` when the machine
cannot run it. Use `ctx.mcp(name)` for a sidecar and `ctx.cli(...)` for the command line; both get a private `PLAYBOX_HOME`.

Not yet scenarios: restart of a host with `--state` (message retry across the restart, calls abandoned after a crash), the latent
channel (`steer/tools/e8_room_hardening.py` is the model for it), a libkrun task. Those need a long-lived `playbox up` process.
