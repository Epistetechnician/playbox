# Playbox cockpit

A local web UI for a Playbox room. One page that works on a desktop browser and on a phone (installable as a PWA).
Hermes-inspired: deep indigo ink, translucent cards, condensed caps headings, a gold ☤.

```sh
python3 ui/playbox_ui.py            # prints http://127.0.0.1:7717/?t=<token>
python3 ui/playbox_ui.py --lan      # also prints a URL for a phone on the same Wi-Fi
```

Needs `playbox` (or `playbox-mcp`) on PATH, or `PLAYBOX_MCP=/path`. Stdlib Python only, nothing to install.

## What it does
| Tab | |
|---|---|
| Room | Host a room (repo, same network or anywhere, optional Docker tasks), make single-use invite links (copy, share, save as file), or join one. Leave. Three presets fill in reach and Docker. A banner on every tab shows when the Hermes witness has paused your agent, and what it was reading. |
| Feed | Live room feed and composer. Text from other parties is marked `member`/`host` with an amber stripe: it is untrusted. Quick-reply chips. |
| Work | Pull and push a branch; six one-tap read-only checks (tests, TODOs, leaked secrets, largest files, file types, shell syntax); run any command in the host's sandbox; read output and the diff; apply the patch to your folder. |
| People | Host only: members, revoke with confirmation. |
| Proof | Verify your signed history log as a chain of blocks, or any log. "Flip a byte" corrupts a **copy** and shows verification catching it at the broken entry; the real log is never touched. Lists recent witness holds (tool and origin only, never the text). |

On wide screens a right-hand "Pulse" panel shows the room state and latest messages beside every tab.

## How it relates to Hermes
The UI runs **its own** `playbox-mcp` sidecar. It is you, the person, in the room, next to the agents; it does not
attach to the sidecar Hermes spawns. Host here and let a Hermes agent join with the link (save it as a file and
tell the agent to use `link_file`), or join a room your agent hosts.

## Security
- Every request needs the access token (random per start, or `--token`). Opening `/?t=<token>` sets an HttpOnly,
  SameSite=Strict cookie. POSTs must be JSON, so a web page on another origin cannot drive it.
- Loopback only unless `--lan`. With `--lan` the token is the only key and traffic is plain HTTP: use a network you trust.
  Anyone with the link can act as you in your rooms.
- Only tools the sidecar lists are callable; the sidecar still enforces grants.
- Every call (including the UI's polling, about every 10 s for status and 3.5 s for the feed while a feed is visible) is
  appended to your signed history log, so the log grows while the UI is open.

## Test
`python3 ui/e2e.py` boots a host UI and a guest UI and drives a full flow over HTTP: access control, create, join, say/feed,
pull, Docker task with patch, verify, a flipped byte caught, leave, rejoin, revoke, refusal (24 checks).
