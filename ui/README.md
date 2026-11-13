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
unreachable rooms, unread. *Add* joins by invite link, hosts a new room, or adopts an existing Playbox home. Hosting is one step: name, who can reach it (anywhere or this network), and an optional git repository (leave it empty for a room to talk and share notes in); a shared room then opens its invite sheet. The hub remembers what it hosts, with or without a repository, and hosts it again after a restart. Open the hub as `localhost`, not `127.0.0.1`: browsers refuse passkeys on an IP address, and the pairing page says so and links across. *Hub* manages devices,
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
**Cloudflare Access.** Put Access in front of the named tunnel (a self-hosted application, Public DNS destination, Allow policy for your emails), then start the hub with
`--cloudflare-access TEAM --cloudflare-aud AUD` (or `PLAYBOX_ACCESS_TEAM` / `PLAYBOX_ACCESS_AUD`). The hub then trusts the signed `Cf-Access-Jwt-Assertion` token, not the network path:
any tunnelled request without a valid RS256 token for that team and AUD tag is refused, including static files, so a second hostname or a misrouted tunnel can't bypass Access.
Requests straight on loopback from your own machine don't need one. Signing keys come from `https://TEAM.cloudflareaccess.com/cdn-cgi/access/certs`, cached an hour; if they can't be fetched the hub fails closed.
**Run it as a service (macOS).** `ui/service/install-macos.sh --cloudflare-tunnel ID --cloudflare-hostname HOST --cloudflare-access TEAM --cloudflare-aud AUD` installs a LaunchAgent: starts at login, restarts if it dies, log at `~/Library/Logs/playbox-hub.log`. `--uninstall` removes it. Pair a device by stopping the service and running the hub by hand with `--pair` (the script refuses to bake a pairing link into the log).
Verified with locally signed tokens (OpenSSL) and a stand-in tunnel; not yet against a real Access application.

Tested end to end through a real quick tunnel: pairing, session cookie (`Secure`, `HttpOnly`), API, manifest, service worker; named tunnels and Access are untested.

**Inviting a friend.** On a room you host, **⋯ → Invite a friend** (or **Invite a friend** in its cockpit) mints a fresh single-use Playbox link and wraps it in a
share URL, `https://<your public address>/i/<id>#<link>`. You choose how long the link works (15 min to 24 h), how long their access lasts (1 h to 7 d), and one person or up to
three; optionally lock it to one person's guest id. The link itself rides in the `#fragment`, which no server sees, and the hub stores only metadata (room name, your name,
expiry, status), never the key. The page a friend opens shows who invited them, what the grant allows and when it ends, then offers: *Open in my Playbox* (type your
hub's address once; it opens with the invite filled in, and you confirm), the installed app via a `web+playbox:` handler, *Copy for my agent*, or, if new, **one command**
(see below). The default "Who can use it" is *them + their agent* (two uses): one for their hub, one for their agent. If you aren't signed in yet, the invite waits through the passkey login. **Hub → Invites** lists them: pending, joined, spent, expired, withdrawn.
Needs a public address (`--cloudflare`); a tailnet address only reaches your own devices. Phase 1: withdrawing hides the share page, but the underlying key still expires on its own clock.

**Joining as someone new.** The share page gives one command, `curl … install.sh | sh -s -- --join '<link>' --name '<room>'`. It installs `playbox`, registers it with the
coding agents found (the existing `playbox setup`), installs the hub from `playbox-hub.tar.gz` (same `SHA256SUMS` as the binary; the release workflow now builds it),
starts the hub, joins the room, and opens `http://localhost:<port>/pair#…` (passkeys need a hostname, so `localhost`, not `127.0.0.1`). The link travels in the
environment, not in argv. After the passkey, a **Get started** card on Rooms walks through: passkey, *Connect your coding agent* (detects Claude Code, Codex, OpenCode
and Hermes from their own config files; the button runs `playbox setup --only <agent>` and needs a passkey touch), *Join or host a room*, and *Bring your agent into the room*
(a ready-to-paste instruction carrying the same link, for the invite's second use). `playbox_hub.py --join LINK --room-name N --open` does the same from a checkout.
An agent joins a room as its own member (its sidecar keeps the join in memory), which is why the agent needs its own use of the invite. The room name on the page is the
host's text, so the command shell-quotes it; a hostile page could still show a different command, so only open invites from people you know.

**Approving who joins (ask-to-join).** By default an invite is an *approval* invite, so a forwarded link can't get the wrong person in. It carries no Playbox key at all:
its share URL, `https://<host>/i/<id>#a.<secret>`, holds only a random secret in the fragment. Opening it and pressing *Ask to join* (or running the install command)
**knocks**: the asker's own hub sends the host its Playbox guest id and a name. Both sides compute the same six-digit **verify code** from (invite id, guest id), for
example `482 913`; the asker's device refuses to continue if the host's reply differs. The asker reads their code to the host in person or in a channel the host
trusts. The host sees **Join requests** (also an urgent attention item and a push) with the name, the room and the code, and approves only if it matches. Approving
takes a passkey touch. Only then does the room mint a join link, **bound to that guest id** (`for_guest`), valid ten minutes, handed over exactly once and then
forgotten. Someone who intercepts it can't use it; someone who forwards the invite URL gets only the ability to ask, and shows up with a different code and name.
Limits: five pending requests per invite, approvals capped at the invite's count, withdrawing the invite closes it, denying needs no touch. An agent asks the same
way with its own guest id (the Get started card has a field for it), so it gets its own code and its own bound link. The host address in a link is untrusted: the
asker's hub only talks to `https` hosts, or to `http` on this machine or a private network, and never follows redirects. The verify code authenticates the *asker to the
host* only as well as the channel it is read over; a 6-digit code is not a defence against someone who is already listening to that channel. Pick "Let them in" in the
invite sheet for the older behaviour (the key rides in the link).

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

**Letting a friend in through Cloudflare Access.** Access only admits the emails on its policy, so a friend could not even open an invite page. The hub treats a short list of paths as public
(`/i/<id>`, `/api/invite/<id>` status, `/api/invite/<id>/knock` and `/poll`, `/icon.svg`): each needs the invite's own secret or is read-only, is rate-limited per client address, and the host
still approves every knock with a passkey touch. Add a second Access application with a **bypass** policy for exactly those paths on the hub hostname (`<host>/i/*`, `<host>/api/invite/*`,
`<host>/icon.svg`) so Cloudflare lets them through; every other path still needs your Access login plus a passkey. The hub refuses all other tunnelled requests that lack a valid Access token.
Rooms the hub hosts are remembered and hosted again when the hub restarts.
