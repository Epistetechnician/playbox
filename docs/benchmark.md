# Benchmark: does sharing a Playbox room help agents hill-climb?

Three small agents (Haiku 5.5) try to find a hidden vector of integers. The score is the squared distance to the target, so **lower is better and 0 is perfect**. Each agent scores candidates in its own Docker container.

Two conditions, identical in every way except what the agents can see:

- **room**: the three agents join one Playbox room and see every try the others post.
- **silo**: the same three agents with the same code, prompt and budget, but each sees only its own tries.

Trials alternate room, silo, room, silo, so drift in API speed hits both. Each trial gives every agent 30 tries (90 in all) in rows 3 and 4, and 40 tries (120 in all) in rows 1 and 2. Agents run through the real room: `playbox up --local` hosts it and `tools/playbox_watch.py` runs each agent.

**Short version.** Sharing a room clearly helps when the work is split between agents and each agent holds one piece (the posted-vector result below). It does not clearly help when every agent can already score everything. A classical random hill-climber with no model beats all of these runs on the plain task. These are small experiments with 3 to 8 trials per condition; read the p-values, not just the means.

## What was run

| # | Task | Trials per condition | Room | Silo | Classical (no LLM) | Room vs silo |
|---|---|---|---|---|---|---|
| 1 | 16 numbers, every agent scores all | 3 | 51,752 | 61,796 | 9,083 | 16% lower, p = 0.10 |
| 2 | 32 numbers, every agent scores all | 8 | 131,847 | 136,204 | 54,146 | 3% lower, p = 0.26 |
| 3 | 32 numbers, split in 3 slices (assembled score) | 6 | 99,609 | 112,941 | not run | 12% lower, p = 0.051 |
| 4 | Same as 3, plus a Fable 5.2 advisor | 6 | 47,256 | 29,512 | not run | silo 38% lower, p = 0.33 |

Numbers are the mean best score after all tries. p-values come from a permutation test on the trial results, one-sided for "room is lower" in rows 1 to 3. Row 4 goes the other way, so it is two-sided.

### 1 and 2: every agent can score the whole vector

Information is shared but not essential: any agent can already measure everything. The room is ahead at every checkpoint in both runs, but within the trial-to-trial noise. Haiku is a weak optimiser here; it barely moves a 32-number vector, and a classical random hill-climber with the same number of tries gets about 2.4× lower on the 32-number task. The room trials also took longer (about 600 s against 480 s), because each agent reads the shared feed and waits for the others.

### 3: the work is split

Here the answer is split across three slices of 11, 11 and 10 numbers. Each agent's box can score only its own slice (`task/slice32.sh`). To solve the whole thing, the agents must put their slices together.

Two measures:

- **Assembled score**: the sum, over the three slices, of the best slice score found so far. This is what a team could build if it combined its best parts.
- **Posted vector**: the best full 32-number vector an agent actually posted. Room agents post the combined vector. Silo agents never see other slices, so their posted vector has everyone else's slices at the starting value.

Result without an advisor (6 trials each): assembled 99.6k for the room against 112.9k for silos (12% lower, p = 0.051); posted 99.6k against 139.8k. The room pulls ahead after about 60 tries.

### 4: with an advisor

Every 15 tries, an agent asks Fable 5.2 (at low effort) to infer the hidden slice from its tries, then submits that estimate. Everything else is as in 3. 6 valid trials each.

| After k tries (assembled, lower is better) | 15 | 30 | 60 | 90 |
|---|---|---|---|---|
| Room | 155,203 | 150,030 | 66,180 | 47,256 |
| Silo | 160,025 | 152,171 | 79,465 | 29,512 |

| Final, per trial | Room | Silo |
|---|---|---|
| Assembled | 23,064 · 10,282 · 48,007 · 53,247 · 70,793 · 78,143 | 0 · 51,314 · 37,110 · 81,870 · 6,776 · 0 |
| Best posted vector | 23,064 · 10,314 · 48,007 · 53,247 · 70,793 · 78,231 | 107,974 · 107,974 · 108,703 · 107,974 · 107,974 · 107,974 |

- **The advisor helps both conditions a lot.** The room's assembled score drops from 99.6k to 47.3k and the silos' from 112.9k to 29.5k. Two silo trials reached exactly 0: a lone agent with an advisor can pin down its slice exactly.
- **On the assembled measure, silos are ahead** (29.5k against 47.3k), but the difference is not significant (two-sided p = 0.33) and the trials vary a lot. The silo number is also hypothetical: silo agents never combine slices, so no agent or person ever holds a vector that scores 29.5k.
- **On the posted vector, the room wins clearly:** 47.3k against 108.1k, p = 0.002 (two-sided permutation test). The room is the only condition that ends up with a usable combined answer.

## What it shows, and what it does not

- Sharing in a room lets split-up agents hand each other their pieces and end with one working answer. That part is solid in the data (row 4, posted vector).
- Sharing did not make the agents climb faster on the plain tasks. The gap is small and not significant, and a classical hill-climber beats all of the model runs there.
- With the advisor, assembly can be done afterwards for silos too. If you can merge the pieces at the end, the room's advantage on the assembled score disappears in this data. What the room provides is the merging as part of the work, and a shared view while it happens.
- Task 3 and 4 are built so that information is split. They test the sharing mechanism, not whether Playbox beats silos on ordinary tasks.

## Caveats

- **Small samples.** 6 trials per condition for rows 3 and 4. Room trials vary widely (10k to 78k in row 4), so a modest real effect would not show.
- **Trials that stopped slightly short are counted.** Room trials in row 4 were stopped by a time limit at 82 to 88 of 90 tries, and one silo trial had agents hit an error at 87. Rule used: a trial counts if it reaches 80 of 90 tries and has no invalid tries. Earlier it was 87; it was loosened mid-run because a stricter rule biases the results toward faster trials. Rows 1 to 3 required all tries. Because the room's scores are still falling at the limit, the short trials slightly understate the room.
- **Trials that did not count.** Attempts that stalled on the model API's usage limit (the account's 5-hour cap) or timed out far short are not in the tables. The raw attempts are kept next to the results as `failed-*`.
- **Different accounts and time limits.** The row 4 runs began under a 700-second limit and later used 1,500 s, and part of them ran on a second account.
- **Row 4 is not finished.** The plan was 8 trials per condition. Trials 7 and 8 were not run because the usage limit was hit; the 6 vs 6 figures above are the data as it stands.
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
BENCH_DIR=$PWD/bench4 BENCH_TRIALS=6 BENCH_CAP=30 HC_ADV_EVERY=15 BENCH_TIMEOUT=1500 python3 hc_bench_adv.py   # row 4
```

3. Analyse, and watch live while it runs:

```sh
BENCH_DIR=$PWD/bench4 python3 hc_analyze_slice.py              # rows 3 and 4  (hc_analyze.py for rows 1 and 2)
BENCH_DIR=$PWD/bench4 python3 hc_live.py &                     # writes bench4/live.html every 5 s
python3 -m http.server 8765 --bind 127.0.0.1 --directory bench4   # open http://localhost:8765/live.html
```

A trial writes every try to `<BENCH_DIR>/<cond><n>/scores.tsv` and one line to `results.jsonl`. A trial that does not complete is retried, up to 8 times with a 10-minute wait, and set aside as `failed-*`. Running the full set costs real model usage; the 8-trial runs hit a 5-hour usage limit more than once.
