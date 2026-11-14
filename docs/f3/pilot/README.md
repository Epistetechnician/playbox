# F3 H100 pilot: INCOMPLETE (stopped when the session's usage limit was reached)

Node `f3-pilot-a` (new, 1x H100 80GB, image pytorch-2.13-cuda12.9) ran from 2026-11-14 15:10Z to about 18:31Z. **Stopped and verified stopped** (`stopped (disk intact)`). The second node was never created. Spend for f3-pilot-a: 197 min, 13.24 USD (`gman usage`), under the 40 USD ceiling.

Measured at calibration (genesis receipt, `host.pub` pinned locally at 16:16:26Z before any candidate): L* = 1.18 bpb at 240 steps; baseline T0 = 245.7 s; timing noise cv 0.0179; eval granularity 0.0417; theta 0.06; holdout-minus-dev gap mean +0.0051 (sd 0.00085), eps 0.0151. Baseline speed about 1.04 s per step at 2**19 tokens (no compile). Smoke overhead of a run subprocess about 1.05 to 1.16x. Each candidate decision took about 27 minutes.

Decisions finished before the stop (`receipts_partial.jsonl`, 6 receipts: genesis + 5 decisions; not a complete round, no reveal, not verified with `verify.py`): 1 matrix lr 0.05 reject (gain 0.001); 2 half batch reject (target_not_reached); 3 Muon ns_steps 3 reject (-0.081); 4 embedding lr 0.4 reject (-0.0001); 5 injected dev memorizer reject, holdout_failed, dev gain 0.866 (caught false promotion). Zero promotions so far.

Left: stand-ins 6 to 8 (resumable: `pilot.py --work ~/run1 --max 3` on the parked disk, then `harness.py reveal`, `verify.py`), the second node and replay, and pulling candidates. The partial files here were pulled from the node and are not on a verified chain end.
