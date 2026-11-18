# R&D room: a multi-family agent team improving Playbox through a Playbox room

`tools/bench/rnd/rnd.py` runs an autonomous R&D team inside one local Playbox room where every member holds its own cryptographic key and posts a signed `::pbx identity` card naming its harness and model.

## Default Triad Mode (`RND_BACKEND=agy`)

Uses the authenticated Antigravity CLI (`agy`) to orchestrate all three Gemini 4 model families (**Carbon**, **Argon**, and **Hydrogen**) with strict cross-family separation of duties and effort right-sizing:

1. **Snapshot refresh & in-flight deduplication:** Fetches `origin/main`, resets `~/.playbox-rnd/snap`, and queries open PRs so agents never inspect stale code or duplicate in-flight `[R&D room]` changes.
2. **Research (`PROPOSAL`):** `Carbon` (`gemini-4-carbon-medium`), `Argon` (`gemini-4-argon-medium`), and `Hydrogen` (`gemini-4-hydrogen-high`) inspect the read-only snapshot across security, reliability, and CLI/MCP experience lenses (2 distinct families per lens) and post signed proposals to the room feed.
3. **Cross-family review (`REVIEW`):** Fast reviewers (`carbon-low`, `argon-low`, `hydrogen-medium`) verify `path:line` claims in the codebase and score only proposals from the *other two* families (4 cross-family reviews per proposal, zero self-family grading).
4. **Advisor triage (`ADVICE`):** `Advisor` (`gemini-4-carbon-high`) verifies top candidates in the code, drops duplicates or overlapping proposals, and ranks non-colliding picks.
5. **3-family implementation, diff review, and ship gate:** For every selected proposal, `{Implementer, Diff Reviewer, Ship Gate}` spans `{Carbon, Argon, Hydrogen}` with `Implementer != Author`:
   - `Carbon-high` or `Argon-high` produces JSON edits from a clean worktree.
   - The harness resets any stray worktree state, applies JSON edits, and runs `rustfmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (plus `py_compile` and hub `pytest` when Python files change).
   - A second family reviews the diff (`VERDICT`), and the third family performs a final `ship`/`hold` check (`ADVICE`).
6. **Provenance & canvas:** Emits live `::pbx card`, `::pbx artifact`, and `::pbx metric` updates to the Playbox Hub canvas and opens **draft** pull requests (`RND_PR=1`) recording the full room provenance.

Legacy mixed mode (`RND_BACKEND=mixed`) remains supported for Claude Code + `agy` runs.

## Safety invariants

- Agents have read-only permissions (`--sandbox` + explicit `write_file`/`edit_file`/mutating command denies); worktrees are reset before applying JSON edits.
- Model-written text never reaches a shell: git commits, pushes, and `gh pr create` calls use argument lists.
- Forbidden paths: `.github/`, `desktop/`, dependency files, release/signing scripts, `grants.rs`, `audit.rs`, attestation, and `steer/`. Diffs over 400 lines are refused.
- The room is the source of truth: proposals and reviews are read back from the signed Playbox feed and verified against each author's member ID.

## Running

```bash
RND_PR=1 RND_HUB=1 python3 tools/bench/rnd/rnd.py
```

Key environment variables: `RND_PICK` (default `4` in `agy` mode), `RND_PER_FAMILY` (default `2`), `RND_PR=1` (push branches and open draft PRs), `RND_HUB=1` (register room in `~/.playbox/hosting`), `RND_RESUME=<run dir>`, `RND_MAX_TOKENS`.
