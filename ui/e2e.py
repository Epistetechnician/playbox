#!/usr/bin/env python3
"""End-to-end check of the Playbox UI: two UI servers (host, guest), driven over HTTP like the browser does.

    python3 ui/e2e.py        # needs playbox-mcp and git; uses Docker for the task step if it answers
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
fails = []


def check(name, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + name + ("" if cond else f"  {detail}"))
    if not cond:
        fails.append(name)


class UI:
    def __init__(self, port, home):
        self.token = "t" + str(port)
        self.base = f"http://127.0.0.1:{port}"
        self.p = subprocess.Popen([sys.executable, os.path.join(HERE, "playbox_ui.py"), "--port", str(port),
                                   "--home", home, "--token", self.token], stdout=subprocess.PIPE, text=True)
        self.p.stdout.readline()

    def req(self, path, body=None, token=True):
        r = urllib.request.Request(self.base + path, data=json.dumps(body).encode() if body is not None else None,
                                   headers={"Content-Type": "application/json", **({"X-Playbox-Token": self.token} if token else {})})
        try:
            with urllib.request.urlopen(r, timeout=120) as f:
                return f.status, f.read().decode()
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode()

    def tool(self, _tool, **args):
        code, body = self.req("/api/tool", {"name": _tool, "args": args})
        j = json.loads(body)
        return j.get("error"), j.get("data", j)

    def stop(self):
        self.p.terminate()


def docker_ok():
    try:
        return subprocess.run(["docker", "info"], capture_output=True, timeout=8).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def load_recipes():
    """The one-tap recipes defined in the page, evaluated with node. None if node is missing."""
    if not shutil.which("node"):
        return None
    page = open(os.path.join(HERE, "static", "index.html")).read()
    block = page[page.index("const NO_GIT"):page.index("const QUICK")]
    r = subprocess.run(["node", "-e", block + "\nconsole.log(JSON.stringify(RECIPES))"], capture_output=True, text=True, timeout=20)
    return json.loads(r.stdout) if r.returncode == 0 else None


def main():
    work = tempfile.mkdtemp(prefix="pbui-")
    repo = os.path.join(work, "repo")
    os.makedirs(repo)
    g = lambda *a: subprocess.run(["git", "-c", "user.email=a@b", "-c", "user.name=t", *a], cwd=repo, check=True, capture_output=True)
    g("init", "-q", "-b", "main")
    open(os.path.join(repo, "hello.txt"), "w").write("hi\n")
    open(os.path.join(repo, "notes.py"), "w").write("# TODO: tidy this up\nprint('x')\n")
    open(os.path.join(repo, "leak.txt"), "w").write("key = AKIA" + "ABCDEFGHIJKLMNOP\n")   # split so this file is not itself a hit
    open(os.path.join(repo, "good.sh"), "w").write("echo fine\n")
    open(os.path.join(repo, "bad.sh"), "w").write("if then fi done\n")
    open(os.path.join(repo, "package.json"), "w").write("{}\n")
    g("add", "."), g("commit", "-qm", "init")
    host, guest = UI(17731, work + "/host"), UI(17732, work + "/guest")
    try:
        # access control
        check("page needs the token", host.req("/", token=False)[0] == 401)
        check("api needs the token", host.req("/api/state", token=False)[0] == 401)
        check("bad token refused on entry", host.req("/?t=nope", token=False)[0] in (403, 303) and host.req("/?t=nope", token=False)[0] == 403)
        code, page = host.req("/")
        check("page served with token", code == 200 and "Playbox" in page and "viewport-fit=cover" in page)
        check("manifest served without token", host.req("/manifest.webmanifest", token=False)[0] == 200)
        check("unknown tool refused", host.req("/api/tool", {"name": "rm -rf", "args": {}})[0] == 400)
        try:
            urllib.request.urlopen(urllib.request.Request(host.base + "/api/tool", data=b"x", headers={"X-Playbox-Token": host.token, "Content-Type": "text/plain"}), timeout=5)
            nonjson = 200
        except urllib.error.HTTPError as e:
            nonjson = e.code
        check("non-json refused", nonjson == 415, nonjson)

        use_docker = docker_ok()
        err, d = host.tool("playbox_create", repo=repo, local=True, docker=use_docker, uses=2)
        check("host creates a room", not err and str(d.get("link", "")).startswith("playbox://"), d)
        link = d["link"]
        err, d2 = host.tool("playbox_create", repo=repo, local=True, docker=use_docker, uses=2)
        check("host makes a fresh invite", not err and d2["link"] != link, d2)

        err, j = guest.tool("playbox_join", link=link)
        check("guest joins", not err and j.get("joined") is True, j)
        member = j["member_id"]

        code, st = host.req("/api/state")
        st = json.loads(st)
        check("host sees the member", member in st["status"]["hosting"]["members"], st)
        code, st = guest.req("/api/state")
        check("guest state is joined", json.loads(st)["status"]["joined"]["member_id"] == member)

        err, _ = guest.tool("playbox_say", text="hello from the guest UI", name="guest")
        check("guest says something", not err)
        err, feed = host.tool("playbox_feed", since=0)
        texts = [i["text"] for i in feed["items"]] if not err else []
        check("host sees the message in the feed", "hello from the guest UI" in texts, feed)
        err, _ = host.tool("playbox_say", text="welcome", name="host")
        err, feed = guest.tool("playbox_feed", since=0)
        check("guest sees the host reply", any(i["text"] == "welcome" for i in feed["items"]), feed)

        err, pull = guest.tool("playbox_pull", ref="main", checkout=True)
        check("guest pulls main", not err and pull.get("head"), pull)
        if use_docker:
            err, run = guest.tool("playbox_delegate", base="main", command="cat hello.txt && echo more > new.txt", apply=True)
            check("task runs in the sandbox", not err and run["exit_code"] == 0 and run["stdout"] == "hi\n" and run["patch_applied"], run)
            check("patch came back", "new.txt" in run.get("patch", ""))
            recipes = load_recipes()
            check("recipes load from the page", bool(recipes) and len(recipes) >= 6, recipes)
            for rc in recipes or []:
                err, out = guest.tool("playbox_delegate", base="main", command=rc["cmd"], apply=False, timeout_secs=120)
                check(f"recipe runs: {rc['t']}", not err and out.get("exit_code") == 0 and bool(out.get("stdout", "").strip()), out)
                want = {"Find TODOs": "notes.py", "Look for leaked secrets": "leak.txt", "Largest files": "notes.py",
                        "File types": "sh", "Check shell scripts": "FAIL ./bad.sh",
                        "Run the tests": "npm is not in the host's task image"}.get(rc["t"])
                if want:
                    check(f"recipe finds the planted content: {rc['t']}", want in out.get("stdout", ""), out.get("stdout"))
                check(f"recipe applies nothing: {rc['t']}", not out.get("patch_applied") and not out.get("patch"), out)
        else:
            print("SKIP task step (Docker not answering)")

        err, log = guest.tool("playbox_status")
        err, v = guest.tool("playbox_verify", path=log["history_log"])
        check("guest history verifies", not err and v["valid"] is True and v["entries"] > 3, v)

        # tamper: one flipped byte must fail
        bad = os.path.join(work, "bad.log")
        raw = bytearray(open(log["history_log"], "rb").read())
        raw[len(raw) // 2] ^= 1
        open(bad, "wb").write(raw)
        err, v2 = guest.tool("playbox_verify", path=bad)
        check("a flipped byte is caught", err or v2.get("valid") is False, v2)

        # witness holds shown in the cockpit (the Hermes plugin appends these; here we write them the same way)
        code, w0 = guest.req("/api/witness")
        check("no holds at first", code == 200 and json.loads(w0)["active"] == [], w0)
        check("witness feed needs the token", guest.req("/api/witness", token=False)[0] == 401)
        hp = os.path.join(work, "guest", "witness")
        os.makedirs(hp, exist_ok=True)
        with open(os.path.join(hp, "holds.jsonl"), "a") as f:
            f.write(json.dumps({"ts": 1, "kind": "held", "session": "s1", "tool": "terminal", "origin": "room:feed", "extra": "SECRET-TEXT"}) + "\n")
            f.write("not json\n")
        w1 = json.loads(guest.req("/api/witness")[1])
        check("a hold shows as active", len(w1["active"]) == 1 and w1["active"][0]["tool"] == "terminal", w1)
        check("only known fields are served", "SECRET-TEXT" not in json.dumps(w1))
        with open(os.path.join(hp, "holds.jsonl"), "a") as f:
            f.write(json.dumps({"ts": 2, "kind": "released", "session": "s1", "origin": "room:feed"}) + "\n")
        w2 = json.loads(guest.req("/api/witness")[1])
        check("the next user message releases it", w2["active"] == [] and len(w2["events"]) == 2, w2)

        # tamper demo flips a byte in a copy and never touches the real log
        before = open(log["history_log"], "rb").read()
        code, tb = guest.req("/api/tamper", {"path": log["history_log"]})
        t = json.loads(tb)
        res = t["result"] if isinstance(t["result"], dict) else {}
        check("tamper demo catches the changed digit", code == 200 and res.get("valid") is False and t["entries"] > 3, t)
        check("tamper demo names the broken entry", "entry" in str(res.get("error")), t)
        # the verify call itself is logged, so the real log may grow, but what was there must be unchanged
        check("real log untouched by the tamper demo", open(log["history_log"], "rb").read().startswith(before))
        check("tamper copy is cleaned up", not os.path.exists(os.path.join(work, "guest", "ui-demo", "tampered.log")))
        check("tamper refuses logs outside the home", guest.req("/api/tamper", {"path": bad})[0] == 400)
        check("tamper needs the token", guest.req("/api/tamper", {"path": log["history_log"]}, token=False)[0] == 401)

        err, _ = guest.tool("playbox_leave")
        check("guest leaves", not err)
        err, j = guest.tool("playbox_join", link=d2["link"])
        check("guest rejoins with a fresh link", not err and j.get("joined"), j)
        new_member = j["member_id"]

        err, r = host.tool("playbox_revoke", member=new_member)
        check("host revokes", not err and r.get("revoked") is True, r)
        err, r = guest.tool("playbox_ping", text="x")
        check("revoked guest is refused", err and "revoked" in str(r), r)
    finally:
        host.stop(), guest.stop()
        shutil.rmtree(work, ignore_errors=True)
    print(f"\n{'ALL PASSED' if not fails else 'FAILED: ' + ', '.join(fails)}")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
