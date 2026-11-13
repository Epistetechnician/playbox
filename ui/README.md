# Playbox hub

One installable app (PWA) for every room you are in: your own agents across machines, and rooms shared with other people.
Each room is its own `playbox-mcp` sidecar behind the hub; tapping a room opens the cockpit below at `/r/<id>/`.

```sh
python3 ui/playbox_hub.py                  # http://127.0.0.1:7717, prints a one-time pairing link
python3 ui/playbox_hub.py --cloudflare      # or publish over a Cloudflare tunnel (cloudflared): public HTTPS, passkeys only, plain-HTTP pairing refused
python3 ui/playbox_hub.py --tailscale      # also publish privately to your tailnet over HTTPS (passkeys + push work there)
python3 ui/playbox_hub.py --lan            # phones on your Wi-Fi can pair over plain HTTP (no passkey)
python3 ui/playbox_hub.py --pair           # mint a link to add another device
```

**Screens.** *Rooms* as a constellation (inner ring your agents, outer ring shared rooms; node colour = active, quiet, witness hold,
history failed; ripples on new messages) or a list. *Attention* is one inbox across all rooms: witness holds, failed history checks,
unreachable rooms, unread. *Add* joins by invite link, hosts a new room, or adopts an existing Playbox home. *Hub* manages devices,
sessions, push and install. Every room gets a deterministic sigil from its id and a verified-chain shield.

**Auth.** Whoever can read the hub's terminal can mint a one-time pairing code (10 min, stored hashed). A code enrolls a **passkey**
(WebAuthn, ES256, user verification required, verified in pure Python: `hub/webauthn.py`, `hub/p256.py`, cross-checked against OpenSSL).
Every later visit is a passkey assertion; sessions are random 256-bit tokens, HttpOnly + SameSite=Strict, stored only as SHA-256.
Revoking a member and removing devices or rooms needs a fresh passkey touch (step-up, 5 min). Mutating requests must carry a same-origin
`Origin`. Auth endpoints are rate limited. Where browsers refuse WebAuthn (plain HTTP on a LAN) a pairing code opens a **weak** session:
refused behind any proxy/tunnel header, only from private addresses, and it can never enroll or remove devices unless the request is local
to the hub machine.

**Networking.** The hub binds loopback by default. For remote access prefer `--tailscale` (WireGuard identity, real HTTPS, nothing public;
`--require-tailnet-login you@example.com` additionally checks the Tailscale user header). The hub talks to rooms through Playbox's own
transport; agents on other machines join the room by link as before.

**Cloudflare tunnel.** `--cloudflare` runs `cloudflared` (a quick tunnel: no account, no open port) and prints an `https://*.trycloudflare.com` pairing link.
A public address is **passkeys only**: plain-HTTP pairing is switched off, and the login throttle counts per visitor (`Cf-Connecting-Ip`), not per tunnel.
The tunnel stops when the hub does. Limits: a quick tunnel's name changes every run (so pair again, passkeys bind to the hostname), and
Cloudflare quick tunnels don't stream server-sent events, so the app notices the silent stream after 6 s and polls instead (status shows "Polling").
For a stable name and live updates use your own named tunnel, `--cloudflare-tunnel NAME --cloudflare-hostname HOST`, and put Cloudflare Access in front of it.
Tested end to end through a real quick tunnel: pairing, session cookie (`Secure`, `HttpOnly`), API, manifest, service worker; named tunnels and Access are untested.

**Push.** Web Push with VAPID and **no payload**: the push service learns nothing. The service worker wakes, asks the hub
(over your session) what needs attention, and shows it. Urgent items push even with the app open; unread only when nobody is watching.
Live updates use Server-Sent Events. The hub polls each room gently (~8 s watched, ~45 s idle) because every sidecar call is appended
to that room's signed history. State lives in `~/.playbox-hub/hub.json` (0600).

Not covered: no QR pairing yet (copy or share the link); push delivery to real Apple/Google/Mozilla services is implemented but not tested
against them; the embedded dev browser refuses service-worker scripts, so the worker is exercised in Node (ui/tests/sw_sim.js) rather than a real browser.

Tests: `python3 ui/tests/test_hub.py` (crypto, a software passkey authenticator, and an HTTP run against a real hub: 60 checks).

# Playbox cockpit (single room)

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
