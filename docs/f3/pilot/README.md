# F3 H100 pilot (Q = 8 scripted stand-ins, one new H100)

Node `f3-pilot-a` (new, 1x H100 80GB, image pytorch-2.13-cuda12.9), 2026-11-14 15:10Z to 20:04Z; stopped and verified stopped. Spec `task_spec_gpu.json` at the pilot setting (depth 8 autoresearch GPT, 2048-token rows, 2**19 tokens per step, no compile, eval every 10 steps), climbmix shards 0 to 9 train, 6542 dev, 6541 holdout. Host key `host.pub` pinned locally at 16:16:26Z before any candidate (see PROTOCOL A3). Ceiling label `local_internal_exploratory`; hardware attestation is self-reported; candidate and evaluator ran as the same Unix user.

**Result: 0 promotions, 7 rejections for gain below threshold or target, 1 caught false promotion.** `verify.txt`: VERIFIED, all checks pass (chain under the pinned key, genesis code and data-manifest pins, git commit 83eba5ad, rule re-derivation, reveal, leak scan). `verify_replay.txt`: the caught candidate fails the holdout again (2.204 bpb against a limit of 1.195), run on the same node, not a second host. There was no promotion to replay, so the second node was never created.

Calibration: L* = 1.18 bpb at 240 steps; T0 = 245.7 s (median of 3 seeds); timing noise cv 0.0179 (one seed, 5 repeats); eval granularity 0.0417; theta 0.06; holdout minus dev gap mean +0.0051, sd 0.00085, so eps = 0.0151 (the Mac gap was -0.05 to -0.11 bpb; on the H100 spec the holdout is nearly the same as dev).

| seq | stand-in | verdict | gain | holdout bpb (3 seeds) |
|---|---|---|---|---|
| 1 | matrix lr 0.05 | reject, gain_below_threshold | 0.001 | 1.180 to 1.184 |
| 2 | half batch | reject, target_not_reached | | |
| 3 | Muon ns_steps 3 | reject, gain_below_threshold | -0.081 | 1.180 to 1.182 |
| 4 | embedding lr 0.4 | reject, gain_below_threshold | -0.0001 | 1.184 to 1.186 |
| 5 | **injected dev memorizer** | reject, **holdout_failed** (caught) | 0.866 on dev | 2.203 to 2.434 (limit 1.195) |
| 6 | Muon ns_steps 2 | reject, gain_below_threshold | -0.158 | 1.182 to 1.184 |
| 7 | weight decay 0 | reject, gain_below_threshold | 0.040 | 1.180 to 1.184 |
| 8 | grad clip 1.0 | reject, gain_below_threshold | 0.039 | 1.183 to 1.186 |

Time per decision about 26 to 27 min (candidate 5 and 2 stopped early, 15 to 19 min; candidate 6 took 57 min wall because the node was stopped and woken in between). Per decision, the sum of training walls was 0.91 to 0.97 of the decision time, so subprocess and evaluation overhead was only about 3 to 10% on the H100 (the Mac showed 70 to 90%). Nodes: f3-pilot-a only, 281.7 min, **18.93 USD** (`gman usage`), under the 40 USD ceiling.

Notes: the Mac-tuned stand-ins (Muon steps, learning rates) do not help on the H100 at this step budget; nothing cleared theta = 0.06 (best 0.040, below threshold). `playbox-tp3` showed `stopped` at the end; this session did not touch it. `candidates/` holds the 10 archived sources; `pilot.log`, `pilot2.log` and `init.log` are the harness logs (pilot.log and pilot2.log are the two driver runs, split by the usage-limit pause). `verify*.txt` ran on the node against its tree committed with fixed author and date (genesis commit 83eba5ad); the same files are in this repo at commit 0534850 but the repo's own commit hash differs, so the git-commit pin does not resolve here.
