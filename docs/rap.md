# RAP server (`playbox-rap`)

Serves the Playbox MCP tools over the Reactive Agent Protocol, so an Infinity agent (or any RAP runtime) can host or join a room. Calls go through the same `playbox_mcp::Server::call` as stdio, so the signed history log, taint tracking and approval checks are unchanged.

```sh
PLAYBOX_RAP_TOKEN=<16+ chars> cargo run -p playbox-rap     # listens on 127.0.0.1:3010
```

| Variable | Meaning |
|---|---|
| `PLAYBOX_RAP_TOKEN` | Required. Bearer token for every endpoint. |
| `PLAYBOX_RAP_ADDR` | Listen address. Default `127.0.0.1:3010`. |
| `PLAYBOX_RAP_URL` | Public base URL put in the manifest. Default from the address. |
| `PLAYBOX_RAP_APPROVER_KEY` | Path to an approver key file (created if missing). Turns on approvals through `user_choice`. Put the public key it prints into `PLAYBOX_APPROVERS`. |
| `PLAYBOX_RAP_CALLBACK_HOSTS` | Comma list of non-loopback hosts a `callback_url` may name. |

Endpoints: `GET /.well-known/rap-toolset`, `POST /invoke` (acks with 200, then posts a `tool_result` to the callback), `POST /cancel_tool_call` and `POST /close_thread` (acknowledged, no effect).

Callbacks to hosts outside the allowlist are refused with 400 (SSRF guard).

## Approvals
When a call stops for approval (text copied from another member's output, or a revoke) and an approver key is configured, the server posts a `user_choice` ("Approve this once" / "Deny", default Deny) to the callback and waits at `POST /user_choice_response` (bearer token, `{id, selected}`). Approve signs a single-use capability for exactly that command, valid 2 minutes, and retries. Deny, no answer in 10 minutes, or a second question for the same call id all refuse the call.

Trade-off: the runtime relays the person's answer, so a compromised runtime could answer for them. Without `PLAYBOX_RAP_APPROVER_KEY` the key stays with the person, calls fail with "needs approval", and `playbox approve` works as before.

## Room events
`playbox_subscribe` (RAP only, not an MCP tool) confirms with a `tool_result` marked `subscription: true`, then posts one `subscription_event` per new feed item until it ends. It starts from now, so the backlog is not replayed. Each event's `text` is the feed item as JSON with `untrusted: true` for `say` items, which are other members' words. Arguments: `interval_ms` (500 to 60000, default 2000) and `max_secs` (default 3600, up to 86400).

It ends with a `final` event on `/cancel_tool_call` for that call, on `/close_thread` for its thread, or at `max_secs`. It works by polling `playbox_feed` through the same server, so peer text is tainted as it is for `playbox_feed` and the events are delayed while a long call such as `playbox_delegate` holds the server.

## What is not done
- Calls run one at a time (one `Server` behind a mutex), and `/cancel_tool_call` does not stop a running tool call, only a subscription.
- Tool results (such as task output) are not marked untrusted in the RAP payload. Taint is tracked inside the server, not shown to the runtime.
- Tested only against our own receiver, not a real Infinity runtime.

## Vendored code
`vendor/infinity/rap-protocol` and `rap-client` are copied from hydro-project/infinity at `1abc6fc` (Apache-2.0, license and NOTICE alongside). Only the manifests were changed.
