#!/usr/bin/env python3
"""R&D room: Gemini 4 Carbon, Argon and Hydrogen agents (via authenticated Antigravity `agy` CLI; optional mixed Claude+Gemini mode)
sit in one Playbox room, research improvements to Playbox itself, cross-review proposals from other model families, implement the best ones
in throwaway worktrees, review diffs with a second family, pass a third-family ship/hold gate, and open DRAFT pull requests for what passes.
Agents never write files and never push: they read a repository snapshot and reply with JSON; the harness applies edits, runs cargo/pytest,
commits and opens the PR. Room messages are signed by each agent's own member key; proposals and reviews are read back from the room feed.
Env: RND_DIR, RND_BACKEND (agy|mixed), RND_PICK, RND_PER_FAMILY, RND_PR=1 (push + open draft PRs), RND_HUB=1, RND_MAX_TOKENS, RND_RESUME."""
import concurrent.futures as cf
import json
import os
import re
import subprocess
import sys
import threading
import time

H = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(H, "..", ".."))
import playbox_watch  # noqa: E402

RND = os.environ.get("RND_DIR") or os.path.expanduser("~/.playbox-rnd")
REPO, SNAP = os.path.join(RND, "repo"), os.path.join(RND, "snap")
RUN = os.path.join(RND, "run-" + time.strftime("%m%d-%H%M%S"))
BACKEND = os.environ.get("RND_BACKEND", "agy")  # "agy": Carbon + Argon + Hydrogen triad via `agy`; "mixed": Claude + Gemini
ADVISOR_ON = os.environ.get("RND_ADVISOR_ON", "1") == "1"
PICK = int(os.environ.get("RND_PICK", "4" if BACKEND == "agy" else "6"))
PER_FAMILY = int(os.environ.get("RND_PER_FAMILY", os.environ.get("RND_PER_VENDOR", "2" if BACKEND == "agy" else "3")))
DO_PR, HUB = os.environ.get("RND_PR") == "1", os.environ.get("RND_HUB", "1") == "1"
MAX_USD = float(os.environ.get("RND_MAX_USD", "30"))
MAX_TOKENS = int(os.environ.get("RND_MAX_TOKENS", "15000000"))

# Model routing: right-size effort per phase so Carbon, Argon and Hydrogen complement each other efficiently.
FAMILIES = ("Carbon", "Argon", "Hydrogen")
RESEARCH_MODELS = {
    "Carbon": os.environ.get("RND_CARBON_RESEARCH", "gemini-4-carbon-medium"),
    "Argon": os.environ.get("RND_ARGON_RESEARCH", "gemini-4-argon-medium"),
    "Hydrogen": os.environ.get("RND_HYDROGEN_RESEARCH", "gemini-4-hydrogen-high"),
}
REVIEW_MODELS = {
    "Carbon": os.environ.get("RND_CARBON_REVIEW", "gemini-4-carbon-low"),
    "Argon": os.environ.get("RND_ARGON_REVIEW", "gemini-4-argon-low"),
    "Hydrogen": os.environ.get("RND_HYDROGEN_REVIEW", "gemini-4-hydrogen-medium"),
}
HEAVY_MODELS = {
    "Carbon": os.environ.get("RND_CARBON_HEAVY", "gemini-4-carbon-high"),
    "Argon": os.environ.get("RND_ARGON_HEAVY", "gemini-4-argon-high"),
    "Hydrogen": os.environ.get("RND_HYDROGEN_HEAVY", "gemini-4-hydrogen-xhigh"),
}
CLAUDE_MODEL = os.environ.get("RND_CLAUDE", "claude-sonnet-5-5")
GEMINI_MODEL = os.environ.get("RND_GEMINI", "gemini-4-argon-high")
ADVISOR_MODEL = os.environ.get("RND_ADVISOR", HEAVY_MODELS["Carbon"] if BACKEND == "agy" else "claude-fable-5-2")

AGY_READONLY_SETTINGS = json.dumps({
    "permissions": {
        "allow": [
            "read_file(*)",
            "run_command(rg *)",
            "run_command(grep *)",
            "run_command(find *)",
            "run_command(ls *)",
            "run_command(git diff*)",
            "run_command(git log*)",
            "run_command(git show*)",
            "run_command(git status*)",
        ],
        "deny": [
            "write_file(*)",
            "edit_file(*)",
            "ask_question(*)",
            "invoke_subagent(*)",
            "define_subagent(*)",
            "swarm(*)",
            "run_workflow(*)",
            "schedule(*)",
            "search_web(*)",
            "run_command(git commit*)",
            "run_command(git push*)",
            "run_command(git reset*)",
            "run_command(git checkout*)",
            "run_command(git clean*)",
            "run_command(rm *)",
            "run_command(mv *)",
            "run_command(cp *)",
            "run_command(cargo *)",
        ],
    }
})

TARGET = os.path.join(RND, "target")
SLUG = os.environ.get("RND_REPO_SLUG", "Epistetechnician/playbox")
DENY = re.compile(
    r"^(\.github/|desktop/|Cargo\.lock$|(.*/)?Cargo\.toml$|scripts/release|.*signing|"
    r"crates/playbox-net/src/(grants|audit)\.rs$|crates/playbox-net/src/steer/|.*attest)"
)
ALLOW_NEW = re.compile(r"^(crates/[^/]+/(src|tests)/|docs/|tools/|ui/tests/|CHANGELOG\.md$)")
LENSES = [
    "security and trust gaps across crates/ (taint/capability enforcement, runner sandboxing & secrets, bridge/MCP input validation, host/replica boundary checks, path/symlink/size guards)",
    "reliability and operations across crates/ and ui/hub/ (restarts, connection/session lifecycle, stale state or partial writes, resource leaks, silent error swallowing, clear failure messages)",
    "developer and operator experience across crates/ and ui/hub/ (CLI/MCP parity gaps, misleading errors or help text, missing parse/config validation, subtle edge-case bugs with high-signal tests)",
]
BASE_FACTS = (
    "Context from Playbox architecture: (1) 8 Rust crates (`playbox-core`, `playbox-git`, `playbox-net`, `playbox-runner`, "
    "`playbox-mcp`, `playbox-cli`, `playbox-bridge`, `playbox-rap`) plus the Python hub (`ui/hub/`). "
    "(2) Every proposal must fix a REAL verified bug, missing check, lifecycle defect, or CLI/MCP parity gap in <=150 changed lines with a concrete test. "
    "(3) Do NOT touch `desktop/`, `.github/`, `Cargo.toml`, `Cargo.lock`, `scripts/release`, signing, `grants.rs`, `audit.rs`, attestation, or `steer/`."
)

ADV_COST = [0.0]
COST, LOCK, CARGO = [0.0], threading.Lock(), threading.Lock()
STATS = {
    f: {"calls": 0, "tokens": 0, "cached_tokens": 0, "thinking_tokens": 0, "seconds": 0.0}
    for f in (*FAMILIES, "Claude", "Gemini")
}


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


def family_of_model(model):
    m = model.lower()
    if "carbon" in m:
        return "Carbon"
    if "argon" in m:
        return "Argon"
    if "hydrogen" in m:
        return "Hydrogen"
    if "claude" in m or "fable" in m or "sonnet" in m:
        return "Claude"
    return "Gemini"


def total_tokens():
    with LOCK:
        return sum(v["tokens"] for v in STATS.values())


def discover_in_flight():
    """Fetch open PR titles and [R&D room] touched files so proposers avoid duplicating in-flight work or colliding on files."""
    extra = [os.environ.get("RND_ALREADY", "").strip()]
    try:
        r = subprocess.run(
            ["gh", "pr", "list", "--repo", SLUG, "--state", "open", "--limit", "40", "--json", "number,title,files"],
            capture_output=True, text=True, timeout=20,
        )
        if r.returncode == 0 and r.stdout.strip():
            prs = json.loads(r.stdout)
            titles = [f"#{p['number']} {p['title']}" for p in prs]
            rnd_files = sorted({
                f["path"]
                for p in prs if "[R&D room]" in p.get("title", "")
                for f in (p.get("files") or [])
                if f.get("path") and f["path"] != "CHANGELOG.md"
            })
            if titles:
                extra.append("Open PRs: " + "; ".join(titles))
            if rnd_files:
                extra.append("Files already modified by open [R&D room] PRs (avoid touching these same files): " + ", ".join(rnd_files))
    except Exception as e:  # noqa: BLE001
        log("in-flight PR check skipped:", str(e)[:100])
    joined = " | ".join(x for x in extra if x)
    return BASE_FACTS + (f"\nALREADY IN FLIGHT — do NOT propose these again or collide with them: {joined}" if joined else "")


# ------------------------------------------------------------ agents (read-only: they can inspect the repo, never modify it)
def ask_agy(prompt, model, cwd, t=900):
    r = subprocess.run(
        [
            "agy", "-p", prompt,
            "--model", model,
            "--sandbox",
            "--disable-slash-commands",
            "--settings", AGY_READONLY_SETTINGS,
            "--output-format", "json",
            "--print-timeout", f"{t}s",
        ],
        capture_output=True, text=True, cwd=cwd, timeout=t + 30,
    )
    lines = [ln for ln in r.stdout.strip().splitlines() if ln.strip().startswith("{")]
    if r.returncode != 0 and not lines:
        raise RuntimeError(f"agy({model}) rc={r.returncode}: {(r.stderr or r.stdout)[-240:]}")
    if not lines:
        raise RuntimeError(f"agy({model}) empty JSON stdout: {(r.stderr or r.stdout)[-240:]}")
    d = json.loads(lines[-1])
    if d.get("status") not in (None, "SUCCESS"):
        raise RuntimeError(f"agy({model}) status={d.get('status')}: {str(d.get('error') or d)[-240:]}")
    u = d.get("usage") or {}
    fam = family_of_model(model)
    with LOCK:
        s = STATS[fam]
        s["calls"] += 1
        s["tokens"] += int(u.get("total_tokens") or 0)
        s["cached_tokens"] += int(u.get("cache_read_tokens") or 0)
        s["thinking_tokens"] += int(u.get("thinking_tokens") or 0)
        s["seconds"] += float(d.get("duration_seconds") or 0.0)
    resp = (d.get("response") or "").strip()
    if not resp:
        raise RuntimeError(f"agy({model}) empty response")
    return resp


def ask_claude(prompt, cwd, model=None, effort=None, is_advisor=False, t=900):
    m = model or CLAUDE_MODEL
    cmd = [
        "claude", "-p", "--model", m,
        * (["--effort", effort] if effort else []),
        "--tools", "Read,Grep,Glob", "--no-session-persistence", "--disable-slash-commands",
        "--setting-sources", "", "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}',
        "--output-format", "json",
    ]
    r = subprocess.run(
        cmd, input=prompt, capture_output=True, text=True, cwd=cwd, timeout=t,
        env={**os.environ, "CLAUDE_CONFIG_DIR": os.path.expanduser("~/.claude-bench")},
    )
    if r.returncode != 0 and not r.stdout.strip():
        raise RuntimeError(f"claude({m}) rc={r.returncode}: {r.stderr[-240:]}")
    d = json.loads(r.stdout)
    with LOCK:
        c = float(d.get("total_cost_usd") or 0)
        COST[0] += c
        if is_advisor:
            ADV_COST[0] += c
        STATS["Claude"]["calls"] += 1
    return (d.get("result") or "").strip()


def call_model(spec, prompt, cwd, t=900):
    """spec is either an agy model id (or family name) or ('claude', model, effort, is_advisor)."""
    last = ""
    for attempt in range(3):
        try:
            if isinstance(spec, tuple) and spec[0] == "claude":
                return ask_claude(prompt, cwd, model=spec[1], effort=spec[2], is_advisor=spec[3], t=t)
            return ask_agy(prompt, spec, cwd, t=t)
        except Exception as e:  # noqa: BLE001
            last = str(e)[:240]
            time.sleep(6 * (attempt + 1))
    raise RuntimeError(f"model {spec} failed: {last}")


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


def ask_json(spec, prompt, cwd, t=900):
    for _ in range(2):
        out = call_model(spec, prompt, cwd, t=t)
        j = extract(out)
        if j:
            return j
        prompt += "\n\nYour last reply did not contain a valid JSON object. Reply with ONLY the raw JSON object (no markdown prose)."
    raise RuntimeError(f"{spec} gave no JSON")


# ------------------------------------------------------------ triad role assignment
def build_roster():
    """Build the room's proposer/reviewer agents.
    In default `agy` mode, Carbon, Argon and Hydrogen each contribute PER_FAMILY agents balanced across LENSES."""
    if BACKEND != "agy":
        return (
            [{"name": f"Claude-{k + 1}", "family": "Claude", "harness": "Claude Code",
              "research_spec": ("claude", CLAUDE_MODEL, None, False),
              "review_spec": ("claude", CLAUDE_MODEL, "low", False),
              "heavy_spec": ("claude", CLAUDE_MODEL, None, False),
              "model_label": CLAUDE_MODEL, "k": k % len(LENSES)} for k in range(PER_FAMILY)]
            + [{"name": f"Gemini-{k + 1}", "family": "Gemini", "harness": "Antigravity",
                "research_spec": GEMINI_MODEL, "review_spec": GEMINI_MODEL, "heavy_spec": GEMINI_MODEL,
                "model_label": GEMINI_MODEL, "k": k % len(LENSES)} for k in range(PER_FAMILY)]
        )
    agents = []
    # Offset lens index per family so with PER_FAMILY=2 all 3 lenses are covered by 2 distinct families each:
    # Carbon -> lenses (0, 2), Argon -> lenses (0, 1), Hydrogen -> lenses (1, 2).
    lens_map = {"Carbon": [0, 2, 1], "Argon": [0, 1, 2], "Hydrogen": [1, 2, 0]}
    for fam in FAMILIES:
        for idx in range(PER_FAMILY):
            k = lens_map[fam][idx % 3]
            agents.append({
                "name": f"{fam}-{idx + 1}",
                "family": fam,
                "harness": "Antigravity",
                "research_spec": RESEARCH_MODELS[fam],
                "review_spec": REVIEW_MODELS[fam],
                "heavy_spec": HEAVY_MODELS[fam],
                "model_label": RESEARCH_MODELS[fam],
                "k": k,
            })
    return agents


def assign_triad(pr_family, idx, agents):
    """3-family separation of duties for `agy` mode:
    Every proposal has {implementer_family, diff_reviewer_family, ship_gate_family} == {'Carbon', 'Argon', 'Hydrogen'}
    AND implementer_family != author_family, with Carbon-high / Argon-high handling heavy code synthesis."""
    by_fam = {}
    for a in agents:
        by_fam.setdefault(a["family"], []).append(a)
    if BACKEND != "agy":
        impl_fam = "Gemini" if pr_family == "Claude" else "Claude"
        rev_fam = "Claude" if impl_fam == "Gemini" else "Gemini"
        impl = by_fam[impl_fam][idx % len(by_fam[impl_fam])]
        rev = by_fam[rev_fam][idx % len(by_fam[rev_fam])]
        adv_spec = ("claude", ADVISOR_MODEL, "low", True)
        return impl, rev, ("Advisor", adv_spec)

    if pr_family == "Carbon":
        impl_fam, rev_fam, gate_fam = "Argon", "Hydrogen", "Carbon"
    elif pr_family == "Argon":
        impl_fam, rev_fam, gate_fam = "Carbon", "Hydrogen", "Argon"
    else:  # Hydrogen authored -> alternate Carbon/Argon implementers, other heavy model reviews, Hydrogen-xhigh gates
        impl_fam = "Carbon" if idx % 2 == 0 else "Argon"
        rev_fam = "Argon" if impl_fam == "Carbon" else "Carbon"
        gate_fam = "Hydrogen"
    impl = by_fam[impl_fam][idx % len(by_fam[impl_fam])]
    rev = by_fam[rev_fam][idx % len(by_fam[rev_fam])]
    return impl, rev, ("Advisor", HEAVY_MODELS[gate_fam])


# ------------------------------------------------------------ the room
class Room:
    def __init__(self, need_links=12):
        self.d = os.path.join(RUN, "playbox-rnd")
        os.makedirs(os.path.join(self.d, "home"), exist_ok=True)
        self.hosting = os.path.join(self.d, "hosting")
        self.say_lock = threading.Lock()
        max_m = max(16, need_links + 4)
        self.host = subprocess.Popen(
            ["playbox", "up", "--home", os.path.join(self.d, "home"), "--local", "--console",
             "--state", os.path.join(self.d, "state"), "--max-members", str(max_m), "--grant-ttl", "43200"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
            env={**os.environ, "PLAYBOX_HOME": os.path.join(self.d, "home"), **({} if HUB else {"PLAYBOX_HOSTING_DIR": self.hosting})},
        )
        self.links, self.mcp, self.ids = [], {}, {}
        threading.Thread(target=self._pump, daemon=True).start()
        for _ in range(60):
            if self.links:
                break
            time.sleep(0.25)
        if not self.links:
            raise RuntimeError("playbox up produced no initial join link")
        for _ in range(need_links):
            self.host.stdin.write("link\n")
            self.host.stdin.flush()
            time.sleep(0.25)
        for _ in range(40):
            if len(self.links) >= need_links:
                break
            time.sleep(0.2)

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
        time.sleep(0.3)

    def say(self, who, text):
        with self.say_lock:
            return self.mcp[who].call("playbox_say", {"text": text[:1900], "name": who})

    def pbx(self, who, kind, payload):
        """Emit a structured ::pbx message for the Playbox Hub room canvas."""
        try:
            self.say(who, f"::pbx {kind} " + json.dumps(payload, separators=(",", ":")))
        except Exception as e:  # noqa: BLE001
            log("pbx emit warning", kind, str(e)[:80])

    def feed(self):
        with self.say_lock:
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

    def close(self):
        for m in self.mcp.values():
            m.close()
        try:
            self.host.stdin.write("quit\n")
            self.host.stdin.flush()
            self.host.wait(timeout=10)
        except Exception:  # noqa: BLE001
            self.host.kill()


# ------------------------------------------------------------ phases
def research(room, agents, facts):
    def one(a):
        fam, k = a["family"], a["k"]
        p = (
            f"You are {a['name']} ({fam} model family), an R&D engineer on a multi-model Playbox R&D team improving Playbox: "
            "8 Rust crates plus a Python hub where agents from different owners collaborate in signed, auditable rooms.\n"
            f"The repository (read-only) is your working directory. Your lens: {LENSES[k]}.\n{facts}\n\n"
            "Inspect the codebase directly using read_file and grep/rg/find/ls (read-only only; do not write files or run cargo). "
            "Verify every line number and claim against the actual source files.\n"
            "Propose 2 to 3 concrete, SURGICAL improvements — each implementable cleanly in <= 150 changed lines in existing crates/ or ui/hub/, "
            "with a deterministic unit or integration test. Prefer real bugs, unvalidated inputs, silent error loss, security/taint/policy gaps, "
            "or CLI/MCP mismatches backed by exact `path:line` evidence.\n"
            "Do NOT propose: dependency changes, CI/workflow changes, desktop/ changes, release/signing changes, or edits to grants.rs, audit.rs, attestation, or steer/.\n"
            "Keep each proposal compact (under 1400 characters total when serialized).\n"
            'Reply with ONLY JSON: {"proposals":[{"title":"","problem":"","evidence":["path:line"],"change":"","files":["path"],"tests":"","risk":"low|medium","impact":""}]}'
        )
        j = ask_json(a["research_spec"], p, SNAP)
        sh("git checkout -q -- . && git clean -fdq", SNAP)
        posted = 0
        for i, pr in enumerate((j.get("proposals") or [])[:3]):
            if not pr.get("title") or not pr.get("files"):
                continue
            pr["id"], pr["author"], pr["family"] = f"{a['name']}-{i + 1}", a["name"], fam
            room.say(a["name"], "PROPOSAL " + json.dumps(pr, separators=(",", ":")))
            posted += 1
        log("research done", a["name"], f"({a['model_label']})", posted)

    def safe(a):
        try:
            one(a)
        except Exception as e:  # noqa: BLE001
            log("research FAILED", a["name"], str(e)[:160])

    with cf.ThreadPoolExecutor(len(agents)) as ex:
        list(ex.map(safe, agents))
    props = {}
    for sender, pr in room.tagged("PROPOSAL"):
        if sender == room.ids.get(pr.get("author")):  # signing key must belong to the claimed author
            props[pr["id"]] = pr
    return props


def review(room, agents, props):
    def one(a):
        # Cross-family review: never review your own family's proposals!
        mine = [p for p in props.values() if p.get("family") != a["family"] and p.get("author") != a["name"]]
        if not mine:
            return
        p = (
            f"You are {a['name']} ({a['family']} reviewer) on the Playbox R&D team. The repository (read-only) is your working directory.\n"
            "Below are proposals from other model families. For EACH proposal, read the cited `path:line` files to verify the claim, "
            "then rate impact (1-5: real security, correctness, or operator value) and feasibility (1-5: cleanly landable in <=150 lines with a solid test), "
            "and give verdict `pursue` or `skip`. Be sceptical: give `skip` if the claim is false, already fixed on main, collides with an open PR, "
            "touches forbidden files, or requires a large cross-crate redesign.\n\n"
            + "\n".join(json.dumps(x, separators=(",", ":")) for x in mine)
            + '\n\nReply with ONLY JSON: {"reviews":[{"id":"","impact":3,"feasibility":3,"verdict":"pursue|skip","comment":"one concise sentence"}]}'
        )
        j = ask_json(a["review_spec"], p, SNAP)
        sh("git checkout -q -- . && git clean -fdq", SNAP)
        for r in (j.get("reviews") or []):
            room.say(a["name"], "REVIEW " + json.dumps(r, separators=(",", ":")))
        log("review done", a["name"], f"({a['review_spec'] if isinstance(a['review_spec'], str) else a['review_spec'][1]})", len(j.get("reviews") or []))

    def safe(a):
        try:
            one(a)
        except Exception as e:  # noqa: BLE001
            log("review FAILED", a["name"], str(e)[:160])

    with cf.ThreadPoolExecutor(len(agents)) as ex:
        list(ex.map(safe, agents))
    rev = {}
    by_id = {v: k for k, v in room.ids.items()}
    fam_by_name = {a["name"]: a["family"] for a in agents}
    for sender, r in room.tagged("REVIEW"):
        who = by_id.get(sender)
        pid = r.get("id")
        if who and pid in props and props[pid]["author"] != who and props[pid].get("family") != fam_by_name.get(who):
            rev.setdefault(pid, []).append({**r, "by": who, "family": fam_by_name.get(who, "")})
    return rev


def advisor_spec():
    return ADVISOR_MODEL if BACKEND == "agy" else ("claude", ADVISOR_MODEL, "low", True)


def advise(room, props, rev, base, facts):
    """Advisor checks top-ranked proposals in the code, drops duplicates/weak ideas, and orders the best non-overlapping ones."""
    top = [pid for pid, _ in base][:10]
    if not top:
        return None
    lines = []
    for pid in top:
        rs = rev.get(pid, [])
        lines.append(json.dumps({
            "proposal": props[pid],
            "reviews": [{k: r.get(k) for k in ("by", "family", "impact", "feasibility", "verdict", "comment")} for r in rs],
        }, separators=(",", ":")))
    p = (
        "You are the Lead Advisor to the Playbox multi-model R&D room. The repository (read-only) is your working directory.\n"
        f"{facts}\n\nBelow are the top-ranked proposals and their cross-family reviews. Verify the cited files in the repository before trusting them. "
        "Drop any proposal that is already solved, duplicates another stronger proposal in the list, touches overlapping files with a better proposal, "
        "collides with open PRs, or is cosmetic. Order `keep` from highest-value/lowest-risk to lowest.\n"
        'Reply with ONLY JSON: {"keep":["ids in priority order"],"drop":[{"id":"","why":""}],"note":"one sentence"}\n\n'
        + "\n".join(lines)
    )
    try:
        j = ask_json(advisor_spec(), p, SNAP)
    except Exception as e:  # noqa: BLE001
        log("advisor triage warning:", str(e)[:140])
        return None
    sh("git checkout -q -- . && git clean -fdq", SNAP)
    if j:
        room.say("Advisor", "ADVICE " + json.dumps(j, separators=(",", ":")))
    return j


def select(props, rev, need=2, advice=None, n=None, only=None):
    ranked = []
    drops = {d.get("id") if isinstance(d, dict) else d for d in (advice or {}).get("drop", [])}
    keeps = list((advice or {}).get("keep", []))
    for pid, rs in rev.items():
        if only and pid not in only:
            continue
        p = props[pid]
        files = [f for f in (p.get("files") or []) if f != "CHANGELOG.md"]
        if len(rs) < need or not files or any(DENY.match(f) for f in files) or p.get("risk") == "high":
            continue
        imp = sum(int(r.get("impact", 1)) for r in rs) / len(rs)
        fea = sum(int(r.get("feasibility", 1)) for r in rs) / len(rs)
        if sum(1 for r in rs if r.get("verdict") == "skip") * 2 > len(rs):
            continue
        sc = imp * fea
        prio = 0
        if advice:
            if pid in drops:
                continue
            if pid in keeps:
                sc *= 1.25
                prio = len(keeps) - keeps.index(pid)
        ranked.append((prio, sc, pid))
    ranked.sort(reverse=True)
    chosen, used_files = [], set()
    for _, sc, pid in ranked:
        fset = {f for f in (props[pid].get("files") or []) if f != "CHANGELOG.md"}
        if fset & used_files:  # ensure zero file collisions across chosen proposals in the same run
            continue
        used_files |= fset
        chosen.append((pid, sc))
        if len(chosen) >= (n or PICK):
            break
    return chosen


def _tail_io(stdout, stderr):
    out = (stdout or "")[-2500:]
    err = (stderr or "")[-1500:]
    return (out + ("\n" + err if err else "")).strip()[-3800:]


def sh(cmd, cwd, t=1800, env=None):
    r = subprocess.run(cmd, shell=True, cwd=cwd, capture_output=True, text=True, timeout=t, env={**os.environ, "CARGO_TARGET_DIR": TARGET, **(env or {})})
    return r.returncode, _tail_io(r.stdout, r.stderr)


def run(args, cwd, t=1800):
    r = subprocess.run(args, cwd=cwd, capture_output=True, text=True, timeout=t, env={**os.environ, "CARGO_TARGET_DIR": TARGET})
    return r.returncode, _tail_io(r.stdout, r.stderr)


def _within_wt(wt, rel):
    if not isinstance(rel, str) or not rel or os.path.isabs(rel):
        return None
    wt_real = os.path.realpath(wt)
    cand = os.path.normpath(os.path.join(wt_real, rel))
    if os.path.commonpath([wt_real, cand]) != wt_real or cand == wt_real or os.path.islink(cand):
        return None
    parent_real = os.path.realpath(os.path.dirname(cand))
    if os.path.commonpath([wt_real, parent_real]) != wt_real:
        return None
    return cand


def apply_edits(wt, j):
    edits = j.get("edits") or []
    new_files = j.get("new_files") or []
    if not edits and not new_files:
        return "JSON contained neither edits nor new_files"
    for e in edits:
        p = _within_wt(wt, e.get("path"))
        if not p or DENY.match(e["path"]) or not os.path.isfile(p):
            return f"edit refused or file missing: {e.get('path')}"
        s = open(p).read()
        find, repl = e.get("find", ""), e.get("replace", "")
        if not find:
            return f"empty find string for {e['path']}"
        if s.count(find) != 1:
            return f"find text must match exactly once in {e['path']} (matched {s.count(find)}): {find[:80]!r}"
        open(p, "w").write(s.replace(find, repl, 1))
    for f in new_files:
        p = _within_wt(wt, f.get("path"))
        if not p or DENY.match(f["path"]) or not ALLOW_NEW.match(f["path"]):
            return f"new file not allowed: {f.get('path')}"
        os.makedirs(os.path.dirname(p), exist_ok=True)
        if os.path.commonpath([os.path.realpath(wt), os.path.realpath(os.path.dirname(p))]) != os.path.realpath(wt):
            return f"new file path escapes worktree: {f.get('path')}"
        open(p, "w").write(f.get("content", ""))
    return ""


def gate(wt):
    _, files = sh("git diff --name-only; git ls-files --others --exclude-standard", wt)
    files = sorted({x for x in files.split() if x})
    if not files:
        return False, "no change", files
    bad = [f for f in files if DENY.match(f)]
    if bad:
        return False, f"touched forbidden paths: {bad}", files
    _, stat = sh("git add -N . && git diff --shortstat", wt)
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
            run(["cargo", "fmt", *sum((["-p", c] for c in crates), [])], wt)
            pk = " ".join(f"-p {c}" for c in crates)
            for name, cmd in (("clippy", f"cargo clippy -q {pk} --all-targets -- -D warnings"), ("test", f"cargo test -q {pk}")):
                rc, out = sh(cmd, wt, 3000)
                if rc:
                    return False, f"{name} failed:\n{out}", files
                msgs.append(f"{name} ok ({','.join(crates)})")
    py_files = [f for f in files if f.endswith(".py")]
    for f in py_files:
        rc, out = run(["python3", "-m", "py_compile", f], wt)
        if rc:
            return False, f"py_compile failed: {out}", files
    if any(f.startswith("ui/") for f in py_files):
        rc, out = run(["python3", "-m", "pytest", "-q", "ui/tests/test_hub.py"], wt, 600)
        if rc:
            return False, f"ui pytest failed:\n{out}", files
        msgs.append("pytest ok (ui/tests/test_hub.py)")
    return True, "; ".join(msgs) or "docs/python compile ok", files


def budget_exceeded():
    with LOCK:
        if (COST[0] + ADV_COST[0]) >= MAX_USD:
            return f"stopped at MAX_USD={MAX_USD}"
        toks = sum(v["tokens"] for v in STATS.values())
        if toks >= MAX_TOKENS:
            return f"stopped at MAX_TOKENS={MAX_TOKENS}"
    return ""


def implement(room, pr, impl_agent, rev_agent, gate_info, score, advisor_note=""):
    pid = pr["id"]
    slug = re.sub(r"[^a-z0-9]+", "-", pr["title"].lower()).strip("-")[:40]
    wt = os.path.join(RUN, "wt", pid)
    os.makedirs(os.path.dirname(wt), exist_ok=True)
    run(["git", "worktree", "add", "-q", "--detach", wt, "origin/main"], REPO)
    gate_name, gate_spec = gate_info
    impl_model_label = impl_agent["heavy_spec"] if isinstance(impl_agent["heavy_spec"], str) else impl_agent["heavy_spec"][1]
    rev_model_label = rev_agent["heavy_spec"] if isinstance(rev_agent["heavy_spec"], str) else rev_agent["heavy_spec"][1]
    gate_model_label = gate_spec if isinstance(gate_spec, str) else gate_spec[1]
    res = {
        "id": pid,
        "title": pr["title"],
        "author": pr["author"],
        "author_family": pr.get("family", ""),
        "implementer": f"{impl_agent['name']} ({impl_model_label})",
        "reviewer": f"{rev_agent['name']} ({rev_model_label})",
        "ship_gate": f"{gate_name} ({gate_model_label})",
        "score": round(score, 2),
        "status": "failed",
        "notes": [],
    }
    room.pbx(impl_agent["name"], "card", {"id": pid, "title": pr["title"], "col": "doing", "owner": impl_agent["name"]})
    feedback, rounds = "", 0
    try:
        for attempt in range(4):
            stop_reason = budget_exceeded()
            if stop_reason:
                res.update(status="budget-capped")
                res["notes"].append(stop_reason)
                break
            sh("git checkout -q -- . && git clean -fdq", wt)
            p = (
                f"You are {impl_agent['name']} ({impl_agent['family']} implementer) on the Playbox R&D team. "
                "The repository (read-only for you) is your working directory; you cannot write files directly, so inspect the target files with `read_file` "
                "first and reply with exact JSON edits for the harness to apply.\n"
                f"PROPOSAL: {json.dumps(pr)}\n\n"
                "Rules:\n"
                "1. Call `read_file` on EVERY file you plan to edit (and lines 1-20 of `CHANGELOG.md`) BEFORE constructing your JSON so every `find` string matches byte-for-byte once.\n"
                "2. Smallest clean change that solves the root problem; add a focused unit/integration test in the same crate (or `ui/tests/` for hub changes); match existing Rust/Python style and clippy `-D warnings` cleanliness.\n"
                "3. No dependency changes; do NOT touch `desktop/`, `.github/`, `Cargo.toml`, `Cargo.lock`, `scripts/release`, signing, `grants.rs`, `audit.rs`, attestation, or `steer/`.\n"
                "4. In `CHANGELOG.md`, add one bullet under `## [Unreleased]` (`### Added` or `### Fixed`, creating that subheading under `## [Unreleased]'` if needed — include `## [Unreleased]` AND the following `## [` release header in `find` so `find` is unique!).\n"
                + (f"Advisor implementation note: {advisor_note}\n" if advisor_note else "")
                + feedback
                + '\nReply with ONLY JSON: {"edits":[{"path":"","find":"","replace":""}],"new_files":[{"path":"","content":""}],"summary":"2-3 sentences for the PR description"}'
            )
            j = ask_json(impl_agent["heavy_spec"], p, wt, t=1200)
            sh("git checkout -q -- . && git clean -fdq", wt)  # enforce read-only agent invariant before applying JSON edits
            err = apply_edits(wt, j)
            if err:
                log("edit error", pid, f"attempt {attempt + 1}:", err[:140])
                feedback = f"\nYour previous edits failed to apply: {err}\nRe-read the exact lines with `read_file` and send the complete edit set.\n"
                res["notes"].append(err[:200])
                continue
            ok, msg, files = gate(wt)
            room.say("orchestrator", f"GATE {pid} attempt {attempt + 1}: {'PASS' if ok else 'FAIL'} {msg[:600]}")
            log("gate", pid, f"attempt {attempt + 1}:", "PASS" if ok else "FAIL", msg[:120].replace("\n", " "))
            if not ok:
                feedback = f"\nYour previous edits failed verification:\n{msg}\nFix all compiler/clippy/test errors and send the complete edit set from clean HEAD.\n"
                res["notes"].append(msg[:200])
                continue
            room.pbx(impl_agent["name"], "card", {"id": pid, "title": pr["title"], "col": "review", "owner": rev_agent["name"]})
            diff = subprocess.run("git add -N . && git diff", shell=True, cwd=wt, capture_output=True, text=True).stdout
            q = (
                f"You are {rev_agent['name']} ({rev_agent['family']} adversarial diff reviewer), reviewing {impl_agent['name']}'s patch to Playbox. "
                f"The repository with the patch applied is your working directory (read-only). Proposal: {json.dumps(pr)}\n\n"
                f"PATCH:\n{diff[:14000]}\n\nChecks passed: {msg}\n"
                "Inspect surrounding context with `read_file` if needed. Check: (a) does it genuinely fix the problem without regressions, "
                "(b) is security/isolation preserved or strengthened (never weakened), (c) does the test actually exercise the new behavior and fail without it?\n"
                'Reply with ONLY JSON: {"verdict":"approve|changes","issues":["..."]}'
            )
            v = ask_json(rev_agent["heavy_spec"], q, wt, t=900)
            room.say(rev_agent["name"], f"VERDICT {pid} {json.dumps(v, separators=(',', ':'))}"[:1900])
            if v.get("verdict") == "approve" and ADVISOR_ON:
                aq = (
                    f"You are the independent 3rd-family Ship Gate ({gate_model_label}). Proposal: {json.dumps(pr)}\n\n"
                    f"PATCH:\n{diff[:12000]}\n\nChecks passed: {msg}. Cross-family reviewer ({rev_agent['name']}) approved it.\n"
                    "Hold ONLY if you spot a concrete bug, subtle race/panic, weakened security invariant, or bogus test.\n"
                    'Reply with ONLY JSON: {"decision":"ship|hold","why":"one sentence"}'
                )
                try:
                    aj = ask_json(gate_spec, aq, wt, t=900)
                except Exception as e:  # noqa: BLE001
                    log("ship-gate warning", pid, str(e)[:120])
                    aj = None
                if aj:
                    room.say(gate_name, f"ADVICE {pid} " + json.dumps(aj, separators=(",", ":")))
                    if aj.get("decision") == "hold":
                        res.update(status="held-by-advisor", advisor=aj.get("why", ""))
                        room.pbx(gate_name, "artifact", {"id": pid, "title": pr["title"], "kind": "code", "status": "failed", "summary": aj.get("why", "held")[:240]})
                        break
            if v.get("verdict") == "approve":
                res.update(status="approved", summary=j.get("summary", ""), gate=msg, files=files, diff_lines=len(diff.splitlines()), verdict=v, patch=diff)
                break
            rounds += 1
            feedback = f"\nReviewer {rev_agent['name']} requested changes: {v.get('issues')}. Fix every valid point and send the complete revised edit set from clean HEAD.\n"
            res["notes"].append("review: " + str(v.get("issues"))[:200])
            if rounds >= 2:
                res["status"] = "rejected-by-review"
                break
        if res["status"] == "approved":
            branch = f"rnd/{slug}-{pid.lower()}"
            res["branch"] = branch
            run(["git", "checkout", "-q", "-b", branch], wt)
            run(["git", "add", "-A"], wt)
            run(
                [
                    "git", "commit", "-q",
                    "-m", "R&D room: " + pr["title"],
                    "-m", pr["problem"][:400] + "\n\nProposed, implemented, reviewed, and ship-gated across Carbon, Argon, and Hydrogen in a Playbox room.",
                ],
                wt,
            )
            res["commit"] = sh("git rev-parse HEAD", wt)[1].strip()
            checks = [{"name": seg.strip(), "ok": True} for seg in res["gate"].split(";") if seg.strip()]
            room.pbx(impl_agent["name"], "artifact", {
                "id": pid, "title": pr["title"], "kind": "code", "status": "verified",
                "summary": res["summary"][:280], "ref": res["commit"][:12], "checks": checks,
            })
            room.pbx(impl_agent["name"], "card", {"id": pid, "title": pr["title"], "col": "done", "owner": impl_agent["name"], "artifact": pid})
    except Exception as e:  # noqa: BLE001
        res["notes"].append(f"error: {str(e)[:200]}")
    return res, wt


def open_pr(room, res, wt, props, rev):
    resolve = os.environ.get("RND_RESOLVE", "")
    push_cmd = ["git", *(["-c", f"http.curloptResolve={resolve}"] if resolve else []), "push", "-q", "origin", res["branch"]]
    rc, out = run(push_cmd, wt, 300)
    if rc:
        res["notes"].append("push failed: " + out[-200:])
        return
    pr = props[res["id"]]
    revs = "\n".join(
        f"- **{r['by']}** ({r.get('family', '')}): impact {r.get('impact')}/5, feasibility {r.get('feasibility')}/5, `{r.get('verdict')}` — {r.get('comment', '')}"
        for r in rev.get(res["id"], [])
    )
    body = (
        f"{res['summary']}\n\n"
        f"## Why\n{pr['problem']}\n\n"
        f"Evidence: {', '.join(pr.get('evidence', []))}\n\n"
        "## How this was made\n"
        f"- **Proposed by:** `{pr['author']}` ({pr.get('family', '')})\n"
        f"- **Implemented by:** `{res['implementer']}`\n"
        f"- **Diff reviewed by:** `{res['reviewer']}`\n"
        f"- **3rd-family ship gate:** `{res['ship_gate']}`\n\n"
        "All proposals, reviews, gate outputs, and verdicts were signed by each agent's own member key in a local Playbox room (`playbox-rnd`). "
        "Agents operated with read-only access to the snapshot; the harness applied JSON edits, ran formatting/clippy/tests, and pushed this branch.\n\n"
        f"## Cross-family proposal reviews\n{revs}\n\n"
        f"## Checks run\n`{res['gate']}`\n\n"
        "This is a draft PR. A human maintainer should inspect the diff before merging."
    )
    bf = os.path.join(RUN, f"body-{res['id']}.md")
    open(bf, "w").write(body)
    rc, out = run(
        ["gh", "pr", "create", "--repo", SLUG, "--draft", "--base", "main", "--head", res["branch"], "--title", "[R&D room] " + res["title"], "--body-file", bf],
        wt, 120,
    )
    res["pr"] = out.strip().splitlines()[-1] if not rc and out.strip() else ""
    if rc:
        res["notes"].append("pr failed: " + out[-200:])
    else:
        res["status"] = "pr-opened"
        room.say("orchestrator", f"PR {res['id']}: {res['pr']}")


def refresh_snapshot():
    if not os.path.isdir(REPO):
        raise RuntimeError(f"missing RND repo clone at {REPO}")
    rc, out = run(["git", "fetch", "-q", "origin", "main"], REPO, 120)
    if rc:
        log("git fetch warning:", out[-160:])
    if not os.path.isdir(SNAP):
        run(["git", "worktree", "add", "-q", "--detach", SNAP, "origin/main"], REPO)
    else:
        sh("git checkout -q --detach origin/main && git reset --hard -q origin/main && git clean -fdq", SNAP)


def main():
    os.makedirs(RUN, exist_ok=True)
    log("run dir", RUN, f"backend={BACKEND}")
    refresh_snapshot()
    head_sha = sh("git rev-parse --short origin/main", REPO)[1].strip()
    facts = discover_in_flight()
    agents = build_roster()
    room = Room(need_links=len(agents) + 4)
    try:
        room.join("orchestrator", "harness", "none")
        if ADVISOR_ON:
            room.join("Advisor", "Antigravity" if BACKEND == "agy" else "Claude Code", ADVISOR_MODEL)
        for a in agents:
            room.join(a["name"], a["harness"], a["model_label"])
        roster_desc = ", ".join(f"{a['name']}:{a['model_label']}" for a in agents)
        room.say(
            "orchestrator",
            f"R&D run started on origin/main@{head_sha} ({BACKEND}): {roster_desc}. "
            "Phases: research -> cross-family review -> advisor triage -> cross-family implement -> adversarial diff review -> 3rd-family ship gate -> draft PR.",
        )
        resume = os.environ.get("RND_RESUME")
        old = json.load(open(os.path.join(resume, "proposals.json"))) if resume else {}
        if old.get("proposals"):
            props = old["proposals"]
            for pr in props.values():
                if pr.get("author") in room.mcp:
                    room.say(pr["author"], "PROPOSAL " + json.dumps(pr, separators=(",", ":")))
        else:
            props = research(room, agents, facts)
        log("proposals", len(props))
        room.pbx("orchestrator", "metric", {"name": "proposals", "value": len(props), "unit": "count"})
        json.dump({"head": head_sha, "proposals": props}, open(os.path.join(RUN, "proposals.json"), "w"), indent=1)

        rev = old.get("reviews") or review(room, agents, props)
        if old.get("reviews"):
            for rs in rev.values():
                for r in rs:
                    if r.get("by") in room.mcp:
                        room.say(r["by"], "REVIEW " + json.dumps(r, separators=(",", ":")))
        room.pbx("orchestrator", "metric", {"name": "reviews", "value": sum(len(v) for v in rev.values()), "unit": "count"})
        min_reviews = min(2, max(1, len(agents) - PER_FAMILY))
        only = set(os.environ["RND_ONLY"].split(",")) if os.environ.get("RND_ONLY") else None
        chosen = select(props, rev, min_reviews, only=only)
        advice = old.get("advice")
        if advice and ADVISOR_ON:
            room.say("Advisor", "ADVICE " + json.dumps(advice, separators=(",", ":")))
        elif ADVISOR_ON and chosen:
            advice = advise(room, props, rev, select(props, rev, min_reviews, None, 10, only=only), facts)
        if advice:
            chosen = select(props, rev, min_reviews, advice, only=only)
        json.dump({"head": head_sha, "proposals": props, "reviews": rev, "advice": advice, "chosen": chosen}, open(os.path.join(RUN, "proposals.json"), "w"), indent=1)
        log("selected", [(pid, round(s, 2)) for pid, s in chosen])
        room.say("orchestrator", "SELECTED " + json.dumps([[pid, round(s, 1)] for pid, s in chosen]))
        for pid, _ in chosen:
            room.pbx("orchestrator", "card", {"id": pid, "title": props[pid]["title"], "col": "todo", "owner": "orchestrator"})

        adv_note = (advice or {}).get("note", "")
        results = []
        with cf.ThreadPoolExecutor(min(3, max(1, len(chosen)))) as ex:
            futs = []
            for i, (pid, sc) in enumerate(chosen):
                pr = props[pid]
                impl_a, rev_a, gate_info = assign_triad(pr.get("family", "Hydrogen"), i, agents)
                futs.append(ex.submit(implement, room, pr, impl_a, rev_a, gate_info, sc, adv_note))
            for f in futs:
                res, wt = f.result()
                if res["status"] == "approved" and DO_PR:
                    open_pr(room, res, wt, props, rev)
                results.append(res)
                log("result", res["id"], res["status"], res.get("pr") or res.get("branch", ""))

        passed = sum(1 for r in results if r["status"] in ("approved", "pr-opened"))
        room.pbx("orchestrator", "metric", {"name": "approved_changes", "value": passed, "unit": "PRs"})
        room.pbx("orchestrator", "metric", {"name": "total_tokens", "value": total_tokens(), "unit": "tokens"})
        summary = {"head": head_sha, "backend": BACKEND, "stats": STATS, "claude_usd": round(COST[0], 4), "results": results}
        json.dump(summary, open(os.path.join(RUN, "results.json"), "w"), indent=1)
        fam_summary = ", ".join(
            f"{k}: {v['calls']} calls / {v['tokens']:,} tok ({v['cached_tokens']:,} cached)"
            for k, v in STATS.items() if v["calls"] > 0
        )
        done_msg = f"DONE on {head_sha}. {passed}/{len(results)} proposals passed gates + cross-family review + ship gate. Usage: {fam_summary}."
        if COST[0] > 0:
            done_msg += f" Claude cost ${COST[0]:.2f}."
        room.say("orchestrator", done_msg)
        log(done_msg)
    finally:
        time.sleep(2)
        room.close()


if __name__ == "__main__":
    main()
