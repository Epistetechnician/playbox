"""Calibrate a topic-ness score for the salience gate: how well a sender's difference vector lines up with any known topic
direction (max cosine to the fit-topic centroids), versus filler and out-of-list text. Run on the node next to the brain:

    BRAIN_API_KEY=... BRAIN_URL=http://127.0.0.1:8000 python tools/telepathy_topicness.py > results/telepathy/topicness.json

Calibration set (thresholds are read off this): 20 evaluation topics x 3 fresh agent-style samples, the neutral triple, and 24
fresh filler sets. Held-out set (never used for thresholds): playbox_steer/telepathy_controls.json (10 filler + 6 off-list sets)."""
import concurrent.futures as cf, json, os, re, sys, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT)); sys.path.insert(0, str(ROOT / "tools"))
os.environ.setdefault("PLAYBOX_DEVICE", "cuda")
import torch
from huggingface_hub import snapshot_download
import telepathy_agentfit_gen as G
from playbox_steer import telepathy as T
from playbox_steer.telepathy_data import NEUTRAL
from playbox_steer.telepathy_data_xl import FIT_TOPICS_XL

EVAL = "aviation,baseball,basketball,cars,chess,classical-music,cryptocurrency,dinosaurs,earthquake,jazz,law,medicine,ocean,photography,poker,politics,religion,soccer,space,volcano".split(",")
FILLER = ["a bored person making small talk", "lorem-ipsum style placeholder prose", "a vague corporate email", "statements that say nothing", "an awkward elevator chat",
          "a generic thank-you note", "a plain description of an empty room", "filler words and hedges", "a polite voicemail greeting", "a rambling non-answer",
          "a weather-neutral status update", "the instructions on a form", "someone confirming an appointment", "a list of mundane observations", "an out-of-office reply",
          "a customer service greeting", "a loose summary of nothing in particular", "repeated pleasantries", "a vague reminder note", "an unremarkable commute",
          "a bland product label", "a neutral news headline pattern with no subject", "a speaker clearing their throat and stalling", "a to-do list with no specifics"]
FRAC = 0.45


def brain(prompt):
    body = {"model": "qwen3.8-27b", "temperature": 0.9, "max_tokens": 300, "chat_template_kwargs": {"enable_thinking": False},
            "messages": [{"role": "user", "content": prompt}]}
    for _ in range(3):
        req = urllib.request.Request(G.URL, json.dumps(body).encode(), {"Authorization": f"Bearer {G.KEY}", "Content-Type": "application/json"})
        t = json.load(urllib.request.urlopen(req, timeout=600))["choices"][0]["message"]["content"]
        s = [y for y in (re.sub(r"^[\-\*\d\.\)\s]+", "", l).strip() for l in t.splitlines() if l.strip()) if 8 < len(y) < 300]
        if len(s) >= 6:
            return s[:6]
    return []


def main():
    with cf.ThreadPoolExecutor(16) as ex:
        topics = {t: list(ex.map(lambda _: G.ask(t, 0), range(3))) for t in EVAL}
        filler = list(ex.map(lambda p: brain(f"Write 6 short sentences, one per line, no numbering: {p}. They must not be about any real subject or topic."), FILLER))
    held = json.loads((ROOT / "playbox_steer/telepathy_controls.json").read_text())
    agent = json.loads((ROOT / "playbox_steer/telepathy_agentfit.json").read_text())
    out = {}
    for src, repo in (("smollm2", "HuggingFaceTB/SmolLM2-135M"), ("qwen3", "Qwen/Qwen3-1.7B-Base")):
        m, tok = T.load(snapshot_download(repo, allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"]))
        layer = T.layer_of(m, FRAC)
        neutral = T.sentence_states(m, tok, NEUTRAL, layer).mean(0)
        cent = {t: T.sentence_states(m, tok, s[:24], layer).mean(0) - neutral for t, s in FIT_TOPICS_XL.items()}
        cent.update({f"agent_{t}": T.sentence_states(m, tok, s, layer).mean(0) - neutral for t, s in agent.items()})
        C = torch.nn.functional.normalize(torch.stack(list(cent.values())), dim=-1)

        def score(texts):
            d = T.sentence_states(m, tok, texts, layer).mean(0) - neutral
            c = (C @ torch.nn.functional.normalize(d, dim=-1)).max().item()
            return {"topicness": round(c, 4), "mag": round(float(d.norm()), 3)}
        out[src] = {
            "eval_topics": [score(s) for ss in topics.values() for s in ss if s],
            "neutral_triple": score(["The item was placed on the table.", "It was a normal day.", "They talked for a while."]),
            "fresh_filler": [score(s) for s in filler if s],
            "held_filler": [score(c["sentences"]) for c in held if c["kind"] == "filler"],
            "held_offlist": [dict(score(c["sentences"]), about=c["about"]) for c in held if c["kind"] == "offlist"],
        }
        del m
        torch.cuda.empty_cache()
    print(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
