"""Command line used inside the sandbox image and by members on their own machines.

    python -m playbox_steer run     --weights W --task T [--guards G] [--deny a,b] --out FILE
    python -m playbox_steer probe   --weights W --task T --out FILE
    python -m playbox_steer extract --weights W --site S --layer N --pairs FILE --out DIR
    python -m playbox_steer random  --dim N --seed S --out DIR
    python -m playbox_steer manifest DIR
    python -m playbox_steer handoff --weights W --task T --expected HEX --audit P --out FILE

`T` holds `spec.txt`, `direction.f32` and `prompts.json`. Exit codes: 0 ok, 2 bad input, 3 guarded
direction, 4 failed gate.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from . import engine, files, spec as specmod


def _write(path: str, obj) -> None:
    Path(path).write_bytes(engine.canonical_json(obj) + b"\n")


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(prog="playbox_steer")
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("run", "probe"):
        p = sub.add_parser(name)
        p.add_argument("--weights", required=True)
        p.add_argument("--task", required=True)
        p.add_argument("--out", required=True)
        if name == "run":
            p.add_argument("--guards")
            p.add_argument("--deny", default="")
    p = sub.add_parser("extract")
    p.add_argument("--weights", required=True)
    p.add_argument("--site", default="resid_post", choices=specmod.SITES)
    p.add_argument("--layer", required=True, type=int)
    p.add_argument("--pairs", required=True, help='JSON {"positive": [...], "negative": [...]}')
    p.add_argument("--out", required=True)
    p = sub.add_parser("random")
    p.add_argument("--dim", required=True, type=int)
    p.add_argument("--seed", required=True, type=int)
    p.add_argument("--out", required=True)
    p = sub.add_parser("handoff")
    p.add_argument("--weights", required=True)
    p.add_argument("--task", required=True, help="directory holding handoff.bin and signer.txt (hex)")
    p.add_argument("--expected", required=True, help="weights manifest hash the cache must be bound to")
    p.add_argument("--audit", type=float, default=0.1)
    p.add_argument("--out", required=True)
    p = sub.add_parser("manifest")
    p.add_argument("dir")
    a = ap.parse_args(argv)

    try:
        if a.cmd == "manifest":
            print(files.weights_manifest(Path(a.dir)))
            return 0
        if a.cmd == "extract":
            pairs = json.loads(Path(a.pairs).read_text())
            vec = engine.extract_diffmean(Path(a.weights), pairs["positive"], pairs["negative"], a.site, a.layer)
            out = Path(a.out)
            out.mkdir(parents=True, exist_ok=True)
            print(files.write_direction(out / "direction.f32", vec))
            return 0
        if a.cmd == "random":
            out = Path(a.out)
            out.mkdir(parents=True, exist_ok=True)
            print(files.write_direction(out / "direction.f32", engine.random_direction(a.dim, a.seed)))
            return 0
        if a.cmd == "handoff":
            from . import handoff as ho
            from . import latent
            task = Path(a.task)
            signer = (task / "signer.txt").read_text().strip()
            model, tok = engine.load(Path(a.weights))
            rx = ho.Receiver(a.expected, {signer}, lambda text: latent.read_context(model, tok, text, None), a.audit)
            d, _ = rx.accept((task / "handoff.bin").read_bytes())
            _write(a.out, {"accepted": d.accepted, "audited": d.audited, "reasons": d.reasons, "max_abs_diff": d.max_abs_diff if d.max_abs_diff is not None and d.max_abs_diff == d.max_abs_diff and abs(d.max_abs_diff) != float("inf") else None,
                           "quarantine": bool(rx.quarantined)})
            return 0 if d.accepted else 5
        task = Path(a.task)
        spec = specmod.parse((task / "spec.txt").read_text())
        if a.cmd == "probe":
            _write(a.out, engine.probe(Path(a.weights), task, spec))
            return 0
        deny = {c for c in a.deny.split(",") if c}
        out = engine.run(Path(a.weights), task, spec, Path(a.guards) if a.guards else None, deny)
        _write(a.out, out)
        return 0 if out["gates"]["pass"] else 4
    except engine.SteerError as e:
        print(f"steer: {e}", file=sys.stderr)
        return e.code
    except (ValueError, KeyError, OSError, json.JSONDecodeError) as e:
        print(f"steer: bad input: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
