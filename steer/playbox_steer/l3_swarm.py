"""L3 swarm demo: agents on the SAME model hand each other context state (KV cache) through a shared LMCache server.

Pure standard library, so it runs against any set of vLLM engines that share an LMCache multiprocess server
(tools/l3_lmcache_modal.py starts them). docs/l3-lmcache.md explains the design and the honest limits.

Roles, one vLLM engine each, all with the LMCacheMPConnector and the same model/tokenizer/layout:
  A   the writer. Holds a private "case file" (about 2.7k tokens: one topic, plus unguessable facts). Prefills it once;
      LMCache stores the KV chunks. Nothing else leaves A.
  B*  the readers. Each gets a sealed handle (salt + a broker-held token list), a question, and nothing else. The agent
      never holds the case file; the engine loads A's KV for it instead of computing it.

Salt note: vLLM's `cache_salt` is mixed into the cache identity, so the salt is a capability. A reader without the salt
cannot hit A's chunks (it prefills everything itself), which is also the clean cold control.

Conditions per trial (all temperature 0):
  ingest   A prefills the case file                                  -> prefill tokens A paid, chunks written
  hit      B asks fact/topic questions with the salt                 -> external-KV tokens, TTFT, answer
  cold     B asks the same with a wrong salt (no capability)         -> full prefill, TTFT, answer (speed control)
  nocontext B asks the question with no case file                    -> accuracy without the state (should be near chance)
  wrong    B is handed ANOTHER trial's handle (its case file) and asked this trial's question
                                                                      -> the answer must follow the cache's content
"""
from __future__ import annotations

import json
import random
import re
import time
import urllib.request

from .telepathy_data_xl import FIT_TOPICS_XL

MODEL = "qwen3-1.7b-base"
FACT_KINDS = ["vault code", "pilot callsign", "safehouse street", "courier name", "passphrase", "account number"]
_ADJ = ["quiet", "amber", "hollow", "silver", "rapid", "crimson", "gentle", "frozen", "narrow", "velvet"]
_ANI = ["otter", "falcon", "badger", "heron", "lynx", "marten", "osprey", "wombat", "ibis", "stoat"]
_STREET = ["Alder", "Quarry", "Linden", "Harbor", "Tallow", "Mercer", "Brine", "Cooper"]
_NAME = ["Imre", "Odalys", "Tobias", "Wrenna", "Casimir", "Leocadia", "Fenwick", "Zora"]
_WORD = ["lantern", "granite", "willow", "copper", "meadow", "falcon", "ember", "harbor", "cobalt", "thistle", "juniper", "saddle"]


def fact_value(kind: str, r: random.Random) -> str:
    if kind == "vault code":
        return "".join(r.choice("0123456789") for _ in range(6))
    if kind == "pilot callsign":
        return f"{r.choice(_ADJ).upper()}-{r.choice(_ANI).upper()}-{r.randrange(10, 99)}"
    if kind == "safehouse street":
        return f"{r.choice(_STREET)} Row {r.randrange(2, 98)}"
    if kind == "courier name":
        return f"{r.choice(_NAME)} {r.choice(_NAME)}son"
    if kind == "passphrase":
        return " ".join(r.sample(_WORD, 3))
    return "".join(r.choice("0123456789") for _ in range(4)) + "-" + "".join(r.choice("ABCDEFGHJK") for _ in range(3))


def make_case(seed: int, topics: list[str], passes: int = 4) -> dict:
    """A private case file: the topic's sentences (shuffled, repeated) with unique facts hidden at random lines."""
    r = random.Random(seed)
    topic = r.choice(topics)
    body: list[str] = []
    for _ in range(passes):
        s = list(FIT_TOPICS_XL[topic])
        r.shuffle(s)
        body += s
    facts = {k: fact_value(k, r) for k in FACT_KINDS}
    for k, v in facts.items():
        body.insert(r.randrange(len(body)), f"NOTE: the {k} for this case is {v}.")
    text = "CASE FILE (private to agent A)\n" + " ".join(body) + "\n"
    return {"seed": seed, "topic": topic, "facts": facts, "text": text}


def question(kind: str, topics: list[str], case: dict) -> tuple[str, str]:
    """(prompt tail, expected answer). The tail never contains the case text."""
    if kind == "topic":
        opts = ", ".join(sorted(topics))
        return (f"\nQuestion: which one of these topics is the case file about? Options: {opts}.\nAnswer (one topic):", case["topic"])
    return (f"\nQuestion: what is the {kind} for this case?\nAnswer (value only):", case["facts"][kind])


def norm(s: str) -> str:
    return re.sub(r"[^a-z0-9]+", " ", s.lower()).strip()


def correct(answer: str, expected: str) -> bool:
    return norm(expected) in norm(answer)


# ---- engine client -------------------------------------------------------------------------------------------------

def _post(port: int, payload: dict, stream: bool = False):
    req = urllib.request.Request(f"http://127.0.0.1:{port}/v1/completions", json.dumps(payload).encode(),
                                 {"content-type": "application/json"})
    return urllib.request.urlopen(req, timeout=600)


def complete(port: int, prompt: str, max_tokens: int = 24, salt: str | None = None) -> dict:
    """One streamed completion: text, time to first token, total time, usage."""
    body = {"model": MODEL, "prompt": prompt, "max_tokens": max_tokens, "temperature": 0, "stream": True,
            "stream_options": {"include_usage": True}}
    if salt:
        body["cache_salt"] = salt
    t0 = time.perf_counter()
    ttft, text, usage = None, "", {}
    with _post(port, body) as r:
        for raw in r:
            line = raw.decode().strip()
            if not line.startswith("data:") or line.endswith("[DONE]"):
                continue
            ev = json.loads(line[5:])
            if ev.get("usage"):
                usage = ev["usage"]
            ch = ev.get("choices") or []
            if ch and ch[0].get("text"):
                if ttft is None:
                    ttft = time.perf_counter() - t0
                text += ch[0]["text"]
    return {"text": text, "ttft": ttft if ttft is not None else time.perf_counter() - t0,
            "total": time.perf_counter() - t0, "usage": usage}


def scrape(url: str) -> dict[str, float]:
    """Prometheus text -> {'name{labels}': value}; only the series this demo reads."""
    out: dict[str, float] = {}
    with urllib.request.urlopen(url, timeout=30) as r:
        for line in r.read().decode().splitlines():
            if line.startswith("#") or not line.strip():
                continue
            if "prompt_tokens_by_source" in line or line.startswith("lmcache_mp_"):
                name, _, val = line.rpartition(" ")
                if name.endswith("_created") or "_created{" in name:
                    continue
                try:
                    out[name] = float(val)
                except ValueError:
                    pass
    return out


def delta(after: dict[str, float], before: dict[str, float]) -> dict[str, float]:
    return {k: round(v - before.get(k, 0.0), 3) for k, v in after.items() if v - before.get(k, 0.0)}


def by_source(d: dict[str, float]) -> dict[str, int]:
    """vLLM's prompt tokens split by where they came from: computed here, local prefix cache, or external KV (LMCache)."""
    out = {"local_compute": 0, "local_cache_hit": 0, "external_kv_transfer": 0}
    for k, v in d.items():
        if "prompt_tokens_by_source" in k:
            for s in out:
                if f'source="{s}"' in k:
                    out[s] += int(v)
    return out


def lm_counts(d: dict[str, float]) -> dict[str, float]:
    return {k.replace("lmcache_mp_", ""): v for k, v in d.items() if k.startswith("lmcache_mp_") and ("chunk" in k or "byte" in k)}


class Engine:
    def __init__(self, name: str, port: int):
        self.name, self.port = name, port
        self.url = f"http://127.0.0.1:{port}/metrics"

    def ask(self, prompt: str, salt: str | None, max_tokens: int = 24) -> dict:
        b = scrape(self.url)
        r = complete(self.port, prompt, max_tokens, salt)
        r["prompt_tokens_by_source"] = by_source(delta(scrape(self.url), b))
        r["engine"] = self.name
        return r


# ---- the experiment ------------------------------------------------------------------------------------------------

def run(engines: dict[str, int], lm_url: str, n_trials: int = 24, kv_bytes_per_token: int = 114688, seed0: int = 1000,
        n_chain: int = 8, log=print) -> dict:
    """engines = {'A': port, 'B1': port, 'B2': port, 'B3': port}. Returns a JSON-able result."""
    topics = sorted({t for t in FIT_TOPICS_XL})
    pick = random.Random(seed0).sample(topics, 20)  # the 20 options B chooses among; case topics are drawn from them
    A, B1, B2, B3 = (Engine(k, engines[k]) for k in ("A", "B1", "B2", "B3"))
    trials, cases = [], []
    lm0 = scrape(lm_url)
    for i in range(n_trials):
        case = make_case(seed0 + i, pick)
        cases.append(case)
        salt, bad = f"cap-{seed0 + i}", f"nocap-{seed0 + i}"
        head = case["text"]
        lm_before = scrape(lm_url)
        ing = A.ask(head, salt, max_tokens=1)
        lm_ing = lm_counts(delta(scrape(lm_url), lm_before))
        n_tok = ing["usage"].get("prompt_tokens", 0)
        row = {"i": i, "topic": case["topic"], "prompt_tokens": n_tok, "text_bytes": len(head.encode()),
               "kv_bytes": n_tok * kv_bytes_per_token, "ingest": {"ttft": ing["ttft"], "src": ing["prompt_tokens_by_source"], "lmcache": lm_ing}}
        qs = [("topic", *question("topic", pick, case))] + [(k, *question(k, pick, case)) for k in ("vault code", "pilot callsign")]
        row["q"] = []
        for kind, tail, want in qs:
            reader = {"topic": B1, "vault code": B2, "pilot callsign": B3}[kind]  # fan-out: three readers, three questions
            lm_b = scrape(lm_url)
            hit = reader.ask(head + tail, salt)
            lm_hit = lm_counts(delta(scrape(lm_url), lm_b))
            cold = B1.ask(head + tail, f"{bad}-{kind}")  # fresh salt per question: a cold control must not hit its own writes
            non = B1.ask(tail.lstrip("\n"), None)
            # wrong handle: this trial's question, another trial's cache (needs a previous case in L1)
            wrong = None
            if i > 0:
                j = random.Random(i).randrange(i)
                other = cases[j]
                w = B1.ask(other["text"] + tail, f"cap-{seed0 + j}")
                wrong = {"from": j, "ok_this": correct(w["text"], want),
                         "ok_that": correct(w["text"], question(kind, pick, other)[1]), "text": w["text"][:80],
                         "src": w["prompt_tokens_by_source"]}
            row["q"].append({
                "kind": kind, "want": want, "reader": reader.name,
                "hit": {"ok": correct(hit["text"], want), "ttft": hit["ttft"], "src": hit["prompt_tokens_by_source"], "lmcache": lm_hit, "text": hit["text"][:80]},
                "cold": {"ok": correct(cold["text"], want), "ttft": cold["ttft"], "src": cold["prompt_tokens_by_source"], "text": cold["text"][:80]},
                "nocontext": {"ok": correct(non["text"], want), "text": non["text"][:80]},
                "wrong": wrong})
        trials.append(row)
        log(json.dumps({"i": i, "topic": case["topic"], "ok": [q["hit"]["ok"] for q in row["q"]], "tok": n_tok}))
    chain = chain_demo(A, B1, B2, lm_url, pick, n_chain, seed0 + 5000, kv_bytes_per_token, log)
    scale = scale_demo(A, B1, pick, seed0 + 9000, kv_bytes_per_token, log)
    edit = edit_demo(A, B1, pick, 8, seed0 + 7000, log)
    return {"trials": trials, "chain": chain, "scale": scale, "edit": edit, "summary": summarize(trials, chain, edit),
            "lm_final": lm_counts(delta(scrape(lm_url), lm0)), "kv_bytes_per_token": kv_bytes_per_token}


def chain_demo(A, B, C, lm_url, pick, n, seed0, kvb, log) -> list[dict]:
    """A blackboard in KV: A prefills the case; B appends private notes (reading A's KV, writing new chunks); C asks about a
    NOTE fact and hits both A's and B's chunks. Nothing but salt-and-handle moves between agents."""
    out = []
    for i in range(n):
        case = make_case(seed0 + i, pick)
        r = random.Random(seed0 + i)
        nv = {k: fact_value(k, r) for k in ("courier name", "account number")}
        notes = "".join(f"B-NOTE: the {k} is {v}.\n" for k, v in nv.items()) + " ".join(
            f"B-OBS: {s}" for s in random.Random(i).sample(FIT_TOPICS_XL[case["topic"]], 18)) + "\n"
        salt = f"chain-{seed0 + i}"
        a = A.ask(case["text"], salt, max_tokens=1)
        b = B.ask(case["text"] + notes, salt, max_tokens=1)
        tail, want = f"\nQuestion: what is the courier name in the B-NOTE?\nAnswer (value only):", nv["courier name"]
        c = C.ask(case["text"] + notes + tail, salt)
        cold = C.ask(case["text"] + notes + tail, f"chaincold-{seed0 + i}")
        out.append({"i": i, "A_src": a["prompt_tokens_by_source"], "B_src": b["prompt_tokens_by_source"],
                    "C_src": c["prompt_tokens_by_source"], "C_ok": correct(c["text"], want), "cold_ok": correct(cold["text"], want),
                    "C_ttft": c["ttft"], "cold_ttft": cold["ttft"], "tokens": c["usage"].get("prompt_tokens", 0)})
        log(json.dumps({"chain": i, "C_ok": out[-1]["C_ok"], "C_src": c["prompt_tokens_by_source"]}))
    return out


def edit_demo(A, B, pick, n, seed0, log) -> list[dict]:
    """Is the answer carried by stale state? A caches the case; B is handed the same case with ONE fact edited mid-file. The
    chunk chain is content-addressed, so reuse must stop at the edit and the answer must follow the new text, never the old."""
    out, kind = [], "vault code"
    for i in range(n):
        case = make_case(seed0 + i, pick)
        salt = f"edit-{seed0 + i}"
        A.ask(case["text"], salt, max_tokens=1)
        old = case["facts"][kind]
        new = fact_value(kind, random.Random(seed0 + i + 77))
        text2 = case["text"].replace(f"the {kind} for this case is {old}.", f"the {kind} for this case is {new}.")
        tail, _ = question(kind, pick, case)
        base = B.ask(case["text"] + tail, salt)
        ed = B.ask(text2 + tail, salt)
        n_tok = base["usage"].get("prompt_tokens", 0)
        frac = text2.index(f"is {new}.") / len(text2)
        out.append({"i": i, "edit_at_token_est": int(frac * n_tok), "tokens": n_tok, "base_src": base["prompt_tokens_by_source"],
                    "edit_src": ed["prompt_tokens_by_source"], "base_ok": correct(base["text"], old),
                    "follows_new": correct(ed["text"], new), "stale_old": correct(ed["text"], old)})
        log(json.dumps({"edit": out[-1]}))
    return out


def median(x):
    x = sorted(x)
    return x[len(x) // 2] if x else None


def scale_demo(A, B, pick, seed0, kvb, log, passes=(4, 10, 20, 40), reps=5) -> list[dict]:
    """When does loading A's KV beat recomputing it? Same question, growing case file; median TTFT of `reps` hits vs `reps` cold."""
    out = []
    for k, p in enumerate(passes):
        try:
            case = make_case(seed0 + k, pick, passes=p)
            salt = f"scale-{seed0 + k}"
            a = A.ask(case["text"], salt, max_tokens=1)
            tail, want = question("vault code", pick, case)
            hits = [B.ask(case["text"] + tail, salt) for _ in range(reps)]
            colds = [B.ask(case["text"] + tail, f"scalecold-{seed0 + k}-{r}") for r in range(reps)]
            n = a["usage"].get("prompt_tokens", 0)
            row = {"passes": p, "tokens": n, "kv_bytes": n * kvb, "text_bytes": len(case["text"].encode()),
                   "A_ingest_ttft": a["ttft"], "hit_ttft_median": median(h["ttft"] for h in hits),
                   "cold_ttft_median": median(c["ttft"] for c in colds), "hit_ok": mean(correct(h["text"], want) for h in hits),
                   "cold_ok": mean(correct(c["text"], want) for c in colds), "hit_src": hits[-1]["prompt_tokens_by_source"],
                   "cold_src": colds[-1]["prompt_tokens_by_source"]}
        except Exception as e:  # the largest size may not fit the engine's KV pool; record it and go on
            row = {"passes": p, "error": f"{type(e).__name__}: {e}"[:200]}
        out.append(row)
        log(json.dumps({"scale": row}))
    return out


def mean(x):
    x = list(x)
    return sum(x) / len(x) if x else None


def summarize(trials: list[dict], chain: list[dict], edit: list[dict] | None = None) -> dict:
    qs = [q for t in trials for q in t["q"]]
    by = {}
    for kind in ("topic", "vault code", "pilot callsign"):
        k = [q for q in qs if q["kind"] == kind]
        by[kind] = {"n": len(k), "hit_acc": mean(q["hit"]["ok"] for q in k), "cold_acc": mean(q["cold"]["ok"] for q in k),
                    "nocontext_acc": mean(q["nocontext"]["ok"] for q in k)}
    w = [q["wrong"] for q in qs if q["wrong"]]
    ext = [q["hit"]["src"]["external_kv_transfer"] for q in qs]
    comp = [q["hit"]["src"]["local_compute"] for q in qs]
    cold_comp = [q["cold"]["src"]["local_compute"] for q in qs]
    toks = [t["prompt_tokens"] for t in trials]
    return {
        "per_question": by,
        "wrong_handle": {"n": len(w), "answer_matches_this_question": mean(x["ok_this"] for x in w),
                         "answer_matches_the_cached_case": mean(x["ok_that"] for x in w)},
        "tokens": {"case_tokens_mean": mean(toks), "hit_external_mean": mean(ext), "hit_local_compute_mean": mean(comp),
                   "cold_local_compute_mean": mean(cold_comp)},
        "ttft_s": {"hit_mean": mean(q["hit"]["ttft"] for q in qs), "cold_mean": mean(q["cold"]["ttft"] for q in qs),
                   "ingest_mean": mean(t["ingest"]["ttft"] for t in trials)},
        "bytes": {"text_mean": mean(t["text_bytes"] for t in trials), "kv_mean": mean(t["kv_bytes"] for t in trials)},
        "chain": {"n": len(chain), "C_acc": mean(c["C_ok"] for c in chain), "cold_acc": mean(c["cold_ok"] for c in chain),
                  "C_external_mean": mean(c["C_src"]["external_kv_transfer"] for c in chain),
                  "C_local_compute_mean": mean(c["C_src"]["local_compute"] for c in chain),
                  "C_ttft_mean": mean(c["C_ttft"] for c in chain), "cold_ttft_mean": mean(c["cold_ttft"] for c in chain)},
        "edit": {"n": len(edit or []), "follows_new": mean(e["follows_new"] for e in edit or []),
                 "stale_old": mean(e["stale_old"] for e in edit or []),
                 "base_external_mean": mean(e["base_src"]["external_kv_transfer"] for e in edit or []),
                 "edit_external_mean": mean(e["edit_src"]["external_kv_transfer"] for e in edit or []),
                 "edit_at_token_est_mean": mean(e["edit_at_token_est"] for e in edit or [])},
    }
