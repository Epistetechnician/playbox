# Benchmark: does sharing a Playbox room help agents hill-climb?

Three small agents (Haiku 5.5) try to find a hidden vector of integers. The score is the squared distance to the target, so **lower is better and 0 is perfect**. Each agent scores candidates in its own Docker container.

Two conditions, identical in every way except what the agents can see:

- **room**: the three agents join one Playbox room and see every try the others post.
- **silo**: the same three agents with the same code, prompt and budget, but each sees only its own tries.

Trials alternate room, silo, room, silo, so drift in API speed hits both. Each trial gives every agent 30 tries (90 in all) in rows 3 and 4, and 40 tries (120 in all) in rows 1 and 2. Agents run through the real room: `playbox up --local` hosts it and `tools/playbox_watch.py` runs each agent.

**Short version.**
- Sharing a room clearly helps when the work is split between agents and each agent holds one piece: the room is the only condition that ends with one usable combined answer (posted-vector result, p = 0.0002).
- It does not clearly help when every agent can already score everything.
- **Per dollar, many cheap tries beat an expensive advisor.** Plain Haiku with about $0.79 of tries solved every slice exactly in all 4 trials. The Fable advisor, at about $1.18 a trial, got 22k on average.
- **Sharing helps a cheap swarm a lot.** With the same Haiku-only agents and about $0.53 of tries, the room reached a mean of 599 after 870 tries against 29,466 for silos (p = 0.029), and every room ended with a near-perfect combined vector (0 to 1,941), where silo agents' own vectors scored about 108,000.
- A classical random hill-climber with no model beats the model runs on the plain task.
- These are small experiments with 3 to 8 trials per condition; read the p-values, not just the means.

## What was run

| # | Task | Trials per condition | Room | Silo | Classical (no LLM) | Room vs silo |
|---|---|---|---|---|---|---|
| 1 | 16 numbers, every agent scores all | 3 | 51,752 | 61,796 | 9,083 | 16% lower, p = 0.10 |
| 2 | 32 numbers, every agent scores all | 8 | 131,847 | 136,204 | 54,146 | 3% lower, p = 0.26 |
| 3 | 32 numbers, split in 3 slices (assembled score) | 6 | 99,609 | 112,941 | not run | 12% lower, p = 0.051 |
| 4 | Same as 3, plus a Fable 5.2 advisor | 8 | 41,425 | 22,134 | not run | silo 47% lower, p = 0.20 |
| 5 | Same as 3, Haiku only, about 18× more tries (silos only) | 4 | not run | 0 | not run | vs row 4 silo: p = 0.31 |
| 6 | Same as 5, Haiku only, in a room (about 10× more tries) | 4 | 599 | 29,466 | not run | room 98% lower, p = 0.029 |

Numbers are the mean best score after all tries (row 5: after 1,800 tries; row 6: after 870 tries, with row 5 silos cut at the same point). p-values come from a permutation test on the trial results, one-sided for "room is lower" in rows 1 to 3. Rows 4 and 5 are two-sided.

### 1 and 2: every agent can score the whole vector

Information is shared but not essential: any agent can already measure everything. The room is ahead at every checkpoint in both runs, but within the trial-to-trial noise. Haiku is a weak optimiser here; it barely moves a 32-number vector, and a classical random hill-climber with the same number of tries gets about 2.4× lower on the 32-number task. The room trials also took longer (about 600 s against 480 s), because each agent reads the shared feed and waits for the others.

### 3: the work is split

Here the answer is split across three slices of 11, 11 and 10 numbers. Each agent's box can score only its own slice (`task/slice32.sh`). To solve the whole thing, the agents must put their slices together.

Two measures:

- **Assembled score**: the sum, over the three slices, of the best slice score found so far. This is what a team could build if it combined its best parts.
- **Posted vector**: the best full 32-number vector an agent actually posted. Room agents post the combined vector. Silo agents never see other slices, so their posted vector has everyone else's slices at the starting value.

Result without an advisor (6 trials each): assembled 99.6k for the room against 112.9k for silos (12% lower, p = 0.051); posted 99.6k against 139.8k. The room pulls ahead after about 60 tries.

### 4: with an advisor

Every 15 tries, an agent asks Fable 5.2 (at low effort) to infer the hidden slice from its tries, then submits that estimate. Everything else is as in 3. 8 trials each.

| Final, per trial | Room | Silo |
|---|---|---|
| Assembled | 7,688 · 10,282 · 23,064 · 40,173 · 48,007 · 53,247 · 70,793 · 78,143 | 0 · 0 · 0 · 0 · 6,776 · 37,110 · 51,314 · 81,870 |
| Mean (sd) | 41,425 (26,296) | 22,134 (31,259) |
| Best posted vector, mean | 48,633 | 108,065 |

- **The advisor helps both conditions a lot.** The room's assembled score drops from 99.6k to 41.4k and the silos' from 112.9k to 22.1k. Four of the eight silo trials reached exactly 0: a lone agent with an advisor can pin down its slice exactly.
- **On the assembled measure, silos are ahead** (22.1k against 41.4k), but the difference is not significant (two-sided p = 0.20) and the trials vary a lot. The silo number is also hypothetical: silo agents never combine slices, so no agent or person ever holds a vector that scores 22.1k.
- **On the posted vector, the room wins clearly:** 48.6k against 108.1k, p = 0.0002. The room is the only condition that ends up with a usable combined answer.

### 5: the same money spent on plain Haiku tries

Row 4 costs about $1.18 a silo trial, almost all of it the advisor (about $0.17 a call, 5 to 6 calls), against about $0.0005 for a Haiku call. Row 5 asks whether that money is better spent on more Haiku tries: the same silo agents with no advisor, 600 tries each (1,800 per trial), 4 trials.

| Haiku-only silo, per trial | Tries | Cost | Assembled score (mean of 4) |
|---|---|---|---|
| | 90 | $0.06 | 136,624 |
| | 300 | $0.21 | 55,970 |
| | 600 | $0.39 | 31,918 |
| | 900 | $0.49 | 29,466 |
| | 1,200 | $0.57 | 27,342 |
| | 1,800 | $0.79 | 0 (all four trials exactly 0) |
| Row 4 silo with advisor, for comparison | 90 | $1.18 | 22,134 |
| Row 4 room with advisor, for comparison | 90 | $0.94 | 41,425 |

- **For the money, plain Haiku wins.** $0.79 of Haiku tries solved all three slices in every trial; $1.18 of advisor calls reached 0 in half. The difference is not significant with 4 trials (p = 0.31), but the direction is clear and the cost is lower.
- **It is slower in wall time.** A row 5 trial takes over an hour; a row 4 trial takes 5 to 25 minutes. If time matters more than money, the advisor is faster.
- Costs are what the `claude` CLI reports per call, at list prices; what a subscription plan charges may differ.
- Like every silo number, the assembled score assumes someone merges the three slices afterwards.

### 6: a room of cheap agents

Row 5 had no room arm, so this adds one: the same three Haiku-only agents sharing a room, no advisor, 300 tries each (900 per trial, about $0.53), 4 trials in parallel. Compared with the row 5 silos at the same number of tries:

| After this many tries | Room (mean of 4) | Silo (mean of 4) | Difference, p |
|---|---|---|---|
| 90 | 134,386 | 136,624 | not significant |
| 300 | 58,308 | 55,970 | not significant |
| 600 | 6,081 | 31,918 | p = 0.057 |
| 870 | **599** | 29,466 | p = 0.029 |

- **Sharing pays off late.** The two arms are level for the first 300 tries; then the room pulls away. A room agent reads the others' tries for its own slice's neighbours and, more importantly, ends up posting a combined vector that keeps improving.
- **The room's posted vector is near-perfect:** per trial 287 · 0 · 1,941 · 171 at 870 tries, against about 108,000 for silo agents, who never see the other slices.
- **Cost is the same:** about $0.53 for a room trial against $0.48 for the matching silo tries. Sharing adds no model calls.
- **Against the advisor:** this room beats the advisor room (41,425 at $0.94) at about half the cost. Cheap tries plus sharing was the best per-dollar arm in this benchmark.
- **Time:** a room trial took 51 to 60 minutes, which is about the same as a silo trial of that many tries.

## What it shows, and what it does not

- Sharing in a room lets split-up agents hand each other their pieces and end with one working answer. That part is solid in the data (row 4, posted vector).
- Sharing did not make the agents climb faster on the plain tasks. The gap is small and not significant, and a classical hill-climber beats all of the model runs there.
- With the advisor, assembly can be done afterwards for silos too. If you can merge the pieces at the end, the room's advantage on the assembled score disappears in this data. What the room provides is the merging as part of the work, and a shared view while it happens.
- Spending on a stronger model was not the best use of money here. Many cheap tries did better per dollar on this task, which is easy for a cheap model to grind out. A task where each try is expensive, or where cheap tries do not converge, could reverse that.
- Putting it together: on this split task, the best result per dollar came from cheap agents in a shared room (row 6). The expensive advisor helped, but less per dollar, and silos need someone to merge their pieces at the end.
- Task 3 and 4 are built so that information is split. They test the sharing mechanism, not whether Playbox beats silos on ordinary tasks.

## Caveats

- **Small samples.** 6 trials per condition for row 3, 8 for row 4 and 4 for rows 5 and 6. Room trials vary widely (8k to 78k in row 4), so a modest real effect would not show.
- **Trials that stopped slightly short are counted.** Several room trials in row 4 were stopped by a time limit at 82 to 89 of 90 tries, and one silo trial had agents hit an error at 87. Rule used: a trial counts if it reaches 80 of 90 tries and has no invalid tries. Earlier it was 87; it was loosened mid-run because a stricter rule biases the results toward faster trials. Rows 1 to 3 required all tries. Because the room's scores are still falling at the limit, the short trials slightly understate the room.
- **Row 6 trials stopped at about 98% of their tries.** All four rooms ended at 876 to 894 of 900 tries, after almost exactly one hour. Each agent's membership grant (the invite link) lasts one hour, and when it ran out the host refused further posts ("not authorized"). Trials were counted under the same proportional rule as row 4 (at least about 89% of tries) and compared at 870 tries, which all four reached. Long-running agents would need a longer grant or a renewal.
- **Trials that did not count.** Attempts that stalled on the model API's usage limit (the account's 5-hour cap) or timed out far short are not in the tables. The raw attempts are kept next to the results as `failed-*`.
- **Different accounts and time limits.** The row 4 runs began under a 700-second limit and later used 1,500 s, and part of them ran on a second account. Rows 4 trials 7 and 8, and row 5, ran after a usage-limit reset on that second account.
- **Row 5 resumed agents.** Three of the four row 5 trials had an agent stop on one malformed model reply. Those agents were resumed from their saved counters (`hc_resume.py`) until they reached 600 tries; nothing was discarded.
- **One advisor design.** Fable every 15 tries at low effort, asked to infer the target. Other advisor schedules were not tried.

## Reproduce

The scripts are in `tools/bench/hillclimb/`. You need `docker`, the `claude` CLI signed in, and a built `playbox`.

1. Start three containers that mount the task scripts read-only:

```sh
cd tools/bench/hillclimb
for i in 1 2 3; do docker run -d --name pbhc-box$i -v "$PWD/task:/task:ro" ubuntu:24.04 sleep infinity; done
```

2. Run a benchmark. Pick the experiment by its driver:

```sh
BENCH_DIR=$PWD/bench BENCH_DIM=16 BENCH_TRIALS=3 BENCH_CAP=40 python3 hc_bench.py        # row 1 (BENCH_DIM=32 BENCH_TRIALS=8 for row 2)
BENCH_DIR=$PWD/bench3 BENCH_TRIALS=6 BENCH_CAP=30 python3 hc_bench_slice.py              # row 3
BENCH_DIR=$PWD/bench4 BENCH_TRIALS=8 BENCH_CAP=30 HC_ADV_EVERY=15 BENCH_TIMEOUT=1500 python3 hc_bench_adv.py   # row 4
BENCH_DIR=$PWD/bench5 BENCH_TRIALS=4 BENCH_CAP=600 python3 hc_bench_cost.py              # row 5 (4 trials in parallel, Haiku only)
BENCH_DIR=$PWD/bench6 BENCH_TRIALS=4 BENCH_CAP=300 BENCH_TIMEOUT=14400 python3 hc_bench_cost_room.py   # row 6 (rooms)
```

3. Analyse, and watch live while it runs:

```sh
BENCH_DIR=$PWD/bench4 python3 hc_analyze_slice.py              # rows 3 and 4  (hc_analyze.py for rows 1 and 2)
BENCH_DIR=$PWD/bench4 python3 hc_live.py &                     # writes bench4/live.html every 5 s
python3 -m http.server 8765 --bind 127.0.0.1 --directory bench4   # open http://localhost:8765/live.html
```

A trial writes every try to `<BENCH_DIR>/<cond><n>/scores.tsv` and one line to `results.jsonl`. A trial that does not complete is retried, up to 8 times with a 10-minute wait, and set aside as `failed-*`. Running the full set costs real model usage; the 8-trial runs hit a 5-hour usage limit more than once.
