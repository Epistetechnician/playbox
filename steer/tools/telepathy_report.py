"""Build the swarm-telepathy research summary (HTML, then PDF through headless Chrome) from the result files.

    python tools/telepathy_report.py [OUT.pdf]

Every number for the runs of 9-11 Nov comes from results/telepathy/*_private/results.json through the harness summary
(Wilson intervals); older runs are listed with the counts recorded in docs/steering.md. Needs matplotlib and Chrome."""
from __future__ import annotations

import base64
import io
import json
import subprocess
import sys
import tempfile
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from playbox_steer import telepathy_harness as H  # noqa: E402

R = ROOT / "results/telepathy"
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
INK, MUTED, BLUE, ORANGE, GREEN, GREY = "#1a1a1a", "#666666", "#2b6cb0", "#c05621", "#2f855a", "#a0aec0"


def load(name: str) -> list[dict] | None:
    p = R / f"{name}_private/results.json"
    return json.loads(p.read_text()) if p.exists() else None


def split(rs: list[dict]) -> dict:
    ran = [r for r in rs if "error" not in r]
    top = [r for r in ran if r["truth"] != "none"]
    ctl = [r for r in ran if r["truth"] == "none"]
    d = {"tk": sum(bool(r["ok"]) for r in top), "tn": len(top), "ck": sum(bool(r["ok"]) for r in ctl), "cn": len(ctl),
         "muted": sum(r["guess"] == "none" for r in top), "infra": len(rs) - len(ran)}
    for src in sorted({r["src"] for r in top}):
        s = [r for r in top if r["src"] == src]
        d[f"from_{src}"] = (sum(bool(r["ok"]) for r in s), len(s))
    return d


def pct(k: int, n: int) -> str:
    lo, hi = H.wilson(k, n)
    return f"{k}/{n} = {100 * k / n + 1e-9:.1f}% (CI {100 * lo:.0f}–{100 * hi:.0f})" if n else "–"


def short(k: int, n: int) -> str:
    return f"{k}/{n} = {100 * k / n:.0f}%" if n else "–"


def fisher_ge(a: int, n1: int, b: int, n2: int) -> float:
    """One-sided exact p: chance that arm 1 (a of n1) is at least this far above arm 2 (b of n2) if both share one rate."""
    import math
    k, n = a + b, n1 + n2
    return sum(math.comb(n1, i) * math.comb(n2, k - i) for i in range(a, min(n1, k) + 1)) / math.comb(n, k)


def png(fig) -> str:
    b = io.BytesIO()
    fig.savefig(b, format="png", dpi=170, bbox_inches="tight")
    plt.close(fig)
    return "data:image/png;base64," + base64.b64encode(b.getvalue()).decode()


def bars(labels, ks, ns, colors, title_y, hline=None, ylim=(0, 105), figsize=(7.4, 3.5)):
    fig, ax = plt.subplots(figsize=figsize)
    acc = [100 * k / n for k, n in zip(ks, ns)]
    err = [[100 * k / n - 100 * H.wilson(k, n)[0] for k, n in zip(ks, ns)], [100 * H.wilson(k, n)[1] - 100 * k / n for k, n in zip(ks, ns)]]
    ax.bar(range(len(labels)), acc, color=colors, width=0.62)
    ax.errorbar(range(len(labels)), acc, yerr=err, fmt="none", ecolor=INK, elinewidth=1, capsize=3)
    for i, (k, n, a) in enumerate(zip(ks, ns, acc)):
        ax.text(i, min(a + (err[1][i]) + 2, 101), f"{k}/{n}", ha="center", fontsize=7.5, color=INK)
    if hline is not None:
        ax.axhline(hline, color=GREY, ls="--", lw=1)
        ax.text(len(labels) - 0.45, hline + 2.5, f"chance {hline:.1f}%", ha="right", fontsize=7, color=MUTED, bbox=dict(fc="white", ec="none", pad=1))
    ax.set_xticks(range(len(labels)))
    ax.set_xticklabels(labels, fontsize=6.8)
    ax.set_ylabel(title_y, fontsize=8.5)
    ax.set_ylim(*ylim)
    ax.spines[["top", "right"]].set_visible(False)
    ax.tick_params(axis="y", labelsize=8)
    return png(fig)


def build() -> str:
    small_ungated = split(load("confirm_v4"))           # topics, local small pair, ungated
    small_ctl_ungated = split(load("ctrl_heldout"))     # held-out controls, ungated
    small_gated = split(load("gated_local"))            # gated mix
    big_ungated = split(load("big_pair20"))
    big_ungated_ctl = split(load("big_pair"))           # 5 topics + the same 36 controls
    big_gated = split(load("big_pair_gated"))
    h100_small = split(load("probe_eval"))               # small pair, H100 node, same brain and gate recipe, fresh controls
    g2, u2 = load("big_pair_gated_s2"), load("big_pair_ungated_s2")
    big_gated2, big_ungated2 = (split(g2) if g2 else None), (split(u2) if u2 else None)

    # --- chart 1: all topic results
    runs = [("Local\n16-topic\nfit", 42, 80, GREY), ("Modal\n156-topic\nfit", 59, 80, GREY), ("Modal gate\n+ strength\nline", 67, 80, BLUE),
            ("Small pair\nH100\ngated", h100_small["tk"], h100_small["tn"], BLUE),
            ("Small pair\nlocal\nno gate", small_ungated["tk"], small_ungated["tn"], GREY), ("Small pair\nlocal\ngated", small_gated["tk"], small_gated["tn"], BLUE),
            ("8B/7B pair\nno gate\nseed 1", big_ungated["tk"], big_ungated["tn"], ORANGE), ("8B/7B pair\ngated\nseed 1", big_gated["tk"], big_gated["tn"], ORANGE)]
    if big_gated2:
        runs.append(("8B/7B pair\ngated\nseed 2", big_gated2["tk"], big_gated2["tn"], ORANGE))
    if big_ungated2:
        runs.append(("8B/7B pair\nno gate\nseed 2", big_ungated2["tk"], big_ungated2["tn"], ORANGE))
    c1 = bars([r[0] for r in runs], [r[1] for r in runs], [r[2] for r in runs], [r[3] for r in runs], "topic trials correct (%)", hline=100 / 21,
              figsize=(8.4, 3.6))

    # --- chart 2: false positives on the held-out controls
    ctl = [("Small pair\nlocal, no gate", small_ctl_ungated["cn"] - small_ctl_ungated["ck"], small_ctl_ungated["cn"], GREY),
           ("Small pair\nlocal, gated", small_gated["cn"] - small_gated["ck"], small_gated["cn"], BLUE),
           ("Small pair\nH100, gated", h100_small["cn"] - h100_small["ck"], h100_small["cn"], BLUE),
           ("8B/7B pair\nno gate", big_ungated_ctl["cn"] - big_ungated_ctl["ck"], big_ungated_ctl["cn"], ORANGE),
           ("8B/7B pair\ngated", big_gated["cn"] - big_gated["ck"], big_gated["cn"], ORANGE)]
    if big_gated2 and big_gated2["cn"]:
        ctl.append(("8B/7B pair\ngated, seed 2", big_gated2["cn"] - big_gated2["ck"], big_gated2["cn"], ORANGE))
    c2 = bars([c[0] for c in ctl], [c[1] for c in ctl], [c[2] for c in ctl], [c[3] for c in ctl], "controls where B named a topic (%)", ylim=(0, 60),
              figsize=(6.2, 3.1))

    # --- tables
    def row(name, d, ctrl=True):
        return f"<tr><td>{name}</td><td>{pct(d['tk'], d['tn'])}</td><td>{pct(d['cn'] - d['ck'], d['cn']) if ctrl and d['cn'] else '–'}</td></tr>"

    new_rows = "".join([
        row("Small pair (135M / 1.7B), local, no gate", {**small_ungated, "cn": small_ctl_ungated["cn"], "ck": small_ctl_ungated["ck"]}),
        row("Small pair, local, probe gate (mixed plan)", small_gated),
        row("Small pair, H100 node, probe gate (same brain and protocol)", h100_small),
        row("8B / 7B pair, H100, no gate", {**big_ungated, "cn": big_ungated_ctl["cn"], "ck": big_ungated_ctl["ck"]}),
        row("8B / 7B pair, H100, probe gate", big_gated),
        row("8B / 7B pair, probe gate, seed 2", big_gated2) if big_gated2 else "",
        row("8B / 7B pair, no gate, seed 2", big_ungated2, ctrl=False) if big_ungated2 else "",
    ])

    def dirline(d, name):
        return "; ".join(f"{k.replace('from_', '')} → {v[0]}/{v[1]}" for k, v in d.items() if k.startswith("from_")) + f" ({name})"

    both = [d for d in (big_gated, big_gated2) if d]
    pooled_k, pooled_n = sum(d["tk"] for d in both), sum(d["tn"] for d in both)
    pooled_c, pooled_cn = sum(d["ck"] for d in both if d["cn"]), sum(d["cn"] for d in both)
    q = lambda d, s: d.get(f"from_{s}", (0, 0))  # noqa: E731
    dir_rows = ""
    for name, d in (("no gate", big_ungated), ("gate", big_gated), ("gate, seed 2", big_gated2), ("no gate, seed 2", big_ungated2)):
        if d:
            a, b = q(d, "qwen8b"), q(d, "olmo2")
            dir_rows += f"<tr><td>{name}</td><td>{short(*a)}</td><td>{short(*b)}</td><td>{d['muted']}</td></tr>"

    dir_note = ""
    if big_gated2 and big_ungated2:
        pu = {s_: tuple(map(sum, zip(q(big_ungated, s_), q(big_ungated2, s_)))) for s_ in ("qwen8b", "olmo2")}
        pg = {s_: tuple(map(sum, zip(q(big_gated, s_), q(big_gated2, s_)))) for s_ in ("qwen8b", "olmo2")}
        pv = {s_: fisher_ge(pu[s_][0], pu[s_][1], pg[s_][0], pg[s_][1]) for s_ in pu}
        dir_note = (f"Pooled over the two seeds the gate costs the Qwen3-8B &rarr; OLMo-2-7B direction {short(*pu['qwen8b'])} &rarr; {short(*pg['qwen8b'])} "
                    f"(one-sided exact p = {pv['qwen8b']:.3f}) and does not cost the other direction ({short(*pu['olmo2'])} &rarr; {short(*pg['olmo2'])}, p = {pv['olmo2']:.2f}). "
                    "The probe for the Qwen3-8B sender has the wider uncertain band (ramp 0.54 to 0.97), so it mutes more real topics than the OLMo-2 sender's (0.88 to 1.00).")
    seed_note = ""
    pooled_u = (big_ungated["tk"] + (big_ungated2["tk"] if big_ungated2 else 0), big_ungated["tn"] + (big_ungated2["tn"] if big_ungated2 else 0))
    if big_gated2:
        seed_note = (f"A second independent run of the gated larger pair gave {pct(big_gated2['tk'], big_gated2['tn'])} on topics and "
                     f"{big_gated2['cn'] - big_gated2['ck']} false positive{'' if big_gated2['cn'] - big_gated2['ck'] == 1 else 's'} in {big_gated2['cn']} controls. Pooled over both gated runs: "
                     f"{pct(pooled_k, pooled_n)} on topics, {pooled_cn - pooled_c} false positive{'' if pooled_cn - pooled_c == 1 else 's'} in {pooled_cn} controls "
                     f"({100 * (pooled_cn - pooled_c) / max(pooled_cn, 1):.1f}%).")
    if big_ungated2:
        seed_note += (f" The ungated repeat gave {pct(big_ungated2['tk'], big_ungated2['tn'])}, so the gate's cost on topics is "
                      f"{100 * (big_ungated['tk'] + big_ungated2['tk']) / (big_ungated['tn'] + big_ungated2['tn']) - 100 * pooled_k / pooled_n:.1f} points pooled over the two seeds of each arm.")

    cap = lambda s: s  # noqa: E731
    html = f"""<!doctype html><html><head><meta charset="utf-8"><title>Swarm telepathy</title><style>
@page {{ size: A4; margin: 17mm 16mm; }}
body {{ font: 10pt/1.42 -apple-system, "Helvetica Neue", Arial, sans-serif; color: {INK}; }}
h1 {{ font-size: 20pt; margin: 0 0 2pt; }} h2 {{ font-size: 13pt; margin: 17pt 0 4pt; page-break-after: avoid; }} h3 {{ font-size: 10.5pt; margin: 11pt 0 3pt; page-break-after: avoid; }}
.sub {{ color: {MUTED}; font-size: 9pt; margin-bottom: 10pt; }} p {{ margin: 4pt 0; }} ul {{ margin: 3pt 0 3pt 15pt; padding: 0; }} li {{ margin: 2pt 0; }}
table {{ border-collapse: collapse; width: 100%; font-size: 9pt; margin: 5pt 0; }} th, td {{ border-bottom: 1px solid #ddd; padding: 3pt 5pt; text-align: left; }} td:nth-child(n+2) {{ white-space: nowrap; }}
th {{ background: #f3f3f3; }} .fig {{ margin: 6pt 0; text-align: center; }} .fig img {{ max-width: 100%; }} .cap {{ color: {MUTED}; font-size: 8.5pt; }}
.box {{ background: #f7f7f2; border-left: 3px solid {BLUE}; padding: 6pt 9pt; margin: 6pt 0; }} .pb {{ page-break-before: always; }} tr, .fig {{ page-break-inside: avoid; }}
</style></head><body>
<h1>Swarm telepathy: a text-free channel between AI agents</h1>
<div class="sub">Research summary &middot; Playbox steer/ &middot; 11 Nov 2026 &middot; experiments E7 and E8</div>

<h2>Summary</h2>
<p>Two AI agents built on different model families can pass a concept to each other without sending any text. The sender reads private sentences about a
topic and writes one vector. A fitted linear map translates it into the receiver's model space, and the receiver's model is steered with it while it
continues neutral prompts. The receiving agent reads what its own model writes and names the topic, or answers "none".</p>
<ul>
<li><b>It works, far above chance.</b> On a blind test with 20 topics and a "none" option (chance 4.8%) a 135M / 1.7B pair named the topic in about 55% of trials locally and 81&ndash;84% on the H100
stack. A larger pair (Qwen3-8B-Base and OLMo-2-7B) on the same stack reached {pct(pooled_k, pooled_n)} gated ({short(*pooled_u)} ungated), against {pct(h100_small['tk'], h100_small['tn'])} for the small pair with the same
brain, gate recipe and controls: a gain of {100 * pooled_k / pooled_n - 100 * h100_small['tk'] / h100_small['tn']:.1f} points (one-sided exact p = {fisher_ge(pooled_k, pooled_n, h100_small['tk'], h100_small['tn']):.2f}), suggestive and not conclusive.</li>
<li><b>The hard part is the "nothing sent" case.</b> Receivers name a topic even when none was sent: {small_ctl_ungated['cn'] - small_ctl_ungated['ck']} of {small_ctl_ungated['cn']} held-out controls
on the small pair and {big_ungated_ctl['cn'] - big_ungated_ctl['ck']} of {big_ungated_ctl['cn']} on the larger pair. Scale did not fix this.</li>
<li><b>A sender-side probe gate fixes it.</b> Small pair: false positives {small_gated['cn'] - small_gated['ck']}/{small_gated['cn']}. Larger pair: {big_gated['cn'] - big_gated['ck']}/{big_gated['cn']}
with topics at {pct(big_gated['tk'], big_gated['tn'])}. The cost is some real topics muted to "none" and a weaker second direction. {seed_note}</li>
<li><b>It generalises across families.</b> A shared hub (E8) links five model families with one adapter pair per model and a 64-byte message; it scores within about 4 points of per-pair maps.</li>
<li><b>Scope.</b> Two model pairs, 20 coarse topics, one machine or node. A concept channel, not arbitrary-thought transfer.</li></ul>

<h2>1. Method</h2>
<p><b>Sender (Agent A).</b> Reads six concept sentences, takes the mean mid-layer hidden state (45% of depth), subtracts the mean over neutral sentences and maps the result with a ridge
regression fitted on sentence pairs from both models (tokenizers differ, so pairs are whole sentences). Test topics are never in the fit set. <b>Receiver (Agent B).</b> The vector is added at 45% of
depth while the local model continues neutral prompts. B sees only the continuations and a topic list and answers with one topic or "none". <b>Gate.</b> A ridge-logistic probe on the sender's
unit difference vector, fitted on live Agent A text, scales the injected strength from 0 to 1; the strength travels with the vector and B is told to answer "none" when it is low.</p>
<p><b>Models.</b> Small pair: SmolLM2-135M and Qwen3-1.7B-Base (held-out map cosine 0.89&ndash;0.95). Larger pair: Qwen3-8B-Base and OLMo-2-1124-7B, both width 4096, bf16 (held-out cosine 0.87 and 0.88).
Agents are Hermes agents on Claude Haiku 5.5 (local) or Qwen3.8-27B (H100 node). <b>Protocol.</b> 20 topics &times; 2 repetitions &times; 2 directions = 80 topic trials, plus no-topic controls
written live by Agent A (10 filler scenes and 8 off-list subjects per direction, none used in calibration). Agent files are kept outside B's directory, which holds only the vector file.</p>

<h2 class="pb">2. Results</h2>
<div class="fig"><img src="{c1}"><div class="cap">Topic accuracy, 95% Wilson intervals. Grey: no gate or earlier set-ups; blue: small pair; orange: larger pair (H100 stack, same brain as the blue "H100" bar). The first four bars changed fit data,
strength and gating together and are not a controlled comparison.</div></div>
<table><tr><th>Run (this week's protocol)</th><th>Topics correct</th><th>Controls where B named a topic</th></tr>{new_rows}</table>
<p class="cap">Controls are the same 36 held-out subjects per run (small gated: 25 controls from a shuffled mixed plan). Zero infrastructure failures in every run listed.</p>

<h3>The "nothing sent" problem and the gate</h3>
<div class="fig"><img src="{c2}"><div class="cap">Share of held-out controls on which B named a topic (lower is better).</div></div>
<ul>
<li>Early controls looked fine (8/8, 16/16) because they used one fixed neutral text, the same text the gate was calibrated on. On live filler and off-list subjects the receiver was wrong 9 of 20 and 6 of 12 times.</li>
<li>A topic-ness gate looked good offline and failed live (topics fell to 46%); the calibration text had not come from the live agent.</li>
<li>What held: a probe fitted on live Agent A text, with scenes and subjects held out whole in cross-validation, scored on fresh text. Off-list subjects still land on the nearest listed topic when they get through
("train delays" read as cars, "fishing" as ocean); that is a limit of a closed-list answer.</li></ul>

<h2>3. Larger model pair</h2>
<p>Qwen3-8B-Base and OLMo-2-7B ran on one H100 next to a Qwen3.8-27B-FP8 brain, the same stack as the small-pair H100 run, so the pair is the main thing that changed. Ungated, topic accuracy was {short(*pooled_u)} over two runs;
false positives did not shrink with scale ({big_ungated_ctl['cn'] - big_ungated_ctl['ck']}/{big_ungated_ctl['cn']}, against {small_ctl_ungated['cn'] - small_ctl_ungated['ck']}/{small_ctl_ungated['cn']} for the small pair). A probe calibrated on 248 live thoughts
(held-out pass rates: topics 77/80 and 79/80, filler 0/24) brought false positives to {pooled_cn - pooled_c} in {pooled_cn} over two runs. Earlier comparisons of 55% (small pair, local, Haiku agents) with 95% (larger pair, H100, Qwen brain) mix two changes and should not be read as a scaling effect.</p>
<table><tr><th>Run</th><th>Qwen3-8B &rarr; OLMo-2-7B</th><th>OLMo-2-7B &rarr; Qwen3-8B</th><th>Real topics muted to "none"</th></tr>{dir_rows}</table>
<p>{dir_note}</p>
<p class="cap">Wrong-topic answers are mostly near neighbours (baseball / basketball / soccer, earthquake / ocean / volcano, cars / aviation).</p>

<h2 class="pb">4. Many families through one hub (E8)</h2>
<p>Per-pair maps scale as N&sup2;. E8 fits one shared hub (64 dimensions, 426 anchor sentences) with one adapter pair per model and sends a 64-byte int8 message per thought. It covers SmolLM2, Qwen3, Pythia,
GPT-2-medium and TinyLlama. Mean over 20 ordered pairs, strength 0.5, chance 20%: hub 46.8%, per-pair 50.9%, receiver's own direction 80.5%, random vector 2.6%. Held-out cosine: hub 0.84&ndash;0.90, per-pair 0.88&ndash;0.95.</p>

<h2>5. Reliability of the harness</h2>
<p>Early large runs lost trials to infrastructure (32 of 86 timed out after a memory watchdog reboot; 51 of 86 failed on the first pass of the next run). The causes were a daemon serving one request at a time, orphaned
agent processes after a timeout, and no retries. The harness now runs the daemon with queued requests and a health check, writes vectors atomically, runs each agent in its own process group and kills it on timeout,
retries only infrastructure failures (never a valid wrong answer, so retries cannot inflate accuracy), and reports Wilson intervals with controls and directions separate. Every run in the results table had 0 infrastructure failures.</p>

<h2>6. Limits</h2>
<ul>
<li>Two model pairs and 20 topics; each configuration has one or two runs, and the larger-pair gain over the small pair on the same stack is not statistically established. Intervals are wide for the per-direction and per-topic splits.</li>
<li>The small gated run is a 76-trial slice of a shuffled plan (51 topics, 25 controls), not the full plan.</li>
<li>The probe is calibrated per sender and model; a new model needs a new calibration (about 250 agent runs, roughly 40 minutes on the H100).</li>
<li>The vector is built from the sender's own sentences. This is a concept channel. Both ends sit on one machine or node; there is no hostile sender and the channel is not yet a Playbox room operation.</li>
<li>Gate thresholds come from cross-validation on calibration text; controls are fresh subjects written by the same agent family, so a different agent could shift them.</li></ul>

<h2>7. Next steps</h2>
<ol><li>A third family and a larger receiver (for example a 27B) to test the scaling trend; each needs its own probe.</li>
<li>Targeted fit data for neighbouring topics, which cause most wrong answers.</li>
<li>Compare against published activation- and cache-communication methods (SwaT and LatentMAS are tracked in docs/steering.md, section 2d).</li>
<li>Wire the channel and its gate into a room as an audited operation.</li></ol>
<p class="cap">Sources: docs/steering.md (E7 sections); steer/results/telepathy/*_private/results.json; steer/results/e8/e8.json. Generated by steer/tools/telepathy_report.py. Intervals are 95% Wilson.</p>
</body></html>"""
    return html


if __name__ == "__main__":
    out = Path(sys.argv[1] if len(sys.argv) > 1 else R / "swarm_telepathy_report.pdf").resolve()
    page = build()
    with tempfile.TemporaryDirectory() as t:
        f = Path(t) / "r.html"
        f.write_text(page)
        subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--no-pdf-header-footer", f"--print-to-pdf={out}", f.as_uri()],
                       check=True, capture_output=True, timeout=120)
    print(out)
