"""Authenticated live HTTP E2E test against a throwaway Playbox Hub instance.

Verifies static ETag/304 caching, /api/system, /api/settings/search (secure classifier),
and /api/search (cross-room archive search) without touching the user's real hub or rooms.
"""
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
UI_DIR = ROOT / "ui"


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def http_req(url, *, headers=None):
    req = urllib.request.Request(url, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            return r.status, dict(r.headers), r.read()
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), e.read()


def test_hub_live_http_e2e():
    tmp = tempfile.mkdtemp(prefix="pbx-live-e2e-")
    port = free_port()
    proc = None
    try:
        # Pre-seed an authenticated local session token + sample archived room in the throwaway hub store
        sys.path.insert(0, str(UI_DIR))
        from hub.archive import Archive
        from hub.auth import Auth
        from hub.store import Store

        hub_dir = Path(tmp)
        store = Store(hub_dir)
        auth = Auth(store)
        now_s = time.time()
        rid = "live-e2e-room"
        room_dir = hub_dir / "rooms" / rid
        room_dir.mkdir(parents=True, exist_ok=True)
        with store.lock:
            store.d["creds"].append({
                "id": "cred-e2e", "rp_id": "localhost", "name": "E2E Key", "created": now_s, "last": now_s,
            })
            store.d["rooms"].append({
                "id": rid, "label": "Live E2E Room", "kind": "join",
                "path": str(room_dir), "created": now_s, "muted": False,
            })
            store.save()
        tok = auth.new_session("cred-e2e", False, "live-e2e", "127.0.0.1", "E2E Key")
        Archive(room_dir).append([
            {"seq": 1, "kind": "event", "from": "peer1", "text": "task ok sh exit 0", "time_ms": 1700000001000,
             "meta": {"event": "task", "ok": True, "runner": "openshell", "image": "pb-swe-django:arm64", "ms": 42}},
        ])

        env = dict(os.environ, PLAYBOX_HUB_HOME=tmp, PLAYBOX_HUB_NO_MACHINE="1")
        proc = subprocess.Popen(
            [sys.executable, str(UI_DIR / "playbox_hub.py"), "--port", str(port), "--home", tmp],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )
        base = f"http://127.0.0.1:{port}"
        deadline = time.time() + 10
        ready = False
        while time.time() < deadline:
            try:
                code, _, _ = http_req(base + "/manifest.webmanifest")
                if code == 200:
                    ready = True
                    break
            except Exception:
                time.sleep(0.15)
        assert ready, "throwaway hub did not start in time"

        # 1. Static asset ETag + 304 Not Modified
        code, hdrs, body = http_req(base + "/settings.js")
        assert code == 200 and len(body) > 1000, (code, len(body))
        etag = hdrs.get("ETag") or hdrs.get("etag")
        assert etag and etag.startswith('W/"'), hdrs
        code304, _, _ = http_req(base + "/settings.js", headers={"If-None-Match": etag})
        assert code304 == 304, f"expected 304, got {code304}"

        # 2. Unauthenticated API endpoints must remain gated (401)
        code_unauth, _, _ = http_req(base + "/api/settings/search?q=tailscale")
        assert code_unauth == 401, f"expected 401 unauthenticated, got {code_unauth}"
        code_unauth2, _, _ = http_req(base + "/api/search?q=openshell")
        assert code_unauth2 == 401, f"expected 401 unauthenticated, got {code_unauth2}"

        # 3. Authenticated API endpoints with seeded session cookie return 200 + expected payloads
        auth_hdr = {"Cookie": f"pbs={tok}"}
        c_sys, _, b_sys = http_req(base + "/api/system", headers=auth_hdr)
        assert c_sys == 200, f"expected 200 /api/system, got {c_sys}"
        sys_data = json.loads(b_sys.decode("utf-8"))
        assert "health" in sys_data and "posture" in sys_data, sys_data.keys()

        c_set, _, b_set = http_req(base + "/api/settings/search?q=tailscale", headers=auth_hdr)
        assert c_set == 200, f"expected 200 /api/settings/search, got {c_set}"
        set_data = json.loads(b_set.decode("utf-8"))
        assert set_data.get("results"), set_data

        c_srch, _, b_srch = http_req(base + "/api/search?q=openshell", headers=auth_hdr)
        assert c_srch == 200, f"expected 200 /api/search, got {c_srch}"
        srch_data = json.loads(b_srch.decode("utf-8"))
        assert srch_data.get("total", 0) >= 1 and srch_data["results"][0]["room"] == rid, srch_data
    finally:
        if proc:
            proc.terminate()
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
        shutil.rmtree(tmp, ignore_errors=True)


def main():
    test_hub_live_http_e2e()
    print("hub live HTTP E2E passed (ETag/304 + auth gating + authenticated API verified)")


if __name__ == "__main__":
    main()
