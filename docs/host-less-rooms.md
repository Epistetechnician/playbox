# Easy to join, hard to kill: a core tool profile, one-command join, and host-less rooms

Status: **design, not built.** Written 2026-11-17 after comparing playbox with Reason Network, pingpong, Adapterscan and Index Network. Claims about those products come from their own pages and are unverified. Claims about playbox come from the files linked below.

Two parts, built in order:

- **Part A: adoption.** A small default tool surface and a join path measured in minutes.
- **Part B: host-less rooms.** Today a room dies or stalls with its host. After this, any member holding the log can carry the room on, no single machine can lie unnoticed, and the root key is no longer one file on one disk.

Part B is the point. Part A is cheap and makes Part B worth having, because a room nobody joins needs no failover.

---

## What is true today

| Fact | Source |
|---|---|
| About 40 MCP tools are dispatched, all listed to every agent | `crates/playbox-mcp/src/core.rs:204-245` |
| Joining already works in one command: `install.sh ... --join 'playbox://...'` installs, registers agents, joins and opens the pairing page | `install.sh` |
| The join link names one primary host (address, invite, transport) | `crates/playbox-net/src/link.rs` |
| One root signing key on one machine admits, grants and revokes. No threshold, no rotation | [revocation.md](revocation.md), Limits |
| A replica knows only the root *public* key. It honours grants and revocation notices (gossip plus pull) but cannot admit, grant or revoke | [revocation.md](revocation.md) |
| A replica has its own repo and runner, does not mirror the primary's, and the primary's member list is not replicated | [revocation.md](revocation.md) |
| There is no automatic failover. A guest picks a host with `playbox_use_host` | [revocation.md](revocation.md) |
| The room log is a signed, hash-chained, append-only text file. Members mirror it to detect tampering | `crates/playbox-core/src/history.rs`, [decisions.md](decisions.md) #4, #14 |
| `Checkpoint` and `Witness` types exist: the log key signs `(size, head)` and a witness cosigns after replaying. Nothing collects cosignatures | [trust-layers.md](trust-layers.md) §5 |
| Two-key promotion is specified, but the "second host" is a second sealed seed on the same machine | [challenge.md](challenge.md) |
| A malicious host can lie about what it ran unless its tier is verified | README, What is not proven |

---

## Part A: adoption

### Goal

A stranger with Claude Code or Codex goes from a link to a first message in a room in **under 3 minutes**, and their agent sees **at most 7 playbox tools** unless it asks for more. Both numbers are checked in CI (see Acceptance).

### A1. A core tool profile

**Built (opt-in).** `PLAYBOX_TOOLS=core` lists 7 tools: `create`, `join`, `status`, `say`, `feed`, `delegate`, `more`. `playbox_more` unlocks a group (`work`, `memory`, `proofs`, `room`). Hidden tools stay callable; the grant is the boundary. The default is still `full` so existing setups and plugins do not change; flipping the default for guests, and having `setup` write the env var, is next. `playbox_ask` is not built. Tests: `crates/playbox-mcp/tests/mcp.rs`, `src/tools.rs`.

Listing 40 tools costs every agent context on every turn, and most agents need five. Add a profile selected by `PLAYBOX_TOOLS`:

| Profile | Tools | Default when |
|---|---|---|
| `core` | `playbox_create`, `playbox_join`, `playbox_status`, `playbox_say`, `playbox_feed`, `playbox_delegate`, `playbox_more` (planned: `playbox_ask`) | The agent joined as a guest |
| `full` | Everything today | The agent is a host (`playbox_create`), or `PLAYBOX_TOOLS=full` |

`playbox_more(group)` returns the next group's tool list on request (`work`, `memory`, `proofs`, `admin`) and the server sends `notifications/tools/list_changed` so the agent re-lists. This keeps one server and no flag-day change for existing users: `full` stays available and hosts keep it.

Why these: the Reason Network page describes four tools (`list_team`, `send_message`, `check_inbox`, `ask_user`). That is the bar for "an agent can use this without reading a manual". We add `delegate` because running a sandboxed task for someone else is what playbox does and Reason does not.

Open: `playbox_ask` needs a place to show the question. The hub PWA and the MCP App card already exist (`crates/playbox-mcp/src/room_card.html`); reuse them, fall back to a terminal prompt.

### A2. A measured join path

**Built (item 1).** `conformance/scenarios/join_time.py` (requirements ROOM-5, ROOM-6) times join to host-confirmed first message and checks the core tool list. Local transport, prebuilt binaries: 0.07 s here, budget 180 s. Items 2 and 3 are not built, and the iroh/relay path and install time are not measured.

No new mechanism. The one-liner exists. Work needed:

1. A **conformance test** that starts a host, runs the guest path against the built binaries, and asserts first message round trip. Record wall time. Budget: 180 s on a warm cache, with the download excluded and reported separately.
**Built (item 2, as found in the code, not as first written).** `install.sh --join` does not call `playbox join`: it starts the hub, which joins through the sidecar's `playbox_join`. The CLI had its own, weaker host check (T0 only, no pinned devices, no tier requirement). The shared piece is now `playbox_net::vet::vet_host`, used by the sidecar (join, reconnect, `use_host`) and by `playbox join` (including `--via`), with `--min-host-tier` and `--trust-host-device` on the CLI. Requirement ROOM-8, scenario `join_cli`. The two entry points (hub pairing page versus one-shot CLI) are still separate by design; they now judge a host identically.

2. A `playbox join <link>` that does everything `install.sh --join` does when the binary is already installed, with the same output. Today the install script does the registering. Keep one code path, called from both.
**Built (item 3, partly).** `playbox join` now connects, checks the host's attestation, prints verified and claimed tier, runner, OS, device and notes, and asks `[y/N]` on a terminal (`--yes` skips; off a terminal it prints and proceeds, so existing scripts keep working). `playbox_join` takes `preview: true`: it reports the same and spends no invite, so an agent can show its human first. **Not shown:** the grant's limits, because the host issues the grant only on join; showing them before needs a host change (publish the limits in the attestation or card). Requirement ROOM-7, scenario `join_consent`. The interactive prompt itself was not exercised (no TTY here).

3. Print the **tier and grant before consent**: "This host is T1 (software attested). You will get: 1 CPU, 10 min tasks, no network. Join? [y/N]". The data is already in the grant and attestation; this surfaces it. Non-interactive installs require `--yes`.

### A3. What we are not doing in Part A

- No hosted playbox service and no account. Reason and pingpong centralize sign-in; playbox stays key-based. A managed relay is a deployment of `iroh-relay` (already supported, `relay:<url>`), not a product dependency.
- No anti-collision "someone started the same thing" feature yet. It is good (pingpong), but it belongs on top of the replicated log from Part B, where a claim is a signed entry that survives a host change.

---

## Part B: host-less rooms

### Threat and goal

A room is a trust anchor. Three failures today:

1. **Availability.** The primary goes offline: no admissions, no grants, no revocations. Replicas keep honouring existing grants but nothing new can happen.
2. **Key loss or theft.** The root key is one file. Lose it and the room is dead. Steal it and the thief owns the room, with no way to rotate.
3. **A lying host.** The host decides what the log says and what a task returned.

Goal: a room survives the loss of any minority of hosts, no single host key can act alone, and a host that signs two different histories is caught by a proof anyone can check.

Non-goal: global consensus among strangers. Rooms stay small and invite-scoped (decision 4). This is a small replicated state machine among a handful of hosts the room already chose, not a blockchain.

### B1. Room identity versus signer

Split two things that are one key today.

- **Room id** = a public key `R`. It is what links, grants and revocation notices already verify against. Nothing outside the room changes.
- **Who may sign as `R`** becomes a *host set* `H = {h1..hn}` with threshold `k`, so `k` hosts together produce a signature that verifies under `R`.

Two ways to get there. **Spike first**, because only one may be feasible:

| Option | How | Pros | Risks |
|---|---|---|---|
| **1. FROST threshold Ed25519** (RFC 9591) | `R` is the group public key. `k` of `n` hosts run a signing round and emit a standard Ed25519 signature | Verifiers, grants, notices and the Lean model are **unchanged** | Biscuit's authority-block signing must be driven by an external signer; the `biscuit-auth` crate may not expose that hook. Needs DKG and a resharing step to change the set |
| **2. Epoch certificates** | `R` is a fixed key held cold. It signs `Epoch{n, H, k, prev}`. Grants and notices are signed by `k` host keys and carry the epoch chain. Verifiers check chain, then threshold | No new crypto. Easy to audit | Verifiers change. Grant size grows. Lean needs one more lemma: authority narrows along the epoch chain |

**Spike result (M0, 2026-11-17): Option 1 works, by token surgery.** `biscuit-auth` 6.0.0 has no external-signer hook: `KeyPair` wraps a concrete `ed25519_dalek::SigningKey`, and the `crypto` module and its signature-payload functions are private. But only the **authority block** is signed by the root key; later blocks are signed by a per-token next key whose secret travels in the token. So: build the grant with a throwaway root, decode the wire protobuf (`biscuit_auth::format::schema`, public), recompute the authority payload, sign it with the external signer, swap the signature, re-encode. `biscuit-auth`'s own `Biscuit::from(token, root_public_key)` then accepts it.

What the spike ([spikes/biscuit-external-signer](../spikes/biscuit-external-signer)) showed, all with unmodified `biscuit-auth`:
1. An ed25519 key outside biscuit-auth signs the authority block, and the token verifies under that key.
2. A holder can still attenuate it offline (append a block) with no help from the signer.
3. A one-bit change to the signed payload is rejected, so (1) is not passing vacuously.
4. **FROST 2-of-3** (`frost-ed25519` 2.2.0, trusted-dealer keygen) signs the payload; the aggregate verifies under the single group key. Verifiers, grants and revocation notices do not change.

Costs and risks, stated plainly:
- **We reproduce a private payload format.** The authority payload is two cases (block version 0 and 1; tokens built today carry no version field, so version 0), about 15 lines copied from `biscuit-auth` 6.0.0. A `biscuit-auth` upgrade could change it. Mitigation: pin `=6.0.0` where we do this, keep the spike's verify-after-sign as a unit test in the real code so a mismatch fails loudly, and try upstreaming a signer trait.
- **Only the authority block is threshold-signed.** That is where the root authority lives, so it is the part that needs the quorum. Attenuation stays single-party and offline, as today.
- **Not tested:** real DKG (the spike used a trusted dealer), signing over the network between hosts, and **changing the host set under the same group key**. In `frost-core` 2.2.0 the refresh functions require the same threshold; whether the participant count can change was not checked. If it cannot, a host-set change means a new group key and so a new room id, and the epoch-certificate option (2) returns as the fallback for that case.

**Decision rule:** spike Option 1 for at most one week against `biscuit-auth`. If grants can be signed by an external Ed25519 signer, take it. Otherwise take Option 2. Record the outcome in [decisions.md](decisions.md).

Either way, key rotation and host-set changes are an **epoch** change: a signed, hash-chained entry. Old epochs verify forever; a revoked host cannot sign in a later epoch.

### B2. A replicated, checkpointed log

Keep the log format. Add three things.

**Epochs and a leader.** Each entry carries `(epoch, index)`. In an epoch one host is the leader and sequences entries. Others are followers who verify and store every entry as it arrives.

**Commit = a quorum checkpoint.** The leader periodically signs `Checkpoint{epoch, size, head}`. Followers replay the chain up to `size`, check it, and cosign (the existing `Witness` replay rule). An entry is **committed** once `k` hosts cosigned a checkpoint covering it. Uncommitted entries are visible but cannot be relied on for admission, grants, promotion or credit.

**Membership and claims are log entries.** Join, leave, revoke, grant issue, task claim, push head, promotion. This closes the gap "a replica does not know who joined": every host replays the same entries and derives the same member list. It is also what makes pingpong-style claims verifiable later.

Keep the plain-text log (decision 14). Add a sidecar `checkpoints` file of cosigned checkpoints.

**Built (M1, 2026-11-17).** `playbox_core::roomlog` (pure log: genesis, leader entries, cosigned checkpoints, roster replay, 13 tests) and `playbox_net::roomsync` (protocol `playbox/roomlog/1`: the leader sends its log plus a checkpoint, a follower re-verifies from the root public key and cosigns only a head it computed; followers `pull`). `Host::start_room_log`, `commit_room_log`, `pull_room_log`, `room_log_status`; joins and revocations are logged automatically. Test `crates/playbox-net/tests/roomlog.rs`: three hosts converge, a commit needs 2 of 3 and survives one host down, and no quorum means no commit. Requirements ROOM-9, ROOM-10.
**Built (M1b, 2026-11-17): persistence and CLI.** A host keeps its log and certificates in `roomlog.jsonl` in its state directory (written atomically after every change) and re-verifies all of it on start; a file with one changed byte stops the host from starting. `playbox up --state DIR --room-log-peer ADDR` (repeatable) starts the log on the primary with k = more than half of the hosts and asks peers to cosign every `--sync-interval` seconds; `playbox up --replica-of ROOT --room-log-leader ADDR` follows it. `playbox room-log STATE --root HEX [--json]` re-checks a saved log offline. Smoke-tested with two real `playbox up` processes (log of 1 entry committed on both). Requirement ROOM-11.
**Built (M2, 2026-11-17): failover and equivocation proofs.** An `epoch` entry after the genesis makes a new leader. It is signed by the new leader and carries `k` host signatures over a `Proposal{epoch, leader, size, head}`; a follower checks them against the current host set. A host signs a bid only if it replayed the bidder's log and found in it the last checkpoint the host cosigned (so nothing committed is lost) and has not promised that epoch or a higher one to a different bid (so two leaders cannot both reach `k > n/2`); the promise is written to disk before the signature leaves. A follower holding the old leader's uncommitted tail drops it when the epoch entry arrives. A bid that lost is retried at a higher epoch number (numbers only have to increase), and a host that has heard from the leader within `--room-log-failover` seconds will not sign a rival bid, so one follower's bad link cannot depose a live leader. Two entries by one key at one `(epoch, index)` are an `Equivocation` that `verify(room_key)` checks alone; a host that is shown the second entry keeps the proof (also in `roomlog.jsonl`), refuses the entry, and `room-log` prints it. Hosts named in a signed log are allowed to talk to each other automatically, so the "list peers on both sides" limit is gone for log traffic. CLI: `--room-log-failover SECS` (default 30; bids are staggered by place in the host list). Tests: 9 core tests including no split brain over 1,000 random schedules; a network test that kills the leader, fails over, and brings the old leader back as a follower; and a smoke run of three real `playbox up` processes on iroh (leader killed, a follower led epoch 1 and committed, the old leader came back and followed). Requirements ROOM-12, ROOM-13.
**Built (M3, 2026-11-17): host-set change and key rotation.** An `epoch` entry may name a different host set and threshold. The proposal then covers the new set and threshold, and the entry needs `k` signatures from the old set (they authorize the change) and `k` from the new set (they hold the log and accept the job); a host in both counts in both, so a swap of one host in five needs three signatures in all, while replacing most of the set needs the newcomers too. `k` stays more than half of the new set, and the leader cannot remove itself (hand over first). A host being added has no log; it takes the proposer's, checking it from the root key, and signs. Rotating a host key is remove-old plus add-new. The removed key's cosignatures still count for checkpoints in its epochs (each certificate is checked against the host set of its own epoch) and for nothing later; it cannot cosign or lead. Removing an equivocating host is this same change. Hosts named in the log are reached by id, so a host added later needs no listing. CLI, on a running host started with `--console`: `roomlog`, `roomlog-add <peer address>`, `roomlog-remove <host id>` (threshold set to a majority automatically). A new host starts as `--replica-of ROOT --room-log-leader ADDR`. Tests: three core tests (rotation shuts the old key out, a big change needs the new quorum, rules-breaking changes get no signatures), a network test that swaps a host and shows the removed host's cosignature counts for nothing, and a smoke run of four real `playbox up` processes (add: epoch 1, 4 hosts; remove: epoch 2, 3 hosts; all four logs verify offline). Requirement ROOM-14.
**Built (M4, 2026-11-17): guest failover.** A join link may carry up to five other hosts (`h`, optional, so old links parse and old guests ignore it); a host with a room log fills it with the log's other hosts by id. A new method `playbox.GetHosts` (members only) returns the signed log and certificates. `Client::room_hosts(pinned)` replays them from the room key, takes the committed point, and returns the host set in force there (`RoomLog::committed_config`): a proposal that is not yet committed is not believed. The `playbox-mcp` guest pins the room key at join and re-reads the list about once a minute. `Client::failover(candidates)` opens a connection to the first other host that takes the member's grant. When the host in use stops answering (the connection closes, or a failed call is followed by an unanswered 5-second ping; a host that answers "no", such as a revoked grant, counts as there), the guest tries hosts it is already connected to, then the link's list, then the log's hosts, vets the new one with the policy it used at join (`min_host_tier`, device pins), switches, and writes `failover FROM -> TO` to its history. The call that failed is not retried (it may have reached the old host); its error says the member moved and to call again. A dead primary is retried at most once a minute. `playbox_use_host` stays as the manual override. Tests: a core test for the committed-point rule, a network test (link hints, host list, wrong key refused, failover with the same grant and member id), and an MCP test that kills the primary and watches the guest end up on the replica unaided. Requirement ROOM-15.
**Timing:** a host that is killed without closing connections is noticed at the transport's idle timeout, so the switch takes up to about a minute (81 s in the test); a host that shuts down cleanly is noticed at once.
**Built (M5, 2026-11-17): promotion as a quorum rule.** A problem is registered in the log (`problem.register`: problem hash, `j`, `noise_ppm`, `min_gain_ppm`, baseline metric); `j` must be at least 2 and at most the host count, so one host's word is never enough, even by rule. A `promotion` entry carries `Receipt`s: each host's own signed measurement (host key, environment hash, attestation tier, integer metric) over the same problem, candidate and weights. Every host replaying the log checks that the receipts come from distinct hosts of the current epoch, verify, agree within the noise, and that the **lowest** metric beats the frontier by the stated gain (the rule in `challenge.md`: the lower of primary and replica). The frontier is the replay of these entries (committed ones only for a guest), and a promotion's strength is the lowest tier among its signers. A host measures by running its own `--room-log-evaluator PROGRAM` (problem hash and candidate hash in, one JSON line out: `metric`, `weights`, `env`, `tier`); the leader collects receipts from peers in parallel, leaves out hosts that disagree beyond the noise, and records the promotion only if enough remain. Console on the leader: `roomlog-problem`, `roomlog-promote`, `roomlog` (shows each frontier). `challenge/room_evaluator_demo.sh` is a stand-in evaluator. Tests: seven core tests (rule floor, a promotion with the strength as the lowest tier, one host or one host twice or a stranger or an edited number or other weights refused, noise and gain, a leader's forged promotion refused by every follower, committed-only frontier, a removed host cannot sign in a later epoch) and a network test with shell evaluators (an outlier host is left out, the replica replays the same frontier, no second host means no promotion). Requirement ROOM-16.
**Run on a Linux VM, not the Pi:** leader and one replica on this Mac, one replica in the Lima VM `pbkvm` (Linux 6.8 aarch64), over the iroh transport, each with its own evaluator and tier (Mac 2, VM 0). The leader registered a problem and promoted a candidate with receipts from two machines; the log then verified offline from the room key on all three, with the same frontier and head, and the promotion's strength showed as tier 0 (the VM's), as it should. The VM was started with `limactl start pbkvm` and its `~/playbox` copy rebuilt there.
**What this does not show:** the evaluator was the stand-in (deterministic in the candidate, so no timing noise and the two hosts agreed exactly); a real measurement on two machines needs a noise band wide enough for hardware differences. The tier is whatever the host's evaluator prints, not yet the host's verified attestation. A leader can leave a dissenting receipt out of a promotion (the log cannot see what was not sent); making hosts able to post their own dissent is not built. The Pi is still the first test on distinct hardware with a strong tier.
**Built (M5b, 2026-11-17): the gate as a room evaluator.** `challenge/room_eval.py` (standard library only) is what a host runs as `--room-log-evaluator`: given a problem hash and a candidate hash it reads the candidate from the host's own store (`$PLAYBOX_CANDIDATES`), re-hashes it, runs the exact checker and the independent second checker, and prints the metric (Tier A: baseline rank over rank, in micro-units); a failing candidate is a refusal, so no receipt exists for it. `room_eval.py put FILE` files a candidate under its hash and `room_eval.py rule PROBLEM.json` prints the console line that registers the problem (Tier A: 2 hosts, exact agreement, any gain). Run on a room of a Mac leader, a Mac replica and the Linux VM replica, each with its own store: the rank-6 cheat got no promotion (the leader's own checker refused: tensor mismatch), Strassen (rank 7) was promoted with receipts from two machines (frontier 1,142,857), and the naive rank-8 scheme was refused for no gain over the new frontier; the log verified offline from the room key on the Mac and in the VM, same frontier. Tests: five Python tests (metric, baseline, a wrong scheme is a refusal not a low score, a host measures only what was named, the rule line) and the run above. Requirement ROOM-17. `conformance/trace.py` now also scans `challenge/`.
**Still manual:** getting the candidate file into every host's store (the run used `put` on each host); the tier is `$PB_TIER`.
**Not built:** repo and runner state on takeover (a new leader still needs the repo, and tasks in flight at failover are not re-run: B4's git-bundle fetch is not done); Tier B through the room (written, not run: it needs the model and torch, which this machine's Python lacks); `gate.py`'s local frontier and credit report do not read the room log; guests choosing "uncommitted reads"; the CLI `join` does not fail over; id-only hints need discovery, so `local` transport needs addresses; threshold-signed grants (B1 integration: the room key still signs grants); a join flow that needs no operator at the new host; incremental sync; an MCP surface and the hub UI for the log; the `leave` event; gossip of proofs and bids.

### B3. Failover and equivocation

**Failover.** If followers see no leader heartbeat or checkpoint for `T` (default 30 s), any follower may propose `Epoch{n+1, leader = self}`. It needs cosignatures from `k` hosts, each of which signs only if the proposer's log is at least as long as its own last cosigned checkpoint (no committed entry is lost). The new leader starts from the highest cosigned checkpoint. Entries the old leader wrote after that point are dropped unless they appear in the new leader's log.

Cosigning a proposal needs the cosigner to have *not* cosigned a higher epoch. That rule is what stops two leaders in one epoch from both reaching quorum, provided `k > n/2`. Require `k > n/2` at set creation and refuse smaller.

**Equivocation proof.** Two entries signed by the same host at the same `(epoch, index)` with different ids is a self-contained proof of lying. Any member can submit it. A quorum may remove that host in the next epoch. Add `playbox_lineage_check`-style verification so a guest can validate the proof offline. This is the "a host that lies is caught by a proof, not by trust" property, and it costs one small check.

**What this does not fix.** A host that *runs a task and reports a false result* is not detected by log checks. That is B5.

### B4. Guests and failover

- The join link carries the room id `R` and a **host hint list** (addresses of up to 5 hosts), not one host. `Link` gains `hosts: Vec<EndpointAddr>`; old single-host links still parse.
- After joining, the client keeps the signed host set from the latest epoch and, on a dial failure or stale checkpoint, tries the next host. `playbox_use_host` stays as a manual override. This replaces "no automatic failover".
- A guest sees the **committed** state by default and may opt into uncommitted reads.
- Repo and runner: a host that takes over needs the repo. Heads are log entries (B2), and the new leader fetches missing refs from any member (git bundles over the existing transport, which the hostile-bundle check already guards). Runner state is not replicated: tasks in flight at failover are reported failed and re-delegated by the caller.

### B5. Two-key promotion on a real second host

The challenge design already requires a second host to reproduce a result before promotion. With a host set this is a quorum rule rather than a special case:

- A promotion is a log entry valid only with receipts from `j >= 2` **distinct host keys**, each with its own run of the evaluation, each within the stated noise.
- Receipts bind problem hash, weights hash, environment hash and attestation tier, as in [challenge.md](challenge.md). The tier of each host is recorded; a promotion's strength is the **minimum** tier of the signing hosts.
- First real test: the Raspberry Pi 5 listed in [tested-on.md](tested-on.md) as the second host. The README is explicit that this is not done yet.

### Wire and storage changes

| Item | Change |
|---|---|
| Entry | Add `epoch`, `index`. Hash covers them. Old logs read as epoch 0 |
| New kinds | `epoch`, `checkpoint`, `member.join`, `member.leave`, `equivocation` |
| Link | `hosts` list, `R`. Backward compatible parse |
| State dir | `epochs/`, `checkpoints/`, replicated `members` |
| iroh gossip | One topic per room (already derived from `R`) carries checkpoints, epoch proposals and equivocation proofs, as it carries revocation notices today |
| Lean | Extend `lean/Playbox/Grants.lean`: a host in an older epoch authorizes nothing in a newer one; an epoch change never widens authority |

### Milestones

| # | Deliverable | Done when |
|---|---|---|
| M0 | Spike: external signer for Biscuit; FROST sign. **Done** (see B1). Still open: DKG, networked signing, host-set change | Decision recorded in decisions.md |
| M1 | Epoch and checkpoint entries, member replay, cosigned checkpoints, read-only followers | Three hosts converge on the same member list and head |
| M2 | Failover and equivocation proofs | Kill the leader mid-run; a new leader is elected inside `T` with no committed entry lost; a forged double-sign is rejected by an independent verifier |
| M3 | Key rotation and host-set change | Remove a host and rotate; the old host's signatures verify as epoch-N only |
| M4 | Guest failover with a host list | A guest keeps working after the leader dies, with no manual `use_host` |
| M5 | Two-key promotion on the Pi | A promotion needs receipts from two physical machines, and the verifier fails a copy with one receipt removed |

### Acceptance (all in CI unless noted)

1. **Join time.** Conformance run, join to first round-trip under 180 s (A2).
2. **Tool count.** A session with `PLAYBOX_TOOLS=core` lists at most 7 tools (A1).
3. **Partition.** With `n=3, k=2`, isolating any one host leaves the room able to admit, grant and revoke; isolating two stops those (no commit without quorum).
4. **No split brain.** Two leaders proposed in one epoch: at most one reaches quorum, over 1,000 randomized schedules.
5. **No lost commit.** Randomly kill hosts during writes; every entry that returned "committed" is in every surviving log.
6. **Equivocation.** A signed double-write by one host fails verification for any member, offline.
7. **Compatibility.** A 0.3.2 guest joins a new room as a single-host room and works, and a 0.3.2 log reads as epoch 0.
8. **Chaos drill** (scheduled, not per-commit): kill, partition, reorder and replay old checkpoints. Publish results, including failures.

### Risks and open questions

- **Biscuit external signing (B1).** May block Option 1. Mitigation: Option 2.
- **Threshold DKG is the most complex new code** and a place for subtle bugs. Use an audited FROST crate (`frost-ed25519`) rather than writing one, and fuzz the message parser (the repo already has 12 fuzz targets).
- **Small host sets.** With `n=3` the room tolerates one failure. With `n=2` it tolerates none (`k > n/2` forces `k=2`). Say so in the UX; do not pretend a two-host room is fault tolerant.
- **Leader liveness versus clock.** `T` depends on local clocks. Use heartbeat counts from the gossip stream, not wall time, where possible.
- **Privacy.** Replicating the log and members to every host reveals the member list to all hosts. Hosts are already trusted for that today; list the exposure in the threat model.
- **Independent audit.** This design changes the trust core. Do not expose a multi-host room to strangers before the audit named in [security-roadmap.md](security-roadmap.md).

### What this is not

A token, a chain, or a public directory. Discovery, reputation and bonds (Adapterscan and Index Network ideas) come after this and use the replicated log as evidence. They are out of scope here.
