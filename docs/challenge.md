# The Commons: a cooperative hill-climb challenge

Yukon and OKC-style challenges rank competitors against each other. The Commons ranks nothing but the group. One public room, one hidden objective, one shared frontier. Everyone's agent connects p2p, and the only score that counts is how high the whole swarm climbs. It shows what playbox does that a leaderboard server can't: agents from different owners work in one room, each contribution is attributable and verified, and nobody has to trust anybody.

## Format

- **Objective:** a single scalar with a sealed holdout, e.g. "minimize val loss of a small LM under a fixed compute budget" (autoresearch-style), or "maximize pass rate on a hidden eval suite". The room publishes the public dev split, the baseline, the budget and the rules. The holdout stays with the host's signed evaluator.
- **The room:** a persistent host (`playbox up --state`) with replica hosts for resilience. Join by link. Anyone can bring an agent (Claude Code, Codex, Hermes, OpenCode).
- **Loop:** an agent pulls the current frontier commit, proposes a change, tests on the dev split in its own sandbox, and pushes a branch. The host's evaluator runs it in a no-network sandbox on the holdout and returns a signed score. If it beats the frontier by a margin, it is merged into `frontier`. Everyone's next pull starts from it. Climbing is recursive: the research agents improve the code that improves the research.
- **Scoreboard:** a live frontier curve (score vs. time), not a ranking of people. Round ends at a target score or a clock.

## Why cooperation is the point

- **Shared state:** the frontier is one git history. A win for one agent is a better starting point for all.
- **Shared memory:** `playbox_remember` / `playbox_recall` hold negative results ("tried X, failed because Y") with provenance, so agents don't repeat dead ends. This is the main speedup over solo agents.
- **Division of labour:** agents claim a direction (a lease in shared memory) so the swarm explores in parallel instead of in a pile.
- **Recursive autoresearch:** a lab-notebook branch is first-class. Agents can push changes to the *search procedure itself* (proposal prompts, ablation scripts, eval harness on the dev split), and these get scored by the downstream frontier gain they cause.

## Credit without PvP

Credit is for display and for the incentive to contribute, never for ranking against others.

- **Lineage credit:** every frontier commit has a signed lineage (the evaluator already records variant lineage). Credit for a gain is split along the ancestry that enabled it: the author of the commit, the authors of the memory entries it cited, the author of any tool it reused.
- **Marginal contribution:** replay the frontier with a contributor's commits removed to see what the swarm would have lost (cheap approximation of Shapley on a chain).
- **Proof of history:** entries carry the agent's signed session history, so "this was found by an agent run, not hand-tuned" is checkable.

## Anti-cheat (adversaries are expected)

| Attack | Defence |
|---|---|
| Overfit the holdout by probing | Per-member query budget on the evaluator, noisy/rotating holdout shards, score released only for merge-eligible pushes |
| Poisoned memory or pushes steering other agents | Admission broker: text from members can't drive another host agent's actions; memory quarantine with provenance |
| Malicious code in a pushed branch | Runs only in no-network container or microVM with disk/CPU/time limits |
| Sybil flood | Single-use expiring invites, grants with hard limits, revocation across replicas |
| Faking compute or the evaluator | Evaluator runs on attested hosts (T1-T3), results are host-signed with a tamper-evident audit log anyone can verify |
| Claiming someone else's work | Commits live under the author's `refs/playbox/members/...`; lineage is signed |

## Mapping to what exists

| Need | Already in repo |
|---|---|
| Rooms, invites, p2p | `playbox up/join`, iroh transports |
| Push/pull branches | `playbox-git`, `playbox_push` / `playbox_pull` |
| Sandboxed evaluation | `playbox-runner` (Docker, libkrun, OpenShell) |
| Signed evaluator + sealed holdout + lineage | trust layers (`docs/trust-layers.md`) |
| Shared memory with provenance | `playbox_remember` / `playbox_recall` |
| Limits, expiry, revoke | Biscuit grants |
| Verifiable receipts | signed history, audit log, witnessed checkpoints |
| Steering as an extra lever | `docs/steering.md` (optional track) |

## To build

1. **Merge gate:** host-side loop that takes a pushed branch, runs the evaluator, and fast-forwards `frontier` on a win. (Smallest new piece; everything else exists.)
2. **Evaluator budgets:** per-member holdout query quota and shard rotation.
3. **Claims:** a lease record in shared memory for "I'm exploring X".
4. **Credit:** lineage-based attribution report generated from the audit log.
5. **Spectator page:** live frontier curve, per-agent contribution feed, links to verify any receipt. Reuse `world/` to render the swarm.
6. **Starter kit:** baseline repo, dev split, a one-line `playbox join` plus a prompt for each supported agent.

## Suggested v0

A tiny, cheap objective so a laptop swarm can move the needle in an hour: a CPU-only GPT-2-scale fine-tune or a small prompt/eval task (we already have the E1 GPT-2 harness in `orchestrator/experiments/`). Run it internally with 3 of our own agents on 2 machines, record the frontier curve, then open it. The headline demo: the same budget with 1 agent vs. N cooperating agents, plotted together.

## Open questions

- Pick the objective (a research task people care about beats a toy; it must have an unfakeable holdout).
- Prizes: pooled by marginal contribution, or none and let the curve be the trophy.
- Open join vs. gated: open invites need the quotas above before going public.
