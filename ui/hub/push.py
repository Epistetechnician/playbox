"""Web Push with VAPID, payload-free.

Each push is an empty POST authenticated by an ES256 VAPID token. Nothing about a room is ever handed to the
push service (Apple, Google, Mozilla): the service worker wakes, fetches /api/attention from the hub over
the authenticated session, and decides what to show. Content encryption (aes128gcm) is not needed because
there is no content.
"""
import ipaddress
import json
import time
import urllib.error
import urllib.parse
import urllib.request

from . import p256
from .webauthn import b64u

MAX_SUBS = 20


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


_opener = urllib.request.build_opener(_NoRedirect)


def valid_endpoint(ep):
    """A push endpoint is chosen by the browser, but the hub POSTs to whatever a session submits, so refuse anything that is not a public
    https name on port 443 (no IP literals, no localhost / .local / single-label intranet names, no credentials). Push services are named hosts."""
    try:
        u = urllib.parse.urlparse(ep)
        port = u.port
    except ValueError:
        return False
    h = (u.hostname or "").lower()
    if u.scheme != "https" or not h or u.username or u.password or port not in (None, 443) or len(ep) > 1000:
        return False
    try:
        ipaddress.ip_address(h)
        return False
    except ValueError:
        pass
    return "." in h and not h.endswith((".local", ".localhost", ".internal", ".lan", ".home", ".corp", ".localdomain"))


SUBJECT = "https://github.com/Epistetechnician/playbox"


def vapid_header(vapid_d, vapid_pub_b64, endpoint, now=None):
    u = urllib.parse.urlparse(endpoint)
    seg = lambda o: b64u(json.dumps(o, separators=(",", ":")).encode())
    head = seg({"typ": "JWT", "alg": "ES256"}) + "." + seg({"aud": f"{u.scheme}://{u.netloc}", "exp": int((now or time.time()) + 12 * 3600), "sub": SUBJECT})
    r, s = p256.sign(vapid_d, head.encode())
    jwt = head + "." + b64u(r.to_bytes(32, "big") + s.to_bytes(32, "big"))
    return f"vapid t={jwt}, k={vapid_pub_b64}"


def send(vapid_d, vapid_pub_b64, endpoint, timeout=10):
    """Returns the HTTP status, or 0 on a network error. 404/410 mean the subscription is gone."""
    if not valid_endpoint(endpoint):
        return 0
    req = urllib.request.Request(endpoint, data=b"", method="POST", headers={
        "Authorization": vapid_header(vapid_d, vapid_pub_b64, endpoint), "TTL": "600", "Urgency": "normal", "Content-Length": "0"})
    try:
        with _opener.open(req, timeout=timeout) as f:
            return f.status
    except urllib.error.HTTPError as e:
        return e.code
    except (urllib.error.URLError, OSError):
        return 0
