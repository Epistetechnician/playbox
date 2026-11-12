"""Web Push with VAPID, payload-free.

Each push is an empty POST authenticated by an ES256 VAPID token. Nothing about a room is ever handed to the
push service (Apple, Google, Mozilla): the service worker wakes, fetches /api/attention from the hub over
the authenticated session, and decides what to show. Content encryption (aes128gcm) is not needed because
there is no content.
"""
import json
import time
import urllib.error
import urllib.parse
import urllib.request

from . import p256
from .webauthn import b64u

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
    if not endpoint.startswith("https://"):
        return 0
    req = urllib.request.Request(endpoint, data=b"", method="POST", headers={
        "Authorization": vapid_header(vapid_d, vapid_pub_b64, endpoint), "TTL": "600", "Urgency": "normal", "Content-Length": "0"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as f:
            return f.status
    except urllib.error.HTTPError as e:
        return e.code
    except (urllib.error.URLError, OSError):
        return 0
