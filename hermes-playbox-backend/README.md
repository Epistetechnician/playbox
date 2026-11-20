# hermes-playbox-backend

Playbox governed end-to-end adapter for Hermes across three modules (`openshell`, `libkrun` microVM, or `docker`):

- **`playbox_terminal/`** (`terminal.backend: playbox`): routes every Hermes shell/file command through a signed, expiring Biscuit grant and host audit log. Defaults to **stateful session mode (`PLAYBOX_KEEP=1`)** with zero edit-command replay, `sync="none"` on read-only commands, `sync="delta"` on edits, `sync="full", full_patch=true` on final export, and optional `PLAYBOX_EGRESS` (`BOX-7`) / `PLAYBOX_HOSTS`. Install by linking `playbox_terminal/` into `~/.hermes/plugins/playbox-terminal` and adding `playbox-terminal` to `plugins.enabled`.
- **`playbox_atlas/`**: AtlasKV-inspired (`KG2KV` + 3-level `HiKVP`) hierarchical repository & verified room-memory index with 1-hop call-graph expansion and Playbox quarantine filtering (`docs/atlaskv-integration.md`).
- **`playbox_router/`**: metered intelligence-per-cost orchestrator (cheap parallel workers, hard USD cap, deterministic 3-way composition, fresh-sandbox sealed verification).

Design, results, and limits: `docs/hermes-playbox.md` and `docs/atlaskv-integration.md`. Harnesses: `tools/bench/hermes-parity-playbox/` and `tools/bench/hermes-parity/run_swe_playbox_v2.py`.
