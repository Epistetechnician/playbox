# Audit of the documentation's claims
Every factual claim in `README.md`, `docs/a2a-binding.md`, `docs/cli.md`, `docs/capabilities.md`, `docs/threat-model.md`, `docs/trust-layers.md`, `docs/attestation.md`, `docs/revocation.md`, `docs/live-delivery.md`, `ui/README.md` and `adapters/README.md` that could be checked against the code, the tests or the git history, checked on 2026-11-13 against 0.3.1 plus the unreleased changes after it. The v0 checklist and `docs/tested-on.md` were brought in line at the same time. This is a sample of the checkable claims, not every sentence: prose about research results (`docs/steering.md`, `docs/latent-channels.md`) is out of scope except where the README quotes a number.

Verdicts: **true** (the code or a test shows it), **stale** (it was true or planned once; the code has moved), **overstated** (it says more than the code or tests show), **untested** (it may be true, but nothing automated checks it, or it rests on a recorded manual run). Where a row says "Fixed", the doc now carries the true statement; "stale" and "overstated" rows with no fix note are in files outside this task.

| Verdict | Rows |
|---|---|
| true | 31 |
| stale | 36 |
| overstated | 6 |
| untested | 6 |
| total | 79 |

| # | Claim | Where it is stated | What the code or tests show | Verdict |
|---|---|---|---|---|
| 1 | The MCP server has 13 tools | Not found in any of the eleven files. `hermes-plugin/PRD.md:49` says 29. | `tool_list()` is `crates/playbox-mcp/src/lib.rs:123`; the tools are declared at lines 135 to 414: 41. Two of them also carry the room-card `_meta` (lines 126-128). | stale |
| 2 | Adapters: ten tools, and "there is no free-form messaging tool yet" | adapters/README.md (Tools) | `playbox_say` lib.rs:196, `playbox_feed` :258, five `playbox_conversation*` tools :205-220. Fixed: the page lists all 41. | stale |
| 3 | Version 0.3.0 | README.md:9 | `Cargo.toml:6` is 0.3.1; tag v0.3.1; `CHANGELOG.md:26`. Fixed. | stale |
| 4 | 44,560 lines of Rust, per-crate counts | README.md At a glance | `find crates -name '*.rs' \| xargs cat \| wc -l` = 46,348 (net 17,958, core 10,397, cli 6,777, runner 4,624, mcp 4,002, rap 1,031, git 916, bridge 643). Fixed. | stale |
| 5 | 478 Rust tests | README.md At a glance | The README's own grep (`#[test]` and `#[tokio::test]` in `crates`) = 494. Fixed. | stale |
| 6 | 172 Python tests | README.md At a glance | The README's own grep over `steer judge orchestrator hermes-plugin-witness` = 172. | true |
| 7 | 10 fuzz targets (also listed in the proof table) | README.md At a glance, Proof table | `fuzz/fuzz_targets/` holds 12 files (`fuzz/Cargo.toml` has 13 `[[bin]]`, one of them the `seeds` generator); `docs/proof-status.md` re-ran all 12. Fixed. | stale |
| 8 | 21 documents, 3,838 lines | README.md At a glance, repository map | `docs/*.md` was 23 files, 4,059 lines before this audit. Fixed (the count now includes this page). | stale |
| 9 | Releases end at 0.3.0 | README.md At a glance | `CHANGELOG.md:26` 0.3.1, tag v0.3.1. Fixed. | stale |
| 10 | Lean: 207 lines, 15 theorems, 0 `sorry` | README.md At a glance | `lean/Playbox.lean` 2 + `Grants.lean` 97 + `Steer.lean` 108 = 207 lines; 7 + 8 theorem declarations; no `sorry` in `lean/Playbox`. | true |
| 11 | "Every claim below links to the file that backs it" | README.md:9 | Many table rows and bullets name no file. Softened to "Most claims". | overstated |
| 12 | "Every result file keeps a ceiling label (`local_internal_exploratory`)" | README.md Proof, end to end | `grep -rl local_internal_exploratory steer/results` finds none of its 145 JSON files; the label is in `docs/latent-channels.md`, `docs/steering.md`, `steer/playbox_steer/hub_run.py`. Fixed. | overstated |
| 13 | "failover to a replica in 0.3 s" | README.md Proof table (Two real machines) | The guest switched with `playbox_use_host` (`crates/playbox-mcp/src/lib.rs:312`); `grep -ri failover crates` finds nothing, and `docs/revocation.md` says there is no automatic failover. Fixed. | overstated |
| 14 | History entry fields `{seq, prev_hash, kind, payload_hash, time, signer, sig}` | README.md Trust | `crates/playbox-core/src/history.rs:14-22`: `seq, prev, kind, payload_hash, time_ms, signer, sig`. Fixed. | stale |
| 15 | Headline numbers: 87.5% gated (140/160), 71/72 controls, 63/80 DeepSeek, swarm 55.7% / 44.5% / 54.2%, hub 64 bytes, 6 receipts (4 reject, 2 promote) | README.md headline table, swarm, hill-climb | `python3 scripts/verify_claims.py` rebuilds them from `steer/results/` and `challenge/results/`: "ALL CLAIMS MATCH". `common.verify_log` on `challenge/results/receipts.jsonl` returns 6. The experiments themselves were not repeated here (`docs/proof-status.md` has the two GPU reproductions). | true |
| 16 | The git 2.43.0 bundle test "not re-run" after the fix | README.md What is not proven | `docs/proof-status.md`: `cargo test -p playbox-git` passes 6/6 on git 2.43.0 and 2.55.0; the test is `crates/playbox-git/src/lib.rs:638`. Fixed. | stale |
| 17 | `up --state DIR` keeps identity, root key, unspent invites, members and bans | README.md How a room works | `crates/playbox-net/tests/persist.rs:31, 59, 92`. | true |
| 18 | Over-limit requests are refused, not clamped | README.md How a room works | `crates/playbox-net/tests/grants.rs:116` (`task_limits_are_enforced_not_clamped`). | true |
| 19 | T3: a real TDX VM verified, a Mac guest joined at `min_host_tier: T3` | README.md Hardware tiers, Proof table | Fixture `crates/playbox-core/tests/fixtures/confidential-space`; verifier `crates/playbox-core/src/confidential.rs`; wired at `platform.rs:1085`. The end-to-end join is a recorded manual run (now in `docs/tested-on.md`), not an automated test. | untested |
| 20 | Mac and Pi, India, relay-only join, 10 ms revocation, Secure Enclave T1 runs | README.md Proof table; docs/tested-on.md | Recorded manual runs on hardware this audit does not have; `docs/proof-status.md` also lists them as stored. The 10 ms figure has no automated test (`crates/playbox-net/tests/replica.rs:106` checks only that gossip alone delivers within seconds). | untested |
| 21 | CI: Linux and macOS matrix, cargo audit, Python suites, rootful and rootless confinement, conformance | README.md At a glance | `.github/workflows/ci.yml:14-17` (matrix), `:32-33` (audit), `:55` (pytest), `:66-76` (confine, root and rootless), `:100` (conformance); `fuzz.yml`. | true |
| 22 | Releases are signed, ship an SBOM and provenance, and `install.sh` and `playbox update` refuse an unsigned one | README.md At a glance, Proof table | `.github/workflows/release.yml:56-86` (SBOM, attestation, signing); `install.sh:22, 84-89` (pinned key; `PLAYBOX_ALLOW_UNSIGNED=1` is the one override); `crates/playbox-cli/src/update.rs:269-273`. The workflow publishes unsigned if the secret is missing, which the installer then refuses. | true |
| 23 | Needs Rust 1.93.1; `up --local`, `join --local`, `hub` | README.md Try it | `rust-toolchain.toml`; `crates/playbox-cli/src/cli.rs:158` (`--local`), `:479-480` (`hub`). | true |
| 24 | ALPN `playbox/a2a/1`; frames capped at 256 MiB | docs/a2a-binding.md Streams | `crates/playbox-net/src/lib.rs:57` and `:60`. First request on an unproven connection: 64 KiB (`:64`), added to the doc. | true |
| 25 | The methods table lists the methods | docs/a2a-binding.md Methods | `wire.rs:41-46` also defines `playbox.GetAttestation` and `playbox.ActivateCredential`, both public. Added. | stale |
| 26 | Operations are echo, pull, push, run, browse, remember, recall | docs/a2a-binding.md Operations | The `Op` enum, `wire.rs:108-190`, has 21 operations: also say, feed, steer, handoff, latent, latent_reveal, latent_pull, conv_new/invite/post/read/list, cap_list, cap, cap_poll. Added. | stale |
| 27 | Grant rights are echo, pull, push, run, tasks | docs/a2a-binding.md Grants | `Op::name()`, `wire.rs:210-222`: also steer, steer_probe, handoff, browse; room talk, conversations, capabilities, memory and latent messages map to `echo`. Fixed. | stale |
| 28 | `CancelTask` is always "not cancelable" or "not found" | docs/a2a-binding.md Methods | `crates/playbox-net/src/lib.rs:825-836`. | true |
| 29 | `GetExtendedAgentCard` is "not supported" | docs/a2a-binding.md Methods | `lib.rs:842-844` answers a specific "extended card not configured" error. Fixed. | stale |
| 30 | Grant failures use JSON-RPC code -32040 | docs/a2a-binding.md Methods | `wire.rs:48`. | true |
| 31 | `playbox bridge` listens on 127.0.0.1:8787, needs a token off loopback, forwards the five A2A methods | docs/a2a-binding.md HTTP bridge | `crates/playbox-cli/src/rooms.rs:360`; `crates/playbox-bridge/src/lib.rs:236-246`; tests in `crates/playbox-bridge/tests/http.rs`. | true |
| 32 | `say`: ids 1 to 64 characters, 20 posts per 10 s, remembered an hour, 4096 ids | docs/a2a-binding.md Room talk | `crates/playbox-net/src/chat.rs:25-35`. | true |
| 33 | The installer checks the signature "once a key is pinned" | docs/cli.md Install | `install.sh:22` pins the key; an unsigned release is refused (`:84-89`). Fixed. | stale |
| 34 | The command table is complete | docs/cli.md | Missing `chat` (`cli.rs:316`), `update` (`:348`), `latent session` (`:375`), `setup --only` (`:345`), `up --tui` (`:266`); `doctor` listed twice. Fixed. | stale |
| 35 | Commands without a typed shell command (`checkpoint`, `receipts_check`, `lineage_check`) need `playbox call` | docs/cli.md | `crates/playbox-cli/src/session.rs:204-210`: every word is run as a tool name. `playbox call` starts a fresh process each time (`session.rs:93-100`). Fixed. | stale |
| 36 | `up --console` offers link, members, audit, revoke | docs/cli.md Rooms; cli.rs:262 help text | `rooms.rs:391-432`: link, members, audit, calls, approve, deny, revoke, quit. There is no `status`, which the `--console` help text in `cli.rs:262` still lists (source, not edited here). Doc fixed. | stale |
| 37 | `init`, `hub` and `secure` flags; 1 500 and 20 000 fuzz requests; exit statuses | docs/cli.md | `cli.rs:32-60, 64-98, 112-149`; `crates/playbox-cli/src/secure.rs:801`. | true |
| 38 | `playbox update` runs `secure` after every update | docs/cli.md | `update.rs:330-332` runs `secure --no-fuzz`. Fixed. | overstated |
| 39 | `--sync-interval` default 15; `--link-ips`; `--for-guest` | docs/cli.md, docs/revocation.md | `cli.rs:254, 260, 257`; `crates/playbox-mcp/src/lib.rs:922`. | true |
| 40 | "Not built: results signed by the host key" | docs/capabilities.md (last paragraph) | Contradicted by its own table and `crates/playbox-net/src/capabilities.rs:228-260` (`playbox.cap-result.v1`, `verify_result`), tagged `REQ: CAP-7` at `:797`. Fixed. | stale |
| 41 | Limits: 8 open calls, 256 stored, 16 arguments, 64 KiB output, 15 minute approval | docs/capabilities.md | `capabilities.rs:27-35`. | true |
| 42 | A call runs once; a running call becomes `abandoned` after a restart | docs/capabilities.md | `crates/playbox-net/tests/capabilities.rs:69`; conformance `restarts.py`; `REQ: CAP-3`, `CAP-8`. | true |
| 43 | Stranger limits: 64 KiB first request, 30 s, 256 connections, 8 per identity, 2 attestation calls | docs/threat-model.md section 1 | `lib.rs:64, 441-446, 1724`; tests `crates/playbox-net/tests/security.rs:244, 271, 284, 562`. | true |
| 44 | 2 tasks host-wide, 1 GiB pushes per member, scratch quota 1 GiB and 100 000 entries | docs/threat-model.md Resource abuse | `lib.rs:1718, 443`; `crates/playbox-runner/src/lib.rs:318-325`; tests `security.rs:695, 753`. | true |
| 45 | Bundles are fsck'd on import; `.git/hooks` trees are rejected | docs/threat-model.md section 4 | `crates/playbox-git/src/lib.rs:638`. | true |
| 46 | "Another session is building an admission broker" | docs/threat-model.md section 6 | `crates/playbox-core/src/taint.rs`; `docs/trust-layers.md`. Fixed. | stale |
| 47 | Eight fuzz targets | docs/threat-model.md Not covered | 12 targets in `fuzz/fuzz_targets/`. Fixed. | stale |
| 48 | Known gap 1: libkrun network has no filtering | docs/threat-model.md Known gaps | True without `--confine`; with it on Linux `crates/playbox-runner/src/netpolicy.rs` filters it (`docs/libkrun.md`). Fixed. | stale |
| 49 | Known gap 3: the libkrun helper is not sandboxed | docs/threat-model.md Known gaps | On Linux with `--confine`: `jail.rs`, `cgroup.rs`, `seccomp.rs`. Still true on macOS. Fixed. | stale |
| 50 | Known gap 8: "No confidential-VM (T3) verifier exists" | docs/threat-model.md Known gaps | `crates/playbox-core/src/confidential.rs`. Fixed. | stale |
| 51 | Our own code has no `unsafe` except the libkrun FFI | docs/threat-model.md Dependencies | `unsafe` also in `crates/playbox-runner/src/jail.rs:106, 119`, `seccomp.rs:127, 132`, `crates/playbox-cli/src/main.rs:27`. Fixed. | stale |
| 52 | All four advisories are in `.cargo/audit.toml` | docs/threat-model.md Dependencies | `.cargo/audit.toml:5-6` has two; `deny.toml:8-11` has all four. Fixed. | stale |
| 53 | Grants cannot be renewed (gap 7) | docs/threat-model.md Known gaps | No renewal code in `crates/` (`grep -ril renew`). | true |
| 54 | The audit log refuses to carry on after tampering | docs/threat-model.md Audit log | `crates/playbox-net/tests/audit.rs:197`. | true |
| 55 | Evaluator and receipts/witness are "library only" | docs/trust-layers.md table | MCP tools `playbox_holdout_*`, `playbox_evaluator_issue`, `playbox_variant_sign`, `playbox_evidence_*`, `playbox_lineage_check` (`lib.rs:366-414`), `playbox_receipts_check`, `playbox_checkpoint*` (`:346-362`). Fixed. | stale |
| 56 | Guarded tools: delegate, push, revoke | docs/trust-layers.md section 1 | `lib.rs:1310, 1372, 1456, 1500`: steer is guarded too. Fixed. | stale |
| 57 | "Not built: ... a real Confidential Space run" | docs/trust-layers.md Not built | Contradicted by the paragraph under section 2 (run on 2026-10-29). Fixed. | stale |
| 58 | Memory review and sharing "not exposed yet" | docs/trust-layers.md Not built | `playbox memory review` (`cli.rs:517`) and `playbox_memory_ingest` apply it; a separate room memory exists (`playbox_room_remember`, `lib.rs:238`). Fixed. | stale |
| 59 | T3 verifier: RS256 only, STABLE release, `PLAYBOX_CS_*` variables | docs/trust-layers.md section 2 | `confidential.rs:152, 217`; `platform.rs:1086-1173`. | true |
| 60 | Repeated failed redemptions freeze non-read actions | docs/trust-layers.md section 1 | `crates/playbox-core/src/taint.rs:344`, test `:648`. The count of three was not checked. | true |
| 61 | `attest::Policy` is not enforced by the host | docs/attestation.md Policy | `crates/playbox-core/src/attest.rs:472`; no caller outside tests. | true |
| 62 | T3 "untested against a real VM"; "SEV-SNP and TDX verifiers are open work"; "T3 has no verifier yet" | docs/attestation.md | `confidential.rs`, `platform.rs:1085`, and the recorded run in `docs/trust-layers.md`. SEV-SNP is still open. Fixed. | stale |
| 63 | T2 is "achievable today" | docs/attestation.md tier table | Only with a software TPM in tests (`crates/playbox-core/tests/tpm_fixtures.rs`); no hardware TPM. Qualified. | overstated |
| 64 | `min_host_tier` defaults to t0; pinning decides T1 | docs/attestation.md | `crates/playbox-mcp/src/lib.rs:182`; `crates/playbox-net/tests/attest.rs`. | true |
| 65 | Gossip alone delivers; pull repairs; late replicas catch up | docs/revocation.md Notices | `crates/playbox-net/tests/replica.rs:106, 136, 164`. | true |
| 66 | No automatic failover | docs/revocation.md Limits | `grep -ri failover crates` is empty; `playbox_use_host` is the only switch (`lib.rs:312`). | true |
| 67 | 5 messages per poll, 1500 characters, every 2 s, handshake first | docs/live-delivery.md | `crates/playbox-mcp/src/live.rs:21-22`; `crates/playbox-mcp/src/main.rs:17`; tests `crates/playbox-mcp/tests/live.rs:100, 144`; conformance `live_delivery.py` (`REQ: LIVE-1, LIVE-2`). | true |
| 68 | "Not tested against a running Claude Code" | docs/live-delivery.md | No test or record drives a real Claude Code with `PLAYBOX_LIVE=1`. Still true. | untested |
| 69 | `test_hub.py`: 60 checks; `e2e.py`: 24 checks | ui/README.md | `ui/tests/test_hub.py` has about 230 `check(` call sites, `ui/e2e.py` about 40. Fixed (no numbers). | stale |
| 70 | Pairing code 10 min, step-up 5 min, link 15 min to 24 h, grant 1 h to 7 d, 5 pending knocks, poll 8 s / 45 s, ten-minute bound link | ui/README.md | `ui/hub/auth.py:23-24`; `ui/hub/invites.py:11-12`; `ui/hub/knock.py:21-22`; `ui/hub/rooms.py:331`. | true |
| 71 | Hub flags `--tailscale`, `--cloudflare-tunnel`, `--cloudflare-access`, `--cloudflare-aud`, `--no-weak`; state in `~/.playbox-hub/hub.json` | ui/README.md | `ui/hub/server.py:1002-1020`; `ui/hub/store.py:20`. | true |
| 72 | Named Cloudflare tunnels and Access are untested; push to real push services is untested | ui/README.md | Stated by the page and still so: no test or record in `docs/tested-on.md`. `CHANGELOG.md:35` says friends can open invite links through Access, with no record of how it was checked. | untested |
| 73 | `docker: true` is the only way a member can run tasks | adapters/README.md Safety notes | `crates/playbox-mcp/src/lib.rs:139-141`: `runtime` takes libkrun, docker or openshell. Fixed. | stale |
| 74 | The config snippets (`playbox-mcp`) work after `install.sh` | adapters/README.md Configure your agent | The release archive holds only `playbox` (`.github/workflows/release.yml:42-46`); `playbox-mcp` needs `cargo install`. `playbox setup` writes `playbox mcp` (`setup.rs:97, 115`). Fixed. | overstated |
| 75 | Codex 0.160.1, OpenCode 1.19.4, Hermes 0.22.5 called `playbox_status`; a headless Claude Code ran a task | adapters/README.md Tested | A recorded manual run on 2026-10-29; no automated test (`crates/playbox-mcp/tests/mcp.rs` uses a scripted client). Now also in `docs/tested-on.md`. | untested |
| 76 | "145 tests on the last run" | docs/tested-on.md | 494 `#[test]` functions; `docs/proof-status.md`: 29 test binaries, 0 failed. Fixed. | stale |
| 77 | Not yet tested: T3, and Codex and OpenCode driving the MCP server | docs/tested-on.md | Both were run (see the rows above). Fixed, and the runs are recorded. | stale |
| 78 | v0 checklist: boxes | docs/v0-checklist.md | Ticked: libkrun backend, platform verifiers, MCP configs, threat model, protocol field, README limitations. Partial: image digests, seccomp/rootless, size caps, demo, errors, CI. Still open: image and min-isolation flags, anchored history head. Fixed. | stale |
| 79 | No MCP-level test calls `playbox_room_remember`, `playbox_room_recall` or `playbox_browse` (a gap in the tests, not a doc claim) | crates/playbox-mcp | The wire operations are tested (`crates/playbox-net/tests/shared_memory.rs`, `browse.rs`) but the MCP tool wrappers (`lib.rs:238-257`) are not called by any test or conformance scenario. | untested |

## The five most misleading claims

1. **A replica "failover in 0.3 s"** (README). There is no failover; a guest moved by hand with `playbox_use_host`. A reader would assume a room survives its primary going down (`ROOM-4` in the requirements register is `planned`).
2. **Adapters: "There is no free-form messaging tool yet" and a ten-tool list.** The server has 41 tools, including `say`, `feed` and conversations. The page told an agent author the room could not talk.
3. **"Every result file keeps a ceiling label."** No file under `steer/results/` has one; only the write-ups do. The sentence promised a safeguard that does not exist in the data.
4. **The T3 verifier "untested against a real VM", "No confidential-VM (T3) verifier exists", and "Not built: a real Confidential Space run"** in four docs, against the recorded 2026-10-29 TDX run. Readers got the opposite of the project's strongest hardware result.
5. **`capabilities.md`: "Not built: results signed by the host key"** in the same file that describes the signature and in code tagged `REQ: CAP-7`. A security reader would conclude results can be forged in transit.

## Claims nothing checks automatically

The `untested` rows are the list to turn into tests or recorded runs: the T3 join, the two-machine and India runs, the 10 ms revocation figure, the agent-loader runs, live delivery into a real Claude Code, Cloudflare Access with a named tunnel, Web Push to real services, and the MCP wrappers for room memory and browse.

## Proposed new rows for docs/requirements.md

Not added to the register (it belongs to another change). Each needs a test that carries `// REQ: ID` before the row can be `shipped`; until then they are `planned`.

| ID | Requirement | The test that would check it |
|---|---|---|
| HUB-1 | Every private hub route answers 401 to a stranger and the public invite paths answer without a session. | `ui/tests/test_hub.py`: walk the route table with no cookie and compare against the public-path list. |
| HUB-2 | A pairing code works once, within 10 minutes, and enrolls one passkey; step-up expires after 5 minutes. | `test_hub.py`: use a code twice, after `PAIR_TTL`, and a sensitive action after `PLAYBOX_HUB_FRESH` seconds. |
| HUB-3 | A weak (plain-HTTP) session can never enroll or remove a device, and is refused behind any proxy header. | `test_hub.py`: request with and without `Cf-Connecting-Ip` and `X-Forwarded-For` from a private address. |
| INV-1 | An approval invite carries no Playbox key; the minted link is bound to the asker's guest id, valid 10 minutes, and handed over once. | `test_hub.py`: knock, approve, fetch the link twice, redeem with another key. |
| INV-2 | Both sides compute the same six-digit code from (invite id, guest id), and the asker refuses a host whose code differs. | `test_hub.py` plus a unit test of `knock.verify_code` with a mismatching host reply. |
| INV-3 | A withdrawn, expired or spent invite cannot be redeemed, and at most 5 knocks are pending per invite. | `test_hub.py`: exhaust each state and the pending cap. |
| MEM-1 | A member's `remember` is stamped with the member's id by the host, and recall returns only entries by current members. | `crates/playbox-net/tests/shared_memory.rs` already has the cases; tag them. |
| MEM-2 | Revoking a member removes what they said from recall. | Tag `revoking_a_member_removes_what_they_said` (`shared_memory.rs:39`). |
| MEM-3 | With `--memory-review`, nothing is recalled until the host accepts it. | Tag `with_review_on_nothing_is_recalled_until_the_host_accepts` (`:61`). |
| MEM-4 | The MCP wrappers `playbox_room_remember` and `playbox_room_recall` reach the same host memory and refuse when the room has none. | A new `crates/playbox-mcp/tests/mcp.rs` case calling both tools against a host started with a memory stub. |
| LAT-1 | A latent message is accepted only if its signer is the sending member and its envelope parses; forged, cross-signed and malformed ones are refused. | Tag `forged_cross_signed_and_malformed_messages_are_refused` (`crates/playbox-net/tests/latent.rs:74`). |
| LAT-2 | An identical latent message is refused as a replay for `latent_max_age_secs`; a member is limited to `latent_posts_per_minute`. | `latent.rs` case with a short max age and a post burst. |
| LAT-3 | A reveal is accepted once, only from the sender, only if `hash(salt + sentences)` equals the signed commit. | `latent.rs`: reveal twice, from another member, with swapped sentences. |
| STEER-1 | A steering spec outside the grant's layers, coefficient or modes is refused, and a narrowed grant cannot widen them. | Tag the narrowing case near `crates/playbox-net/tests/steer.rs:574`. |
| STEER-2 | The same signed spec on the same weights gives the same digest on host and guest. | A test comparing `spec_digest` across two runs (the 2026-10-29 India run did this by hand). |
| STEER-3 | A model or direction whose hash is not in the host's store is refused. | `steer.rs`: a spec naming an unknown weights hash. |
| ATT-1 | A statement's evidence cannot be moved to another subject, nonce or position (the binding). | Tag the replay cases in `crates/playbox-core/tests/tpm_fixtures.rs`. |
| ATT-2 | A host below the guest's `min_host_tier` is refused before the invite is shown. | Tag the unpinned-then-pinned case in `crates/playbox-net/tests/attest.rs`. |
| ATT-3 | A T3 token with the wrong image digest or project is rejected and one with no digest pinned reports T1. | A unit test in `confidential.rs` over the stored fixture with altered claims. |
| ATT-4 | An unpinned device key stays at T0 and its fingerprint is reported. | `crates/playbox-mcp/tests/platform.rs` (trust on first use and pin persistence). |
| HIST-1 | Editing, removing or inserting a log entry is detected by `playbox verify`, and a host will not write into a log that fails the check. | Tag `crates/playbox-net/tests/audit.rs:197` and the CLI `verify` flipped-byte case. |
