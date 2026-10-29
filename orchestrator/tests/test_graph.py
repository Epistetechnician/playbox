"""Three fake peers (stdlib A2A servers) return patches; the graph merges them."""
import base64
import json
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

from playbox_orchestrator import Peer, Subtask, delegate_and_merge


def git(d, *a):
    subprocess.run(["git", "-C", str(d), *a], check=True, capture_output=True)


@pytest.fixture
def repo(tmp_path):
    git(tmp_path, "init", "-q", "-b", "main")
    git(tmp_path, "config", "user.email", "t@e.com")
    git(tmp_path, "config", "user.name", "t")
    (tmp_path / "a.txt").write_text("one\ntwo\nthree\n")
    git(tmp_path, "add", "-A")
    git(tmp_path, "commit", "-q", "-m", "init")
    return tmp_path


def patch(path, old, new, body_old, body_new):
    return (f"diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,3 +1,3 @@\n"
            + "".join(f" {l}\n" if l not in (body_old,) else f"-{l}\n+{body_new}\n" for l in ["one", "two", "three"]))


def serve(reply):
    class H(BaseHTTPRequestHandler):
        def do_POST(self):
            n = int(self.headers["content-length"])
            req = json.loads(self.rfile.read(n))
            out = json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": {"task": reply}}).encode()
            self.send_response(200)
            self.send_header("content-length", str(len(out)))
            self.end_headers()
            self.wfile.write(out)

        def log_message(self, *a):
            pass

    s = HTTPServer(("127.0.0.1", 0), H)
    threading.Thread(target=s.serve_forever, daemon=True).start()
    return s, Peer(f"http://127.0.0.1:{s.server_port}")


def task(state, patch_text=""):
    arts = [{"name": "stdout", "parts": [{"text": "ok"}]}]
    if patch_text:
        arts.append({"name": "patch", "parts": [{"raw": base64.b64encode(patch_text.encode()).decode()}]})
    return {"id": "t", "status": {"state": state}, "artifacts": arts}


def test_three_peers_merge_and_a_conflict_is_reported(repo):
    new_file = "diff --git a/b.txt b/b.txt\nnew file mode 100644\n--- /dev/null\n+++ b/b.txt\n@@ -0,0 +1 @@\n+from peer 2\n"
    servers = [
        serve(task("TASK_STATE_COMPLETED", patch("a.txt", "", "", "one", "ONE"))),      # edits line 1
        serve(task("TASK_STATE_COMPLETED", new_file)),                                  # adds a file
        serve(task("TASK_STATE_COMPLETED", patch("a.txt", "", "", "one", "UNO"))),      # same line as peer 0: conflict
    ]
    subs = [Subtask(f"s{i}", p, "true") for i, (_, p) in enumerate(servers)]
    r = delegate_and_merge(str(repo), subs)
    assert r["applied"] == ["s0", "s1"]
    assert [c["subtask"] for c in r["conflicts"]] == ["s2"]
    assert r["files"]["a.txt"].splitlines()[0] == "ONE"
    assert r["files"]["b.txt"] == "from peer 2\n"
    assert (repo / "a.txt").read_text().startswith("one"), "the source repo is never touched"


def test_a_failed_or_unreachable_peer_does_not_sink_the_merge(repo):
    ok = serve(task("TASK_STATE_COMPLETED", patch("a.txt", "", "", "one", "ONE")))[1]
    failed = serve(task("TASK_STATE_FAILED"))[1]
    dead = Peer("http://127.0.0.1:1", timeout=2)
    r = delegate_and_merge(str(repo), [Subtask("good", ok, "x"), Subtask("bad", failed, "x"), Subtask("dead", dead, "x")])
    assert r["applied"] == ["good"]
    assert sorted(f["subtask"] for f in r["failed"]) == ["bad", "dead"]
