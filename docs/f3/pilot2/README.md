# F3 pilot 2 (H100, 4 scripted stand-ins): the first promotions on a real target, replayed on a second node

Spec `task_spec_gpu_p2.json` (request: [GPU_RUN_REQUEST_P2.md](../GPU_RUN_REQUEST_P2.md)). Primary node `f3-pilot-a`, second host `f3-pilot-b`, both one H100. Host key `host.pub`, pinned locally before any candidate (`PINNED.txt`). Ceiling label `local_internal_exploratory`; hardware attestation is self-reported; the second node has the same operator, so it is a second host, not an independent party.

**Result: 2 promotions, 1 rejection for gain below threshold, 1 caught false promotion; all re-derive from the receipts (`verify2.txt`: VERIFIED), and all reproduce on the second node (`replay2.txt`: VERIFIED).**

| seq | stand-in | verdict | gain vs frontier | second-node replay |
|---|---|---|---|---|
| 1 | compile the model | promote | 0.610 | recorded seeds reproduce; fresh seeds [1508, 1513, 1852] pass the gates, gain 0.615 |
| 2 | weight decay 0 and clip 1.0 | reject, gain_below_threshold | 0.037 | |
| 3 | anneal to step 240 | promote | 0.118 | recorded seeds reproduce; fresh seeds pass, gain 0.129 (floor theta/2) |
| 4 | **injected dev memorizer** | reject, **holdout_failed** (caught) | 0.807 on dev | fails the holdout again: 2.249 bpb against a limit of 1.2225 |

The second node set itself up from the primary's tokenizer (`tok.tgz`) and a clean tree at the same genesis commit, never given the harness state or `custodian/`, and ran `verify.py --replay all` under the pinned key. Wall-clock is compared only within a node.

What this does and does not show. It shows the positive path on a real pretraining target: a promotion, re-derived from signed measurements and reproduced on a second H100, and a cheat stopped by the sealed holdout. It does not show that agents find improvements: promotion 1 is `torch.compile`, a speed-up the baseline did not use, and promotion 3 is a schedule change, both scripted. The gains are time-to-target ratios on this one setup, not a better model.

Files: `p2out.tgz` (receipts, archived candidates, feed, host key), `arm.log` (driver log; RESUME lines are node stops and wakes), `verify2.txt`, `replay2.txt`, `host.pub`, `PINNED.txt`.
