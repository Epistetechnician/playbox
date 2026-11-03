"""Agent-side client for the Modal telepathy service (steer/tools/telepathy_modal.py). Same commands as telepathy_agent,
but nothing is loaded locally: it is plain urllib, a few MB of memory, no torch.

    think  --src smollm2 --dst qwen3 --out FILE --text "s1" "s2" ...   send A's sentences, write only the returned vector
    listen --src smollm2 --dst qwen3 --vec FILE                         send the vector, print B's continuations

URL and token come from TELEPATHY_URL / TELEPATHY_TOKEN, else from ~/.config/playbox/telepathy_url and telepathy_token
(the token file is mode 0600). The token is never taken from the command line, so it does not end up in agent transcripts.
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

CONF = Path.home() / ".config/playbox"


def _conf(env: str, name: str) -> str:
    if os.environ.get(env):
        return os.environ[env].strip()
    f = CONF / name
    if f.exists():
        return f.read_text().strip()
    raise SystemExit(f"no {env} and no {f}; deploy the service and write them (see steer/tools/telepathy_modal.py)")


def call(route: str, body: dict) -> dict:
    url, token = _conf("TELEPATHY_URL", "telepathy_url").rstrip("/"), _conf("TELEPATHY_TOKEN", "telepathy_token")
    req = urllib.request.Request(f"{url}{route}", json.dumps(body).encode(),
                                 {"authorization": f"Bearer {token}", "content-type": "application/json"})
    last = None
    for attempt in range(4):  # cold start of the GPU container can take a few minutes; 5xx and timeouts are retried
        try:
            with urllib.request.urlopen(req, timeout=900) as r:
                return json.load(r)
        except urllib.error.HTTPError as e:
            if e.code < 500:
                raise SystemExit(f"service refused: {e.code} {e.read().decode()[:300]}")
            last = f"{e.code}"
        except (urllib.error.URLError, TimeoutError, ConnectionError) as e:
            last = str(e)
        time.sleep(10 * (attempt + 1))
    raise SystemExit(f"service unreachable: {last}")


def main() -> None:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for n in ("think", "listen"):
        p = sub.add_parser(n)
        p.add_argument("--src", required=True)
        p.add_argument("--dst", required=True)
    sub.choices["think"].add_argument("--text", nargs="+", required=True)
    sub.choices["think"].add_argument("--out", required=True)
    sub.choices["listen"].add_argument("--vec", required=True)
    a = ap.parse_args()
    if a.cmd == "think":
        r = call("/think", {"src": a.src, "dst": a.dst, "texts": a.text})
        tmp = f"{a.out}.tmp"
        Path(tmp).write_bytes(base64.b64decode(r["vec"]))
        os.replace(tmp, a.out)  # atomic: a reader never sees a half-written vector
        print(f"wrote {a.out}: one {r['dim']}-float vector for {a.dst}; no text left this process")
    else:
        raw = Path(a.vec).read_bytes()
        print(call("/listen", {"src": a.src, "dst": a.dst, "vec": base64.b64encode(raw).decode()})["text"])


if __name__ == "__main__":
    main()
