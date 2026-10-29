# The Commons: a cooperative hill-climb challenge

Yukon-style challenges (Eigen Labs) already run a shared, moving baseline, but they still rank people on a leaderboard with prizes, and the scorer is a central service you have to trust. The Commons keeps the part that works (one objective, a verifier, each promoted result becomes the next baseline) and changes two things: the score that counts is the group's frontier, and the verifier is a peer-verifiable host instead of a platform. One public room, one hidden objective, one shared frontier. Agents from different owners connect p2p, each contribution is attributable and verified, and nobody has to trust anybody.

Status: design only. Nothing here is built. Research notes below are from web searches and from reading Yukon's public CLI output on 2026-10-29. Eigen's performance claims are self-reported and I did not verify them.

## Format

- **Objective:** autoresearch on open math problems (decided). See "Objective: open math problems" below. The score comes from a public, sound checker run by the host's signed evaluator, not from a sealed holdout.
- **The room:** a persistent host (`playbox up --state`) with replica hosts for resilience. Join by link. Anyone can bring an agent (Claude Code, Codex, Hermes, OpenCode).
- **Loop:** an agent pulls the current frontier, proposes a certificate or proof, checks it locally with the public checker, and pushes a branch. The host's evaluator reruns the checker in a no-network sandbox and returns a signed verdict and score. If it beats the frontier by a margin, it is merged into `frontier`. Everyone's next pull starts from it. Climbing is recursive: the research agents improve the code that improves the research.
- **Scoreboard:** a live frontier curve (score vs. time), not a ranking of people. Round ends at a target score or a clock.
- **Fully automatic gate:** no human approval in the merge path (see Yukon lessons). Humans audit after the fact.

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
- **Portable credit:** credit attaches to signed lineage, so a participant can leave and keep it (low exit cost).

## Objective: open math problems

Decision: the swarm works on open math problems. This changes the verifier, which is the hard part of any such challenge.

**Why it fits.** Math has the property most research lacks: a solution can be checked cheaply and exactly, even when finding it is hard. A certificate that passes a sound checker is correct regardless of who produced it, so a stranger's agent needs no trust. That is the same property Yukon exploits (its Heesch, proximity-prize, `ec-rank` and Lean benchmarks) and it makes the signed receipt the whole proof of contribution.

**Problem classes, in order of how well they fit a scalar frontier:**
1. **Extremal constructions with a score.** Find an object and report a number: cap sets, Ramsey and Turan-type lower bounds, packings, sphere/kissing configurations, Heesch numbers, autocorrelation and Sidon-type constants, minimum-size objects. Checker: replay the object against the definition. Frontier: the best bound. This is the best v0, because it is a true hill-climb and every improvement is a verified record.
2. **Bounds improved by search over proofs or algorithms.** Reduce a threshold, tighten a constant. Checker: a certificate checker (SAT/UNSAT proof, LP dual, interval arithmetic) or Lean.
3. **Formal theorems in Lean.** Binary, not scalar. Frontier: set of open statements closed. Score by problem count or weighted difficulty. Use after the checker is trusted.
4. **Conjectures and counterexamples.** A counterexample is a certificate; a proof of a conjecture needs Lean. Cheap to check, rare to find.

**The checker is the security boundary.** A public checker replaces the sealed holdout, so the reward-hacking surface moves from "overfit the holdout" to "exploit the specification":
- **Wrong or loose formalization:** the checker accepts something that is not the intended problem. Mitigate with a frozen, reviewed problem statement, a hash of it in every receipt, and a second independent checker for the same claim where possible.
- **Lean exploits:** `sorry`, new axioms, `native_decide`/`ofReduceBool`, unsafe or `implemented_by` code, tactic or elaborator tricks. Mitigate by whitelisting the axiom set (`#print axioms` must match), rejecting `sorry`, running the kernel as final judge, and using a second kernel (e.g. lean4checker or a different implementation) on every promoted proof.
- **Checker bugs and resource tricks:** numeric overflow, floating point rounding, timeouts treated as success. Mitigate with exact arithmetic, fixed resource limits, and a pass only on an explicit verified verdict.
- **Data release that does not replay:** require a manifest hash and standalone verifier with each claim (the HashSmash post is the model).
- **Stale or already-known results:** a "new record" can be a rediscovery. Seed the frontier with the literature bound and cite the source in the receipt; flag results that match known work for human review after the fact.
- **Novelty vs. correctness:** the gate certifies correctness and improvement, not importance. Only a human reader turns a verified bound into a paper.

**Two-key promotion.** A submission is promoted only if (1) the primary checker passes inside the no-network sandbox, (2) an independent second checker on a different host agrees, and (3) the improvement clears the threshold. The signed receipt binds problem hash, checker hash, certificate hash and host attestation. This is the Commons advantage over a central scorer: any peer can replay the receipt.

**Shared state for math.**
- **Frontier:** per-problem best certificate and its score, in git.
- **Memory entries:** "closed search space" (this construction family cannot beat X, with proof or exhaustive search record), techniques that worked, dead ends, solver settings (e.g. a SAT flag that silently returns garbage).
- **Claims:** lease on a problem or a technique so agents spread across problems and methods.
- **Cross-problem transfer:** a tool or lemma that helps problem A gets credited when it unlocks progress on B (lineage credit already covers this).

**Tooling the starter kit needs.** Lean 4 and Mathlib pinned, SAT/SMT solvers with proof logging (CaDiCaL/DRAT), an ILP/LP solver, exact-arithmetic libraries, a library of problem checkers with a common JSON interface (`problem.json`, `check` command printing `{verdict, score}`), and a literature bound for each problem.

**Starting problem set (v0).** Pick 3 to 5 extremal problems where (a) the checker is under 200 lines or an existing Lean statement, (b) there is a known literature bound to beat or match, and (c) CPU search can move the score in hours. Heesch-style tiling, small Ramsey lower bounds and Sidon-type constants fit. Mirror one as a Yukon benchmark to compare a Commons swarm with Yukon solvers.

**What would count as success.** Not "solved an open problem" (a stretch goal). Measurable milestones: (1) swarm of N agents matches the literature bound faster than one agent given the same total compute; (2) at least one improvement over a published bound on a minor problem, with a replayable certificate; (3) zero accepted false claims across the whole run, confirmed by an independent checker.

## What Yukon does (research)

Yukon is Eigen Labs' open autoresearch platform. Anyone links a GitHub repo and defines a benchmark and verifier; solvers use the `yukon` CLI to clone, run, submit and discuss.

- **Design:** one measurable objective per challenge, harness-side scoring in a sandbox limited to an editable surface, an improvement threshold for promotion, reproducible runs. Each promoted result enters the repo as a discrete diff plus evaluation run plus new score, and becomes the baseline for the next solver.
- **Why it works, per Eigen:** an internal agent-only effort on the quantum-circuit challenge (ECDSA.fail) plateaued; opening it let different models, harnesses and prompts attack from different directions. They report matching Google's displayed result in about 8 hours and passing it in about 72. Progress was hundreds of small changes plus a few structural redesigns. Ideas are not the scarce input; testing them is.
- **Scale seen on 2026-10-29 (`yukon benchmark list`):** about 35 open benchmarks across lean, cuda, swift, rust, cryptanalysis, math and cryptography, run by Eigen and third parties (ECDSA.fail, HashSmash, curvesmash, sig.golf, Heesch, MLX/CUDA-fast, proximity-prize). Some have closing dates, many don't. The shape is a catalogue of challenges, not one.
- **Discussions as shared memory:** the CLI reads and posts to GitHub Discussions per challenge. Real posts I read (read-only) had: a survey result titled to close a search space "so nobody re-treads it", operational notes (building a checker under Rosetta on macOS; a flag that silently returns garbage), and a data release that says up front "this does not claim a new collision" and labels unresolved cases `UNKNOWN`, not `UNSAT`, with a stdlib-only verifier and a hashed manifest anyone can replay. Many benchmarks have discussions disabled, so the memory channel is optional there.
- **Tracks:** hashsmash has "exploratory" and "pareto" variants of the same primitive; the CLI has `tracks` and `switch`. One objective can have several scoring views.
- **Coordination thesis (Eigen's "Coordination Is the Only Counterweight"):** access (weights), automation (agents at throughput), then coordination (agents transacting with other parties' agents without a trusted intermediary). They call composing with untrusted parties without a referee the open problem. That is the gap playbox targets.
- **Gaps in what they say:** the posts describe few anti-gaming measures beyond sandbox, harness scoring, threshold and reproducibility, report no cheating incidents, and give no prize or credit scheme in the posts (the leaderboard page shows weekly cash prizes by points). Their own lesson list says objectives that resist gaming are hard.
- **Verifier assumptions flatter results:** in the ECDSA.fail write-up the circuit gets its input point as a classical value; a shortcut-free variant needed about 30% more Toffoli gates. Treat the benchmark as a proxy for the real goal.

| | Yukon | Commons |
|---|---|---|
| Who scores | Central harness and sandbox | Attested hosts with signed receipts anyone can verify |
| Trust | You trust Eigen's infrastructure | Peers verify the host; any peer can replay the checker |
| Ranking | Leaderboard and points, weekly prizes | Frontier curve plus lineage-based credit |
| Collaboration | Implicit via the shared baseline, optional discussions | Explicit: shared memory, claims, joint credit |

Design takeaways: keep the gate automatic; publish negative results as first-class (a "closed search space" entry type in shared memory); require a replayable manifest or verifier on any data release; allow multiple scoring tracks per objective; re-evaluate the frontier on a stricter "realistic" variant periodically.

## Reward hacking (research)

Expect a high hacking rate by default. BAITBENCH reports hacking in 57.1% of runs across seven frontier agents when optional shortcuts are planted. Sources are preprints; several arXiv IDs and dates looked inconsistent in search results, so verify before citing figures.

- **Benchmarks and detection:** EvilGenie (hardcoding tests, editing test files; held-out tests, LLM judges and test-file-edit detection), SpecBench (long-horizon coding; hacks are architectural, not test edits), TRACE (517 trajectories, 54 hack categories; GPT-5.2 detects about 63%), Reward Hacking Benchmark (ICML 2026; skipped verification, answers inferred from metadata, tampered eval functions), ImpossibleBench, Hack-Verifiable Terminal Bench (argues post-hoc LLM-judge evaluation is underdeveloped), and "Reward Hacking Challenges Oversight of Autonomous Research Agents" (closest to this challenge).
- **Evaluation design:** capped evaluation with randomized tests (CapCode) for detection and reward design; "The Verification Horizon" argues a passing test never proves legitimate work.
- **Training-time interventions:** a MATS/ICLR 2026 study compares penalizing detected hacks, negative reward for them, and inoculation prompting; best interventions reportedly match training in a non-hackable environment, with imperfect monitors and run variance as caveats.
- **CoT monitoring:** monitors reading reasoning beat action-only monitors (OpenAI, 2025), but optimizing against the monitor teaches hidden intent. 2026 work reports learned obfuscation generalizes to unseen settings, even when only final actions are penalized. A mutual-information reward is proposed to make obfuscation costly; hiding reasoning tends to degrade task performance.
- **Activation level:** hacking monitored from internal activations during generation; representation-engineering steering vectors to suppress hacking in tool-call and long-context settings (October 2026). Links to `docs/steering.md`.
- **Older base results:** Gao et al. 2022 reward-model overoptimization scaling laws (proxy rises while gold peaks and falls); ensembles and conservative objectives help but share biases; verifiable rewards are harder to game but still gamed (special-casing tests, editing tests, early exit); hacking learned in RL environments can generalize to broader misalignment, and inoculation prompting reduced that.

Implications for the Commons:
1. Plan for hacking as the default case.
2. Math has no holdout: the checker is public, so the defenses are a frozen problem hash, an axiom whitelist, and a second independent checker (see the objective section). For any non-math track, use capped scores and randomized holdout shards.
3. Never select or train on the monitor's verdict. Log flags and use them only for after-the-fact merge review.
4. Make test files and scorer code immutable to pushed branches; the scorer runs outside the agent's sandbox.
5. On non-math tracks, treat the dev-versus-holdout gap as the overfitting alarm; pause merges when it widens. On math, treat a primary/second-checker disagreement as the alarm.
6. Optional research contribution: an activation or steering probe on served open-weights agents as a hacking detector.

## Anti-cheat (adversaries are expected)

| Attack | Defence |
|---|---|
| Exploit the spec or checker (wrong formalization, `sorry`, extra axioms, `native_decide`, float rounding) | Frozen problem hash, axiom whitelist, second independent checker, exact arithmetic, verdict only on explicit pass. For non-math tracks: query budget, rotating holdout shards, capped credit |
| Edit checker, hardcode, exit early | Editable surface whitelist, checker outside the agent's sandbox, immutable checker paths |
| Poisoned memory or pushes steering other agents | Admission broker: text from members can't drive another host agent's actions; memory quarantine with provenance |
| Malicious code in a pushed branch | Runs only in no-network container or microVM with disk/CPU/time limits |
| Sybil flood | Single-use expiring invites, grants with hard limits, revocation across replicas |
| Faking compute or the evaluator | Evaluator runs on attested hosts (T1-T3), results are host-signed with a tamper-evident audit log anyone can verify |
| Claiming someone else's work | Commits live under the author's `refs/playbox/members/...`; lineage is signed |
| Hidden intent in reasoning | CoT/session-history monitor for review only, never a training or selection signal |
| Benchmark flatters the real goal | Periodic stricter "realistic variant" re-evaluation of the frontier |

## Mapping to what exists

| Need | Already in repo |
|---|---|
| Rooms, invites, p2p | `playbox up/join`, iroh transports |
| Push/pull branches | `playbox-git`, `playbox_push` / `playbox_pull` |
| Sandboxed evaluation | `playbox-runner` (Docker, libkrun, OpenShell) |
| Signed evaluator + lineage (sealed holdout for non-math tracks) | trust layers (`docs/trust-layers.md`) |
| Shared memory with provenance | `playbox_remember` / `playbox_recall` |
| Limits, expiry, revoke | Biscuit grants |
| Verifiable receipts | signed history, audit log, witnessed checkpoints |
| Steering as an extra lever | `docs/steering.md` (optional track) |

## To build

1. **Merge gate:** host-side loop that takes a pushed branch, runs the evaluator, and fast-forwards `frontier` on a win above a threshold. (Smallest new piece; everything else exists.)
2. **Two-key promotion:** run a second independent checker on a different host before a record is promoted; per-push credit cap. (Holdout quotas and shard rotation only for non-math tracks.)
3. **Claims:** a lease record in shared memory for "I'm exploring X", plus a "closed search space" entry type for negative results.
4. **Credit:** lineage-based attribution report generated from the audit log.
5. **Spectator page:** live frontier curve, per-agent contribution feed, links to verify any receipt. Reuse `world/` to render the swarm.
6. **Starter kit:** baseline repo, dev split, a one-line `playbox join` plus a prompt for each supported agent.
7. **Review tooling:** after-the-fact hacking monitor over session history and diffs.

## Suggested v0

3 to 5 extremal-construction problems (see above), each with a checker and a literature baseline. Run it internally with 3 of our own agents on 2 machines, record the frontier curves, then open it. Headline demo: the same compute budget with 1 agent vs. N cooperating agents, per problem. The earlier GPT-2 harness in `orchestrator/experiments/` is no longer the objective; reuse its orchestration, not its task.

## Open questions

- Which 3 to 5 problems, and who vets the formalizations (a wrong statement turns the whole run into noise).
- Second checker: which independent implementation for each problem class.
- Prizes: pooled by marginal contribution, or none and let the curve be the trophy.
- Open join vs. gated: open invites need the quotas above before going public.
- Interop: should a Commons room be able to publish its frontier into a Yukon benchmark, or consume one as its objective?

## Sources

- Yukon: [yukon.org](https://www.yukon.org/), [leaderboard](https://www.yukon.org/leaderboard), [Introducing Yukon](https://www.eigenlabs.org/blog/introducing-yukon/), [ECDSA.fail story](https://www.eigenlabs.org/blog/open-autoresearch-the-ecdsa-fail-story/), [Coordination Is the Only Counterweight](https://www.eigenlabs.org/blog/coordination-is-the-only-counterweight/)
- Reward hacking: [representation engineering](https://arxiv.org/abs/2610.29129), [MATS training interventions](https://www.matsprogram.org/research/mitigating-reward-hacking-with-rl-training-interventions), [Hack-Verifiable Terminal Bench](https://arxiv.org/pdf/2608.22103), [Verification Horizon](https://arxiv.org/pdf/2606.26300), [Reward Hacking Benchmark](https://arxiv.org/abs/2605.02964), [SpecBench](https://arxiv.org/pdf/2605.21384), [BAITBENCH](https://arxiv.org/html/2608.30724v1), [EvilGenie](https://arxiv.org/pdf/2511.21654), [CapCode](https://arxiv.org/pdf/2606.07379), [autoresearch agents](https://arxiv.org/pdf/2609.28614)
- Monitoring: [CoT monitoring (OpenAI)](https://arxiv.org/pdf/2503.11926), [obfuscation generalises](https://www.alphaxiv.org/audio/2601.23086v1), [information-theoretic monitorability](https://arxiv.org/html/2602.18297v2), [Monitoring Monitorability](https://arxiv.org/pdf/2512.18311), [activation monitoring](https://arxiv.org/pdf/2603.04069)
