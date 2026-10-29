"""The M8 usability check: do people recognise the room's topics from the rendered clusters?

Target (PRD): 3 of 4 raters agree with the cluster labels. `make_packet` builds a multiple-choice
sheet (for each cluster, a sample of its events and four candidate labels: the shown label and three
taken from other clusters, in shuffled order); `write_form` renders it as a page a rater fills in;
`score` takes each rater's picks and reports agreement. `purity` is a machine proxy against known
topics, useful for comparing embedders but not a substitute for raters.
"""

import collections
import json
import random

STOP = {"this", "that", "with", "from", "have", "your", "will", "what", "when", "then", "into", "there", "about"}


def top_words(events, labels, j, n=4):
    c = collections.Counter(w for e, l in zip(events, labels) if l == j for w in e.text.lower().split() if len(w) > 3 and w not in STOP)
    return [w for w, _ in c.most_common(n)]


def make_packet(events, labels, samples=8, seed=0):
    rng = random.Random(seed)
    ks = sorted(set(int(l) for l in labels))
    names = {j: ", ".join(top_words(events, labels, j)) for j in ks}
    packet = []
    for j in ks:
        idx = [i for i, l in enumerate(labels) if l == j]
        shown = [events[i].text for i in rng.sample(idx, min(samples, len(idx)))]
        decoys = rng.sample([names[o] for o in ks if o != j], min(3, len(ks) - 1))
        options = [names[j]] + decoys
        rng.shuffle(options)
        packet.append({"cluster": j, "events": shown, "options": options, "answer": options.index(names[j])})
    return packet


def score(packet, rater_answers, need=3):
    """rater_answers: one list of picks (option index per cluster) per rater."""
    out = []
    for c, item in enumerate(packet):
        right = sum(1 for r in rater_answers if r[c] == item["answer"])
        out.append({"cluster": item["cluster"], "agree": right, "raters": len(rater_answers), "pass": right >= need})
    return {"clusters": out, "passed": all(c["pass"] for c in out)}


def purity(labels, truth):
    by = collections.defaultdict(collections.Counter)
    for l, t in zip(labels, truth):
        by[int(l)][t] += 1
    return sum(max(c.values()) for c in by.values()) / len(labels)


def write_form(path, packet):
    """A rater page; answers are shown as JSON to send back (the answer key stays out of the page)."""
    blind = [{k: v for k, v in p.items() if k != "answer"} for p in packet]
    html = """<!doctype html><meta charset=utf-8><title>Cluster labels</title>
<body style="font:15px system-ui;max-width:720px;margin:24px auto"><h2>Which label fits each group?</h2>
<div id=f></div><button onclick="done()">Finish</button><pre id=o></pre>
<script>const P=__P__;f.innerHTML=P.map((p,c)=>`<h3>Group ${c+1}</h3><ul>${p.events.map(e=>`<li>${e.replace(/</g,'&lt;')}</li>`).join('')}</ul>`+
p.options.map((o,i)=>`<label><input type=radio name=g${c} value=${i}> ${o}</label><br>`).join('')).join('');
function done(){o.textContent=JSON.stringify(P.map((_,c)=>+(document.querySelector(`input[name=g${c}]:checked`)||{value:-1}).value))}</script>"""
    with open(path, "w") as f:
        f.write(html.replace("__P__", json.dumps(blind).replace("</", "<\\/")))
