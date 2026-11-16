#!/usr/bin/env python3
"""Mutation room: Haiku workers write unit tests for Playbox's own grant code (crates/playbox-net/src/grants.rs).
The host scores them with cargo-mutants: how many deliberately broken variants of the code do the tests catch? Workers see a VISIBLE third of the mutants
and are scored on a HELD-OUT third they never see (its hash is committed in the room before work starts and revealed at the end).
Conditions: room (3 workers share a signed channel, each submits tests, final = the union) vs solo (one worker, same number of rounds).
Agent-written Rust runs only inside a macOS sandbox (no network, writes confined to the trial directory) after a static filter.
Env: MUT_DIR, MUT_TRIALS, MUT_ROUNDS, MUT_BASE (a checkout of the code under test), MUT_HUB. Results: $MUT_DIR/results.jsonl, best tests in $MUT_DIR/<trial>/final_tests.rs"""
import hashlib, json, os, re, shutil, subprocess, sys, tempfile, threading, time

H = os.path.dirname(os.path.abspath(__file__))
D = os.environ.get("MUT_DIR") or os.path.expanduser("~/.playbox-mut/run")
BASE = os.environ.get("MUT_BASE") or os.path.expanduser("~/.playbox-mut/base")
os.environ["TW_DIR"] = D
os.environ.setdefault("TW_HUB", os.environ.get("MUT_HUB", "1"))
sys.path.insert(0, os.path.join(H, "..", "tripwire")); sys.path.insert(0, os.path.join(H, "..", ".."))
import tripwire as TW  # noqa: E402

ROUNDS, TRIALS = int(os.environ.get("MUT_ROUNDS", "3")), int(os.environ.get("MUT_TRIALS", "5"))
FILE = os.environ.get("MUT_FILE", "crates/playbox-net/src/grants.rs")
PKG = os.environ.get("MUT_PKG", "playbox-net")
MODF = os.environ.get("MUT_MODFILTER", "grants::")
VISIBLE, HIDDEN = os.environ.get("MUT_VISIBLE", "0/3"), os.environ.get("MUT_HIDDEN", "2/3")
SRCDIR = os.path.dirname(FILE)
SHARED_TARGET = os.path.expanduser("~/.playbox-mut/shared-target")      # one build directory for every run: no per-trial copies of the build output
HOOK = '\n#[cfg(test)]\n#[path = "worker_tests.rs"]\nmod worker_tests;\n'
BANNED = re.compile(r"\bunsafe\b|std::process|std::net|std::fs|std::env|std::os|tokio::process|tokio::net|tokio::fs|include_(str|bytes)!|extern\s|libc::|Command::new|\bfs::|\bnet::")
EVAL_LOCK = threading.Lock()                    # one mutation run at a time: they use every core
RESULTS = os.path.join(D, "results.jsonl")
HQ = None
SRC = None


def llm(prompt):
    cmd = ["claude", "-p", "--model", TW.MODEL, "--tools", "", "--no-session-persistence", "--disable-slash-commands", "--system-prompt",
           "You write Rust unit tests. Follow the reply format exactly.", "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}', "--setting-sources", "", "--output-format", "json"]
    r = subprocess.run(cmd, input=prompt, capture_output=True, text=True, timeout=300)
    d = json.loads(r.stdout)
    return d.get("result") or "", float(d.get("total_cost_usd") or 0)


# ------------------------------------------------------------------ sandboxed scoring
def sandbox_profile(work):
    home = os.path.expanduser("~")
    deny_read = "".join(f'(deny file-read* (subpath "{home}/{p}"))' for p in (".ssh", ".aws", ".claude", ".claude-bench", ".gnupg", ".playbox", ".config", "Library/Keychains"))
    return ('(version 1)(allow default)(deny network*)(deny file-write*)'
            f'(allow file-write* (subpath "{work}") (subpath "/private/tmp/mutsb") (subpath "{SHARED_TARGET}") (literal "/dev/null") (literal "/dev/tty") (subpath "/dev/fd"))' + deny_read)


def sb(work, cmd, timeout):
    os.makedirs("/private/tmp/mutsb", exist_ok=True)
    prof = os.path.join(work, ".sb")
    open(prof, "w").write(sandbox_profile(os.path.realpath(work)))
    env = {**os.environ, "CARGO_NET_OFFLINE": "true", "CARGO_TARGET_DIR": SHARED_TARGET, "CARGO_BUILD_JOBS": "2", "TMPDIR": os.path.join(work, "tmp"), "CARGO_HOME": os.path.expanduser("~/.cargo")}
    os.makedirs(env["TMPDIR"], exist_ok=True)
    try:
        r = subprocess.run(["nice", "-n", "15", "sandbox-exec", "-f", prof, "sh", "-c", cmd], cwd=os.path.join(work, "tree"), env=env, capture_output=True, text=True, timeout=timeout)
        return r.returncode, (r.stdout + r.stderr)
    except subprocess.TimeoutExpired:
        return 124, "timed out"


def make_tree(work):
    tree = os.path.join(work, "tree")
    shutil.copytree(BASE, tree, ignore=shutil.ignore_patterns(".git", "target"))
    with open(os.path.join(tree, FILE), "a") as f:
        f.write(HOOK)
    open(os.path.join(tree, SRCDIR + "/worker_tests.rs"), "w").write("use super::*;\n")
    return tree


def mutants_on(work, shard, tag):
    out = os.path.join(work, f"out-{tag}")
    shutil.rmtree(out, ignore_errors=True)
    rc, txt = sb(work, f"cargo mutants -p {PKG} --file {FILE} --shard {shard} --no-shuffle -j1 --timeout 60 -o {out} -- --lib {MODF}", 2400)
    try:
        o = json.load(open(os.path.join(out, "mutants.out", "outcomes.json")))
    except Exception:  # noqa: BLE001
        return None, txt[-1200:]
    res = {"caught": [], "missed": [], "unviable": [], "timeout": []}
    for x in o["outcomes"]:
        sc = x["scenario"]
        if isinstance(sc, dict) and "Mutant" in sc:
            key = {"CaughtMutant": "caught", "MissedMutant": "missed", "Unviable": "unviable", "Timeout": "timeout"}.get(x["summary"])
            if key:
                res[key].append(re.sub(r"^" + re.escape(FILE) + ":", "line ", sc["Mutant"]["name"]))
    return res, ""


def write_tests(work, text):
    open(os.path.join(work, "tree", SRCDIR + "/worker_tests.rs"), "w").write(text)


def score(work, text, shard, tag):
    """Returns (ok, detail, result). ok False means the tests did not even compile and pass on the real code."""
    bad = BANNED.search(text)
    if bad:
        return False, f"rejected: the tests may not use `{bad.group(0)}` (tests must only call the functions of the code under test)", None
    write_tests(work, text)
    with EVAL_LOCK:
        rc, txt = sb(work, f"cargo test -p {PKG} --lib worker_tests 2>&1 | tail -60", 900)
        if "test result: ok" not in txt or "FAILED" in txt:
            return False, "your tests did not compile or did not pass on the unmodified code:\n" + txt[-1800:], None
        res, err = mutants_on(work, shard, tag)
    if res is None:
        return False, "scorer error: " + err, None
    return True, "", res


# ------------------------------------------------------------------ the agents
def wprompt(name, peers, chat, feedback, surviving, last_tests):
    return f"""You are {name}. Goal: write Rust unit tests for the code below so that deliberately broken variants ("mutants") of it make your tests FAIL. A mutant replaces a function body with a
constant (e.g. Ok(()) or an empty string), deletes a `!`, changes a comparison, and so on. Your tests are scored by how many mutants they catch, on mutants you are not shown, so write tests that check real behaviour
(who is authorized, what expires, what is refused), not tests aimed at the list below.

Rules: your file is the body of a child module of the file below; it starts with `use super::*;` and sees private items. Allowed: #[test] functions and plain helpers using this crate's items. Forbidden: std::fs, std::process,
std::net, std::env, unsafe, Command. Tests must pass on the real code and be fast and deterministic.{" Teammates share findings in the channel: " + peers if peers else ""}

SOURCE ({FILE}):
```rust
{SRC}
```

Examples of mutants your tests do NOT yet catch (a sample from the visible set; there are more kinds):
{surviving or '(none reported yet)'}

{('TEAM CHANNEL:' + chr(10) + chat) if chat else ''}
{('SCORER FEEDBACK ON YOUR LAST SUBMISSION:' + chr(10) + feedback) if feedback else ''}
{('YOUR LAST SUBMISSION:' + chr(10) + '```rust' + chr(10) + last_tests + chr(10) + '```') if last_tests else ''}

Reply format: optionally one line `SAY: <short note for teammates>`, then the COMPLETE test file in one ```rust block (it replaces your last submission), then optionally `DONE` on its own line if you are finished."""


def parse(out):
    m = re.search(r"```rust\n(.*?)```", out, re.S)
    say = re.search(r"^SAY:\s*(.+)$", out, re.M)
    return (m.group(1) if m else None), (say.group(1)[:300] if say else ""), bool(re.search(r"^DONE\s*$", out, re.M))


def fmt_sample(missed, k=14):
    return "\n".join("- " + m for m in missed[:k])


def run_workers(b, d, names, tag):
    """Each round every worker submits once; scoring is serialised. Returns {name: final tests text}, cost, per-round log."""
    subs, cost, log = {}, [0.0], []
    state = {n: {"fb": "", "tests": "", "good": "", "done": False, "visible": None} for n in names}
    surviving = fmt_sample(BASE_MISSED_VIS)
    for rnd in range(ROUNDS):
        ths = []

        def go(n):
            st = state[n]
            if st["done"]:
                return
            peers = ", ".join(x for x in names if x != n)
            chat = b.tail(10, who=n) if (b and len(names) > 1) else ""
            try:
                out, c = llm(wprompt(n, peers, chat, st["fb"], surviving, st["tests"]))
            except Exception as e:  # noqa: BLE001
                st["fb"] = f"[harness error {str(e)[:80]}]"; return
            cost[0] += c
            tests, say, done = parse(out)
            if not tests:
                st["fb"] = "no ```rust block found in your reply"; return
            st["tests"], st["done"] = tests, done
            work = os.path.join(d, f"w-{n}")
            os.makedirs(work, exist_ok=True)
            if not os.path.exists(os.path.join(work, "tree")):
                make_tree(work)
            ok, detail, res = score(work, tests, VISIBLE, f"{n}-r{rnd}")
            if not ok:
                st["fb"] = detail
                msg = f"{n} round {rnd + 1}: submission rejected by the scorer"
            else:
                caught, missed = res["caught"], res["missed"]
                st["visible"] = len(caught); st["good"] = tests
                st["fb"] = f"your tests catch {len(caught)} of {len(caught) + len(missed)} visible mutants. Still not caught:\n" + fmt_sample(missed, 12)
                msg = f"{n} round {rnd + 1}: [EVAL] {len(caught)}/{len(caught) + len(missed)} visible mutants caught (sha {hashlib.sha256(tests.encode()).hexdigest()[:10]}, {tests.count('#[test]')} tests)"
            log.append({"worker": n, "round": rnd + 1, "visible_caught": st["visible"], "ok": ok})
            if b and say:
                try:
                    b.say(n, n, say)
                except Exception:  # noqa: BLE001
                    pass
            if b:
                try:
                    b.say("bench", "scorer", msg)
                except Exception:  # noqa: BLE001
                    pass
        for n in names:
            t = threading.Thread(target=go, args=(n,)); ths.append(t); t.start()
        [t.join() for t in ths]
        if all(s["done"] for s in state.values()):
            break
    return {n: s["good"] for n, s in state.items() if s["good"]}, cost[0], log, {n: s["visible"] for n, s in state.items()}


def merged(subs):
    parts = []
    for i, (n, t) in enumerate(subs.items()):
        body = "\n".join(l for l in t.splitlines() if l.strip() != "use super::*;")
        parts.append(f"mod w{i} {{\n    use super::super::*;\n{body}\n}}")
    return "use super::*;\n" + "\n".join(parts) + "\n"


# ------------------------------------------------------------------ trials
BASE_MISSED_VIS, BASE_MISSED_HID, BASE_CAUGHT_VIS, BASE_CAUGHT_HID = [], [], 0, 0


def trial(cond, n):
    name = f"mut-{cond}-{n}"
    d = os.path.join(D, name)
    shutil.rmtree(d, ignore_errors=True); os.makedirs(d)
    t0, note, b, rec = time.time(), "", None, {}
    try:
        names = ["Worker-A", "Worker-B", "Worker-C"] if cond == "room" else ["Solo"]
        if cond == "room":
            os.makedirs(os.path.join(d, "repo")); subprocess.run(["git", "init", "-q", os.path.join(d, "repo")], check=True)
            b = TW.Playbox(d, 0, "unused")
            for w in names + ["bench"]:
                b.join(w)
            b.hub_register()
            commit = hashlib.sha256(("\n".join(sorted(BASE_MISSED_HID + BASE_CAUGHT_HID_NAMES))).encode()).hexdigest()
            b.say("bench", "scorer", f"Mutation room #{n}. Held-out mutants (shard {HIDDEN}) committed: sha256 {commit[:16]}. Workers: write tests for {os.path.basename(FILE)}.")
        subs, cost, log, vis_by = run_workers(b, d, names, name)
        if not subs:
            raise RuntimeError("no submissions")
        final = merged(subs) if cond == "room" else next(iter(subs.values()))
        work = os.path.join(d, "final"); os.makedirs(work); make_tree(work)
        open(os.path.join(d, "final_tests.rs"), "w").write(final)
        rec_fb = ""
        okh, det, hid = score(work, final, HIDDEN, "hidden")
        if not okh and cond == "room" and len(subs) > 1:        # the union failed: fall back to the best single worker
            best = max(subs, key=lambda k: vis_by.get(k) or 0)
            final = subs[best]; open(os.path.join(d, "final_tests.rs"), "w").write(final)
            okh, det, hid = score(work, final, HIDDEN, "hidden")
            rec_fb = best
        okv, detv, vis = score(work, final, VISIBLE, "visible")
        if not okh:
            raise RuntimeError("final tests failed: " + det[:200])
        rec = {"hidden_caught": len(hid["caught"]), "hidden_viable": len(hid["caught"]) + len(hid["missed"]), "hidden_missed": hid["missed"],
               "visible_caught": len(vis["caught"]) if vis else None, "baseline_hidden_caught": BASE_CAUGHT_HID, "tests": final.count("#[test]"), "fallback_single": rec_fb, "rounds": log, "cost_usd": round(cost, 3)}
        if b:
            rec["audit_ok"] = b.audit()[0]
            b.say("bench", "scorer", f"RESULT: held-out mutants caught {rec['hidden_caught']}/{rec['hidden_viable']} (existing tests alone: {BASE_CAUGHT_HID}); visible {rec['visible_caught']}")
    except Exception as e:  # noqa: BLE001
        note = f"driver error: {str(e)[:300]}"
    finally:
        if b:
            b.close()
    rec.update({"cond": cond, "trial": n, "elapsed_s": round(time.time() - t0), "complete": not note and "hidden_caught" in rec, "note": note, "at": time.strftime("%H:%M:%S")})
    open(RESULTS, "a").write(json.dumps(rec) + "\n")
    if HQ:
        try:
            HQ.say("bench", "mutation", f"{cond} #{n}: held-out mutants caught {rec.get('hidden_caught')}/{rec.get('hidden_viable')} (baseline {BASE_CAUGHT_HID}); {rec.get('tests')} tests; ${rec.get('cost_usd')}")
        except Exception:  # noqa: BLE001
            pass
    print(json.dumps({k: v for k, v in rec.items() if k not in ("hidden_missed", "rounds")}), flush=True)


BASE_CAUGHT_HID_NAMES = []


def baseline():
    """What the existing tests catch, on both shards (needs no agent)."""
    global BASE_MISSED_VIS, BASE_MISSED_HID, BASE_CAUGHT_VIS, BASE_CAUGHT_HID, BASE_CAUGHT_HID_NAMES, SRC
    SRC = open(os.path.join(BASE, FILE)).read()
    work = os.path.join(D, "baseline"); shutil.rmtree(work, ignore_errors=True); os.makedirs(work); make_tree(work)
    v, e1 = mutants_on(work, VISIBLE, "vis")
    h, e2 = mutants_on(work, HIDDEN, "hid")
    if not v or not h:
        raise SystemExit("baseline failed: " + e1 + e2)
    BASE_MISSED_VIS, BASE_CAUGHT_VIS = v["missed"], len(v["caught"])
    BASE_MISSED_HID, BASE_CAUGHT_HID, BASE_CAUGHT_HID_NAMES = h["missed"], len(h["caught"]), h["caught"]
    open(os.path.join(D, "baseline.json"), "w").write(json.dumps({"visible": v, "hidden": h}))
    print(f"baseline: visible caught {BASE_CAUGHT_VIS}/{BASE_CAUGHT_VIS + len(BASE_MISSED_VIS)}, hidden caught {BASE_CAUGHT_HID}/{BASE_CAUGHT_HID + len(BASE_MISSED_HID)}", flush=True)


def main():
    global HQ
    os.makedirs(D, exist_ok=True)
    baseline()
    if TW.HUB:
        hd = os.path.join(D, "mutation-hq"); shutil.rmtree(hd, ignore_errors=True); os.makedirs(os.path.join(hd, "repo"))
        subprocess.run(["git", "init", "-q", os.path.join(hd, "repo")], check=True)
        HQ = TW.Playbox(hd, 0, "unused"); HQ.join("bench"); HQ.hub_register()
        HQ.say("bench", "mutation", f"Mutation room: Haiku workers write tests for {os.path.basename(FILE)}; scored by cargo-mutants on held-out mutants. Existing tests catch {BASE_CAUGHT_HID} of {BASE_CAUGHT_HID + len(BASE_MISSED_HID)} viable held-out mutants.")
    only = os.environ.get("MUT_ONLY")
    for n in range(1, TRIALS + 1):
        for cond in ("room", "solo"):
            if only and only != cond:
                continue
            trial(cond, n)
    if HQ:
        HQ.close()


if __name__ == "__main__":
    main()
