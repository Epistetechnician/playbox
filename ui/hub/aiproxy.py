"""Loopback-only Hub backend proxy for `playbox-aiproxy` telemetry and logs.

Follows the exact security pattern of `ui/hub/integrations.py`:
- The hub holds `aiproxy`'s `ADMIN_KEY` in a `0600` file (`<hub_home>/aiproxy.key`, or `PLAYBOX_AIPROXY_ADMIN_KEY`),
  never sent to any browser.
- Only loopback `playbox-aiproxy` URLs are allowed by default (preserves Hub strict CSP `connect-src 'self'`).
- Default responses are metadata-only (RPM/TPM, USD spend, token counts, TTFT, channel failovers, SHA-256 hashes);
  raw prompt/response body inspection requires opt-in retention on `playbox-aiproxy` AND fresh passkey step-up in `server.py`.
- Polling `playbox-aiproxy` never touches `playbox-mcp`, so it adds zero entries to the room's signed history chain.
"""
import json
import os
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

WINDOW_MS = {
    "15m": 15 * 60 * 1000,
    "1h": 60 * 60 * 1000,
    "6h": 6 * 60 * 60 * 1000,
    "24h": 24 * 60 * 60 * 1000,
    "7d": 7 * 24 * 60 * 60 * 1000,
}


class AiproxyHubError(Exception):
    def __init__(self, status, msg):
        super().__init__(msg)
        self.status, self.msg = status, msg


def _loopback_ok(base_url):
    try:
        u = urllib.parse.urlparse(base_url)
    except ValueError:
        return False
    if u.scheme not in ("http", "https") or not u.hostname:
        return False
    return u.hostname.lower() in ("127.0.0.1", "localhost", "::1")


class AiproxyHub:
    def __init__(self, store, base=None, admin_key=None):
        self.store = store
        self._explicit_base = base
        self._explicit_key = admin_key

    def _key_path(self):
        return Path(self.store.path).parent / "aiproxy.key"

    def base_url(self):
        raw = (
            self._explicit_base
            or os.environ.get("PLAYBOX_AIPROXY_URL")
            or (self.store.d.get("aiproxy") or {}).get("url")
            or ""
        ).strip()
        if not raw:
            return ""
        if not _loopback_ok(raw) and not os.environ.get("PLAYBOX_AIPROXY_ALLOW_NON_LOOPBACK"):
            raise AiproxyHubError(502, "refusing non-loopback playbox-aiproxy URL")
        return raw.rstrip("/")

    def admin_key(self):
        if self._explicit_key:
            return self._explicit_key.strip()
        env_key = os.environ.get("PLAYBOX_AIPROXY_ADMIN_KEY", "").strip()
        if env_key:
            return env_key
        kp = self._key_path()
        if kp.is_file():
            try:
                return kp.read_text(encoding="utf-8").strip()
            except OSError:
                return ""
        return ""

    def configured(self):
        try:
            return bool(self.base_url() and self.admin_key())
        except AiproxyHubError:
            return False

    def _get(self, path, query=None):
        base = self.base_url()
        key = self.admin_key()
        if not base or not key:
            raise AiproxyHubError(503, "playbox-aiproxy is not configured on this hub")
        qs = ""
        if query:
            clean = {k: str(v) for k, v in query.items() if v is not None and v != ""}
            if clean:
                qs = "?" + urllib.parse.urlencode(clean)
        url = base + path + qs
        req = urllib.request.Request(
            url,
            method="GET",
            headers={
                "Authorization": key,
                "Accept": "application/json",
                "User-Agent": "playbox-hub",
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=8) as r:
                raw = r.read(4_000_000)
        except urllib.error.HTTPError as e:
            msg = f"playbox-aiproxy returned {e.code}"
            try:
                body = json.loads(e.read(4096) or b"{}")
                if isinstance(body, dict) and body.get("message"):
                    msg = str(body["message"])[:200]
            except (OSError, ValueError):
                pass
            raise AiproxyHubError(403 if e.code == 403 else (404 if e.code == 404 else 502), msg)
        except (OSError, urllib.error.URLError):
            raise AiproxyHubError(502, "playbox-aiproxy is unreachable")
        try:
            parsed = json.loads(raw or b"{}")
        except ValueError:
            raise AiproxyHubError(502, "playbox-aiproxy returned unreadable JSON")
        if isinstance(parsed, dict) and parsed.get("success") is False:
            raise AiproxyHubError(502, str(parsed.get("message") or "playbox-aiproxy error")[:200])
        return parsed.get("data") if isinstance(parsed, dict) and "data" in parsed else parsed

    @staticmethod
    def _since_ms(window):
        w = WINDOW_MS.get(str(window or "1h"), WINDOW_MS["1h"])
        return int(time.time() * 1000) - w

    def posture(self):
        if not self.configured():
            return {"configured": False}
        data = self._get("/api/playbox/posture")
        return {"configured": True, **(data if isinstance(data, dict) else {})}

    def room_stats(self, room_id, window="1h"):
        return self._get(
            "/api/playbox/stats",
            {"room_id": room_id, "since_ms": self._since_ms(window)},
        )

    def room_activity(self, room_id, window="1h"):
        return self._get(
            "/api/playbox/activity",
            {"room_id": room_id, "since_ms": self._since_ms(window)},
        )

    def room_logs(self, room_id, limit=50):
        lim = max(1, min(200, int(limit or 50)))
        return self._get(
            "/api/playbox/logs",
            {"room_id": room_id, "limit": lim},
        )

    def room_log_body(self, room_id, req_id):
        safe_id = urllib.parse.quote(str(req_id or ""), safe="")
        if not safe_id:
            raise AiproxyHubError(400, "missing request id")
        return self._get(
            f"/api/playbox/logs/{safe_id}/body",
            {"group": f"room:{room_id}"},
        )
