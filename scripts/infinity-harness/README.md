# Infinity harness

Two ways to run Infinity against `playbox-rap`. Neither uses a real model: the Bedrock account tried returned `ValidationException: Operation not allowed` for every Claude model (an account-level restriction, not something in these files), so the model is scripted: create a room, subscribe, say hello, report the event that comes back.

## 1. Stock Infinity daemon (needs the auth patch)

`infinity-auth-headers.patch` adds a `headers` field to `toolset_server` entries in `rap.json`, sent only to that server. Without it stock Infinity cannot send a bearer token. It applies to hydro-project/infinity at the commit we vendored from (`1abc6fc`) and to the current default branch, and includes a test and docs.

```sh
git clone https://github.com/hydro-project/infinity && cd infinity
git apply /path/to/infinity-auth-headers.patch
# add scripted_provider.rs as crates/playbox-harness/src/bin/ with Cargo.toml.txt as its Cargo.toml (workspace member)
cargo build -p infinity-agent-cli -p playbox-harness --bins
```

In a scratch `HOME`, write `~/.infinity/providers.json` (`{"scripted": {"command": ["<path>/scripted_provider"]}}`) and `~/.infinity/rap.json`:

```json
{"tool_sets": [{"type": "toolset_server", "server_url": "http://127.0.0.1:3010",
                "headers": {"Authorization": "Bearer <PLAYBOX_RAP_TOKEN>"}}]}
```

Start `playbox-rap` (`PLAYBOX_RAP_TOKEN=... PLAYBOX_RAP_ADDR=127.0.0.1:3010`), then:

```sh
echo "Start a Playbox room and tell me what happens in it." | \
  HOME=<scratch> REPO=<git repo to share> python3 drive_acp.py <path>/infinity <cwd> acp.err
```

`drive_acp.py` talks to `infinity acp` and keeps listening after the first `end_turn`, because RAP results arrive later and wake the agent. Expected: the three tool calls complete and the model's last line is `GOT EVENT: {...,"text":"hello from Infinity",...}`.

## 2. In-process runtime

`main.rs` runs Infinity's local runtime, RAP client and callback bridge in one process with its own bearer-adding `HttpClient`, so it needs no patch. Copy it to `crates/playbox-harness/src/main.rs` in a checkout, then:

```sh
RAP_URL=http://127.0.0.1:3010 RAP_TOKEN=<token> REPO=<git repo> cargo run -p playbox-harness
```

It prints `RESULT: ok` when the agent's own message comes back as a subscription event.
