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
| BOX-3 | The whole confinement holds around a real libkrun guest on KVM. | planned |
| BOX-4 | The syscall filter is an allow-list measured on a real workload. | planned |
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
| HUB-1 | A passkey is the only way to a full hub session; a pairing code works once and then expires. | shipped |
| HUB-2 | Revoking a member and removing a device or a room need a fresh passkey touch (waived only on the hub machine itself). | shipped |
| HUB-3 | A room with no repository hosts, mints invites, and is hosted again after the hub restarts. | shipped |
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
| **Command line** | | |
| CLI-1 | Arguments are typed, errors exit non-zero with a cause chain, and a closed pipe ends quietly. | shipped |
