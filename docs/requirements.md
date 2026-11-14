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
| HUB-1 | The hub starts answering without a reverse-DNS lookup, so a slow resolver cannot stall it. | shipped |
| **Command line** | | |
| CLI-1 | Arguments are typed, errors exit non-zero with a cause chain, and a closed pipe ends quietly. | shipped |
