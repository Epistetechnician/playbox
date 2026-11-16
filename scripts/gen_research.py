#!/usr/bin/env python3
"""Generate ui/research/room-vs-silo.json and ui/research/f3-h100-pilot.json from their sources.

Sources: docs/benchmark.md (tables) and docs/f3/pilot/{README.md,receipts.jsonl,feed.jsonl,candidates/}.
Raw per-try results (tools/bench/hillclimb/**/scores.tsv) are not committed, so benchmark samples come from the markdown tables;
if a scores.tsv is present it is used for the per-try samples instead.

  python3 scripts/gen_research.py           write the JSON files
  python3 scripts/gen_research.py --check   exit 1 if the committed JSON differs from what the sources produce (CI)
"""
import json, re, sys, pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "ui" / "research"
CAP = 200  # max samples embedded per run; the rest stay in the source file


def tables(md):
    """All markdown tables as (preceding heading, header cells, rows)."""
    out, heading, lines = [], "", md.splitlines()
    i = 0
    while i < len(lines):
        if lines[i].startswith("#"):
            heading = lines[i].lstrip("# ").strip()
        if lines[i].startswith("|") and i + 1 < len(lines) and re.match(r"^\|[\s\-|:]+\|$", lines[i + 1]):
            cells = lambda l: [c.strip() for c in l.strip().strip("|").split("|")]
            hdr, rows = cells(lines[i]), []
            i += 2
            while i < len(lines) and lines[i].startswith("|"):
                rows.append(cells(lines[i]))
                i += 1
            out.append((heading, hdr, rows))
            continue
        i += 1
    return out


def n(s):
    return float(re.sub(r"[^0-9.\-]", "", s))


def clean(s):
    return s.replace("×", "x").replace("·", "x") if False else s


def benchmark():
    md = (ROOT / "docs" / "benchmark.md").read_text()
    tbs = tables(md)
    main = next(r for h, hd, r in tbs if hd[:2] == ["#", "Task"])
    trial = next(r for h, hd, r in tbs if hd[0] == "Final, per trial")
    cost = next(r for h, hd, r in tbs if hd[:3] == ["Haiku-only silo, per trial", "Tries", "Cost"])
    posted_p = re.search(r"posted-vector result, p = ([0-9.]+)", md).group(1)
    row5 = next(r for r in cost if r[1] == "1,800")
    # Results table: keep the schema (Row, Task, Trials, Room, Silo, Classical, Room vs silo)
    task_short = {"1": "16 numbers, all scored by everyone", "2": "32 numbers, all scored by everyone", "3": "32 numbers, split in 3 slices",
                  "4": "as 3, plus Fable 5.2 advisor", "5": "as 3, Haiku only, 18x more tries", "6": "as 5, in a room, 10x more tries"}
    rows = []
    for r in main:
        cls = r[5] if re.match(r"^[0-9,]+$", r[5]) else "-"
        room = r[3] if re.match(r"^[0-9,]+$", r[3]) else "-"
        rows.append([r[0], task_short[r[0]], r[2], room, r[4], cls, r[6]])
    usage = {"title": "Cost per trial (list prices)", "unit": "USD", "items": []}
    by = {c[0] or c[1]: c for c in cost}
    usage["items"] = [
        {"label": "Haiku-only, 1800 tries", "value": n(row5[2]), "c": "ok"},
        {"label": "Fable advisor, silo", "value": n(cost[6][2]), "c": "warn"},
        {"label": "Fable advisor, room", "value": n(cost[7][2]), "c": "blue"},
        {"label": "Haiku-only, 90 tries", "value": n(cost[0][2]), "c": "mut"},
    ]
    # Samples: row 4 per-trial finals (room and silo), row 5 per-checkpoint means. Best-so-far = running minimum within a condition.
    samples = []
    asm = next(r for r in trial if r[0] == "Assembled")
    for ci, cond in ((1, "room"), (2, "silo")):
        best = None
        for k, v in enumerate(asm[ci].split("·"), 1):
            v = n(v)
            best = v if best is None else min(best, v)
            samples.append({"id": f"row4-{cond}-{k}", "row": "4", "agent": f"{cond} trial {k}", "kind": "trial", "score": v, "best": best, "cond": cond,
                            "steps": [{"label": "result", "text": f"assembled score {v:,.0f} after 90 tries (Fable advisor every 15 tries)", "c": "ok" if v == 0 else "blue"},
                                      {"label": "note", "text": "assembled = sum over the 3 slices of the best slice score found; silo agents never combine slices", "c": "mut"}]})
    best = None
    for k, r in enumerate(cost[:6], 1):
        v = n(r[3].split("(")[0])
        best = v if best is None else min(best, v)
        samples.append({"id": f"row5-{k}", "row": "5", "agent": f"haiku silos, {r[1]} tries", "kind": "checkpoint", "score": v, "best": best, "cond": "silo",
                        "steps": [{"label": "cost", "text": f"{r[2]} per trial", "c": "warn"}, {"label": "result", "text": f"mean assembled score of 4 trials: {r[3]}", "c": "ok" if v == 0 else "blue"}]})
    samples = samples[:CAP]
    return {
        "id": "room-vs-silo",
        "title": "Room vs silos: hill-climbing with three Haiku 5.5 agents",
        "kind": "benchmark", "status": "complete",
        "model": "claude-haiku-5-5 (+ Fable 5.2 advisor in row 4)",
        "completed": "2026-11-15", "source": "docs/benchmark.md",
        "summary": "Does sharing a Playbox room help small agents find a hidden vector? Same code, prompt, boxes and budget; only what each agent can see differs.",
        "headline": [{"k": "POSTED VECTOR p", "v": posted_p}, {"k": "BEST ARM", "v": "Haiku x1800"}],
        "config": {"agents": "3 per trial", "task": "hidden integer vector; score = squared distance + 2x sum gap", "lower_is_better": "true",
                   "conditions": "room | silo (alternating trials)", "boxes": "Docker, one per agent", "significance": "permutation test", "trials_per_condition": "3 to 8"},
        "usage": usage,
        "results": {"title": "Mean best score after all tries (lower is better)", "columns": ["Row", "Task", "Trials", "Room", "Silo", "Classical", "Room vs silo"], "rows": rows},
        "samples": samples,
        "samples_note": "Per-try scores.tsv files are not committed; samples are per-trial finals (row 4) and per-checkpoint means (row 5) from docs/benchmark.md.",
        "timeline": [
            {"kind": "finding", "text": "Sharing helps when the work is split: the room is the only condition that ends with one usable combined answer (posted vector 48.6k vs 108.1k, p = 0.0002).", "by": "row 4"},
            {"kind": "finding", "text": "Sharing did not clearly help when every agent can score everything; a classical random hill-climber beats the model runs on the plain task.", "by": "rows 1-2"},
            {"kind": "finding", "text": "Per dollar, many cheap tries beat an expensive advisor: $0.79 of Haiku tries solved every slice in 4 of 4 trials; $1.18 of advisor calls reached 0 in half.", "by": "row 5"},
            {"kind": "caveat", "text": "Small samples (3 to 8 trials per condition). Read the p-values, not just the means.", "by": "all"},
            {"kind": "caveat", "text": "No room arm in row 5: whether sharing helps a cheap swarm was not run.", "by": "row 5"},
            {"kind": "caveat", "text": "Several room trials were stopped by a time limit at 82 to 89 of 90 tries and counted (rule: at least 80 of 90 and no invalid tries).", "by": "rows 3-4"},
        ],
    }


def f3():
    d = ROOT / "docs" / "f3" / "pilot"
    readme = (d / "README.md").read_text()
    recs = [json.loads(l) for l in (d / "receipts.jsonl").read_text().splitlines() if l.strip()]
    feed = [json.loads(l) for l in (d / "feed.jsonl").read_text().splitlines() if l.strip()]
    genesis = next(r["body"] for r in recs if r["body"]["kind"] == "genesis")
    decs = [r["body"] for r in recs if r["body"]["kind"] == "decision"]
    tb = next(r for h, hd, r in tables(readme) if hd[0] == "seq")
    rows = [[r[0], re.sub(r"\*\*", "", r[1]), re.sub(r"\*\*", "", r[2]), r[3] or "-", r[4] or "-"] for r in tb]
    promoted = sum(1 for x in decs if x["verdict"] == "promote")
    caught = sum(1 for x in decs if x.get("caught_false_promotion"))
    below = sum(1 for x in decs if x.get("reason") == "gain_below_threshold")
    notreach = sum(1 for x in decs if x.get("reason") == "target_not_reached")
    cal = re.search(r"L\* = ([0-9.]+) bpb at (\d+) steps; T0 = ([0-9.]+) s \(median of (\d+) seeds\).*?theta ([0-9.]+).*?eps = ([0-9.]+)", readme, re.S)
    win = re.search(r"(\d{4}-\d\d-\d\d) (\d\d:\d\dZ) to (\d\d:\d\dZ)", readme)
    date = win.group(1)
    notes = {x["candidate"]: x for x in feed}
    samples = []
    for dd, row in zip(decs, rows):
        cand = next((f for f in feed if f["seq"] == dd["seq"]), {})
        full = cand.get("candidate", "")
        src = next(iter((d / "candidates").glob(full + "*.py")), None) if full else None
        steps = [{"label": "submitted", "text": f"{dd['author']} proposed {row[1]}", "c": "blue"},
                 {"label": "dev gate", "text": {True: "passed", False: "failed", None: "not reached"}[dd["dev_gate"]], "c": "ok" if dd["dev_gate"] else "warn"},
                 {"label": "holdout gate", "text": {True: "passed", False: "failed", None: "not reached"}[dd["holdout_gate"]], "c": "ok" if dd["holdout_gate"] else "bad"},
                 {"label": "verdict", "text": f"{dd['verdict']}: {dd['reason']}", "c": "bad" if dd.get("caught_false_promotion") else "warn"}]
        if dd.get("caught_false_promotion"):
            steps.append({"label": "caught", "text": "dev gain did not survive the holdout: a false promotion stopped by the rule", "c": "bad"})
        if src:
            steps.append({"label": "source", "text": f"docs/f3/pilot/candidates/{src.name}", "c": "mut"})
        samples.append({"id": f"decision-{dd['seq']}", "row": str(dd["seq"]), "agent": dd["author"], "kind": "decision", "candidate": row[1],
                        "verdict": f"{dd['verdict']}, {dd['reason']}", "gain": dd["gain"], "holdout": row[4], "receipt": feed[dd["seq"] - 1]["receipt"] if dd["seq"] <= len(feed) else "", "steps": steps})
    usage = {"title": f"Verdicts ({len(decs)} decisions)", "unit": "decisions", "items": [
        {"label": "reject: gain below threshold", "value": below, "c": "warn"},
        {"label": "reject: target not reached", "value": notreach, "c": "mut"},
        {"label": "reject: holdout failed (caught)", "value": caught, "c": "bad"},
        {"label": "promoted", "value": promoted, "c": "ok"}]}
    mem = re.search(r"injected dev memorizer\*\*.*?\| 0\.866 on dev \| ([0-9.]+) to ([0-9.]+) \(limit ([0-9.]+)\)", readme)
    return {
        "id": "f3-h100-pilot",
        "title": "F3 verified hill-climb: H100 pilot (8 scripted stand-ins)",
        "kind": "verified-search", "status": "complete",
        "model": "autoresearch GPT, depth 8, 1x H100 80GB",
        "completed": date, "source": "docs/f3/pilot/README.md",
        "summary": "A promotion rule with a hidden holdout: candidates are accepted only if they gain on dev, clear the threshold and hold up on a holdout. The pilot checks the rule, not a better model.",
        "headline": [{"k": "PROMOTIONS", "v": str(promoted)}, {"k": "CAUGHT", "v": str(caught)}],
        "config": {"node": "f3-pilot-a (new, stopped and verified stopped)", "window": f"{date} {win.group(2)[:-1]}Z to {win.group(3)}".replace("Z Z", "Z"),
                   "spec": "task_spec_gpu.json, pilot setting", "tokens_per_step": "2**19", "L*": f"{cal.group(1)} bpb at {cal.group(2)} steps",
                   "T0": f"{cal.group(3)} s (median of {cal.group(4)} seeds)", "theta": cal.group(5), "eps": cal.group(6), "verify": "VERIFIED, all checks pass"},
        "usage": usage,
        "results": {"title": "Decisions", "columns": ["Seq", "Stand-in", "Verdict", "Gain", "Holdout bpb (3 seeds)"], "rows": rows},
        "samples": samples[:CAP],
        "samples_note": "One sample per signed decision in docs/f3/pilot/receipts.jsonl; candidate sources are in docs/f3/pilot/candidates/.",
        "timeline": [
            {"kind": "finding", "text": f"{promoted} promotions, {below + notreach} rejections for gain below threshold or target, {caught} caught false promotion; verify.txt: VERIFIED.", "by": "result"},
            {"kind": "finding", "text": f"The injected dev memorizer scored 0.866 gain on dev but {mem.group(1)} to {mem.group(2)} bpb on the holdout (limit {mem.group(3)}): the holdout caught it.", "by": "decision 5"},
            {"kind": "caveat", "text": "Mac-tuned stand-ins (Muon steps, learning rates) do not help on the H100 at this step budget; nothing cleared theta = 0.06 (best 0.040).", "by": "calibration"},
            {"kind": "caveat", "text": "About 26 to 27 minutes per decision; candidate 6 took 57 minutes wall because the node was stopped and woken in between.", "by": "timing"},
        ],
    }


def dump(o):
    return json.dumps(o, indent=1, ensure_ascii=False) + "\n"


def main():
    outs = {"room-vs-silo.json": dump(benchmark()), "f3-h100-pilot.json": dump(f3())}
    check = "--check" in sys.argv
    bad = 0
    for name, text in outs.items():
        p = OUT / name
        if check:
            if not p.exists() or p.read_text() != text:
                print(f"STALE: ui/research/{name} differs from its sources; run python3 scripts/gen_research.py", file=sys.stderr)
                bad = 1
        else:
            p.write_text(text)
            print("wrote", p.relative_to(ROOT))
    sys.exit(bad)


if __name__ == "__main__":
    main()
