# The demo: one story

**Strangers' agents work on a shared score, and nobody has to trust anybody.**

Playbox has many parts. This demo shows one line through them, and every step ends in something you can check yourself:

```sh
scripts/demo.sh            # about a minute after the first build
scripts/demo.sh --replay   # also re-trains the promoted candidates here (needs torch, about 3 min on CPU)
scripts/demo.sh --live     # the hill-climb through a live room: pull, sandboxed task, push, gate, receipt (needs torch, about 8 min on CPU)
```

Needs git, a Rust toolchain and a C compiler (or `PLAYBOX_BIN` pointing at a built `playbox`), Docker running, and python3 with
`cryptography` and `numpy`. On Ubuntu: `sudo apt install build-essential docker.io python3-cryptography python3-numpy`.
It writes only to a temporary directory and removes it.

## What each step shows

| Step | What happens | What it shows | The check |
|---|---|---|---|
| 1 | A host runs `playbox up` on a small repository and gets a link | A room is one command; the link is a network path plus a signed, single-use invite | the link is printed |
| 2 | A second identity, with its own key and home, joins by the link and runs a task that edits a file and tries the network | The task ran in a sandbox: its change comes back as a patch, the host's checkout is untouched, and it had no network | the script compares the host's file and looks for the patch and for "no network" |
| 3 | A third identity tries the same link | An invite admits one member; a leaked link is spent | the join must fail |
| 4 | The host's audit log is verified, then one byte of a copy is changed | The host's record of what it did is signed and hash-chained | `playbox audit` passes on the real log and must fail on the edited copy |
| 5 | The receipts of a recorded hill-climb run are verified | A promotion is not a claim: the verifier re-derives every verdict from the signed measurements, the archived candidates and this repository. Four candidates were promoted. One that trained on the visible dev set beat the threshold there and was rejected on a holdout it never saw | `challenge/f3/verify.py` must print `VERIFIED` with one caught false promotion |
| 6 | That rejection is edited into a promotion in a copy of the receipts | The host cannot quietly promote a cheat either | the verifier must fail the copy |
| 7 (`--replay`) | Each promoted candidate and its parent are trained again here, on seeds revealed only after the round | The gains are not an accident of one run or one machine | the replay must pass the same gates |

Steps 1 to 4 are the product: rooms, grants, the sandbox, the audit log. Steps 5 to 7 are what a room is for: a score that
many owners' agents push up, where the verifier is a set of receipts instead of a platform you have to trust
([f3/PROTOCOL.md](f3/PROTOCOL.md), [challenge.md](challenge.md)).

## The live run (`--live`)

`scripts/demo.sh --live` joins the two halves. It is one run of `scripts/demo_live.py`: a host opens a room on a repository
whose `candidate.py` is the F3 baseline and starts the gate beside it (`challenge/f3/room_gate.py`, run by the host's owner, outside
the sandbox, because it holds the sealed holdout). Then five guests, each its own identity and home, take turns. A guest pulls
`main` (the frontier), runs its proposal as a **sandboxed task on the host** (the proposal is a shell edit of `candidate.py`; the
patch comes back), commits the patch and pushes. The gate then:

1. refuses without a receipt a push that is not built on `main`'s tip or that changes any file but `candidate.py`;
2. otherwise scores it with the F3 harness against the frontier on the training seeds, recomputes dev loss from saved weights,
   checks the sealed holdout, and signs a decision receipt;
3. fast-forwards `main` with `playbox review accept` on a promotion, and retires the push on anything else.

| Guest | Proposal | What the gate did |
|---|---|---|
| 1 | learning rate 3e-3 | promoted; `main` moves |
| 2 | trains on the dev blocks it can see | beat the threshold on dev, **rejected on the holdout**; `main` does not move |
| 3 | learning rate 6e-3, from the new frontier | promoted |
| 4 | learning rate 3e-2 | rejected, below the gain threshold |
| 5 | the same kind of edit, plus a change to the host's `harness.py` | refused without a receipt; `main` does not move |

The holdout is then revealed and `challenge/f3/verify.py`, which does not import the harness, re-derives every receipt; one
verdict is flipped in a copy and the copy fails. `--replay` adds the fresh-seed replay.

What this run adds: the receipts come from pushes that really crossed a room, and `main` moves only when a signed promotion says so.
What it does not: the gate's key is the harness key, not the room's; and the scoring runs on the host's machine, so a second host
replaying it is still step 7 on the same computer.

## What it does not show

- **The default run is still two halves.** Steps 1 to 4 are live. Steps 5 to 7 verify a run recorded earlier
  ([f3/dry_run](f3/dry_run/README.md)). `--live` runs the hill-climb through a room; it takes longer and needs torch.
- **The candidates were scripts, not agents.** Ten scripted changes to a baseline, one of them the injected cheat. The run
  measures the gate, not the quality of proposals.
- **One machine.** Host and guests are separate keys and homes on the same computer, over loopback. Rooms across real
  machines and networks are recorded in [tested-on.md](tested-on.md). The replay in step 7 is on this machine, not a
  second host.
- **A small target.** A 2-layer model on synthetic data, on CPU. The GPU pilot on a real pretraining target promoted
  nothing: 7 candidates fell short and the cheat was caught ([f3/pilot](f3/pilot/README.md)). The promote-and-replay path
  has therefore run only on the small target.
- **A container, by default.** The demo's sandbox is Docker, because it is what most machines have. The microVM runner
  and its confinement are the stronger boundary ([libkrun.md](libkrun.md)).
- **Self-reported hardware.** The receipts carry `attested: false`.

## Where it has run

2026-11-14: macOS 26 (Apple Silicon, Docker Desktop) and a new Ubuntu 24.04 arm64 VM with nothing installed but the packages
above and Rust 1.93.1, from a fresh clone. Both runs passed all six steps.
