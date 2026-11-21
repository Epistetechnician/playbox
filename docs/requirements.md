# Requirements

What Playbox promises, one line each, with a stable ID. Every `shipped` requirement is checked by at least one test or conformance
scenario that carries its ID in a `REQ: ID` comment; `python3 conformance/trace.py` fails CI when a shipped requirement has no check, when a
check names an ID that does not exist, or when a planned requirement already has one (so its state gets updated). The reasoning behind a
choice is in [decisions.md](decisions.md); what is not yet true is in the `planned` rows and nowhere else.

To add a promise: add a row here, write the test, tag it `// REQ: AREA-N` (or `# REQ:` in Python), run `python3 conformance/trace.py`.
IDs are never reused. A requirement that stops being true is marked `retired` with the reason, not deleted.

| ID | Requirement | State |
|---|---|---|
| **Grants** | | |
| GRANT-1 | A grant works only for the key it was issued to. | shipped |
| GRANT-2 | A member can narrow a grant offline and can never widen it. | shipped |
| GRANT-3 | A revoked member loses access at once and cannot rejoin with the same key. | shipped |
| GRANT-4 | An invite is not a grant. | shipped |
| **Rooms** | | |
| ROOM-1 | Members join, talk, retry a send without duplicating it, and are refused after revocation. | shipped |
| ROOM-5 | A guest goes from a join link to a confirmed round trip (join, say, the host sees it) within the join budget (180 s), measured end to end. | shipped |
| ROOM-6 | With `PLAYBOX_TOOLS=core` a session lists at most 7 tools, and `playbox_more` unlocks the rest by group. | shipped |
| ROOM-7 | A guest can see what a host is (verified tier, runner, device) before joining, without spending the invite; `playbox join` shows the same and asks on a terminal. | shipped |
| ROOM-8 | The CLI and the MCP sidecar judge a host with the same code (`playbox_net::vet`): a required tier a host does not reach refuses the join in both, and the invite stays unspent. | shipped |
| ROOM-9 | A room's log is signed (genesis by the root key, later entries by the named leader), hash-chained and bound to the room; a follower holding only the root public key rejects forged, reordered, gapped or foreign entries and replays the same roster. | shipped |
| ROOM-10 | A log entry counts as committed only after `k` (more than half) distinct hosts cosigned a checkpoint they replayed themselves; with no quorum nothing new commits. | shipped |
| ROOM-11 | A host that restarts keeps its room log and commit point, re-verified on load; a log file with one changed byte stops the host from starting, and `playbox room-log` checks a saved log offline. | shipped |
| ROOM-12 | When the leader is silent, a follower can take over with an epoch entry carrying `k` host signatures; a host signs only a bid whose log holds every checkpoint it cosigned and never two rival bids for one epoch, so nothing committed is lost and two leaders never both reach `k`. | shipped |
| ROOM-13 | Two different entries signed by one key at one epoch and index are a proof that anyone holding the room key can check offline; a host shown the second one keeps the proof and refuses the entry. | shipped |
| ROOM-14 | The leader can change the host set and threshold (add, remove, rotate a key) only with signatures from `k` hosts of the old set and `k` of the new one; a removed host's cosignatures count for the epochs it was in and for nothing after, and the threshold stays more than half. | shipped |
| ROOM-15 | A guest can read the room's host set from the signed log (checked from the room key, as of the committed point, with the key pinned at join) and, when its host stops answering, moves to another host of the room that takes its grant and passes the checks made at join, with no manual host switch. | shipped |
| ROOM-16 | A challenge promotion is a log entry valid only with receipts from at least two distinct host keys of the current epoch, each signed over the same problem, candidate and weights, agreeing within the problem's noise, with the lowest measurement beating the frontier by the stated gain; every host replaying the log re-checks this, so a leader cannot promote alone, and a promotion's strength is the lowest attestation tier among its signers. | shipped |
| ROOM-17 | A host's promotion receipt is what its own checker measured for the exact candidate the room named (re-hashed from the host's store): a candidate that fails the exact check, or whose stored file does not hash to its name, gets no receipt rather than a low one. | shipped |
| ROOM-2 | A host that is killed and started again keeps its room, members, talk and audit chain. | shipped |
| ROOM-3 | A guest bound to its own key is the same member each time it rejoins. | shipped |
| ROOM-4 | A multi-host room admits members through a replica when the primary is down. | planned |
| **Capabilities** | | |
| CAP-1 | Only members whose grant lists `cap/NAME` can call a capability. | shipped |
| CAP-2 | Arguments are bounded: declared, within length and allowed values, and with no control, bidi or zero-width characters. | shipped |
| CAP-3 | A `(member, call_id)` runs once, however often it is retried, across restarts. | shipped |
| CAP-4 | A call that needs approval does nothing until the owner approves, and expires if unanswered. | shipped |
| CAP-5 | Authority is checked again when an approved call starts and while it runs: a member revoked in between gets no run. | shipped |
| CAP-6 | A result is hashed over a canonical encoding and the hash is in the audit log. | shipped |
| CAP-7 | A result is signed by the host and verified by the caller against the host id it dialed. | shipped |
| CAP-8 | A call that was running when the host stopped is abandoned, never started again. | shipped |
| CAP-9 | Capability bounds are Biscuit checks, so a verifier needs no host config to read them. | planned |
| **Conversations** | | |
| CONV-1 | Only the roster can read or post; everyone else is refused. | shipped |
| CONV-2 | Every entry carries its author's signature, which readers can verify. | shipped |
| CONV-3 | A retried post is stored once, and conversations survive a host restart. | shipped |
| CONV-4 | Attachments, removal and per-author clocks. | planned |
| **Live delivery** | | |
| LIVE-1 | Nothing is pushed into an agent before it completes the handshake. | shipped |
| LIVE-2 | Only new messages from others are pushed, once each, framed as untrusted. | shipped |
| **Sandbox** | | |
| BOX-1 | A task reads its granted secrets and they never reach the workspace or the record. | shipped |
| BOX-2 | The jail, the run cgroup, the syscall filter and the network filter each hold, rootless where the kernel allows. | shipped |
| BOX-3 | The whole confinement holds around a real libkrun guest on KVM. | shipped |
| BOX-4 | The syscall filter is an allow-list measured on a real workload. | planned |
| BOX-5 | A libkrun task that asks for a network is refused on a host that cannot filter it, unless the owner opts in. | shipped |
| BOX-6 | A battery of breakout attempts run from inside a real libkrun guest all fail. | shipped |
| BOX-8 | A libkrun task that has a network can resolve names: a task with a host list gets its pinned addresses by name, any other a resolver it is allowed to reach. | shipped |
| BOX-7 | A task that has a network can reach only the model API through a host-side proxy, and every outbound request is logged. | shipped |
| **Releases** | | |
| REL-1 | `playbox update` installs only an archive whose SHA-256 is in `SHA256SUMS`. | shipped |
| REL-2 | A release without a valid signature from a pinned key is refused by `update` and by `install.sh`. | shipped |
| REL-3 | A refused or failed update leaves the installed binary exactly as it was. | shipped |
| REL-4 | `playbox update --rollback` restores the previous binary. | shipped |
| REL-5 | A tag that does not match `Cargo.toml` and have changelog entries cannot be released. | shipped |
| REL-6 | An installed older release upgrades in place and its room, member and audit chain carry over. | shipped |
| REL-7 | The current build and the previous release work together in one room, both ways. | shipped |
| REL-8 | A host on a wire protocol this build cannot speak is refused with the side that must update. | shipped |
| REL-9 | The installer refuses a hub bundle that does not match its checksum. | shipped |
| REL-10 | Builds are reproducible: the same commit gives the same archive hash. | planned |
| **Hub** | | |
| HUB-1 | The hub starts answering without a reverse-DNS lookup, so a slow resolver cannot stall it. | shipped |
| HUB-2 | A passkey is the only way to a full hub session; a pairing code works once and then expires. | shipped |
| HUB-3 | Revoking a member and removing a device or a room need a fresh passkey touch (waived only on the hub machine itself). | shipped |
| HUB-4 | A room with no repository hosts, mints invites, and is hosted again after the hub restarts. | shipped |
| **Invites** | | |
| INV-1 | An approval invite carries no Playbox key; the host approves each asker against a matching verify code, and the link is bound to that asker and handed over once. | shipped |
| INV-2 | The hub never stores a join link, and the public invite page shows metadata only. | shipped |
| INV-3 | An invite works for as many people as it says and no more, and withdrawing it closes it. | shipped |
| **Room memory** | | |
| MEM-1 | An entry is recalled with the author the host stamped from the member's key, and posting it again stores it once. | shipped |
| MEM-2 | Revoking a member deletes their entries from the index and hides them from recall at once. | shipped |
| MEM-3 | With review on, nothing pending reaches the index or a recall until the host accepts it, and a quarantined entry cannot be posted back. | shipped |
| MEM-4 | Recall drops anything the host's ledger does not vouch for: unknown documents, derived observations, entries of non-members. | shipped |
| MEM-5 | The bridge works against an unmodified Hindsight server (an opt-in test, run by hand). | shipped |
| MEM-6 | Consolidation never blends two authors' claims into one recalled belief. | planned |
| **Identity** | | |
| IDENT-1 | An agent card counts only if it is signed by the key that sent it, names this room, has not run out, and lives at most 24 hours. | shipped |
| IDENT-2 | A claim that is not a verified card (an unsigned `::pbx agent` message) never becomes a member's identity. | shipped |
| IDENT-3 | An owner's endorsement is per room, bound to the card's content, and needs a fresh passkey touch; a changed card shows as changed. | shipped |
| IDENT-4 | The host records how each member was admitted (invite, limits, expiry), never a token. | shipped |
| IDENT-5 | An owner revoking a member from the hub leaves an entry in the security trail. | shipped |
| IDENT-6 | The Harnesses and Timeline views show identity: a state per member, and card events as their own kind. | shipped |
| IDENT-7 | Only a host-signed launch record can show a harness or model as host-observed; a member's own, another room's or a tampered record is ignored, and a card that disagrees with the record is flagged. | shipped |
| IDENT-8 | An ANS name shows as registered only after a receipt is checked offline against a log key the owner pinned, for that exact name; the receipt never claims the agent holds the name. | shipped |
| IDENT-9 | `playbox launch` starts an agent with a key the host's owner made, and the host-signed record names that key and the executable's digest; the host will not vouch for a key that is not a member. | shipped |
| **Command line** | | |
| CLI-1 | Arguments are typed, errors exit non-zero with a cause chain, and a closed pipe ends quietly. | shipped |
| **Model gateway (`playbox-aiproxy`)** | | |
| PROXY-1 | Rate-limited (`429`) upstream channels enter short clamped cooldown and fail over cleanly; `same_model_only` refuses cross-family model substitution while `alias_cascade` resolves configured aliases. | shipped |
| PROXY-2 | Per-worker gateway tokens enforce hard pre-flight and mid-stream USD caps, cutting off runaway output with `X-Playbox-Stop-Reason: budget_exhausted`. | shipped |
| PROXY-3 | Host-observed usage is exported as canonical `playbox.llm_ledger.v1` with deterministic SHA-256 bound into hybrid `Ed25519+ML-DSA-65` receipts; unproxied runs explicitly report `client_unverified`. | shipped |
| PROXY-4 | `BOX-7` strips guest-supplied `x-playbox-*` headers, injects host correlation headers, and blocks `/api/*`, `/mcp/*`, and `/swagger/*` paths on the `aiproxy_preset` upstream. | shipped |
| PROXY-5 | Gateway and Hub logs default to metadata-only; opted-in body inspection scrubs secrets and requires fresh passkey step-up in the Hub. | shipped |
| PROXY-6 | `playbox_atlas` hybrid dense + rerank retrieval runs strictly after `TRUSTED_TIERS` quarantine gating, caches embeddings by content SHA-256, and falls back to TF-IDF when offline. | shipped |
| PROXY-7 | Hardened `playbox-aiproxy` (`-tags playbox_hardened`) compiles out MCP servers, Web Search, Swagger, and public UI, and refuses non-loopback binds without explicit override. | shipped |
