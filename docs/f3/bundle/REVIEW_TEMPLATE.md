# F3 bundle: independent review

Fill in, sign (any detached signature or a signed git tag over this file is fine), and send back with your output files. Delete nothing; write "not done" where applicable.

**Bundle:** `f3-bundle.tar.gz` SHA-256 you computed: `________` (compare with the value you were given out of band)

## Reviewer
- Name / organisation: 
- Public key or identity used to sign (e.g. GPG fingerprint, ssh-ed25519 key, GitHub handle): 
- Relationship to the authors (none / colleague / paid / other): 
- Date (UTC): 

## What I ran
| Item | Done? | Command | Output file |
|---|---|---|---|
| `SHA256SUMS` check | | | |
| Tier A `check.sh` | | | |
| Tier A on a machine other than the authors' | | | |
| Read `protocol/PROTOCOL.pilot*.md` against the code | | | |
| Read the candidate sources in `runs/*/candidates/` | | | |
| Tier B replay on an H100 with the data | | | |
| Re-derived the promotion rule independently (not with `verify.py`) | | | |

## Versions
OS / kernel: ____  Python: ____  `cryptography`: ____  `numpy`: ____
Tier B only: GPU model/driver: ____  PyTorch/CUDA: ____  data source and shard hashes matched genesis manifest? (y/n): ____

## Results
- Tier A, pilot 1: VERIFIED / FAILED (n failing checks): 
- Tier A, pilot 2: VERIFIED / FAILED (n failing checks): 
- Tier B (if run): per promotion, recorded seeds reproduce (y/n), fresh-seed gain, holdout loss:
- Anything unexpected:

## Disagreements and concerns
(With the claim in the README they concern, and what evidence would change your mind.)

## What I could not check
(At minimum, say whether you checked each: re-running candidates; data manifest against real shards; the holdout Merkle opening; whether the second host was independent; whether candidates could read the holdout.)

## Conclusion
- I confirm the claims in README section "What is claimed" (all / only these: ____ ): 
- I do not confirm: 

Signature:
