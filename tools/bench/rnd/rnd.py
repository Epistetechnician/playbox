#!/usr/bin/env python3
"""R&D room: Claude and Gemini (Antigravity `agy`) agents sit in one Playbox room, research improvements to Playbox itself, review each other's proposals,
implement the best ones in throwaway worktrees, review each other's diffs, and the harness opens DRAFT pull requests for what passes the gates.
Agents never write files and never push: they read a repository snapshot and reply with JSON; the harness applies edits, runs cargo, commits and opens the PR.
Room messages are signed by each agent's own member key; proposals and reviews are read back from the room feed, not from harness memory.
Env: RND_DIR, RND_PICK (proposals to implement), RND_PER_VENDOR (proposer agents per vendor), RND_PR=1 (really push + open draft PRs), RND_HUB=1, RND_MAX_USD."""
import concurrent.futures as cf, json, os, re, shutil, subprocess, sys, threading, time

H = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(H, "..", ".."))
import playbox_watch  # noqa: E402

RND = os.environ.get("RND_DIR") or os.path.expanduser("~/.playbox-rnd")
REPO, SNAP = os.path.join(RND, "repo"), os.path.join(RND, "snap")
RUN = os.path.join(RND, "run-" + time.strftime("%m%d-%H%M"))
PICK, PER = int(os.environ.get("RND_PICK", "6")), int(os.environ.get("RND_PER_VENDOR", "3"))
DO_PR, HUB = os.environ.get("RND_PR") == "1", os.environ.get("RND_HUB", "1") == "1"
MAX_USD = float(os.environ.get("RND_MAX_USD", "30"))
CLAUDE_MODEL, GEMINI_MODEL = os.environ.get("RND_CLAUDE", "claude-sonnet-5-5"), os.environ.get("RND_GEMINI", "gemini-4-argon-high")
TARGET = os.path.join(RND, "target")
HOSTING = os.path.expanduser("~/.playbox/hosting")
DENY = re.compile(r"^(\.github/|Cargo\.lock$|(.*/)?Cargo\.toml$|scripts/release|.*signing|crates/playbox-net/src/(grants|audit)\.rs$|crates/playbox-net/src/steer/|.*attest)")
ALLOW_NEW = re.compile(r"^(crates/[^/]+/(src|tests)/|docs/|tools/|CHANGELOG\.md$)")
LENSES = ["security and trust gaps (threat model, grants, secrets, images, admission, revocation, audit)",
          "reliability and operations (restarts, errors, hub and host lifecycle, stale state, clear failure messages)",
          "developer and operator experience (CLI flags, docs that disagree with code, confusing defaults, missing tests)"]
FACTS = ("Facts from recent experiments you may use: (1) `playbox up --allow-secret NAME` puts the secret in EVERY member's grant; there is no per-member narrowing at the command line. "
         "(2) A host's `playbox up` had no way to set the grant lifetime or the allowed container images until recently, a sign other host policy knobs may be missing from the CLI. "
         "(3) An expired grant used to give a cryptic datalog error; the message was made readable. (4) The hub lists local hosts only after you adopt them. "
         "(5) 400 controlled trials showed Playbox contained 7 attacks that a shared workspace did not.")
COST, LOCK, CARGO = [0.0], threading.Lock(), threading.Lock()


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


# ------------------------------------------------------------ agents (read-only: they can look at the repo, never change it)
def ask_claude(prompt, cwd, t=1200):
    r = subprocess.run(["claude", "-p", "--model", CLAUDE_MODEL, "--tools", "Read,Grep,Glob", "--no-session-persistence", "--disable-slash-commands", "--setting-sources", "",
                        "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}', "--output-format", "json"], input=prompt, capture_output=True, text=True, cwd=cwd, timeout=t,
                       env={**os.environ, "CLAUDE_CONFIG_DIR": os.path.expanduser("~/.claude-bench")})
    d = json.loads(r.stdout)
    with LOCK:
        COST[0] += float(d.get("total_cost_usd") or 0)
    return d.get("result") or ""


def ask_gemini(prompt, cwd, t=1200):
    r = subprocess.run(["agy", "-p", prompt, "--model", GEMINI_MODEL, "--output-format", "json", "--print-timeout", f"{t}s"], capture_output=True, text=True, cwd=cwd, timeout=t + 30)
    return json.loads(r.stdout.strip().splitlines()[-1]).get("response") or ""


def ask(vendor, prompt, cwd):
    last = ""
    for attempt in range(3):
        try:
            return (ask_claude if vendor == "Claude" else ask_gemini)(prompt, cwd)
        except Exception as e:  # noqa: BLE001
            last = str(e)[:200]
            time.sleep(10)
    raise RuntimeError(f"{vendor} failed: {last}")


def extract(text):
    dec, best = json.JSONDecoder(), None
    for m in re.finditer(r"\{", text):
        try:
            obj, _ = dec.raw_decode(text[m.start():])
        except ValueError:
            continue
        if isinstance(obj, dict) and (best is None or len(json.dumps(obj)) > len(json.dumps(best))):
            best = obj
    return best


def ask_json(vendor, prompt, cwd):
    for _ in range(2):
        out = ask(vendor, prompt, cwd)
        j = extract(out)
        if j:
            return j
        prompt += "\n\nYour last reply was not a JSON object. Reply with ONLY the JSON object."
    raise RuntimeError(f"{vendor} gave no JSON")


# ------------------------------------------------------------ the room
class Room:
    def __init__(self):
        self.d = os.path.join(RUN, "playbox-rnd")
        os.makedirs(os.path.join(self.d, "home"))
        self.hosting = os.path.join(self.d, "hosting")
        self.host = subprocess.Popen(["playbox", "up", "--home", os.path.join(self.d, "home"), "--local", "--console", "--state", os.path.join(self.d, "state"), "--max-members", "16",
                                      "--grant-ttl", "43200"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                                     env={**os.environ, "PLAYBOX_HOME": os.path.join(self.d, "home"), "PLAYBOX_HOSTING_DIR": self.hosting})
        self.links, self.mcp, self.ids = [], {}, {}
        threading.Thread(target=self._pump, daemon=True).start()
        for _ in range(60):
            if self.links:
                break
            time.sleep(0.5)
        for _ in range(16):
            self.host.stdin.write("link\n"); self.host.stdin.flush(); time.sleep(1.2)

    def _pump(self):
        for ln in self.host.stdout:
            self.links += [w for w in ln.split() if w.startswith("playbox://")]

    def join(self, who, harness, model):
        m = playbox_watch.Mcp(["playbox", "mcp"], os.path.join(self.d, f"m-{who}"))
        m.call("playbox_join", {"link": self.links[len(self.mcp)]})
        self.mcp[who] = m
        st = m.call("playbox_status")
        self.ids[who] = (st.get("joined") or {}).get("member_id") or st.get("guest_id")
        try:
            m.call("playbox_announce", {"name": who, "role": "R&D agent for Playbox", "harness": harness, "model": model, "machine": "dev-laptop"})
        except Exception as e:  # noqa: BLE001
            log("announce failed", who, str(e)[:80])
        time.sleep(1.5)

    def say(self, who, text):
        return self.mcp[who].call("playbox_say", {"text": text[:1900], "name": who})

    def feed(self):
        return self.mcp["orchestrator"].call("playbox_feed", {"limit": 1000})["items"]

    def tagged(self, tag):
        """Messages beginning with TAG, parsed as JSON, with the member id that signed them."""
        out = []
        for i in self.feed():
            if i["kind"] == "say" and i["text"].startswith(tag + " "):
                try:
                    out.append((i["from"], json.loads(i["text"][len(tag) + 1:])))
                except ValueError:
                    pass
        return out

    def register(self):
        if not HUB:
            return
        os.makedirs(HOSTING, exist_ok=True)
        for f in os.listdir(self.hosting):
            if f.endswith(".json"):
                shutil.copy(os.path.join(self.hosting, f), os.path.join(HOSTING, f))
                s = os.path.join(HOSTING, f[:-5] + ".sock")
                try:
                    os.remove(s)
                except OSError:
                    pass
                os.symlink(os.path.join(self.hosting, f[:-5] + ".sock"), s)

    def close(self):
        for m in self.mcp.values():
            m.close()
        try:
            self.host.stdin.write("quit\n"); self.host.stdin.flush(); self.host.wait(timeout=10)
        except Exception:  # noqa: BLE001
            self.host.kill()


# ------------------------------------------------------------ phases
def research(room, agents):
    def one(a):
        vendor, k = a["vendor"], a["k"]
        p = (f"You are {a['name']}, a {vendor} agent on a cross-vendor R&D team improving Playbox: Rust crates plus a Python hub, where agents from different owners collaborate in signed, auditable rooms. "
             f"The repository (read-only) is your working directory. Your lens: {LENSES[k]}.\n{FACTS}\n\n"
             "Read README.md, docs/threat-model.md, docs/benchmark.md, docs/tripwire.md, CHANGELOG.md and the code that your lens points at. Verify everything you claim by reading the files. "
             "Propose up to 3 concrete, SMALL improvements, each implementable in at most about 150 changed lines in existing crates, with a test. Prefer real defects, missing guard rails, and gaps that a user would hit, "
             "backed by evidence (path:line). Do NOT propose: dependency changes, CI or workflow changes, release or signing changes, or changes to grants.rs, audit.rs, attestation or steer/. "
             'Keep every field short (the whole proposal must be under 1500 characters). Reply with ONLY JSON: {"proposals":[{"title":"","problem":"","evidence":["path:line"],"change":"","files":["path"],"tests":"","risk":"low|medium","impact":""}]}')
        j = ask_json(vendor, p, SNAP)
        for i, pr in enumerate((j.get("proposals") or [])[:3]):
            pr["id"], pr["author"] = f"{a['name']}-{i + 1}", a["name"]
            room.say(a["name"], "PROPOSAL " + json.dumps(pr, separators=(",", ":")))
        log("research done", a["name"], len(j.get("proposals") or []))
    def safe(a):
        try:
            one(a)
        except Exception as e:  # noqa: BLE001
            log("research FAILED", a["name"], str(e)[:120])
    with cf.ThreadPoolExecutor(len(agents)) as ex:
        list(ex.map(safe, agents))
    props = {}
    for sender, pr in room.tagged("PROPOSAL"):
        if sender == room.ids.get(pr.get("author")):                 # the signing key must belong to the claimed author
            props[pr["id"]] = pr
    return props


def review(room, agents, props):
    def one(a):
        mine = [p for p in props.values() if p["author"] != a["name"]]
        p = (f"You are {a['name']}, a {a['vendor']} reviewer on the Playbox R&D team. The repository (read-only) is your working directory. Below are proposals from teammates. "
             "For each, check the evidence in the code, then rate impact (1-5: how much it helps real users or operators) and feasibility (1-5: can one engineer land it safely in about 150 lines with a test), and say pursue or skip. "
             "Be sceptical: lower the score if the evidence is wrong, the problem is already handled, or it is cosmetic.\n\n" + "\n".join(json.dumps(x, separators=(",", ":")) for x in mine)
             + '\n\nReply with ONLY JSON: {"reviews":[{"id":"","impact":3,"feasibility":3,"verdict":"pursue|skip","comment":"one sentence"}]}')
        j = ask_json(a["vendor"], p, SNAP)
        for r in (j.get("reviews") or []):
            room.say(a["name"], "REVIEW " + json.dumps(r, separators=(",", ":")))
        log("review done", a["name"])
    def safe(a):
        try:
            one(a)
        except Exception as e:  # noqa: BLE001
            log("review FAILED", a["name"], str(e)[:120])
    with cf.ThreadPoolExecutor(len(agents)) as ex:
        list(ex.map(safe, agents))
    rev = {}
    by_id = {v: k for k, v in room.ids.items()}
    for sender, r in room.tagged("REVIEW"):
        who = by_id.get(sender)
        if who and r.get("id") in props and props[r["id"]]["author"] != who:
            rev.setdefault(r["id"], []).append({**r, "by": who})
    return rev


def select(props, rev, need=2):
    ranked = []
    for pid, rs in rev.items():
        p = props[pid]
        if len(rs) < need or any(DENY.match(f) for f in p.get("files", [])) or p.get("risk") == "high":
            continue
        imp, fea = sum(int(r.get("impact", 1)) for r in rs) / len(rs), sum(int(r.get("feasibility", 1)) for r in rs) / len(rs)
        if sum(1 for r in rs if r.get("verdict") == "skip") * 2 > len(rs):
            continue
        ranked.append((imp * fea, pid))
    ranked.sort(reverse=True)
    chosen, used = [], set()
    for sc, pid in ranked:
        key = (props[pid].get("files") or ["?"])[0]
        if key in used:
            continue
        used.add(key); chosen.append((pid, sc))
        if len(chosen) >= PICK:
            break
    return chosen


def sh(cmd, cwd, t=1800, env=None):
    r = subprocess.run(cmd, shell=True, cwd=cwd, capture_output=True, text=True, timeout=t, env={**os.environ, "CARGO_TARGET_DIR": TARGET, **(env or {})})
    return r.returncode, (r.stdout + r.stderr)[-3500:]


def run(args, cwd, t=1800):
    r = subprocess.run(args, cwd=cwd, capture_output=True, text=True, timeout=t, env={**os.environ, "CARGO_TARGET_DIR": TARGET})
    return r.returncode, (r.stdout + r.stderr)[-3500:]


def apply_edits(wt, j):
    for e in j.get("edits") or []:
        p = os.path.join(wt, e["path"])
        if DENY.match(e["path"]) or not os.path.isfile(p):
            return f"edit refused or file missing: {e.get('path')}"
        s = open(p).read()
        if s.count(e["find"]) != 1:
            return f"find text must match exactly once in {e['path']} (matched {s.count(e['find'])}): {e['find'][:80]!r}"
        open(p, "w").write(s.replace(e["find"], e["replace"], 1))
    for f in j.get("new_files") or []:
        if DENY.match(f["path"]) or not ALLOW_NEW.match(f["path"]):
            return f"new file not allowed: {f['path']}"
        os.makedirs(os.path.dirname(os.path.join(wt, f["path"])), exist_ok=True)
        open(os.path.join(wt, f["path"]), "w").write(f["content"])
    return ""


def gate(wt, j):
    rc, files = sh("git diff --name-only; git ls-files --others --exclude-standard", wt)
    files = [x for x in files.split() if x]
    if not files:
        return False, "no change", files
    bad = [f for f in files if DENY.match(f)]
    if bad:
        return False, f"touched forbidden paths: {bad}", files
    rc, stat = sh("git add -N . && git diff --shortstat", wt)
    m = re.findall(r"(\d+) (?:insertion|deletion)", stat)
    if sum(map(int, m)) > 400:
        return False, f"diff too large: {stat.strip()}", files
    rs = [f for f in files if f.endswith(".rs")]
    msgs = []
    if rs:
        crates = sorted({f.split("/")[1] for f in rs if f.startswith("crates/")})
        if not crates:
            return False, "rust files outside crates/", files
        with CARGO:
            run(["rustfmt", "--edition", "2021", *rs], wt)
            pk = " ".join(f"-p {c}" for c in crates)
            for name, cmd in (("clippy", f"cargo clippy {pk} --all-targets -- -D warnings"), ("test", f"cargo test {pk}")):
                rc, out = sh(cmd, wt, 3000)
                if rc:
                    return False, f"{name} failed:\n{out}", files
                msgs.append(f"{name} ok ({','.join(crates)})")
    for f in files:
        if f.endswith(".py"):
            rc, out = run(["python3", "-m", "py_compile", f], wt)
            if rc:
                return False, f"py_compile failed: {out}", files
    return True, "; ".join(msgs) or "docs/python only", files


def implement(room, pr, vendor, reviewer, score):
    pid, slug = pr["id"], re.sub(r"[^a-z0-9]+", "-", pr["title"].lower()).strip("-")[:40]
    wt = os.path.join(RUN, "wt", pid)
    sh(f"git worktree add -q --detach {wt} origin/main", REPO)
    res = {"id": pid, "title": pr["title"], "author": pr["author"], "implementer": vendor, "reviewer": reviewer, "score": round(score, 2), "status": "failed", "notes": []}
    feedback, rounds = "", 0
    try:
        for attempt in range(4):
            sh("git checkout -q -- . && git clean -fdq", wt)
            p = (f"You are the implementer ({vendor}) on the Playbox R&D team. The repository (read-only for you) is your working directory; you cannot write, so reply with edits as JSON and the harness will apply them.\n"
                 f"PROPOSAL: {json.dumps(pr)}\n\nRules: smallest change that does the job; add a test in the same crate; follow the repo's style; no dependency changes; do not touch grants.rs, audit.rs, attestation, steer/, workflows, Cargo files. "
                 "Add one line under the Unreleased 'Added' or 'Fixed' heading of CHANGELOG.md. For every edit, `find` must be an exact, unique snippet of the current file.\n" + feedback +
                 '\nReply with ONLY JSON: {"edits":[{"path":"","find":"","replace":""}],"new_files":[{"path":"","content":""}],"summary":"2-3 sentences for the PR description"}')
            j = ask_json(vendor, p, wt)
            err = apply_edits(wt, j)
            if err:
                feedback = f"\nYour previous edits failed: {err}\n"; res["notes"].append(err[:200]); continue
            ok, msg, files = gate(wt, j)
            room.say("orchestrator", f"GATE {pid} attempt {attempt + 1}: {'PASS' if ok else 'FAIL'} {msg[:600]}")
            if not ok:
                feedback = f"\nYour previous edits did not pass the checks. Output:\n{msg}\nFix and send the complete edit set again.\n"; res["notes"].append(msg[:200]); continue
            diff = sh("git add -N . && git diff", wt)[1] if False else subprocess.run("git add -N . && git diff", shell=True, cwd=wt, capture_output=True, text=True).stdout
            q = (f"You are {reviewer}, reviewing a teammate's patch to Playbox. The repository with the patch applied is your working directory (read-only). Proposal: {json.dumps(pr)}\n\nPATCH:\n{diff[:14000]}\n\n"
                 f"Checks: {msg}\nIs it correct, minimal, safe (no weakening of any security check), and does the test really exercise the change? "
                 'Reply with ONLY JSON: {"verdict":"approve|changes","issues":["..."]}')
            v = ask_json("Claude" if reviewer.startswith("Claude") else "Gemini", q, wt)
            room.say(reviewer, f"VERDICT {pid} {json.dumps(v, separators=(',', ':'))}"[:1900])
            if v.get("verdict") == "approve":
                res.update(status="approved", summary=j.get("summary", ""), gate=msg, files=files, diff_lines=len(diff.splitlines()), verdict=v)
                res["patch"] = diff
                break
            rounds += 1
            feedback = f"\nA reviewer asked for changes: {v.get('issues')}. Send the complete revised edit set.\n"; res["notes"].append("review: " + str(v.get("issues"))[:200])
            if rounds >= 2:
                res["status"] = "rejected-by-review"; break
        if res["status"] == "approved":
            branch = f"rnd/{slug}-{pid.lower()}"
            res["branch"] = branch
            run(["git", "checkout", "-q", "-b", branch], wt); run(["git", "add", "-A"], wt)
            run(["git", "commit", "-q", "-m", "R&D room: " + pr["title"], "-m", pr["problem"][:400] + "\n\nProposed and implemented by a cross-vendor agent team in a Playbox room; reviewed by the other vendor."], wt)
            res["commit"] = sh("git rev-parse HEAD", wt)[1].strip()
    except Exception as e:  # noqa: BLE001
        res["notes"].append(f"error: {str(e)[:200]}")
    return res, wt


def open_pr(room, res, wt, props, rev):
    resolve = os.environ.get("RND_RESOLVE", "")
    cfg = f"-c http.curloptResolve={resolve}" if resolve else ""
    rc, out = sh(f"git {cfg} push -q origin {res['branch']}", wt, 300)
    if rc:
        res["notes"].append("push failed: " + out[-200:]); return
    pr = props[res["id"]]
    revs = "\n".join(f"- {r['by']}: impact {r.get('impact')}, feasibility {r.get('feasibility')}, {r.get('verdict')}: {r.get('comment', '')}" for r in rev.get(res["id"], []))
    body = (f"{res['summary']}\n\n## Why\n{pr['problem']}\n\nEvidence: {', '.join(pr.get('evidence', []))}\n\n## How this was made\n"
            f"Proposed by **{pr['author']}**, implemented by **{res['implementer']}**, diff reviewed by **{res['reviewer']}**, in a Playbox room (messages signed by each agent's own key; hub room `playbox-rnd`). "
            f"Agents had read-only access to a snapshot; the harness applied edits, ran the gates and pushed this branch.\n\n## Proposal reviews\n{revs}\n\n## Checks run\n{res['gate']}\n\n"
            "This is a draft. A human should read the diff before anything is merged.")
    bf = os.path.join(RUN, f"body-{res['id']}.md")
    open(bf, "w").write(body)
    rc, out = run(["gh", "pr", "create", "--draft", "--base", "main", "--head", res["branch"], "--title", "[R&D room] " + res["title"], "--body-file", bf], wt, 120)
    res["pr"] = out.strip().splitlines()[-1] if not rc and out.strip() else ""
    if rc:
        res["notes"].append("pr failed: " + out[-200:])
    else:
        res["status"] = "pr-opened"
        room.say("orchestrator", f"PR {res['id']}: {res['pr']}")


def main():
    os.makedirs(RUN)
    log("run dir", RUN)
    if not os.path.isdir(SNAP):
        sh("git worktree add -q --detach ../snap origin/main", REPO)
    room = Room()
    try:
        agents = [{"name": f"Claude-{k + 1}", "vendor": "Claude", "k": k} for k in range(PER)] + [{"name": f"Gemini-{k + 1}", "vendor": "Gemini", "k": k} for k in range(PER)]
        room.join("orchestrator", "harness", "none")
        for a in agents:
            room.join(a["name"], "Claude Code" if a["vendor"] == "Claude" else "Antigravity", CLAUDE_MODEL if a["vendor"] == "Claude" else GEMINI_MODEL)
        room.register()
        room.say("orchestrator", f"R&D run started on {sh('git rev-parse --short origin/main', REPO)[1].strip()}: {PER} Claude + {PER} Gemini agents. Phases: research, cross-review, select, implement, diff review, draft PR.")
        resume = os.environ.get("RND_RESUME")
        old = json.load(open(os.path.join(resume, "proposals.json"))) if resume else {}
        if old.get("proposals"):                          # reuse an earlier run's proposals (and reviews, if it got that far)
            props = old["proposals"]
            for pr in props.values():                     # re-post them so this room's feed, not the harness, is the record
                room.say(pr["author"], "PROPOSAL " + json.dumps(pr, separators=(",", ":")))
        else:
            props = research(room, agents)
        log("proposals", len(props))
        json.dump({"proposals": props}, open(os.path.join(RUN, "proposals.json"), "w"), indent=1)
        rev = old.get("reviews") or review(room, agents, props)
        chosen = select(props, rev, min(2, len(agents) - 1))
        if os.environ.get("RND_ONLY"):
            chosen = [c for c in chosen if c[0] in os.environ["RND_ONLY"].split(",")]
        json.dump({"proposals": props, "reviews": rev, "chosen": chosen}, open(os.path.join(RUN, "proposals.json"), "w"), indent=1)
        room.say("orchestrator", "SELECTED " + json.dumps([[pid, round(s, 1)] for pid, s in chosen]))
        results = []
        with cf.ThreadPoolExecutor(3) as ex:
            futs = []
            for i, (pid, sc) in enumerate(chosen):
                pr = props[pid]
                vendor = "Gemini" if pr["author"].startswith("Claude") else "Claude"        # the other vendor implements, the proposer's vendor reviews
                reviewer = f"{'Claude' if vendor == 'Gemini' else 'Gemini'}-{(i % PER) + 1}"
                futs.append(ex.submit(implement, room, pr, vendor, reviewer, sc))
            for f in futs:
                res, wt = f.result()
                if res["status"] == "approved" and DO_PR:
                    open_pr(room, res, wt, props, rev)
                results.append(res)
                log("result", res["id"], res["status"], res.get("pr", ""))
        json.dump(results, open(os.path.join(RUN, "results.json"), "w"), indent=1)
        room.say("orchestrator", f"DONE. {sum(1 for r in results if r['status'] in ('approved', 'pr-opened'))}/{len(results)} proposals passed the gates and review. Claude cost ${COST[0]:.2f}.")
    finally:
        time.sleep(3)
        room.close()


if __name__ == "__main__":
    main()
