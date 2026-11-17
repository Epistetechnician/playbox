# Playbox Desktop: release plan

Status: Phase 1 started. `desktop/` holds the supervision core (`pbd-core`, tested, including the real hub) and a working Tauri shell (window, tray, graceful quit); bundling, signing, updates and the passkey spike are not done. See `desktop/README.md`. Decisions that need the owner are marked **DECIDE**; claims that must be tested before they are relied on are marked **SPIKE**.

## Goal

One signed download that installs the `playbox` binary and the hub together, keeps them running, updates them as one unit, and gives a machine that hosts rooms a clean, least-privilege footing. The PWA stays: it is how a phone or a second device reaches the hub. The desktop app is the *host's* home, not a replacement for the PWA and not a replacement for any agent harness.

Today the same job is done by scripts: `ui/service/install-macos.sh` (LaunchAgent + pinned hub copy), `ui/service/install-host-macos.sh`, `scripts/dev-sign.sh` and `scripts/macos-allow.sh` (stable self-signed identity, firewall allow), `playbox update`. Each works; together they are the part users cannot be asked to run by hand.

## What we looked at

All four are public; licences checked on GitHub on 2026-11-16.

| Project | Licence | What it is | Useful to us for |
|---|---|---|---|
| [NousResearch/hermes-agent `apps/desktop`](https://github.com/NousResearch/hermes-agent/tree/main/apps/desktop) | MIT | Electron + React shell around a **Python backend**, built with electron-builder | The closest shape to ours: a shell that supervises a Python server. Backend child supervision, single-owner "claim" of the backend, update feed contract with stable/canary channels, one product-identity file, `hermes://` protocol handler, macOS signer, MSIX for Windows, themes/skins, command palette, e2e and perf scripts |
| [rustfs/rustfs](https://github.com/rustfs/rustfs) | Apache-2.0 | S3-compatible object store in Rust (a large workspace) | **Release and security hygiene, not storage.** Staged release pipeline (freeze inputs, candidate, publish, complete) with pinned action SHAs; `deny.toml`, `fuzz/`, SPDX headers, `SECURITY.md`; small focused crates for `audit`, `signer`, `tls-runtime`, `trusted-proxies`, `security-governance`, `policy`, `iam` that show how to structure those concerns |
| [glimapp/glim](https://github.com/glimapp/glim) | MIT | Native macOS file viewer in Rust on GPUI (Zed's UI framework) | Release discipline: sign the update feed with ed25519, build installers in CI and **promote the exact-commit artifacts** into a draft release from a protected environment, checksums per file, `unsafe_code = "forbid"` and clippy denials at workspace level, a written privacy page, `THIRD_PARTY_LICENSES` |
| [Zesty0wl/mac-performance-monitor](https://github.com/Zesty0wl/mac-performance-monitor) | MIT | Swift menu-bar app, Developer ID signed and notarized, Sparkle updates | Menu-bar status and alerts pattern; **separate signing keys** for app updates and for downloaded data catalogs; a privileged helper that is demand-launched (`SMAppService`), user-approved in Login Items, and idle-exits; "no telemetry" stated plainly |

Licences are compatible with ours (Apache-2.0). Anything copied keeps its notice: MIT copyright lines in `THIRD_PARTY_LICENSES`, Apache-2.0 `NOTICE` handling for rustfs. Prefer re-implementing an idea in Rust over copying TypeScript or Swift; copy only small, self-contained pieces and say where they came from.

### What we will not adopt

- **GPUI / a native UI rewrite (glim).** Our UI is web, with a theme engine, a headless-browser smoke test and passkeys. A rewrite buys nothing the hub needs and loses all of that.
- **The RustFS storage engine.** Playbox rooms keep signed JSONL history; they do not need an object store. (An optional "export history to an S3-compatible bucket" target is a possible later feature, not part of desktop.)
- **Sparkle (glim, mac-performance-monitor).** macOS only. We already have a cross-platform update path: signed `release.json` + `SHA256SUMS`, verified against a key built into the binary (`docs/releasing.md`). One trust root is better than two.
- **A privileged helper by default (mac-performance-monitor).** Playbox should not need root. Revisit only if the firewall step (`scripts/macos-allow.sh`, currently a manual `sudo`) cannot be removed another way.
- **Electron wholesale** (see the shell decision below), unless the spike forces it.

## The shell: Tauri 2, with Electron as the fallback

**DECIDE after SPIKE 1.**

Hermes shows the shortest path (Electron, copy their supervision and updater code). Tauri keeps one Rust toolchain and a small download, and the app's core can *be* the `playbox` crates. Recommendation: **Tauri 2**, because the host binary is Rust and a second runtime (Chromium) is a large, separately-patched dependency on a security-sensitive machine.

**SPIKE 1 result (run 2026-11-16, macOS 26.7.2, `desktop/spikes/passkey_probe.py`): the embedded webview has no platform passkeys on `localhost`.**
The probe stands in for the hub inside the real Tauri app and asks WKWebView what WebAuthn can do:

| Build | `isUserVerifyingPlatformAuthenticatorAvailable()` | Result |
|---|---|---|
| ad-hoc signed (a plain `cargo build`) | `false` | no platform authenticator |
| signed with our stable local identity (`scripts/dev-sign.sh`) | `false` | same: a stable signature does not change it |
| signed with `com.apple.developer.web-browser.public-key-credential` | not reached | macOS kills the app at launch (exit 137): that entitlement is Apple-managed and needs a provisioning profile Apple grants to browsers |

(The `create()` calls in the same runs failed with "The document is not focused", so the *create* path was not exercised; the availability answer is the finding. A tap on the probe's button in a focused window repeats it.)

What this means: in a macOS WKWebView, a passkey needs either Apple's browser entitlement or an Associated Domains link between the app and a real HTTPS domain. Neither can apply to a `localhost` relying party, so **Tauri cannot give the hub's current passkey sign-in inside its own window, and no signing choice of ours changes that.** Electron was not measured (its binary download failed from this network); Chromium on macOS also needs a team-provisioned keychain access group for the platform authenticator, so assume the same until a build proves otherwise.

Outcomes, updated:
1. ~~Works in Tauri~~: ruled out for platform passkeys.
2. Electron: unmeasured; do not switch shells on a hope.
3. **Base case: the system browser is the passkey surface.** The window shows status and management and has an "Open in browser" action for sign-in; Safari, Chrome and the phone PWA handle platform passkeys natively. Ship this first.
4. **Follow-up (needs hub changes and a security review):** let the app sign in the owner it is running for, without a passkey in the window. Candidates: (a) a device key held by the app (Secure Enclave where the signing identity permits, otherwise the keychain) enrolled as a second credential type, with step-up still requiring a passkey in a browser; (b) a one-time loopback-only desktop session minted by the hub for the process that started it. Both weaken "who is at the keyboard" compared with a passkey touch, so sensitive actions must keep requiring a real passkey step-up.

**SPIKE 2: the hub is Python.** The hub is stdlib Python, so the app must either bundle a relocatable Python (python-build-standalone, roughly tens of MB) or the hub moves into the Rust binary. Plan: bundle Python first (small change, same code that runs today and is tested), and treat a Rust hub as an optional later project, not a prerequisite.

## Phases

Sizes are rough: S = days, M = 1–2 weeks, L = a month or more.

### Phase 0 — Spikes (S)
- Spike 1 and Spike 2 above, on a signed, notarized build.
- (Spike 1 is done, see above.) Confirm a loopback-only hub in a webview can still reach the Cloudflare tunnel for invite links (the tunnel is the hub's own child process, unchanged).
- **DECIDE:** an Apple Developer ID (paid) for notarization. Without it the app is ad-hoc signed and users must right-click to open, and Gatekeeper warns. Glim ships this way and says so in `SECURITY.md`; mac-performance-monitor is notarized. A host-machine product should be notarized.

### Phase 1 — Supervised shell (M)
Replaces the LaunchAgent scripts.
- The app starts and supervises `playbox` (hosts, `playbox mcp`), the hub (loopback, `--pair` never logged) and the tunnel, with the backoff and "why it could not" reporting the hub already has. Pattern from Hermes `backend-child` and `backend-claim`: **one owner per backend**, so a second launch attaches or refuses instead of fighting over the port.
- Menu-bar/tray item driven by `/api/system` health (tunnel, hosts, grant expiry), with the alert rules the hub already has. Pattern from mac-performance-monitor's status item and alerts.
- Launch at login via `SMAppService`/login-item registration, with the user-visible approval macOS provides.
- `playbox://` invite links open the app (Hermes registers `hermes://`; we already use `playbox://` and `web+playbox`).
- One product-identity file (name, bundle id, channel, URL scheme), as Hermes does, so the app, installer and updater cannot drift.

### Phase 2 — One signed bundle and updates (M)
*Started:* `desktop/scripts/stage.py` and `tauri.bundle.conf.json` stage and pack the hub, Python and binary; the tray has "Open in browser". Not yet: pinned Python per platform, the signing CI job, updates.
- Per-platform bundle: `Playbox.app` / AppImage / MSIX containing the binary, the hub, and bundled Python. Windows is new work; Hermes has MSIX/signing code to learn from.
- **Updates reuse our existing trust:** the app downloads `release.json` and the archive, checks SHA-256, the ed25519 signature against the key built into the binary, and the version the new binary reports. Same rules as `playbox update` (see `docs/releasing.md`), shared code.
- **Two keys, not one** (mac-performance-monitor): the release-signing key stays in `~/.playbox-release`; any downloaded *data* (for example a future signed checks catalog) gets its own key, so one compromise cannot forge the other.
- **Channels** stable and canary in the feed contract (Hermes). Canary is opt-in, never the default.
- **Staged release pipeline** (RustFS + glim): freeze the source SHA and inputs, build candidates, run the full CI on that exact commit, then promote *those same artifacts* to a draft release from a protected environment where the signing key lives, then publish. Pin third-party actions by SHA. No rebuilding at publish time.
- Notarize and staple the macOS bundle in the candidate stage (needs the Developer ID above).

### Phase 3 — Host hygiene, enforced by the app (M)
The "cleanliness and security" goal. Each item is a check the app runs and shows, backed by `playbox secure` where it exists.
- Unprivileged by default: no root, no helper. Hosts run sandboxed tasks by default; a host with no runtime refuses task requests rather than running them bare.
- Loopback-only hub and outbound-only guests unless the owner opts in (already the default in the binary).
- Secrets in the OS keychain, not files: the hub's passkey store key, the Cloudflare tunnel token, any integration keys. Migrate on first run, then delete the files.
- Approval enforcement on by default for the owner (`PLAYBOX_APPROVERS` is advisory today).
- Workspace lints: `unsafe_code = "forbid"` where possible and clippy denials for `dbg_macro`/`todo` (glim); keep `deny.toml`, `cargo-deny`, the fuzz targets and add SPDX headers (RustFS). Ship `THIRD_PARTY_LICENSES` in the bundle.
- A written privacy page: nothing leaves the machine except what a room carries and update checks; no telemetry (glim, mac-performance-monitor).
- `SECURITY.md` with private reporting (all three projects have one).
- Move trust-boundary code into small crates with clear names, as RustFS does (`audit`, `trusted-proxies`, `tls-runtime`): the hub's Cloudflare Access check and proxy-header handling are the first candidates once the hub is Rust, and are fine as they are until then.

### Phase 4 — Many machines, verified (L)
Builds on `docs/agent-identity-prd.md` (identity cards, admissions, delegation).
- Each machine has a stable identity key and fingerprint, kept in the keychain/Secure Enclave.
- Joining a room compares a short code out-of-band (the ask-to-join flow already does this for friends); machines are then pinned by key, so a changed key is a visible event, not a silent one.
- One-tap revoke of a machine across every room it is in, with the signed record of who revoked what.
- The app shows, per room, which machines are verified, pending or revoked, and when each grant expires.

### Phase 5 — Optional
- Rust hub (removes bundled Python).
- Export room history to an S3-compatible bucket (RustFS can be the target).
- Linux packaging beyond AppImage (deb/rpm, as RustFS ships an rpm spec and Helm chart).

## Risks and open questions

- **Passkeys in a webview** is the one result that can change the plan (Spike 1).
- **Python bundling** adds a runtime to patch. Mitigation: pin it, ship it inside the signed bundle, update it with the app, and list it in the SBOM we already produce.
- **Notarization cost and process** is the owner's call; without it, macOS users see warnings.
- **Windows** is untested ground for Playbox. The first desktop release should be macOS and Linux; Windows follows once the shell is stable.
- **Scope.** Phase 1 is a useful release on its own (one app that runs and supervises everything). Phases 2–4 can each ship independently.

## Suggested first release

"Playbox Desktop 0.x" = Phase 0 + Phase 1 + the Phase 2 signed-bundle and update path, macOS first. That removes every install script and gives the host a menu-bar status. Phases 3 and 4 follow as ordinary releases of the same app.
