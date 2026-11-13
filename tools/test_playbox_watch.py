#!/usr/bin/env python3
"""End-to-end check of playbox_watch.py against a real local room (needs `playbox` on PATH). Run: python3 tools/test_playbox_watch.py"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import playbox_watch as pw  # noqa: E402

fails = []


def check(name, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + name + ("" if cond else f"  {detail}"))
    if not cond:
        fails.append(name)


def wait(f, secs=20):
    end = time.time() + secs
    while time.time() < end:
        v = f()
        if v:
            return v
        time.sleep(0.2)
    return None


def main():
    if not shutil.which("playbox"):
        print("SKIP (playbox not on PATH)")
        return 0
    d = tempfile.mkdtemp(prefix="pbwatch-")
    repo = os.path.join(d, "repo")
    os.makedirs(repo)
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(["git", "-c", "user.email=a@b", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "i"], cwd=repo, check=True)
    cmd = ["playbox", "mcp"]
    host = pw.Mcp(cmd, d + "/host")
    guests = []
    try:
        host.call("playbox_create", {"repo": repo, "local": True})
        stub = os.path.join(d, "stub.py")
        open(stub, "w").write(
            "import json,sys\np=json.load(sys.stdin)\nt=p['new'][-1]\n"
            "print('NOOP' if 'ignore' in t else 'ack: '+t.split(': ',1)[1])\n")

        def link():
            f = os.path.join(d, f"l{len(guests)}.txt")
            open(f, "w").write(host.call("playbox_create", {"repo": repo, "local": True})["link"])
            return f

        poster = pw.Mcp(cmd, d + "/poster")
        guests.append(poster)
        poster.call("playbox_join", {"link": open(link()).read()})
        lf = link()
        log = os.path.join(d, "w.log")
        res = {}
        t = threading.Thread(target=lambda: res.update(rc=pw.main(["--link-file", lf, "--home", d + "/w", "--name", "Bot", "--responder", f"{sys.executable} {stub}",
                                                                   "--interval", "0.3", "--cooldown", "0.6", "--max-streak", "2", "--duration", "40", "--log", log])))
        t.start()
        wait(lambda: os.path.exists(log) and "joined" in open(log).read())
        say = lambda who, text: who.call("playbox_say", {"text": text})
        feed = lambda: poster.call("playbox_feed", {"since": 0})["items"]
        texts = lambda: [i["text"] for i in feed() if i["kind"] == "say"]

        say(poster, "hello bot")
        check("it answers a new message", wait(lambda: "Bot: ack: hello bot" in texts()), str(texts()))
        n = len(texts())
        say(poster, "please ignore this one")
        time.sleep(2.5)
        check("NOOP from the responder posts nothing", len(texts()) == n + 1 and "Bot: ack" not in texts()[-1], str(texts()))
        check("it never answers its own message", not any("ack: Bot:" in x for x in texts()))
        say(poster, "second question")
        check("second reply", wait(lambda: "Bot: ack: second question" in texts()))
        say(poster, "third question")
        t.join(25)
        check("streak limit stops it after 2 replies with no host message", not t.is_alive() and res.get("rc") == 0 and "streak_limit" in open(log).read(), open(log).read()[-300:])
        check("the third message was not answered", "Bot: ack: third question" not in texts())
        check("the join link is not in the log", "playbox://" not in open(log).read())
    finally:
        for g in guests:
            g.close()
        host.close()
        shutil.rmtree(d, ignore_errors=True)
    print(f"\n{len(fails)} failed" if fails else "\nall passed")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
