# Hermes terminal commands through a Playbox room

`hermes-playbox-backend/playbox_terminal/` is a Hermes terminal-backend plugin (`terminal.backend: playbox`). Every command Hermes runs
becomes a `playbox_delegate` task. The Playbox host runs it in an OpenShell microVM under the member's signed, expiring grant and
records it in the host audit log. The model call and the Anthropic OAuth credential stay in the Hermes process on the host; only
command text goes into the room and only command output comes back.

## Run it

```
export PLAYBOX_ALLOW_MISSING_LIMITS=1                      # OpenShell has no CPU/memory limits; preflight refuses without this
playbox up --local --console --state S --repo R --runtime openshell --allow-image hermes-agent-nonroot:latest --grant-ttl 43200
# console: `link` prints a one-time join link; put it in a file
PLAYBOX_BIN=... PLAYBOX_HOME=<member home> PLAYBOX_LINK_FILE=<file> PLAYBOX_IMAGE=hermes-agent-nonroot:latest \
  TERMINAL_ENV=playbox hermes ...                           # plugin enabled via plugins.enabled: [playbox-terminal]
```

The benchmark driver is `tools/bench/hermes-parity-playbox/run.py` (arms L and P; one private `HERMES_HOME` per arm, copied from
`~/.hermes/config.yaml` and edited only in the copy, so the real config is never touched). The image needs python3 and git
(`hermes-agent-nonroot:latest` has both). Tasks get no network: the OpenShell runner refuses network unless the grant lists hosts.

## How state persists

Each delegate runs against a fresh checkout of `base` in a fresh sandbox and returns a patch. The backend keeps the list of **edit
commands** (a command is an edit when the returned diff differs from the previous one) with the directory each ran in, and replays
them before every new command. It never pastes returned patches or output into a command, so the Playbox taint check only has
something to object to if the *model* copies task output into a command it writes. Costs of this choice: replay grows with the
number of edits, exported variables and background processes do not survive, and non-idempotent commands run again. The final tree
is the last returned patch (the MCP truncates it at 20 kB; the driver falls back to replaying the edits on the host).

## Things that bit

* The OpenShell sandbox forbids redirecting to `/dev/null`: a command containing `>/dev/null` fails before it runs. The backend
  uses `/tmp` files instead.
* Hermes creates several environments (terminal, file tools); a join link is single-use, so they share one joined member.
* `hermes -z` uses a per-home gateway daemon that outlives the run and keeps its own environment and plugin code; the driver stops
  the daemon of its own home between runs (by PID).
* Hermes's own Playbox MCP tools lure the model into creating rooms; both arms run with the `playbox` and `playbox-witness` plugins
  and MCP server removed from the config copy.
* Each command is a microVM start (~6.5 s warm). Concurrent file-tool calls queue behind one another.

## Results

Task: `tools/bench/tripwire/task.py` (package `mini`, six seeded bugs, 24 hidden tests; unfixed scores 9). One Hermes agent
(claude-haiku-4-5, Anthropic OAuth, toolsets `terminal,file`, `--yolo`) fixes all six per run, given only the repo and the visible-test
command. Arms run one at a time, alternating, 7 runs each (the 30-run cap was shared with failed starts while the backend was being
debugged). Raw data: `tools/bench/hermes-parity-playbox/results.jsonl`.

| | L (local terminal) | P (via Playbox room, OpenShell microVM) |
|---|---|---|
| runs completed | 7/7 | 7/7 |
| hidden tests passed (mean of 24) | 24.0 | 24.0 |
| runs reaching 24 | 7 | 7 |
| tokens per run (mean) | 119k | 194k |
| model calls per run | 12.3 | 18.9 |
| cost per run (Hermes estimate) | $0.043 | $0.054 (+26%) |
| wall-clock per run | 36 s (35–39) | 478 s (244–580), about 13x |
| per-command overhead | ~0 | 6.4 s per delegate (every command is a fresh microVM) |

Two earlier smoke runs (L-2, P-9, same code apart from timing fields) also scored 24/24.

Playbox side, 466 delegated tasks over the 7 P runs: 0 refusals, every command in the host audit log (audit task lines equal
delegates in each run), and `playbox audit` verified the chain in 7/7 runs. The host was registered with the hub for the duration of
each run and removed afterwards.

**Taint check.** It fired, in advisory mode (the backend runs with `PLAYBOX_APPROVERS` unset, so it warns instead of refusing):
9 to 62 delegates per run (309 of 466 in total) carried text the guard traced to an earlier task's output. The backend never
pastes room text; the cause is the model writing edit commands that quote code it had just read, and every such edit is replayed
in all later commands, so one copy fires the check again and again. With approvals required, those commands would have been
refused until a person issued a capability. A real deployment should not replay: it should keep the workspace on the sandbox side;
the warm sandbox below keeps the sandbox, not the workspace, so replay remains.

**Parity.** Held on this task: mean 24.0 vs 24.0, difference 0 tests, inside a 2-test margin. Confidence is modest: 7/7 in each arm
(8/8 with the smoke runs) only rules out a per-run failure rate above about 35% at 95% confidence, so this shows no sign of a
quality loss, not a proof of non-inferiority. What the Playbox layer added: a signed, expiring per-member grant, a host audit log
that verifies, a microVM boundary around every command (no network, no credentials), at the price of about 13x wall-clock, about
60% more tokens (more calls because a slow, state-replaying tool changes how the model works, plus longer commands) and +26% cost.
Per-command microVM start was the dominant cost; the warm sandbox below removes most of it.

**Warm sandbox.** `playbox_delegate` takes `warm: true`. On a host running the `openshell` runtime, the host then keeps one sandbox
per member (keyed by the authenticated sender, never a peer-supplied name) running between that member's commands. Each command
still gets a fresh checkout of `base` and the workspace is replaced every time (the runner empties `/sandbox` and `/tmp` first), so
file state matches a fresh sandbox; what is saved is the sandbox start. Limits: a different image, resources or network policy gets a
new sandbox; one idle 120 s or beyond the 8-sandbox cap is deleted; a member revoked mid-task loses theirs; a timeout or error
deletes it; stale ones left by a host that died are deleted when the next host starts. Processes a command leaves running survive
into the member's next command (only the same member's), which a fresh sandbox would not do. The backend asks for it by default
(`PLAYBOX_WARM=0` turns it off).

Three runs with it (tag `warm`, same task and image): 24/24 each; wall-clock 86, 96 and 85 s against 478 s without it
(about 5x faster, roughly 2.4x the local arm's 36 s); delegates 69, 43 and 2; audit verified 3/3. Cost per run (Hermes estimate)
$0.073, $0.077 and $0.055, against $0.054 for the earlier P runs; with so few runs and a model that varies its approach, that
difference is not meaningful. Commands now run fast enough to hit the host's default limit of 30 tasks per member per minute: runs 1
and 2 saw 35 and 9 refusals from it (the model retried and still finished). The backend now waits out that refusal (up to 90 s)
instead of showing it to the model; that change is covered by unit tests but was not re-run live, because the 30-run Hermes budget for
these experiments was used up. Replaying edit commands is unchanged, so the taint advisories remain (5, 12 and 2 in these runs).

**Keep mode.** `warm` plus `keep` leaves the member's files in the sandbox between commands, so the
backend no longer re-sends earlier edit commands (the cause of the repeated taint advisories). Each command first checks a marker in
the sandbox; if the sandbox was dropped (idle 15 min, timeout, error), the command stops with a marker and the backend replays its edit
list once on a fresh checkout. `PLAYBOX_KEEP=0` goes back to replay. Tested: unit tests for the backend and live OpenShell tests for the
runner. Three live Hermes runs (tag `keep`): 24/24 each, 126 to 182 s, 57 to 79 delegates, 0 refusals, audit verified 3/3. Taint advisories
were 11, 10 and 13, so they did not go away: the model itself copies text it read into later commands, replay was not the only cause.

**Caveats.** One smoke run (P-9) reported a post-write verification mismatch that still ended at 24/24; concurrent file-tool
calls queue behind a single delegate (queue time is logged as `queued_s`, per-command time excludes it). The `hermes` shim at
`~/.local/bin/hermes` was broken mid-session by a sibling run, so the driver calls the Hermes Python module directly.
