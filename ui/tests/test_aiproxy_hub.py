"""Hub backend proxy & security posture tests for `playbox-aiproxy` (`ui/hub/aiproxy.py`, `ui/hub/sec.py`)."""
import json
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

UI_DIR = Path(__file__).resolve().parents[1]
REPO_ROOT = UI_DIR.parent
sys.path.insert(0, str(UI_DIR))
sys.path.insert(0, str(REPO_ROOT / "hermes-playbox-backend"))

from hub.aiproxy import AiproxyHub, AiproxyHubError  # noqa: E402
from hub.sec import posture  # noqa: E402


class _FakeStore:
    def __init__(self, path):
        self.path = path
        self.d = {}


# REQ: PROXY-1, PROXY-2, PROXY-3, PROXY-5, PROXY-6, PROXY-7
def test_hub_aiproxy_loopback_proxy_and_posture():
    seen = []

    class FakeAiproxy(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def _send(self, code, obj):
            raw = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def do_GET(self):
            seen.append(self.path)
            if self.headers.get("Authorization") != "hub-0600-fixture":
                return self._send(401, {"success": False, "message": "unauthorized"})
            if self.path == "/api/playbox/posture":
                return self._send(200, {"success": True, "data": {
                    "playbox_hardened": True,
                    "mcp_disabled": True,
                    "web_search_disabled": True,
                    "loopback_only": True,
                    "body_retention_days": 0,
                }})
            if self.path.startswith("/api/playbox/stats"):
                return self._send(200, {"success": True, "data": {
                    "provenance": "host_aiproxy",
                    "totals": {"requests": 4, "cost_usd": 0.012, "failovers": 1, "budget_cutoffs": 0},
                    "by_model": {"claude-sonnet-4-6": {"requests": 4, "tokens": 5000, "cost_usd": 0.012}},
                }})
            if self.path.startswith("/api/playbox/activity"):
                return self._send(200, {"success": True, "data": {"provenance": "host_aiproxy", "spans": []}})
            if self.path.startswith("/api/playbox/logs/req-1/body"):
                return self._send(403, {"success": False, "message": "body retention is disabled"})
            if self.path.startswith("/api/playbox/logs"):
                return self._send(200, {"success": True, "data": {"provenance": "host_aiproxy", "entries": []}})
            return self._send(404, {"success": False})

    srv = HTTPServer(("127.0.0.1", 0), FakeAiproxy)
    thr = threading.Thread(target=srv.serve_forever, daemon=True)
    thr.start()
    try:
        with tempfile.TemporaryDirectory() as td:
            hub_json = Path(td) / "hub.json"
            marker_file = Path(td) / "aiproxy.key"
            marker_file.write_text("hub-0600-fixture\n", encoding="utf-8")
            marker_file.chmod(0o600)
            store = _FakeStore(hub_json)

            # Rejects non-loopback URLs by default.
            bad_hub = AiproxyHub(store, base="http://192.168.1.9:3000")
            try:
                bad_hub.base_url()
                assert False, "expected non-loopback refusal"
            except AiproxyHubError as exc:
                assert exc.status == 502

            ap = AiproxyHub(store, base=f"http://127.0.0.1:{srv.server_port}")
            assert ap.configured() is True
            p = ap.posture()
            assert p["configured"] is True and p["playbox_hardened"] is True

            st = ap.room_stats("room-1", "1h")
            assert st["totals"]["cost_usd"] == 0.012
            assert ap.room_activity("room-1", "1h")["provenance"] == "host_aiproxy"
            assert ap.room_logs("room-1", 25)["provenance"] == "host_aiproxy"

            # Body endpoint stays 403 when retention is disabled.
            try:
                ap.room_log_body("room-1", "req-1")
                assert False, "expected 403 when body retention is disabled"
            except AiproxyHubError as exc:
                assert exc.status == 403

            # SecLog posture reports ok for hardened loopback gateway, bad for unhardened.
            ok_checks = posture(
                public=False, access=False, creds=1, weak_sessions=0,
                invites={"keyed_open": 0, "guest_locked_open": 0},
                knocks=0, push=1, rooms=[], fresh=True, aiproxy=p,
            )
            ap_check = next(c for c in ok_checks if c["id"] == "aiproxy")
            assert ap_check["level"] == "ok"

            bad_checks = posture(
                public=False, access=False, creds=1, weak_sessions=0,
                invites={"keyed_open": 0, "guest_locked_open": 0},
                knocks=0, push=1, rooms=[], fresh=True,
                aiproxy={"configured": True, "playbox_hardened": False, "loopback_only": False},
            )
            bad_ap = next(c for c in bad_checks if c["id"] == "aiproxy")
            assert bad_ap["level"] == "bad"
    finally:
        srv.shutdown()
        srv.server_close()


# REQ: PROXY-3, PROXY-5, PROXY-7
def test_drill3_live_hub_pwa_with_real_aiproxy_and_zero_audit_growth():
    """Drill 3: Live Hub HTTP server + real compiled hardened `target/playbox-aiproxy` + passkey session.

    Verifies:
    - `/api/system` posture shows `Model gateway: ok`
    - `/api/rooms/<id>/llm/{stats,activity,logs}` populate live from real SQLite gateway traffic
    - `audit.log` has 0 extra entries appended across repeated Hub LLM polls
    - `/api/rooms/<id>/llm/logs/<id>/body` returns 403 when retention is disabled (and 403 step-up on stale remote session)
    - `instruments.js` `PBI.llmPanels()` renders the live Hub payloads cleanly
    """
    import http.client
    import os
    import socket
    import subprocess
    import time
    import urllib.request

    aiproxy_bin = REPO_ROOT / "target" / "playbox-aiproxy"
    if not aiproxy_bin.is_file():
        return

    from hub.auth import Auth  # noqa: E402
    from hub.sec import SecLog  # noqa: E402
    from hub.server import make_handler  # noqa: E402
    from hub.store import Store  # noqa: E402
    from http.server import ThreadingHTTPServer  # noqa: E402

    # 1. Start mock OpenAI upstream returning usage tokens.
    class FakeUpstream(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_POST(self):
            n = int(self.headers.get("Content-Length", "0") or 0)
            _ = self.rfile.read(n)
            body = json.dumps({
                "id": "chatcmpl-hub",
                "object": "chat.completion",
                "created": 1795130000,
                "model": "gpt-hub-drill",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "hub ok"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 200, "completion_tokens": 50, "total_tokens": 250},
            }).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    up_srv = HTTPServer(("127.0.0.1", 0), FakeUpstream)
    threading.Thread(target=up_srv.serve_forever, daemon=True).start()

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        ap_port = s.getsockname()[1]

    with tempfile.TemporaryDirectory(prefix="pbx-drill3-") as td:
        td_p = Path(td)
        ap_dir = td_p / "aiproxy"
        ap_dir.mkdir()
        fixture_handle = "drill3-hub-admin-fixture-0123456"
        ap_base = f"http://127.0.0.1:{ap_port}"

        env = dict(os.environ)
        env["ADMIN_KEY"] = fixture_handle
        env["LISTEN"] = f"127.0.0.1:{ap_port}"
        env["SQL_DSN"] = ""
        env["LOG_SQL_DSN"] = ""
        ap_proc = subprocess.Popen(
            [str(aiproxy_bin), "--listen", f"127.0.0.1:{ap_port}", "--pprof-port", "0"],
            cwd=str(ap_dir),
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

        def _ap_req(method, path, body=None, auth=fixture_handle, extra_headers=None):
            hdrs = {"Authorization": auth, "Content-Type": "application/json"}
            if extra_headers:
                hdrs.update(extra_headers)
            req = urllib.request.Request(
                ap_base + path,
                method=method,
                data=None if body is None else json.dumps(body).encode(),
                headers=hdrs,
            )
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, json.loads(r.read() or b"{}")

        hub_srv = None
        old_url = os.environ.get("PLAYBOX_AIPROXY_URL")
        old_nomach = os.environ.get("PLAYBOX_HUB_NO_MACHINE")
        try:
            ready = False
            for _ in range(60):
                try:
                    st, _ = _ap_req("GET", "/api/playbox/posture")
                    if st == 200:
                        ready = True
                        break
                except Exception:
                    time.sleep(0.15)
            assert ready, "playbox-aiproxy did not start for Drill 3"

            rid = "drill3-room"
            _ap_req("POST", "/api/model_config/gpt-hub-drill", {
                "model": "gpt-hub-drill", "type": 1, "owner": "openai",
                "price": {"input_price": 1.0, "input_price_unit": 1000, "output_price": 2.0, "output_price_unit": 1000},
            })
            _ap_req("POST", "/api/channel/", {
                "name": "hub-up", "type": 1, "key": "sk-hub",
                "base_url": f"http://127.0.0.1:{up_srv.server_port}",
                "models": ["gpt-hub-drill"], "priority": 10, "status": 1,
            })
            _ap_req("POST", f"/api/group/room:{rid}", {"rpm_ratio": 10, "tpm_ratio": 10})
            _, tok_resp = _ap_req("POST", f"/api/token/room:{rid}", {
                "name": "hub-worker", "quota": 5.0, "subnets": ["127.0.0.1/32", "::1/128"],
            })
            tok_key = tok_resp["data"]["key"]
            time.sleep(0.2)

            # Fire 2 real LLM requests tagged to `room:drill3-room` so SQLite has live rows.
            for i in range(2):
                st, _ = _ap_req(
                    "POST",
                    "/v1/chat/completions",
                    {"model": "gpt-hub-drill", "messages": [{"role": "user", "content": f"ping {i}"}]},
                    auth=f"Bearer {tok_key}",
                    extra_headers={"X-Playbox-Room-Id": rid, "X-Playbox-Member-Id": "w0"},
                )
                assert st == 200

            # Wait for async consumer to flush into SQLite.
            for _ in range(30):
                _, chk = _ap_req("GET", f"/api/playbox/stats?room_id={rid}")
                if (chk.get("data") or {}).get("totals", {}).get("requests", 0) >= 2:
                    break
                time.sleep(0.15)

            # 2. Boot an isolated real Hub HTTP server pointed at `PLAYBOX_AIPROXY_URL` with 0600 `aiproxy.key`.
            hub_dir = td_p / "hub"
            hub_dir.mkdir()
            (hub_dir / "aiproxy.key").write_text(fixture_handle + "\n", encoding="utf-8")
            (hub_dir / "aiproxy.key").chmod(0o600)
            audit_log = hub_dir / "audit.log"
            audit_log.write_text('{"seq":0,"kind":"session_start"}\n', encoding="utf-8")
            audit_before = audit_log.read_text(encoding="utf-8")

            store = Store(hub_dir)
            auth = Auth(store)
            from hub.auth import sha  # noqa: E402
            now_s = time.time()
            with store.lock:
                store.d["creds"].append({"id": "cred1", "rp_id": "127.0.0.1", "name": "Drill3 Passkey", "created": now_s, "last": now_s})
                store.save()
            sess_tok = auth.new_session("cred1", False, "pytest", "127.0.0.1", "Drill3 Passkey")
            stale_tok = auth.new_session("cred1", False, "pytest", "203.0.113.5", "Remote Stale")
            with store.lock:
                store.d["sessions"][sha(stale_tok)]["fresh_until"] = now_s - 3600
                store.save()

            class _FakeHub:
                def __init__(self):
                    self.mu = threading.Lock()
                    self.rooms = {}
                    self.sec = SecLog(None)
                    self._rooms = {rid: {"id": rid, "label": "Drill 3 Room", "mode": "host", "state": "live", "holds": []}}

                def get(self, r):
                    return self._rooms.get(r)

                def digests(self):
                    return list(self._rooms.values())

                def down_alert_s(self):
                    return 300

                class knocks:
                    @staticmethod
                    def pending():
                        return []

            os.environ["PLAYBOX_AIPROXY_URL"] = ap_base
            os.environ["PLAYBOX_HUB_NO_MACHINE"] = "1"
            handler = make_handler(_FakeHub(), auth, store, {"port": 0, "cloudflare": False})
            hub_srv = ThreadingHTTPServer(("127.0.0.1", 0), handler)
            threading.Thread(target=hub_srv.serve_forever, daemon=True).start()
            hport = hub_srv.server_port

            def _hub_get(path, tok=sess_tok, extra_hdrs=None):
                conn = http.client.HTTPConnection("127.0.0.1", hport, timeout=5)
                hdrs = {"Cookie": f"pbs={tok}", "Host": f"127.0.0.1:{hport}"}
                if extra_hdrs:
                    hdrs.update(extra_hdrs)
                conn.request("GET", path, headers=hdrs)
                resp = conn.getresponse()
                raw = resp.read()
                conn.close()
                return resp.status, json.loads(raw or b"{}")

            # Check /api/system posture reports Model gateway: ok!
            st, sys_j = _hub_get("/api/system")
            assert st == 200, sys_j
            ap_posture = next((c for c in sys_j.get("posture", []) if c["id"] == "aiproxy"), None)
            assert ap_posture is not None and ap_posture["level"] == "ok", f"unexpected posture: {ap_posture}"

            # Poll live Hub LLM endpoints 5 times and verify data + 0 audit.log growth!
            stats_j = act_j = logs_j = None
            for _ in range(5):
                st1, stats_j = _hub_get(f"/api/rooms/{rid}/llm/stats?window=1h")
                st2, act_j = _hub_get(f"/api/rooms/{rid}/llm/activity?window=1h")
                st3, logs_j = _hub_get(f"/api/rooms/{rid}/llm/logs?limit=20")
                assert (st1, st2, st3) == (200, 200, 200)

            assert stats_j["totals"]["requests"] >= 2
            assert stats_j["totals"]["cost_usd"] > 0
            assert "gpt-hub-drill" in stats_j["by_model"]
            assert len(act_j["spans"]) >= 2
            assert len(logs_j["entries"]) >= 2
            assert logs_j["body_retention"] is False
            assert audit_log.read_text(encoding="utf-8") == audit_before, "Hub LLM polls must NOT append to audit.log"

            # Body endpoint returns 403 when retention is disabled (even with fresh session).
            req_id = logs_j["entries"][0]["req_id"]
            st_body, _ = _hub_get(f"/api/rooms/{rid}/llm/logs/{req_id}/body")
            assert st_body == 403, f"expected 403 on /body without retention, got {st_body}"

            # Stale remote session (via proxy header so not loopback-waived) is rejected for step-up BEFORE reaching aiproxy!
            st_stale, stale_j = _hub_get(
                f"/api/rooms/{rid}/llm/logs/{req_id}/body",
                tok=stale_tok,
                extra_hdrs={"X-Forwarded-For": "203.0.113.5"},
            )
            assert st_stale == 403 and stale_j.get("error") == "stepup", f"expected passkey step-up 403, got {st_stale} {stale_j}"

            # Render live Hub responses with real `ui/static/instruments.js` PBI.llmPanels() in Node.
            node_code = (
                "const fs = require('fs');"
                "global.window = {}; global.document = {getElementById: () => ({}), createElement: () => ({}), head: {appendChild: () => {}}};"
                "eval(fs.readFileSync(process.argv[1], 'utf8'));"
                "const d = JSON.parse(process.argv[2]);"
                "const html = window.PBI.llmPanels(d);"
                "if (!html.includes('Model &amp; Spend') || !html.includes('Inference Activity') || !html.includes('Model Trail') || !html.includes('Host-observed (aiproxy)') || !html.includes('gpt-hub-drill')) { process.exit(2); }"
                "console.log('INSTRUMENTS_RENDER_OK', html.length);"
            )
            nr = subprocess.run(
                ["node", "-e", node_code, str(UI_DIR / "static" / "instruments.js"), json.dumps({
                    "stats": stats_j, "activity": act_j, "logs": logs_j, "budgetCapUsd": 1.0,
                })],
                capture_output=True, text=True, check=True,
            )
            assert "INSTRUMENTS_RENDER_OK" in nr.stdout
        finally:
            if hub_srv is not None:
                hub_srv.shutdown()
                hub_srv.server_close()
            if old_url is None:
                os.environ.pop("PLAYBOX_AIPROXY_URL", None)
            else:
                os.environ["PLAYBOX_AIPROXY_URL"] = old_url
            if old_nomach is None:
                os.environ.pop("PLAYBOX_HUB_NO_MACHINE", None)
            else:
                os.environ["PLAYBOX_HUB_NO_MACHINE"] = old_nomach
            ap_proc.terminate()
            try:
                ap_proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                ap_proc.kill()
            up_srv.shutdown()
            up_srv.server_close()
