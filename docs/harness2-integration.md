# Harness² (google-research/harness2): what to take, what to skip

Reviewed at commit HEAD of a shallow clone, 2026-11-15. Ran `harness2-demo --k 2` (no credentials): 2 tasks, 0 failed, 28 executions, 32 editor calls. Model-backed benchmarks were not run.

## What it is

A harness *optimizer*, not a secure host. Model weights fixed; it edits eight components (system prompt, guardrails, memory, tool guidance, subagents, skills, scripts, plugins) of the harness around an agent. About 6.8k lines of Python under `harness2/core` and `harness2/substrate`, Apache 2.0, research code.

- **Task level, Propose–Probe–Compose.** Per task and budget k: k shared base runs, k general (INV) and k domain (DOM) candidate edits, each probed by re-running the task, then k compositions built from behavioral contrasts between executions, then k finals. Budget identity: 4k rollouts, 3k improvement stages (`core/ledger.py`). No ground truth. A selector then picks one anonymized final (`core/selection.py`).
- **Harnessing level.** A per-domain experience library (`core/library.py`): the full adopted harness tree plus `EXPERIENCE.md` (cue, lesson, evidence) per task, retrieved only within the same domain.
- **Substrate seam** (`substrate/base.py`): OpenCode and Codex executors behind one interface, so a new executor is one adapter.

## What it is not

- No sandbox, no signing, no provenance. README: executor network access unrestricted; Codex runs with its sandbox disabled.
- Not a room or a host. Nothing is shared between agents of different owners; the library is local files.
- Linux-first; needs Node 20+, gcloud/Vertex for real runs. Not worth adopting wholesale.

## How it maps onto playbox

| Harness² piece | Room equivalent | Gap it fills / what rooms add |
|---|---|---|
| Candidate harness edits (INV + DOM) | Branches pushed by members | Rooms already run them sandboxed under signed grants |
| Probe (re-run same task per candidate) | Sandboxed task run | Receipts make each probe verifiable |
| Compose from behavioral contrasts | Missing today | **The real gap.** Rooms promote or reject single candidates; they never merge several into one |
| Selector over anonymized finals | Judge / holdout gate | Rooms have a stronger gate (holdouts, receipts) |
| Experience library (cue, lesson, evidence) | `room_remember` / `room_recall` | Rooms add signing and lineage; Harness² has neither |
| Budget identity 4k/3k | Metered benchmark | Reuse the accounting idea |

## Recommendation

1. Do not vendor or depend on Harness². Different trust model, heavy runtime.
2. Reimplement **Compose** as a room op: given N probed candidates with receipts, an improver session reads their diffs and run evidence and emits one merged harness, which then goes through the existing promotion gate.
3. Store each adopted harness and its lesson as a signed room memory (`cue / lesson / evidence`), same-domain retrieval only.
4. Test it on the existing shared-room vs silos hill-climb benchmark: add a compose arm and compare. Keep the L4 lesson in mind: item noise there was about 1 point, so use multiple seeds.

Open risk: their gains (up to 11.5 points WorkBuddy, 4.5 JobBench) are single-step, on their benchmarks, with their models. Unverified for your tasks.
