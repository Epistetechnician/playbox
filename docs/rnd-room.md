# R&D room: a cross-vendor agent team improving Playbox through a Playbox room

`tools/bench/rnd/rnd.py` runs Claude agents (Claude Code) and Gemini agents (Antigravity `agy`) as separate members of one local Playbox room. Each has its own key and announces a signed card naming its harness and model.

Phases: research (each agent reads a read-only snapshot of the repo through a lens such as security, reliability or developer experience, and posts proposals) → cross-review (every agent rates the others' proposals) → selection (the harness ranks by impact times feasibility, drops risky or forbidden paths and proposals that touch the same file) → implementation (the *other* vendor writes the change, the harness applies it and runs fmt, clippy and the crate's tests) → diff review (the proposer's vendor must approve) → a **draft** pull request with the room's provenance in its body.

Rules that keep it safe:

- Agents can read the snapshot and nothing else. They reply with JSON; they never write files, run commands or push. The harness applies edits, runs the gates, commits and opens the PR.
- Model-written text never reaches a shell: commits and PRs are made with argument lists. (An early version built the title as a shell string, and backticks in a title were executed. It was found on the first real run and fixed.)
- Forbidden paths: workflows, dependency files, release and signing, the grant and audit core, attestation and steering. Diffs over 400 lines are refused.
- The room is the record: proposals and reviews are read back from the room feed and only count if the signing key belongs to the claimed author.

Run: `RND_PER_VENDOR=3 RND_PICK=6 RND_PR=1 python3 tools/bench/rnd/rnd.py`. With `RND_HUB=1` the room is registered with the hub (adopt it to watch). `RND_RESUME=<run dir>` reuses an earlier run's proposals.

First run (2026-11-16): 17 proposals, 85 reviews, 4 selected, 3 drafts opened (#16, #17, #18), 1 rejected in review.
