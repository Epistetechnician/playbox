# BOX-7 egress proxy: implementation plan

Status: **implemented** (`crates/playbox-runner/src/egress.rs`, wired into `libkrun`, `netpolicy`, `playbox-net` Biscuit grants as `egress/<name>`, `playbox up --egress/--allow-egress`, and `playbox-mcp`). Each task gets a capability-scoped loopback URL (`http://127.0.0.1:PORT/t/<token>/<upstream>`) and upstream hostnames are resolved & IP-pinned against DNS rebinding. Requirement text: `docs/requirements.md` BOX-7; roadmap: `docs/security-roadmap.md`.

## Shape

A host-side **reverse proxy with named upstreams**, one per task, on a loopback port. No CONNECT, no TLS interception.

- Guest calls `http://127.0.0.1:PORT/<upstream>/<path>`; the proxy forwards to the owner-configured upstream and adds the credential header. The guest only ever gets a base URL (via env, e.g. `ANTHROPIC_BASE_URL`), never a key.
- Owner config (new file or `up --egress FILE`): per upstream `name`, `base` (https; `http` only with an explicit `insecure_http` for tests), `auth_header`, `auth_prefix`, `secret` (a name in the existing `SecretStore`), `allow_paths` (required, non-empty: fail closed), `pass_headers` (default `content-type`, `accept`, `user-agent`, plus e.g. `anthropic-version`).
- Task side: `TaskSpec.egress: Vec<String>` (names), clamped by a new `Limits.allowed_egress`, the same pattern as `secrets` / `allowed_secrets` (`crates/playbox-runner/src/lib.rs`).

## Proxy rules (new `crates/playbox-runner/src/egress.rs`)

- Strip every guest header not in `pass_headers`; in particular `authorization`, `x-api-key`, `cookie`, `host`. Inject the credential after.
- No redirects (a redirect would carry the injected header elsewhere). Reject `..`, `%2e`, `//`, absolute-form URIs. Reject chunked requests, `Upgrade`, `Expect`. Content-Length body cap (about 8 MiB), connection cap, read and total timeouts.
- Forward the response by chunk loop (`Response::chunk()`), so SSE streams work without the reqwest `stream` feature.
- Per-request JSONL log: time, upstream, method, path **without query**, status, request and response bytes, duration, denial reason. Never headers or bodies. Written separately from the audit log (roadmap: opt-in 30-day retention).
- Secret value is read host-side at proxy start; only the secret's name and blake3 go to the audit log, as in `secrets.rs`.
- Dependencies: add `reqwest` (0.13, `rustls`, no default features, as in `playbox-bridge`) to `playbox-runner`; HTTP/1.1 server hand-rolled on tokio.

## Wiring (the parts that cannot be tested on a Mac)

1. `krun.rs` `run`: start the proxy before the helper, pass `--env NAME=value` (new helper flag; the helper builds an explicit `envp`, `playbox-krun-helper.rs:114`), drop the proxy when the task ends.
2. Linux `--confine`: `netpolicy.rs` always rejects `127.0.0.0/8`; add an accept for exactly `127.0.0.1:PORT` ahead of that rule, with a unit test on the rule text like the existing ones.
3. A task with `egress` and no `network` needs the guest to reach host loopback; under TSI that works but also exposes the host's loopback. Until BOX-7's "replaces TSI" half exists, refuse this combination off `--confine` unless `PLAYBOX_ALLOW_UNFILTERED_NETWORK` is set, matching decision 42.

## Tests

- Unit: path normalisation, header stripping, injection, cap and refusal cases.
- Integration: proxy against a local fake upstream (`insecure_http`): header injected, guest-supplied `x-api-key` removed, redirect not followed, SSE streamed, log has no secret or query.
- Not testable here: a real libkrun guest through the proxy, and the nftables accept (needs Linux, root, KVM).

## Not in scope

Replacing TSI with a filtered network backend (the rest of BOX-7), DNS, raw TCP, UDP.
