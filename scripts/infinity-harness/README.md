# Infinity harness

Drives Infinity's local agent runtime against `playbox-rap`. The model is scripted (create a room, subscribe, say hello), so it needs no credentials.

It is not part of this workspace. Copy `main.rs` to `crates/playbox-harness/src/main.rs` and `Cargo.toml.txt` to `crates/playbox-harness/Cargo.toml` in a checkout of hydro-project/infinity (add the crate to its workspace members), then:

```sh
PLAYBOX_RAP_TOKEN=run-token-0123456789abcdef cargo run -p playbox-rap &
RAP_URL=http://127.0.0.1:3010 RAP_TOKEN=run-token-0123456789abcdef REPO=/path/to/a/git/repo cargo run -p playbox-harness
```

It prints `RESULT: ok` when the agent's own message comes back as a subscription event.
