"""Tool an agent calls to send or receive a latent thought. Two bodies (model families), one fitted map per direction.

    fit    --src smollm2 --dst qwen3                     fit and cache the map (once per ordered pair)
    think  --src smollm2 --dst qwen3 --text "s1" "s2"... read private sentences, write ONLY a vector file
    listen --dst qwen3 --src smollm2 --vec FILE          steer the local model with the vector, print continuations
"""
from __future__ import annotations

import argparse
import glob
import urllib.error
import json
import os
from pathlib import Path

import torch

from . import telepathy as T
from .telepathy_data import NEUTRAL, PROMPTS

HUB = Path(os.environ.get("HF_HUB_CACHE") or Path.home() / ".cache/huggingface/hub")
BODIES = {"smollm2": "models--HuggingFaceTB--SmolLM2-135M", "qwen3": "models--Qwen--Qwen3-1.7B-Base"}
STATE = Path(os.environ.get("TELEPATHY_STATE", Path(__file__).resolve().parents[1] / "results/telepathy/maps"))
COEF = float(os.environ.get("TELEPATHY_COEF", "0.5"))
FRAC = 0.45
# Sender-side topic-ness probe (tools/telepathy_probe.py), the same gate the remote service uses: it reads the unit difference
# vector, so it depends on the sender model and layer, not on the receiver. TELEPATHY_PROBE=off disables it.
PROBE_FILE = Path(os.environ.get("TELEPATHY_PROBE") or Path(__file__).with_name("telepathy_probe.json"))


def load_probe(src: str) -> dict | None:
    if os.environ.get("TELEPATHY_PROBE") == "off" or not PROBE_FILE.is_file():
        return None
    return json.loads(PROBE_FILE.read_text()).get(src)


def probe_strength(d: torch.Tensor, probe: dict) -> tuple[float, float]:
    """(topic-ness, strength): the probe's probability that d is a topic, and the 0..1 ramp between its lo and hi thresholds."""
    z = (torch.nn.functional.normalize(d, dim=-1).double() - torch.tensor(probe["mu"], dtype=torch.float64)) @ \
        torch.tensor(probe["w"], dtype=torch.float64) + probe["b"]
    t = float(torch.sigmoid(z))
    return t, min(1.0, max(0.0, (t - probe["lo"]) / (probe["hi"] - probe["lo"])))


def path(body: str) -> str:
    return glob.glob(str(HUB / BODIES[body] / "snapshots/*"))[0]


def mapfile(src: str, dst: str) -> Path:
    return STATE / f"{src}_to_{dst}{'_big' if os.environ.get('TELEPATHY_FIT') == 'big' else ''}.pt"


def fit(src: str, dst: str) -> None:
    ma, ta = T.load(path(src)); mb, tb = T.load(path(dst))
    la, lb = T.layer_of(ma, FRAC), T.layer_of(mb, FRAC)
    r, info = T.fit_map(ma, ta, la, mb, tb, lb)
    STATE.mkdir(parents=True, exist_ok=True)
    torch.save({"w": r.w, "neutral_a": T.sentence_states(ma, ta, NEUTRAL, la).mean(0),
                "norm_b": T.mean_norm(mb, tb, NEUTRAL + PROMPTS, lb), "info": info}, mapfile(src, dst))
    print(json.dumps(info))


def think(src: str, dst: str, texts: list[str], out: str, resident: bool = False) -> None:
    m = torch.load(mapfile(src, dst))
    ma, ta = _resident(src) if resident else T.load(path(src))
    la = T.layer_of(ma, FRAC)
    d = T.sentence_states(ma, ta, texts, la).mean(0) - m["neutral_a"]
    probe = load_probe(src)
    topicness, g = probe_strength(d, probe) if probe else (None, None)
    v = (d @ m["w"])
    v = v / v.norm() * COEF * (1.0 if g is None else g) * m["norm_b"]
    tmp = f"{out}.tmp"  # atomic: a reader never sees a half-written vector
    # the strength travels with the vector (a number, no text); a bare tensor means "no gate" and is still readable
    torch.save(v if g is None else {"v": v, "strength": g}, tmp)
    os.replace(tmp, out)
    print(f"wrote {out}: one {v.numel()}-float vector for {dst}; no text left this process"
          + ("" if g is None else f"; topic-ness {topicness:.3f}, strength {g:.2f}"))


def listen(dst: str, src: str, vec: str, gen: int = 24, resident: bool = False) -> None:
    v = torch.load(vec)
    if isinstance(v, dict):
        print(f"signal strength {v['strength']:.2f} (0 = no thought was sent, 1 = a clear thought)")
        v = v["v"]
    mb, tb = _resident(dst) if resident else T.load(path(dst))
    lb = T.layer_of(mb, FRAC)
    for p in PROMPTS:
        print(f"- {p}{T.generate(mb, tb, lb, v, p, gen)!r}")


URL = os.environ.get("TELEPATHY_URL", "http://127.0.0.1:8765")
_RESIDENT: dict = {}


def _resident(body: str):
    """Each model is loaded once per daemon, not once per command."""
    if body not in _RESIDENT:
        _RESIDENT[body] = T.load(path(body))
    return _RESIDENT[body]


def serve(port: int) -> None:
    """One process holds both models and the maps and serves think/listen one request at a time (bounded memory and CPU)."""
    import io
    import contextlib
    import threading
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    lock = threading.Lock()  # the models run one request at a time; connections wait here, not in a 5-deep TCP backlog
    torch.set_num_threads(max(1, (os.cpu_count() or 8) // 2))

    def handle(req: dict) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            if req["cmd"] == "think":
                think(req["src"], req["dst"], req["texts"], req["out"], resident=True)
            else:
                listen(req["dst"], req["src"], req["vec"], resident=True)
        return buf.getvalue()

    class H(BaseHTTPRequestHandler):
        def do_GET(self):  # liveness probe for the harness; does not wait for the models
            data = b"ok"
            self.send_response(200); self.send_header("content-length", "2"); self.end_headers(); self.wfile.write(data)

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["content-length"])))
            try:
                with lock:
                    code, out = 200, handle(body)
            except Exception as e:  # report to the caller; keep serving
                code, out = 500, f"{type(e).__name__}: {e}"
            data = out.encode()
            self.send_response(code); self.send_header("content-length", str(len(data))); self.end_headers(); self.wfile.write(data)

        def log_message(self, *_):
            pass

    print(f"telepathy daemon on 127.0.0.1:{port}", flush=True)
    class S(ThreadingHTTPServer):
        daemon_threads = True
        request_queue_size = 128

    S(("127.0.0.1", port), H).serve_forever()


def via_daemon(req: dict) -> bool:
    """Send a request to the resident daemon; False if none is running and in-process loading was not asked for."""
    import time
    import urllib.request
    for attempt in range(3):  # a daemon that is busy or restarting is not a missing daemon
        try:
            r = urllib.request.urlopen(urllib.request.Request(URL, json.dumps(req).encode()), timeout=1800)
            print(r.read().decode(), end="")
            return True
        except urllib.error.HTTPError as e:
            raise SystemExit(f"daemon error: {e.read().decode()}")
        except OSError:
            if os.environ.get("TELEPATHY_INPROC") == "1":
                return False
            if attempt < 2:
                time.sleep(5 * (attempt + 1))
    raise SystemExit(f"no telepathy daemon at {URL}; start one with `serve`, or set TELEPATHY_INPROC=1 to load models in this process (about 8 GB)")


def main() -> None:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for n in ("fit", "think", "listen"):
        p = sub.add_parser(n); p.add_argument("--src", required=True); p.add_argument("--dst", required=True)
    sub.add_parser("serve").add_argument("--port", type=int, default=8765)
    sub.choices["think"].add_argument("--text", nargs="+", required=True)
    sub.choices["think"].add_argument("--out", required=True)
    sub.choices["listen"].add_argument("--vec", required=True)
    a = ap.parse_args()
    if a.cmd == "serve":
        return serve(a.port)
    if a.cmd == "think" and via_daemon({"cmd": "think", "src": a.src, "dst": a.dst, "texts": a.text, "out": os.path.abspath(a.out)}):
        return
    if a.cmd == "listen" and via_daemon({"cmd": "listen", "src": a.src, "dst": a.dst, "vec": os.path.abspath(a.vec)}):
        return
    {"fit": lambda: fit(a.src, a.dst), "think": lambda: think(a.src, a.dst, a.text, a.out),
     "listen": lambda: listen(a.dst, a.src, a.vec)}[a.cmd]()


if __name__ == "__main__":
    main()
