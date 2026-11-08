"""The telepathy service on a GPU node (gman) instead of Modal: same routes and behaviour as tools/telepathy_modal.py.

    TELEPATHY_FIT=agent TELEPATHY_GATE=0.9,1.1 TELEPATHY_TOKEN=... python tools/telepathy_node.py   # listens on 127.0.0.1:8765

gman nodes take no inbound connections, so the agents (Hermes), the brain (vLLM) and this service all run on the node and talk
over loopback. The routes, maps (cached under ~/tp/maps) and strength gate are identical to the Modal service.
"""
import base64
import hmac
import json
import os
import random
import struct
import sys
import threading
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
os.environ.setdefault("PLAYBOX_DEVICE", "cuda")
import torch
import uvicorn
from fastapi import Depends, FastAPI, HTTPException
from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer
from huggingface_hub import snapshot_download
from pydantic import BaseModel

from playbox_steer import telepathy as T
from playbox_steer.telepathy_data import NEUTRAL, PROMPTS
from playbox_steer.telepathy_data_xl import FIT_TOPICS_XL

REPOS = {"smollm2": "HuggingFaceTB/SmolLM2-135M", "qwen3": "Qwen/Qwen3-1.7B-Base"}
PAIRS = (("smollm2", "qwen3"), ("qwen3", "smollm2"))
COEF, FRAC, GEN = 0.75, 0.45, 24
FIT = os.environ.get("TELEPATHY_FIT", "agent")
GATE = tuple(float(x) for x in os.environ.get("TELEPATHY_GATE", "0.9,1.1").split(","))
# topic-ness gate per sender: strength ramps 0 to 1 as the vector's best cosine to any known topic direction goes lo to hi
# ("smollm2:0.78,0.86;qwen3:0.72,0.80"); thresholds from results/telepathy/topicness.json. Empty: off.
TOPIC_GATE = {k: tuple(map(float, v.split(","))) for k, v in (e.split(":") for e in os.environ.get("TELEPATHY_TOPIC_GATE", "").split(";") if e)}
# TELEPATHY_LOG: append every /think (tag, texts, mag, the difference vector) as JSON lines, for calibrating gates offline.
# TELEPATHY_PROBE: json {src: {mu, w, b, lo, hi}} from tools/telepathy_probe.py; when set for a sender it replaces the magnitude gate.
LOG = os.environ.get("TELEPATHY_LOG")
PROBE = json.loads(Path(os.environ["TELEPATHY_PROBE"]).read_text()) if os.environ.get("TELEPATHY_PROBE") else {}
TOKEN = os.environ["TELEPATHY_TOKEN"]
MAPS = Path(os.environ.get("TELEPATHY_MAPS", Path.home() / "tp/maps"))

lock = threading.Lock()
models, maps, ref = {}, {}, {}
for name, repo in REPOS.items():
    models[name] = T.load(snapshot_download(repo, allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"]))
MAPS.mkdir(parents=True, exist_ok=True)
for src, dst in PAIRS:
    f = MAPS / f"{src}_to_{dst}_{FIT}.pt"
    (ma, ta), (mb, tb) = models[src], models[dst]
    la, lb = T.layer_of(ma, FRAC), T.layer_of(mb, FRAC)
    if not f.exists():
        r, info = T.fit_map(ma, ta, la, mb, tb, lb)
        torch.save({"w": r.w, "neutral_a": T.sentence_states(ma, ta, NEUTRAL, la).mean(0),
                    "norm_b": T.mean_norm(mb, tb, NEUTRAL + PROMPTS, lb), "info": info}, f)
        print("fitted", src, dst, info, flush=True)
    maps[(src, dst)] = torch.load(f)
rng = random.Random(1)  # salience reference: typical size of a concept's difference vector in each sender's space
sets = [rng.sample(v, 6) for v in rng.sample(list(FIT_TOPICS_XL.values()), 40)]
for src in {a for a, _ in PAIRS}:
    ma, ta = models[src]
    la, na = T.layer_of(ma, FRAC), maps[next(k for k in maps if k[0] == src)]["neutral_a"]
    ref[src] = float(torch.stack([(T.sentence_states(ma, ta, x, la).mean(0) - na).norm() for x in sets]).median())
cent = {}
if TOPIC_GATE:
    import json
    agent = json.loads((Path(__file__).resolve().parents[1] / "playbox_steer/telepathy_agentfit.json").read_text())
    for src in TOPIC_GATE:
        ma, ta = models[src]
        la, na = T.layer_of(ma, FRAC), maps[next(k for k in maps if k[0] == src)]["neutral_a"]
        rows = [t[:24] for t in FIT_TOPICS_XL.values()] + list(agent.values())
        cent[src] = torch.nn.functional.normalize(torch.stack([T.sentence_states(ma, ta, x, la).mean(0) - na for x in rows]), dim=-1)
print("ref", ref, "topic gate", TOPIC_GATE, "ready", flush=True)

api, bearer = FastAPI(), HTTPBearer()


def auth(c: HTTPAuthorizationCredentials = Depends(bearer)):
    if not hmac.compare_digest(c.credentials.encode(), TOKEN.encode()):
        raise HTTPException(401, "bad token")


class Think(BaseModel):
    src: str
    dst: str
    texts: list[str]
    tag: str = ""


class Listen(BaseModel):
    src: str
    dst: str
    vec: str


def pair(src, dst):
    if (src, dst) not in maps:
        raise HTTPException(400, "unknown pair")


@api.get("/health")
def health():
    return {"ok": True}


@api.post("/think", dependencies=[Depends(auth)])
def think(r: Think):
    pair(r.src, r.dst)
    if not 1 <= len(r.texts) <= 16 or any(len(t) > 600 for t in r.texts):
        raise HTTPException(400, "1-16 texts of at most 600 characters")
    m = maps[(r.src, r.dst)]
    ma, ta = models[r.src]
    with lock:
        d = T.sentence_states(ma, ta, r.texts, T.layer_of(ma, FRAC)).mean(0) - m["neutral_a"]
    mag = float(d.norm()) / ref[r.src]
    g = 1.0 if GATE[1] <= GATE[0] else min(1.0, max(0.0, (mag - GATE[0]) / (GATE[1] - GATE[0])))
    topicness = None
    if r.src in PROBE:
        P = PROBE[r.src]
        z = (torch.nn.functional.normalize(d, dim=-1).double() - torch.tensor(P["mu"], dtype=torch.float64)) @ torch.tensor(P["w"], dtype=torch.float64) + P["b"]
        topicness = float(torch.sigmoid(z))
        g = min(1.0, max(0.0, (topicness - P["lo"]) / (P["hi"] - P["lo"])))
    elif r.src in cent:
        topicness = float((cent[r.src] @ torch.nn.functional.normalize(d, dim=-1)).max())
        lo, hi = TOPIC_GATE[r.src]
        g = min(g, min(1.0, max(0.0, (topicness - lo) / (hi - lo))))
    if LOG:
        with lock, open(LOG, "a") as f:
            f.write(json.dumps({"src": r.src, "tag": r.tag, "texts": r.texts, "mag": mag, "topicness": topicness, "d": [round(float(x), 5) for x in d]}) + "\n")
    v = d @ m["w"]
    v = (v / v.norm() * COEF * g * m["norm_b"]).float().cpu().contiguous()
    print("think mag", round(mag, 3), "topicness", topicness and round(topicness, 3), "gate", round(g, 3), flush=True)
    return {"mag": mag, "topicness": topicness, "gate": g, "vec": base64.b64encode(v.numpy().tobytes()).decode(), "dim": v.numel()}


@api.post("/listen", dependencies=[Depends(auth)])
def listen(r: Listen):
    pair(r.src, r.dst)
    mb, tb = models[r.dst]
    raw = base64.b64decode(r.vec)
    n = 4 * mb.config.hidden_size
    if len(raw) not in (n, n + 4):
        raise HTTPException(400, "vector has the wrong size for this receiver")
    strength = struct.unpack("<f", raw[n:])[0] if len(raw) == n + 4 else None
    v = torch.frombuffer(bytearray(raw[:n]), dtype=torch.float32).clone()
    lb = T.layer_of(mb, FRAC)
    with lock:
        lines = [f"- {p}{T.generate(mb, tb, lb, v, p, GEN)!r}" for p in PROMPTS]
    head = [] if strength is None else [f"signal strength {strength:.2f} (0 = no thought was sent, 1 = a clear thought)"]
    return {"text": "\n".join(head + lines)}


if __name__ == "__main__":
    uvicorn.run(api, host="127.0.0.1", port=int(os.environ.get("TELEPATHY_PORT", 8765)), log_level="warning")
