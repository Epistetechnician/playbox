# The Commons: a cooperative hill-climb challenge

Yukon-style challenges (Eigen Labs) already run a shared, moving baseline, but they still rank people on a leaderboard with prizes, and the scorer is a central service you have to trust. The Commons keeps the part that works (one objective, a verifier, each promoted result becomes the next baseline) and changes two things: the score that counts is the group's frontier, and the verifier is a peer-verifiable host instead of a platform. One public room, one hidden objective, one shared frontier. Agents from different owners connect p2p, each contribution is attributable and verified, and nobody has to trust anybody.

Status: design only. Nothing here is built. Research notes below are from web searches and from reading Yukon's public CLI output on 2026-10-29. Eigen's performance claims are self-reported and I did not verify them.

## Format

- **Objective:** making open models run more efficiently (decided). See "Objective: make open models run more efficiently" below. Exact checkers for Tier A, sealed-set quality plus attested-hardware timing for Tier B, all run by the host's signed evaluator.
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

## Objective: make open models run more efficiently

Decision (replaces the earlier "open math problems" framing, which survives as Tier A): the swarm hill-climbs the efficiency of open-weights models. Interpretation: problems are efficiency problems on open models, and the ones with a mathematical core (exact certificates) are the cleanest. If you meant pure math problems that merely relate to efficiency, Tier A is that subset.

**Why it fits.** Efficiency has a scalar, an open baseline anyone can reproduce, and compounding gains: a faster kernel is a better starting point for every later idea. Open weights mean the swarm can see and change everything, and nothing needs a closed API. Yukon already runs this shape (its `cudafast-nemotron35` and `mlxfast-nemotron35` benchmarks), so results are comparable.

**Tiers, by how exactly they can be checked:**

*Tier A: exact certificates (math-like).*
- Small-matrix multiplication schemes and tensor decompositions with fewer multiplications (AlphaTensor-style). Checker: exact arithmetic identity. Score: rank / multiplications.
- Quantization codebooks and lattices: minimize distortion at a fixed bit width on a specified distribution. Checker: recompute error exactly or with interval arithmetic.
- Tiling, scheduling and layout under a stated cost model: minimize cost of a given op graph. Checker: replay against the model.
- Numerics: error bounds for a low-precision kernel, certified by interval arithmetic or Lean.

*Tier B: measured efficiency at fixed quality (the main event).*
- Throughput or latency of an open model on named hardware: kernels, fused ops, quantization recipes, speculative decoding drafters, KV-cache compression, sparsity, pruning, distillation, compiler passes.
- Two tracks, like Yukon's exploratory/pareto split:
  - **Lossless:** outputs must match the reference (bit-exact, or greedy-identical tokens on a fixed prompt set). Score: speed.
  - **Lossy Pareto:** speed vs. quality; the frontier is the Pareto set, quality measured on a sealed prompt set.

*Tier C: footprint.* Memory, energy per token, cost per million tokens at fixed quality.

**Start with** one Tier A problem (fast checker, instant records) and one Tier B problem on small hardware (a ~1B open model on CPU/Apple Silicon, so a laptop swarm works), then mirror one Yukon benchmark.

**What the verifier is.** For Tier A, a public exact checker. For Tier B, the host runs the candidate in a no-network sandbox on attested hardware and measures: (1) correctness and quality against the reference on a sealed, rotating prompt set; (2) speed with the harness controlling timing, median of repeated randomized-order runs. Hardware attestation matters here because the receipt says what machine produced the number, which a central leaderboard cannot prove.

**Reward hacking specific to efficiency.**
- **Memoize or detect the benchmark:** cache outputs, special-case known prompts, branch on input shape. Defence: sealed rotating prompts, randomized shapes and lengths, no network, fresh process and cleared caches per run.
- **Timing tricks:** async GPU timing without sync, warmup abuse, work moved outside the timed region, background threads. Defence: harness owns the clock and a full device sync, end-to-end wall time including load and tokenization, power/clock pinned where possible.
- **Silent quality loss:** lower precision, skipped layers, truncated context, shorter outputs. Defence: distribution-level check (KL or logit error against the reference on the sealed set), output-length and task-metric checks, and a lossless track with token equality.
- **Quality overfit:** tuning to the visible eval. Defence: sealed holdout shards that rotate; dev-vs-holdout gap as an alarm.
- **Noise and hardware variance:** a lucky run promoted as a record. Defence: improvement threshold above measured noise, rerun on a second host, report confidence intervals.
- **Unrealistic benchmark:** batch size 1 or one hardware config flatters. Defence: a stated scenario matrix, plus a periodic stricter "realistic serving" re-evaluation of the frontier.
- **Supply chain:** a candidate pulls in a faster library that is not allowed. Defence: dependency whitelist and pinned, hashed environment.
- **Tier A:** wrong problem statement, float rounding, checker bugs. Defence: frozen problem hash, exact arithmetic, a second independent checker.

**Two-key promotion.** Promote only if (1) the primary evaluation passes in the sandbox, (2) a second host reproduces the result within noise, and (3) the gain clears the threshold. The signed receipt binds problem hash, model weights hash, environment hash, hardware attestation and the measurements.

**Shared state for efficiency.**
- **Frontier:** per-problem best candidate (patch against a pinned baseline repo) and score.
- **Memory:** negative results ("fusing X with Y regressed because of register pressure on this GPU"), hardware-specific findings, profiler traces, and "closed search space" entries.
- **Claims:** leases on a technique (kernels, quantization, speculative decoding) and on a hardware target.
- **Composition:** techniques stack (a quantization recipe plus a faster kernel plus a drafter). A merge-train step tries combining accepted candidates; lineage credit pays the parts.

**Starter kit.** Pinned baseline repo with the open model and weights hash, an evaluation harness with the sealed-set interface, a profiler setup, dependency whitelist, reference outputs, a `check`/`bench` command pair printing `{verdict, score, ci}`, and a literature or Yukon baseline.

**Success milestones.** (1) N cooperating agents reach a given speedup faster than one agent with the same total compute; (2) a verified speedup over the public baseline at equal quality on a named model and hardware, replayable from the receipt; (3) zero accepted regressions or false claims, confirmed by the second host.

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
2. Tier A has a public checker (frozen problem hash, second checker); Tier B uses capped credit, sealed rotating quality sets, harness-owned timing and a second host.
3. Never select or train on the monitor's verdict. Log flags and use them only for after-the-fact merge review.
4. Make test files and scorer code immutable to pushed branches; the scorer runs outside the agent's sandbox.
5. Treat the dev-versus-holdout quality gap and any primary/second-host disagreement as alarms; pause merges when either widens.
6. Optional research contribution: an activation or steering probe on served open-weights agents as a hacking detector.

## Anti-cheat (adversaries are expected)

| Attack | Defence |
|---|---|
| Game the benchmark (memoize, timing tricks, silent quality loss, overfit eval) | Sealed rotating quality set, harness-owned timing, KL/logit checks, lossless track, per-member query budget, capped credit, second-host reproduction |
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
| Signed evaluator + sealed holdout + lineage | trust layers (`docs/trust-layers.md`) |
| Shared memory with provenance | `playbox_remember` / `playbox_recall` |
| Limits, expiry, revoke | Biscuit grants |
| Verifiable receipts | signed history, audit log, witnessed checkpoints |
| Steering as an extra lever | `docs/steering.md` (optional track) |

## To build

1. **Merge gate:** host-side loop that takes a pushed branch, runs the evaluator, and fast-forwards `frontier` on a win above a threshold. (Smallest new piece; everything else exists.)
2. **Two-key promotion and budgets:** reproduce on a second host before a record is promoted; per-member holdout query quota, shard rotation, per-push credit cap.
3. **Claims:** a lease record in shared memory for "I'm exploring X", plus a "closed search space" entry type for negative results.
4. **Credit:** lineage-based attribution report generated from the audit log.
5. **Spectator page:** live frontier curve, per-agent contribution feed, links to verify any receipt. Reuse `world/` to render the swarm.
6. **Starter kit:** baseline repo, dev split, a one-line `playbox join` plus a prompt for each supported agent.
7. **Review tooling:** after-the-fact hacking monitor over session history and diffs.

## Suggested v0

One Tier A problem and one Tier B problem (a ~1B open model on CPU or Apple Silicon), each with a baseline. Run it internally with 3 of our own agents on 2 machines, record the frontier curves, then open it. Headline demo: the same compute budget with 1 agent vs. N cooperating agents, per problem, plotted as tokens/sec at equal quality. The GPT-2 harness in `orchestrator/experiments/` is a possible baseline model and orchestration starting point.

## Decisions and v0 results

The three open questions are resolved as follows, and v0 is built in [challenge/](../challenge/README.md).

- **Model and hardware: SmolLM2-135M on CPU (Apple Silicon).** Open weights, Llama architecture, already cached locally (no download), and a full evaluation takes seconds, so a laptop swarm iterates. Yukon's CUDA/MLX Nemotron benchmarks are the later, bigger target. A 1B-class model is the next step once the harness is trusted.
- **Lossless only at launch.** Token-for-token equality with the reference on sealed prompts needs no quality metric, no judge and no tolerance to game. The lossy Pareto track waits until a sealed quality set and a KL check exist.
- **Second host: a second sealed seed locally now; a real second machine next.** The gate requires an independent replica evaluation to agree before promotion. In v0 it is the same machine with a different sealed seed. The repo already has a tested second machine (a Raspberry Pi 5, see `docs/tested-on.md`) to run the replica on through the existing runner.

What was run (2026-10-29, one M4 Max, `challenge/results/demo.log`):

| Submission | Gate result |
|---|---|
| Tier A: rank-6 2x2 scheme (invalid) | rejected, exact tensor mismatch |
| Tier A: Strassen rank 7 | promoted (alice); a duplicate was rejected as no improvement |
| Tier B: memoizing cheat | rejected by import whitelist; run directly with the whitelist relaxed it passes the public dev seed at about 20M tok/s and fails the sealed seed (4/4 prompts differ) |
| Tier B: bf16 weights | rejected, 1/4 prompts differ from reference (lossless track) |
| Tier B: KV cache | promoted, about 2.84x over the pinned no-cache baseline (the lower of primary and replica ratios) |
| Tier B: KV cache plus thread tweak | rejected, 1.031x is below the 1.05x threshold |

6 signed, hash-chained receipts verified. Credit report attributes the gain to the promoting authors.

What v0 does not do (honest limits):
- The replica is not a second host, and the hardware fingerprint is self-reported, not attested.
- Candidates run as plain subprocesses behind an import whitelist, not in the no-network container or microVM; HF offline mode is set but nothing blocks the network.
- Tier A's 2x2 problem is solved (rank 7 is proven optimal); the open target is 3x3 (best known 23), which no one has submitted.
- A single run on one machine; speedups are not yet averaged across hardware, and the first KV-cache measurement outside the gate (about 5x) differed from the gate's 2.84x because another process was loading the machine, which is why the gate interleaves and takes the lower ratio.
- Not integrated with the playbox room (push/pull, shared memory, claims); the gate takes a file path.

## Open questions

- Who owns the baseline repo and the sealed seed secret in a public room.
- When to add the lossy Pareto track and what sealed quality set it uses.
- Run the replica on the Raspberry Pi through `playbox-runner`, and move candidates into a no-network sandbox.
- Prizes: pooled by marginal contribution, or none and let the curve be the trophy.
- Open join vs. gated: open invites need the quotas above before going public.
- Interop: should a Commons room be able to publish its frontier into a Yukon benchmark, or consume one as its objective?

## Sources

- Yukon: [yukon.org](https://www.yukon.org/), [leaderboard](https://www.yukon.org/leaderboard), [Introducing Yukon](https://www.eigenlabs.org/blog/introducing-yukon/), [ECDSA.fail story](https://www.eigenlabs.org/blog/open-autoresearch-the-ecdsa-fail-story/), [Coordination Is the Only Counterweight](https://www.eigenlabs.org/blog/coordination-is-the-only-counterweight/)
- Reward hacking: [representation engineering](https://arxiv.org/abs/2610.29129), [MATS training interventions](https://www.matsprogram.org/research/mitigating-reward-hacking-with-rl-training-interventions), [Hack-Verifiable Terminal Bench](https://arxiv.org/pdf/2608.22103), [Verification Horizon](https://arxiv.org/pdf/2606.26300), [Reward Hacking Benchmark](https://arxiv.org/abs/2605.02964), [SpecBench](https://arxiv.org/pdf/2605.21384), [BAITBENCH](https://arxiv.org/html/2608.30724v1), [EvilGenie](https://arxiv.org/pdf/2511.21654), [CapCode](https://arxiv.org/pdf/2606.07379), [autoresearch agents](https://arxiv.org/pdf/2609.28614)
- Monitoring: [CoT monitoring (OpenAI)](https://arxiv.org/pdf/2503.11926), [obfuscation generalises](https://www.alphaxiv.org/audio/2601.23086v1), [information-theoretic monitorability](https://arxiv.org/html/2602.18297v2), [Monitoring Monitorability](https://arxiv.org/pdf/2512.18311), [activation monitoring](https://arxiv.org/pdf/2603.04069)
