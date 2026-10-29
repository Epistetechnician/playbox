# Revocation across hosts

A room can be served by more than one host. The **primary** holds the room's root signing key: it admits
members, issues grants, and revokes. A **replica** knows only the root *public* key: it accepts the room's
grants (a grant is checkable by anyone with the public key) but cannot admit members, issue grants, or revoke.

Revocation then has to reach every host, or a revoked member could keep using a replica that never heard.

## Notices

A revocation is a **notice**: text signed by the root key naming the grant id (the authority block, which
also covers every attenuated copy of the grant), the member's endpoint id, and a time. Only the root key can
make one, so a host accepts a notice from *any* source, including peers it does not otherwise trust: it
checks the signature against the room's public key and ignores anything that does not verify. Notices are bound to
their room, so one room's key cannot revoke in another. Applying a notice revokes the grant and bars the
member's key from rejoining. Notices only accumulate; there is no un-revoke, so order and repeats don't matter.

(`lean/Playbox/Grants.lean` proves the property this relies on: a host that has heard more revocations never
authorizes more than one that has heard fewer.)

## How they spread

- **Gossip** (iroh-gossip, topic derived from the root public key) carries a new notice to every host quickly.
  A test with the pull interval set to an hour shows a replica refusing the member within seconds, so gossip
  alone delivers.
- **Pull** repairs what gossip missed. Every `--sync-interval` seconds (default 15) a host asks each peer it can
  dial for all the notices it holds. A replica that was offline, or started after the revocation, catches up the
  next time it reaches a peer. Tested: a revocation made before the replica existed still reached it.
- **Storage.** Each host keeps the notices in its state directory and re-verifies them when it starts.

## Setting it up

```sh
# primary
playbox up --state ~/.playbox/team --transport iroh
#   prints: room root public key, peer address, and the join link
# replica, on another machine
playbox up --state ~/.playbox/team-r --transport iroh --replica-of <root public key> --sync-peer <primary peer address>
# then restart the primary naming the replica (an endpoint id is enough) so it will serve the replica's pulls
playbox up --state ~/.playbox/team --transport iroh --sync-peer <replica host id>
```

From an agent: `playbox_create` takes `replica_of`, `sync_peers` (peer addresses, or bare host ids) and
`sync_interval_secs`; calling it again on a running host adds peers. `playbox_revoke` on the primary signs and
spreads the notice; `playbox_status` shows each host's `revocations` count.

A host only serves its notice list to peers it was told about; strangers are refused. Gossip is open to anyone
who can compute the topic (it needs the root public key), but notices are unforgeable.

## Limits

- **Eventually consistent.** Until a notice reaches a replica, that replica still honours the grant. Gossip
  usually closes the window in well under a second; pull bounds it by the sync interval if the replica is
  reachable at all. A replica cut off from every peer keeps honouring the grant until the grant expires, so short grant
  lifetimes (default 1 hour) remain the real upper bound.
- **Guests choose a host explicitly.** A join link names the primary. After joining, `playbox_use_host` (MCP) or
  `playbox join <link> --via <replica peer address>` (CLI) sends calls to a replica with the grant the primary issued; the
  replica is vetted like the primary was and must accept the grant first. There is no automatic failover, and a replica
  has its own repository and runner (it does not mirror the primary's), so pull before delegating there.
- **One signing host.** There is one root key on one machine. If it is lost no one can revoke or admit; there is no
  threshold signing or key rotation.
- **Local mode is awkward.** Ports change on restart, so addresses given to `--sync-peer` go stale; use the `iroh` or
  `relay:` transports, where an id plus relay is stable.
- **The primary's own member list is not replicated.** A replica does not know who joined, only who was revoked.
