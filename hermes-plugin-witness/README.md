# Playbox Witness for Hermes

Companion to the `playbox` plugin. Two jobs, both through public hooks only:

1. **Whole-session record.** Once a session has used any Playbox room tool, every tool call is appended
   to a signed, hash-chained log (`~/.playbox/history/hermes-session.log`, via `playbox record`). Entries hold
   the tool name, status, timing, and SHA-256 plus size of the arguments and result, not their text.
   `playbox verify <log>` checks the chain; any edit breaks it.
2. **Pause on peer-triggered writes.** When a call returns text another member wrote (`feed`, `delegate`
   output, `recall`, a file read from a room workspace), write-capable tools are blocked for the rest of
   that turn. The agent is told to report what the member asked for and ask you. Your next message lifts
   the hold. Read-only tools and room `status/ping/say/verify/pull` stay available.

## Limits (read these)

- Taint is per turn, not per string: a paraphrased instruction in the same turn is still covered, but the
  hold is coarse and may pause harmless writes. It lifts on your next message.
- Hermes hooks fail open. If this plugin crashes while room text is in context it blocks; otherwise it does nothing.
- A tool that is not recognised as read-only is treated as a write.
- Only text that arrives through room tools or room workspace file reads is tracked. Text a member sends
  by another channel is not.
- Needs the `playbox` binary on PATH (see the playbox plugin). Without it the record step is skipped silently;
  blocking still works.

## Disclosure

Runs `playbox record` as a subprocess on each tool call after a room tool is used; writes the log under
`~/.playbox` (or `PLAYBOX_HOME`, `PLAYBOX_LOG`). No network, no telemetry, no credentials read. Argument and
result contents are hashed, never stored.

## Test

`python3 -m unittest discover -s tests`
