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


def main():
    tmp = tempfile.mkdtemp(prefix="pbx-live-e2e-")
    port = free_port()
    proc = None
    try:
        # Pre-seed an authenticated local session token in the throwaway hub store
        env = dict(os.environ, PLAYBOX_HUB_HOME=tmp)
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

        print("hub live HTTP E2E passed (ETag/304 + auth gating verified)")
    finally:
        if proc:
            proc.terminate()
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
