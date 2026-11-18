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
| `join_time` | fast | link to a confirmed round trip within the join budget (`PLAYBOX_JOIN_BUDGET_SECS`, default 180); a `PLAYBOX_TOOLS=core` session lists at most 7 tools |
| `join_consent` | fast | a guest previews what a host is without spending the invite |
| `join_cli` | fast | `playbox join` refuses a host below `--min-host-tier`, leaves the invite unspent, and joins with it afterwards |
| `capabilities` | fast | a call runs once however often it is sent, bounds refuse bad arguments, the result hash is in the audit log |
| `conversations` | fast | roster-only access, order, signatures, unread marks, invites |
| `live_delivery` | fast | hello, nothing before the handshake, then only new messages, framed as untrusted |
| `restarts` | fast | a host killed and restarted keeps its room, does not re-run finished calls, and abandons in-flight calls |
| `version_skew` | fast | the current build and the previous release work together in one room in both directions (`PLAYBOX_OLD_BIN`) |
| `installed_upgrade` | full | an older release installed via `install.sh` upgrades in place and preserves room, member and audit chain |
| `secrets_docker` | full | a task reads a granted secret at `/run/secrets`; an ungranted one is refused; the log has the name, not the value |

To add one: a file in `scenarios/` with `TIER` and `run(ctx)`; raise `AssertionError` to fail and `harness.Skip` when the machine
cannot run it. Use `ctx.mcp(name)` for a sidecar and `ctx.cli(...)` for the command line; both get a private `PLAYBOX_HOME`.

Not yet scenarios: the latent channel (`steer/tools/e8_room_hardening.py` is the model for it) and a libkrun task.
