#!/usr/bin/env python3
"""Passkey spike: stands in for the hub (same --port/--home arguments, answers the health probe) and serves one page that asks the embedded
webview what WebAuthn can do on `localhost`. The page posts its findings to /report, which are appended as JSON lines to $SPIKE_REPORT.

  PLAYBOX_DESKTOP_HUB_SCRIPT=desktop/spikes/passkey_probe.py SPIKE_REPORT=/tmp/report.jsonl PLAYBOX_DESKTOP_PORT=7741 cargo run -p playbox-desktop

The first run happens on load (no user gesture); the button repeats the platform-authenticator attempt with a real click."""
import argparse
import http.server
import json
import os
import time

PAGE = r"""<!doctype html><meta charset=utf-8><title>passkey spike</title>
<body style="font:14px system-ui;padding:24px"><h3>Playbox passkey spike</h3><pre id=o>running…</pre>
<button id=b style="padding:10px 16px;font-size:15px">Try a platform passkey (click)</button>
<script>
const out = {origin: location.origin, secure: isSecureContext, ua: navigator.userAgent, hasPublicKeyCredential: !!window.PublicKeyCredential};
const show = () => document.getElementById('o').textContent = JSON.stringify(out, null, 1);
const send = tag => fetch('/report', {method: 'POST', body: JSON.stringify({tag, ...out})}).catch(() => {});
const opts = att => ({publicKey: {challenge: crypto.getRandomValues(new Uint8Array(32)), rp: {id: 'localhost', name: 'Playbox'},
  user: {id: crypto.getRandomValues(new Uint8Array(16)), name: 'spike', displayName: 'spike'}, pubKeyCredParams: [{type: 'public-key', alg: -7}, {type: 'public-key', alg: -257}],
  ...(att ? {authenticatorSelection: {authenticatorAttachment: att, userVerification: 'preferred'}} : {}), timeout: 15000}});
async function step(name, fn) { const t = performance.now(); try { out[name] = await fn(); } catch (e) { out[name] = 'ERR ' + e.name + ': ' + e.message; } out[name + '_ms'] = Math.round(performance.now() - t); show(); }
async function platform() { const c = await navigator.credentials.create(opts('platform')); return 'created ' + c.id.slice(0, 10); }
(async () => {
  if (window.PublicKeyCredential) {
    await step('isUserVerifyingPlatformAuthenticatorAvailable', () => PublicKeyCredential.isUserVerifyingPlatformAuthenticatorAvailable());
    await step('isConditionalMediationAvailable', async () => PublicKeyCredential.isConditionalMediationAvailable ? await PublicKeyCredential.isConditionalMediationAvailable() : 'n/a');
    await step('create_platform_noclick', platform);
    await step('create_crossplatform_noclick', async () => { const c = await navigator.credentials.create(opts('cross-platform')); return 'created ' + c.id.slice(0, 10); });
  }
  await send('auto');
})();
document.getElementById('b').onclick = async () => { await step('create_platform_click', platform); await send('click'); };
</script></body>"""


class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _send(self, code, body, ctype="text/html; charset=utf-8"):
        b = body.encode() if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        if self.path.startswith("/api/auth/state"):
            return self._send(200, '{"authed":true,"enrolled":true}', "application/json")
        return self._send(200, PAGE)

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n).decode(errors="replace")
        with open(os.environ.get("SPIKE_REPORT", "/dev/null"), "a") as f:
            f.write(json.dumps({"at": time.strftime("%H:%M:%S"), "report": json.loads(body or "{}")}) + "\n")
        self._send(200, "{}", "application/json")


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=7741)
    ap.add_argument("--home")
    a, _ = ap.parse_known_args()
    http.server.ThreadingHTTPServer(("127.0.0.1", a.port), H).serve_forever()
