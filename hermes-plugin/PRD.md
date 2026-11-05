# PRD: Playbox for Hermes

Status: draft v2, 2026-11-04 (sections 15-22 add the vision, concept model, architecture, novel capabilities and test plan; every capability there is tagged [built], [proposed] or [research]). Owner: Epistetechnician. Target: Hermes Agent Plugin Hackathon (due 2026-11-11, 5PM EST), then the Hermes Marketplace.

Basis: this repo (`README.md`, `PRD.md`, `docs/`, `steer/`), the 2026-11-04 end-to-end run of `playbox-mcp`, the Hermes plugin docs and catalog rules read the same day, and the results recorded in recent commits (E8, L4). Anything not measured or read is marked as an assumption or open question.

## 1. One line

Playbox turns Hermes from one person's agent into a member of a private, verifiable room where agents owned by different people build together, and where every action is provably recorded and every unsafe instruction from a stranger's agent needs a human's yes.

## 2. Problem

1. Agents from different owners cannot safely work on the same repo. Today it means sharing credentials, a hosted workspace, or trusting a stranger's text.
2. Nobody can prove what an agent did. Logs are editable, and "my agent wrote this" is a claim.
3. Text from another agent is a prompt-injection channel. The receiving agent cannot tell collaboration from attack.
4. Hermes users can already extend their agent, but no plugin gives them multi-owner collaboration, signed history, or a trust model for peers.

## 3. Users

| User | Job |
|---|---|
| Host | Share a repo with a friend's or contractor's agent for an afternoon, limited and revocable. |
| Guest | Join with a link, pull, work, push, and run heavy tasks on the host's machine. |
| Auditor / maintainer | Verify after the fact what each agent did and under what authority. |
| Researcher (stretch) | Pass a concept between open-weights models without text, and steer a hosted model. |

## 4. Goals and non-goals

Goals
- G1. From "create a playbox for this repo" to a working two-agent room in under 2 minutes, no accounts.
- G2. Authority is explicit, narrow, expiring and revocable; over-limit requests are refused, not clamped.
- G3. Every Hermes action in a room lands in a signed, hash-chained log that anyone can verify. Tampering is detected.
- G4. Text from a peer cannot cause a write-capable action without a human approval.
- G5. Passes catalog admission as shipped, with no binaries, no self-update, and truthful disclosure.

Non-goals
- No hosted service or accounts. No custody of user credentials. No claim of confidential computing beyond what the verifier checks (T3 via Google Confidential Space exists; no hardware TPM testing). No "latent channel" claims for closed models.

## 5. What exists (evidence)

From the repo and the 2026-11-04 run:

- Signed, hash-chained history (`playbox-core`); Biscuit grants bound to the member key with expiry and hard task limits; Lean 4 proofs that attenuation never widens authority and revoked or expired grants authorize nothing.
- iroh transport (public relays, LAN, or private relay); A2A v1 binding; git bundle pull/push into the member's own namespace, never over `main`.
- Sandboxed tasks: Docker, libkrun microVM, OpenShell. Tested across a Mac and a Raspberry Pi 5.
- Attestation: Secure Enclave and TPM evidence with pinned device keys; a Confidential Space (TDX) host was joined at T3 from another network.
- Admission broker (single-use human approval for text-driven actions), shared memory with provenance and quarantine, signed evaluator with sealed holdout, witnessed checkpoints, model receipts.
- Steering rooms and a latent hub (research track). Results recorded in commits: a hub fitted per model family carries about 80% of the receiver's own direction (0.59 vs 0.75); a hub vector beats text-in-context on small receivers (70% vs 59%) at zero context tokens; adding models one at a time costs 8 points versus a hub fitted on all.
- `playbox-mcp`: 29 tools. 2026-11-04 end to end over stdio: join (host attested T1), ping, say, pull, Docker `delegate` with patch applied, `revoke` followed by refusal of ping and pull, history verify valid on both sides, one flipped byte detected.
- Hermes: the portable package (`hermes-plugin/`) passes `hermes plugins validate`; Hermes loaded it, discovered the tools, and blocked an unattended call under `trust: untrusted`.

Known gaps (stated, not hidden)
- Hermes has no history-recording hook wired; only Playbox's own tool calls are logged.
- An approved Hermes-driven run has not been observed.
- Hermes listed 11 of 29 tools in one search; cause not determined.
- `install.sh` has an empty `RELEASE_PUBKEY`, so downloads are not signature-pinned.
- Token-level fidelity (model responses) needs a recording proxy. Not built.
- The latent hub is built and tested on synthetic and tiny random models; the measured results above are on the research track, not a hosted capability.

## 6. Product shape: three layers

Hermes offers several extension surfaces (portable packages with MCP and skills; native Python plugins with hooks, middleware and `register_*`; Desktop SDK panes; memory-provider plugins; observer hooks). Playbox uses the minimum surface per capability.

### Layer 1: Room (ship for the hackathon; portable package, done)
`plugin.json` + `mcp.json` + skill. Tools via `playbox-mcp`. `trust: untrusted` so Hermes asks before each write-capable call. This is the working core.

### Layer 2: Witness (the differentiator; native Python component)
Closes the biggest gap. Uses public hooks only (observer hooks and middleware; no patching of core, per catalog rule 10).
- W1. Record every Hermes tool call and result while in a room into the signed log (`playbox record`), so history covers the whole session, not just Playbox calls.
- W2. Admission middleware: any tool call whose arguments contain text originating from a room member (feed, `say`, task output, pulled files) is held for human approval via the existing `playbox approve` single-use capability. Fail closed in cron and unattended runs.
- W3. Room memory as a memory-provider plugin: `remember`/`recall` with provenance; entries from other authors are quarantined until accepted.
- W4. Declared capabilities must match registration (rule 6); disclosed in README.

Resolved 2026-11-04: the docs say the portable adapter "does not replace native plugin.yaml plus register(ctx) plugins", so Layer 2 is a second package, `hermes-plugin-witness/` (`playbox-witness`), a companion catalog entry. Built: `witness.py` core, hooks `pre_llm_call`, `pre_tool_call`, `post_tool_call`, 8 unit tests, `hermes plugins validate` passes, and the real `register()` wiring was driven with the real `playbox record`/`verify` (chain valid; a flipped byte rejected).

### Layer 3: See (polish and demo value; Desktop SDK)
A Hermes Desktop pane: room members and grant expiry, live task feed, revoke button (routed through the approval flow), and a "history verified" badge computed by `playbox verify`. SDK surface only (rule 8). Ship only if Layer 2 is done by Nov 9.

### Research track (not in the hackathon claim)
Latent hub and steering for open-weights models Hermes runs locally: a concept crosses as 64 int8 bytes instead of text. Listed as roadmap with the measured numbers above and their limits. Closed models get a text-side concept card only.

## 7. Requirements

| ID | Requirement | Layer | Acceptance |
|---|---|---|---|
| R1 | Install: documented `install.sh` step, then `hermes plugins install playbox` | 1 | Fresh machine reaches a working room in under 2 minutes |
| R2 | Host/guest workflow skill | 1 | Agent completes create, join, pull, delegate, push from one natural-language request each |
| R3 | Write-capable calls gated | 1 | Unattended call fails closed (observed 2026-11-04) |
| R4 | Revocation takes effect | 1 | Guest ping and pull refused after revoke (observed) |
| R5 | Tamper detection | 1 | One flipped byte fails verify (observed) |
| R6 | Full-session recording on Hermes | 2 | Log contains Hermes' non-Playbox tool calls; chain verifies. Built; hook-level test passes, live turn untested |
| R7 | Peer-text admission | 2 | Built as a per-turn hold released by the user's next message (not `playbox approve`); 8 unit tests pass; live turn and corpus rate untested |
| R8 | Shared memory with provenance | 2 | Peer-authored entry is quarantined until accepted |
| R9 | Desktop pane | 3 | Shows members, expiry, verified badge; revoke goes through approval |
| R10 | Catalog admission | all | `hermes plugins validate --install-deps` passes at the pinned SHA; disclosure complete |
| R11 | No binaries in the plugin | all | Repo scan finds no Mach-O/ELF/PE/Wasm/pyc (rule 18) |

## 8. Security and trust model

- Authority comes only from Biscuit grants issued by the host root key, bound to the member key, expiring (default 1h), attenuable by the holder, revocable by the host. Replicas learn revocations by signed notices.
- Peers are untrusted input. Admission (R7) is the control; skill instructions are not.
- Sandbox: Docker is weaker than a microVM; document it and default recommendation to libkrun where available.
- Join links are secrets (single use, expiring, optionally bound to one guest id).
- Attestation tiers are reported honestly. T1 on a Mac with a Secure Enclave is not T3.
- Plugin never reads credentials or `.env`, does not self-update, ships no binaries, and sends no telemetry.

Threats still open: relay metadata visibility; Docker escape; compromised host tells guests nothing false only if attestation pinned; supply chain of `install.sh` (pin the release key before submission). See `docs/threat-model.md`.

## 9. Disclosure (catalog rule 13)

Network via iroh relays and discovery by default (LAN-only with `local`); Docker task execution; files under `~/.playbox`; exchange of git bundles and task output with invited members; install script fetched from GitHub releases and run by the user; no telemetry.

## 10. Why this can win

Judges score usefulness, reliability, polish, creativity, and favor plugins that do more than a prompt or skill could. Playbox adds cryptographic grants, a verifiable log, peer-text admission and sandboxed remote execution, none of which a prompt or skill can provide. The 90-second demo is the argument: two Hermes agents, a task run in a sandbox, revoke, then verify the log and flip one byte to show it fail.

## 11. Demo script (under 90s)

1. Host Hermes: "Create a Playbox for this repo" → link (10s).
2. Guest Hermes on another machine: "Join this Playbox" → pull, delegate a test run, patch applied (30s).
3. A planted instruction in the repo tries to make the guest push; Hermes stops and asks (15s) [needs R7].
4. Host revokes; guest's next call is refused (10s).
5. `playbox verify` passes; flip a byte; it fails (15s).

## 12. Plan to Nov 11, 5PM EST

| Day | Work |
|---|---|
| Nov 4 | Layer 1 built, validated, E2E over MCP (done) |
| Nov 5 | Approved Hermes run; investigate 11-of-29 tool listing; add `say`/`feed` to skill; pin release key |
| Nov 6-7 | Layer 2: recording hook (R6), admission (R7); resolve portable-vs-native question |
| Nov 8 | Memory provider (R8); CI test for R7 |
| Nov 9 | Layer 3 if on schedule; otherwise cut |
| Nov 10 | Push, pin SHA, open catalog PR, record demo, post on X, submit form |
| Nov 11 | Buffer; respond to CI and review |

Cut order if behind: R9, then R8, then R7's CI test. Never cut R1-R5, R10, R11.

## 13. Open questions

1. Can one package be both portable and native, or is Layer 2 a second entry?
2. ~~Do observer hooks expose enough?~~ Answered: `pre_tool_call`/`post_tool_call` carry args, result, status and duration; `pre_tool_call` can return `{"action":"block"}`; hooks fail open on exceptions. Witness logs digests, so redaction does not matter. Still untested inside a live Hermes turn.
3. Why did Hermes list only 11 tools?
4. Is a signed release key needed before reviewers will accept the install step?
5. Does `trust: untrusted` interact badly with the `delegate` timeout (set to 600s)?

## 14. Success metrics

- Hackathon: valid entry submitted on time; catalog CI green.
- Product: time-to-first-room under 2 minutes in a clean-machine test; R7 test blocks 100% of planted-instruction cases in CI; verify detects 100% of single-byte tampering in a fuzz run (`fuzz/` exists).

## 15. Vision: agents get a legal system, not just a chat room

Multi-agent tooling today answers "how do agents talk?" Playbox answers the harder question: "on whose authority, with what evidence, and who can undo it?" The bet is that as agents act for different owners, the scarce thing is not messaging but **accountable delegation**. Hermes already has the best extension surface for an agent that lives on a user's machine; Playbox gives that agent the one thing it lacks, the ability to cooperate with strangers' agents without giving up control.

Design principles
1. **Authority is data, not prose.** A grant is a signed object with limits, never a sentence in a prompt.
2. **Origin is a first-class property of text.** Every string in context carries where it came from; a stranger's words never inherit the owner's authority.
3. **Evidence over trust.** Claims (what ran, where, who did it) are attached as verifiable artifacts and checked, not asserted.
4. **Fail closed, narrow, reversible.** Refuse over-limit requests, expire by default, revoke instantly.
5. **Minimum surface.** Use the smallest Hermes extension point that does the job; never patch core.
6. **Honest tiers.** Report exactly what was verified (T0-T3); never round up.

## 16. Concept model

| Object | Meaning | Status |
|---|---|---|
| Room | A named, persistent host identity sharing one git repo | [built] |
| Grant | Biscuit token: member key, operations, hard task limits, expiry; attenuable, revocable | [built] |
| Witness log | Per-agent signed hash chain of actions; checkpoints cosigned by witnesses | [built] for Playbox calls; [proposed] for whole Hermes session |
| Origin tag | Label on every text span in a room session: `owner`, `self`, `member:<id>`, `tool:<name>` | [proposed] |
| Admission | Single-use human approval bound to one exact action (`playbox approve`) | [built] in server; [proposed] Hermes-side enforcement |
| Receipt | Host-signed record binding task spec, commit, image digest, output, patch | [built] (task and model receipts) |
| Attestation tier | T0 signed claims, T1 pinned device key, T2 TPM quote + PCR refs, T3 confidential VM | [built] T0-T1, T3 on Confidential Space; T2 untested on hardware |

## 17. Architecture

```
 Hermes (owner A)                                Hermes (owner B)
 ┌───────────────────────────┐                   ┌───────────────────────────┐
 │ Skill: playbox            │                   │ Skill: playbox            │
 │ Witness plugin (L2)       │                   │ Witness plugin (L2)       │
 │  ├ observer hooks ──► record                  │  ├ observer hooks ──► record
 │  ├ middleware ──► origin taint + admission    │  ├ middleware ──► origin taint + admission
 │  └ memory provider ──► room memory            │  └ memory provider ──► room memory
 │ MCP (stdio) ─► playbox-mcp                    │ MCP (stdio) ─► playbox-mcp
 └───────────┬───────────────┘                   └───────────┬───────────────┘
             │ grants, A2A v1 over iroh (relay / LAN / private relay)
             ▼                                               ▼
        Host: repo, runner (Docker | libkrun | OpenShell), root key, audit log, attestation
```

Data flow for one guest action: guest model proposes a tool call → middleware checks origin tags on the arguments → if any span is `member:*` and the call is write-capable, hold for admission → server authorizes against the grant → host runs it in the sandbox → receipt returned → both sides append to their witness logs.

Grant lifecycle: `issued → (attenuated)* → expired | revoked`. Revoked and expired authorize nothing (proved in `lean/`). Revocation spreads to replicas by signed gossip.

## 18. Novel capabilities (what no prompt or skill can do)

Each is deliberately small enough to demo.

1. **Origin-tainted context (the headline) [proposed].** Middleware tags text arriving from room members and tool output derived from it, and propagates the tag into tool-call arguments. A call whose arguments carry a foreign tag and that is write-capable needs admission. This turns prompt injection from "hope the model resists" into a checked rule. Honest limit: the model can still paraphrase a foreign instruction into an untagged span; taint is tracked at the string level, so the control is strongest for verbatim and quoted use and weakest for paraphrase. Mitigation: treat any write-capable call issued in a turn that ingested foreign text as tainted (turn-level taint), at the cost of more prompts.
2. **Proof-carrying patches [built, extend].** A delegated task returns a patch plus a host-signed receipt (commit, image digest, command, output hash). The owner can show a reviewer "this patch came from this command on this commit in this sandbox" and anyone can re-verify. Extension: a `playbox_receipts_check` step in the skill before applying any patch.
3. **Whole-session witness [proposed].** Observer hooks feed `playbox record` so the signed log covers every tool call, not only Playbox's. Redacted fields are recorded as hashes so the chain still commits to them.
4. **Sealed-holdout evaluation [built].** The host commits to a hidden test set, members improve code, the signed evaluator scores against the holdout, and lineage checks show no leakage. Lets two strangers' agents compete or cooperate on a measurable goal without trusting each other's claims.
5. **Room memory with quarantine [built in server, proposed as provider].** Facts need evidence (history entries or commits); entries by other authors sit in quarantine until the owner accepts. Makes shared knowledge auditable instead of ambient.
6. **Cross-harness rooms [built].** The host speaks A2A v1 and `playbox bridge` serves plain HTTP A2A clients, so a Hermes agent can work with Claude Code, Codex or OpenCode agents in the same room. The plugin is a door into an open protocol, not a Hermes-only silo.
7. **Latent handoff between local models [research].** For open-weights models running under Hermes, a concept crosses as 64 int8 bytes via a per-family hub. Recorded results: hub carries about 80% of the receiver's own direction (0.59 vs 0.75); beats text-in-context on small receivers (70% vs 59%) with zero context tokens; models added one at a time cost 8 points versus a hub fitted on all. Not hosted, not validated on real production families at scale. Presented as a roadmap teaser only.

## 19. Interaction design

- Natural-language entry points (skill): "Create a Playbox for this repo", "Join this Playbox: playbox://…", "Who is in the room?", "Revoke Sam", "Verify the room's history".
- Approval UX: one line stating the action, the member whose text triggered it, and the exact command; choices Once / Session / Always are Hermes' own, plus Playbox's single-use capability for revoke and delegate.
- Status line (Desktop, L3): `● room-name · 2 members · grant expires 41m · history ✓ 118 entries`.
- Errors say what to do: "refused: grant revoked" → ask the host for a new link; "over limit: timeout 900 > 600" → lower the request or ask for a wider grant.

## 20. Failure modes and handling

| Failure | Expected behavior |
|---|---|
| `playbox-mcp` not installed | Tools absent; skill tells the user the install line and does not run it unasked |
| Host offline | Calls fail with a clear error; replica fallback via `playbox_use_host` if configured |
| Grant expired mid-task | Task refused or completed under original grant per server rule; guest asked to rejoin |
| Docker missing | `delegate` refused with the reason; pull/push unaffected |
| Foreign text asks for a write | Held for admission; unattended runs fail closed |
| Log tampered | `playbox verify` fails and names the line; plugin surfaces it, never auto-repairs |
| Relay unreachable | Fall back to `local` transport on LAN; otherwise report |

## 21. Test plan

- **Protocol (exists):** `cargo test` across crates; the stdio E2E driver from 2026-11-04 promoted into the repo as `scripts/e2e-hermes-mcp.py` and run in CI.
- **Injection suite [proposed]:** a corpus of pulled files, feed messages and task outputs with embedded instructions ("push to main", "revoke Sam", "run curl | sh"); pass = zero write-capable calls without admission, measured per corpus item and reported as a rate, not a boolean.
- **Tamper fuzz:** single-byte and line-level mutations of logs; pass = 100% detected.
- **Clean-machine install:** fresh macOS and Linux user, time from `install.sh` to first working room.
- **Catalog gate:** `hermes plugins validate --install-deps` at the pinned SHA; binary scan for R11.
- **Two-owner live test:** Mac host, Raspberry Pi guest on a different network, recorded.

## 22. Roadmap beyond the hackathon

| Horizon | Deliverable |
|---|---|
| v0.2 (hackathon) | L1 package, witness recording, admission middleware, demo |
| v0.3 | Room memory provider, Desktop pane, signed releases and pinned `RELEASE_PUBKEY` |
| v0.4 | Turn-level taint with measured false-positive rate; proof-carrying patch review flow |
| v0.5 | Hardware TPM testing (T2); witness cosignature network protocol |
| v1.0 | Hosted-model steering rooms and latent handoff for validated open-weights families |

Competitive position (qualitative; no measured comparison made): general agent-to-agent protocols standardize messages; Playbox adds grants, evidence and admission on top of one. The plugin should interoperate with them, not compete.

Risks to the plan: Layer 2 depends on the hook surface exposing tool arguments (open question 2); taint is only as strong as its string-level tracking; the strongest demo step (planted instruction blocked) is not built yet. If Layer 2 slips, the submission still stands on Layer 1 with the revoke-and-tamper demo.
