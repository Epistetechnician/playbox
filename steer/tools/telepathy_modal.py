"""The text-free telepathy channel as a Modal service (SmolLM2-135M <-> Qwen3-1.7B-Base, 56-topic maps, strength 0.75).

    modal deploy steer/tools/telepathy_modal.py
    modal app stop playbox-telepathy            # when done; the container also scales to zero after 15 idle minutes

One A10G container holds both models and both maps and serves one request at a time. Auth is a bearer token from the Modal
secret `telepathy-token` (variable TELEPATHY_TOKEN). The agents' machine only needs steer/playbox_steer/telepathy_remote.py,
which is plain urllib: no torch, no models, a few MB of memory.

    POST /think   {"src","dst","texts":[...]}   -> {"vec": base64 float32, "dim": N}   (A reads text, only the vector leaves)
    POST /listen  {"src","dst","vec": base64}   -> {"text": "..."}                       (B continues prompts under the vector)
    GET  /health
"""
import base64
import hmac
import threading
from pathlib import Path

import modal

STEER = Path(__file__).resolve().parents[1]
REPOS = {"smollm2": "HuggingFaceTB/SmolLM2-135M", "qwen3": "Qwen/Qwen3-1.7B-Base"}
PAIRS = (("smollm2", "qwen3"), ("qwen3", "smollm2"))
COEF, FRAC, GEN = 0.75, 0.45, 24

app = modal.App("playbox-telepathy")
vol = modal.Volume.from_name("playbox-e8", create_if_missing=True)  # shared Hugging Face cache (/vol/hf) with the E8 app
image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install("torch>=2.5,<2.8", "transformers>=4.51,<4.60", "numpy<2.2", "blake3", "safetensors",
                 "huggingface_hub", "fastapi[standard]")
    .add_local_dir(STEER / "playbox_steer", "/root/svc/playbox_steer", ignore=["__pycache__", "*.pyc"])
)


@app.cls(image=image, gpu="A10G", volumes={"/vol": vol}, secrets=[modal.Secret.from_name("telepathy-token")],
         max_containers=1, scaledown_window=900, timeout=3600)
@modal.concurrent(max_inputs=16)
class Service:
    @modal.enter()
    def start(self):
        import os
        import sys
        os.environ.update(PLAYBOX_DEVICE="cuda", HF_HOME="/vol/hf", TELEPATHY_FIT="big")
        sys.path.insert(0, "/root/svc")
        import torch
        from huggingface_hub import snapshot_download
        from playbox_steer import telepathy as T
        from playbox_steer.telepathy_data import NEUTRAL, PROMPTS

        self.T, self.torch, self.PROMPTS = T, torch, PROMPTS
        self.lock = threading.Lock()
        self.token = os.environ["TELEPATHY_TOKEN"]
        self.models = {}
        for name, repo in REPOS.items():
            p = snapshot_download(repo, allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"])
            self.models[name] = T.load(p)
        self.maps = {}
        mdir = Path("/vol/telepathy_maps")
        mdir.mkdir(exist_ok=True)
        for src, dst in PAIRS:
            f = mdir / f"{src}_to_{dst}_big.pt"
            (ma, ta), (mb, tb) = self.models[src], self.models[dst]
            la, lb = T.layer_of(ma, FRAC), T.layer_of(mb, FRAC)
            if not f.exists():  # fitted once on this GPU, then reused from the volume
                r, info = T.fit_map(ma, ta, la, mb, tb, lb)
                torch.save({"w": r.w, "neutral_a": T.sentence_states(ma, ta, NEUTRAL, la).mean(0),
                            "norm_b": T.mean_norm(mb, tb, NEUTRAL + PROMPTS, lb), "info": info}, f)
                vol.commit()
                print("fitted", src, dst, info, flush=True)
            self.maps[(src, dst)] = torch.load(f)
        # Typical size of a real topic's mapped thought, so a weak (neutral) thought stays weak instead of being rescaled to full strength.
        from playbox_steer.telepathy_data import TEST_CONCEPTS
        self.ref = {}
        for src, dst in PAIRS:
            m = self.maps[(src, dst)]
            ma, ta = self.models[src]
            la = T.layer_of(ma, FRAC)
            norms = [((T.sentence_states(ma, ta, c["sentences"], la).mean(0) - m["neutral_a"]) @ m["w"]).norm()
                     for c in TEST_CONCEPTS.values()]
            self.ref[(src, dst)] = torch.stack(norms).mean()
        print("ready", {f"{a}->{b}": float(v) for (a, b), v in self.ref.items()}, flush=True)

    @modal.asgi_app()
    def web(self):
        from fastapi import Depends, FastAPI, HTTPException
        from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer
        from pydantic import BaseModel

        api, bearer = FastAPI(), HTTPBearer()

        def auth(c: HTTPAuthorizationCredentials = Depends(bearer)):
            if not hmac.compare_digest(c.credentials.encode(), self.token.encode()):
                raise HTTPException(401, "bad token")

        class Think(BaseModel):
            src: str
            dst: str
            texts: list[str]

        class Listen(BaseModel):
            src: str
            dst: str
            vec: str

        def pair(src, dst):
            if (src, dst) not in self.maps:
                raise HTTPException(400, "unknown pair")

        @api.get("/health")
        def health():
            return {"ok": True}

        @api.post("/think", dependencies=[Depends(auth)])
        def think(r: Think):
            pair(r.src, r.dst)
            if not 1 <= len(r.texts) <= 16 or any(len(t) > 600 for t in r.texts):
                raise HTTPException(400, "1-16 texts of at most 600 characters")
            T, torch, m = self.T, self.torch, self.maps[(r.src, r.dst)]
            ma, ta = self.models[r.src]
            with self.lock:
                d = T.sentence_states(ma, ta, r.texts, T.layer_of(ma, FRAC)).mean(0) - m["neutral_a"]
            v = d @ m["w"]
            # strength follows the thought's size relative to a real topic's (capped), so a neutral thought is quiet
            v = (v / self.ref[(r.src, r.dst)] * COEF * m["norm_b"]).float()
            cap = 1.25 * COEF * m["norm_b"]
            v = (v * min(1.0, float(cap / v.norm()))).contiguous()
            return {"vec": base64.b64encode(v.numpy().tobytes()).decode(), "dim": v.numel()}

        @api.post("/listen", dependencies=[Depends(auth)])
        def listen(r: Listen):
            pair(r.src, r.dst)
            T, torch = self.T, self.torch
            mb, tb = self.models[r.dst]
            raw = base64.b64decode(r.vec)
            if len(raw) != 4 * mb.config.hidden_size:
                raise HTTPException(400, "vector has the wrong size for this receiver")
            v = torch.frombuffer(bytearray(raw), dtype=torch.float32).clone()
            lb = T.layer_of(mb, FRAC)
            with self.lock:
                lines = [f"- {p}\n    plain:   {T.generate(mb, tb, lb, None, p, GEN)!r}\n    steered: {T.generate(mb, tb, lb, v, p, GEN)!r}"
                         for p in self.PROMPTS]
            return {"text": "\n".join(lines)}

        return api
