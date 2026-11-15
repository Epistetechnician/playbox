# F3 pilot 2 (H100)

Scripted stand-ins (arm `h100`, amendment A4), one round, primary node `f3-pilot-a`, replay on `f3-pilot-b`. Nothing here was edited after signing.

## Decisions (chain: 6 receipts, `verify2.txt`: VERIFIED, replay none)
| seq | stand-in | verdict | gain | theta |
|---|---|---|---|---|
| 1 | compile_model | PROMOTE | 0.6096 | ~0.05 |
| 2 | weight_decay_0 + clip_1 | REJECT gain_below_threshold | 0.0371 | ~0.05 |
| 3 | anneal_to_step_240 | PROMOTE | 0.1178 | ~0.05 |
| 4 | dev memorizer (injected bad) | REJECT holdout_failed (caught false promotion) | dev 0.807 | holdout 2.249 vs limit 1.2225 |

Promotions 2, rejections 2, caught false promotions 1, missed false promotions 0 found. Then the reveal receipt (last).

## Replay (`replay2.txt`, node b, `verify.py --replay all`): VERIFIED
- seq 1 compile: recorded seeds reproduce; fresh seeds [1508, 1513, 1852] gain 0.615.
- seq 3 anneal: recorded seeds reproduce; fresh seeds gain 0.129.
- seq 4 memorizer fails the holdout again (2.249 vs limit 1.2225).
- No false promotion. 0 FAIL lines.

## Spend (gman billing)
f3-pilot-a $32.88, f3-pilot-b $8.37, total $41.25 (of the $50 ceiling). Both nodes stopped.

## Not run
Random-search arm, null control, and extra stand-ins (budget/priority). So no comparison to random search.

## Honest limits
- Stand-ins are scripted patches, not agents; no claim about agent skill.
- The compile gain is a speed-up (training seconds), not better loss per step.
- Second host is the same operator and same GPU type; not independent.
- Candidates and evaluator share a Unix user (A3): "agents never see the holdout" is not shown.
- Node b was built from a tarball of the tree committed with a fixed author/date, not a clone of the genesis commit; the verifier's genesis commit checks still passed.
- Anneal decision 3 was lost once with no receipt and rerun (the log shows RESUME2); only signed receipts count.
- Two promotions and one caught false promotion: wide intervals, descriptive only.

Files: `receipts.jsonl`, `candidates/`, `host.pub`, `PINNED.txt` (key pinned off-node before any candidate), `arm.log`, `verify2.txt`, `replay2.txt`, `p2out.tgz`.
