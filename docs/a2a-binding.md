# The Playbox A2A binding

A room host is an [A2A](https://a2a-protocol.org) v1 agent. Peers talk A2A JSON-RPC over iroh streams
(ALPN `playbox/a2a/1`). Types come from the `a2a-lf` crate (the Rust SDK of the A2A project); the
binding itself is in `crates/playbox-net/src/wire.rs`.

This is a custom A2A protocol binding (`PLAYBOX-IROH`). Stock A2A clients speak HTTP, so they cannot
dial it directly yet; a bridge or an iroh-aware client is needed.

## Streams

One bidirectional stream per call. The caller writes one JSON document and finishes; the host writes
one JSON-RPC response and finishes. Documents are capped at 256 MiB (bytes are base64, so about
190 MiB of payload).

```json
{ "svc": { "A2A-Version": "1.0", "Authorization": "Bearer <biscuit grant>" },
  "rpc": { "jsonrpc": "2.0", "id": 1, "method": "SendMessage", "params": { } } }
```

`svc` holds A2A service parameters, the stream equivalent of HTTP headers. The response is a plain
JSON-RPC 2.0 response.

## Methods

| Method | Auth | Notes |
|---|---|---|
| `playbox.GetAgentCard` | none | The public agent card (stands in for the well-known URL). Lists only the skills the host offers. |
| `playbox.Join` | invite | `{ "invite": "<token>" }` returns `{ "member", "grant" }`. |
| `SendMessage` | grant | Carries a Playbox operation (below). Returns a `message` or a `task`. |
| `GetTask`, `ListTasks` | grant with the `tasks` right | A member sees only their own tasks. Stored records drop byte payloads and report their size. |
| `CancelTask` | grant with the `tasks` right | Every task finishes before it is returned, so this is always "not cancelable" (or "not found"). |
| `SendStreamingMessage`, `SubscribeToTask`, push configs, `GetExtendedAgentCard` | | Not supported; the card says `streaming: false`. |

Errors are JSON-RPC errors. Grant failures (missing, invalid, expired, revoked, or out of scope) use
code `-32040`, which A2A does not define because it expects HTTP 401/403.

## Operations

A `SendMessage` message carries a data part `{"playbox": {"op": ...}}`:

| `op` | Fields | Result |
|---|---|---|
| `echo` | `text` | A `message` with the same text. |
| `pull` | `refname` | A task; artifact `bundle` (raw, `application/x-git-bundle`), metadata `head`. |
| `push` | `branch`, plus a raw part holding the bundle | A task; metadata `reference` (stored under the sender's namespace, never over host branches). |
| `run` | `base`, `spec` (cmd, image, cpus, mem_mib, timeout_secs, network) | A task; artifacts `stdout`, `stderr`, and `patch` (raw, `text/x-diff`); metadata `runner`, `exitCode`, `timedOut`. State `completed` on exit 0, `failed` otherwise. On hosts that keep results in a versioned workspace: metadata `branch`, `commit`, `changed`, and no patch. |

A message with no Playbox operation (plain text from a generic A2A client) gets a message back
describing how to use the room.

Task states: `completed`; `failed` (the command failed, or the host hit an error); `rejected` (the
room will not do it: no repository, no runner, a bad ref name, a malformed push).

## Grants

Every authorized call is checked against the Biscuit grant in `Authorization`, with facts the host
supplies: the time, the caller's authenticated iroh key, the operation (`echo`, `pull`, `push`, `run`,
`tasks`) and, for `run`, the CPU, memory, timeout, network and program. See `grants.rs`.

## HTTP bridge

`playbox bridge <link> [--listen 127.0.0.1:8787] [--token T]` (crate `playbox-bridge`) joins a room as
one member and serves standard A2A HTTP+JSON-RPC to stock clients: the agent card at
`/.well-known/agent-card.json` (rewritten to a `JSONRPC` interface pointing at the bridge) and
`POST /jsonrpc`. It forwards `SendMessage`, `GetTask`, `ListTasks`, `CancelTask` and
`GetExtendedAgentCard` with the caller's JSON-RPC id and the host's error codes intact.
`playbox.Join` is not reachable over HTTP.

All HTTP callers act as the bridge's one member and share its tasks and limits. It binds loopback by
default; a non-loopback `--listen` requires `--token` (or `PLAYBOX_BRIDGE_TOKEN`), checked as a Bearer
token. The bridge's grant expires like any other (default 1h), so restart it with a fresh link then.
Plain HTTP only: put a TLS proxy in front for anything beyond localhost or a trusted network.
