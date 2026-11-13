# Capabilities: named operations a host offers to members

A host owner lists operations in a JSON file and starts the room with `playbox up --runtime docker --capabilities caps.json`.
A member sees the names and parameters (not the commands) and calls them; each call runs once in the host's sandbox.

```json
[{
  "name": "run-tests",
  "argv": ["cargo", "test", "{filter}"],
  "params": {"filter": {"allowed": ["unit", "integration"], "max_len": 16}},
  "approval": true,
  "image": "rust:1.93",
  "timeout_secs": 600
}]
```

What is enforced, and where:

| Property | How |
|---|---|
| Only granted members | the grant lists `cap/NAME` in the same allow-list as hosts and secrets; a member can narrow it offline, never widen it |
| Bounded arguments | every parameter is declared; values outside `allowed`, over `max_len`, with control characters, or (when unlisted) starting with `-` are refused at submission. Values are separate argv entries and are substituted in one pass, so a value cannot add words, flags or placeholders |
| Not the owner's authority | the command runs through the host's runner (microVM or container) in an empty scratch directory with the capability's image, memory and time |
| Exactly once | the call is written to `calls.json` before it runs; the same `(member, call_id)` with the same request returns the same call; a different request under the same id is refused; after a restart a running call becomes `abandoned` and is never re-run |
| Owner approval | with `"approval": true` the call waits; the owner uses `approve ID` / `deny ID` in `up --console` (`calls` lists them); unanswered calls expire after 15 minutes |
| Verifiable result | `result_hash` is blake3 over a canonical encoding (sorted keys) of call id, name, arguments, exit code, timeout flag and output; the same hash is in the audit log (`capability_result`), as are `capability_call` and `capability_decision` |
| Host-signed result | The host signs (ed25519, its endpoint key, which is its id) `playbox.cap-result.v1`, its id, the member, call id, capability, request hash, state and `result_hash`. The caller verifies that against the host id it dialed and recomputes `result_hash` from the output, so a relayed or stored result cannot be altered, moved to another member or call, or attributed to another host. Hosts before 0.3.0 do not sign; their results are refused |
| Untrusted output | the MCP tool registers stdout and stderr as untrusted text in the witness tracker, like a remote task's output |

Members: `playbox_capabilities` lists, `playbox_capability_call name=… args={…} call_id=…` calls. Pass your own `call_id` so a retry
after a dropped connection cannot run the command twice.

Limits: 8 open calls and 256 stored calls per member, 16 arguments, output clipped to 64 KiB. Not built: arguments as Biscuit checks
(today the bounds are the host's definition, enforced by the host, not carried in the token), results signed by the host key.
