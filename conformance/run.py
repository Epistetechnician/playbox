#!/usr/bin/env python3
"""Run the conformance scenarios against the built binaries.

    python3 conformance/run.py                 # the fast tier (every push)
    python3 conformance/run.py --tier full     # fast + scenarios that need docker or take long
    python3 conformance/run.py cli_contract    # one scenario by name
    PLAYBOX_BIN=/usr/local/bin python3 conformance/run.py   # installed binaries instead of target/debug

CONFORMANCE_REQUIRE_ALL=1 turns a skipped scenario into a failure (CI sets it where the environment is meant to support everything)."""
import argparse
import importlib
import os
import sys
import time
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import Ctx, Skip  # noqa: E402


def load(tier: str, only: list[str]):
    out = []
    for f in sorted((Path(__file__).resolve().parent / "scenarios").glob("*.py")):
        if f.name.startswith("_"):
            continue
        m = importlib.import_module(f"scenarios.{f.stem}")
        t = getattr(m, "TIER", "fast")
        if only and f.stem not in only:
            continue
        if not only and t == "full" and tier == "fast":
            continue
        out.append((f.stem, t, m.run, (m.__doc__ or "").strip().splitlines()[0] if m.__doc__ else ""))
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tier", choices=["fast", "full"], default="fast")
    ap.add_argument("names", nargs="*")
    a = ap.parse_args()
    require_all = bool(os.environ.get("CONFORMANCE_REQUIRE_ALL"))
    failed = skipped = 0
    for name, tier, fn, doc in load(a.tier, a.names):
        ctx = Ctx()
        t0 = time.time()
        try:
            fn(ctx)
            print(f"ok    {name:<28} {time.time() - t0:5.1f}s  {doc}")
        except Skip as s:
            if require_all:
                failed += 1
                print(f"FAIL  {name:<28} skipped but required: {s}")
            else:
                skipped += 1
                print(f"skip  {name:<28} {s}")
        except BaseException:  # noqa: BLE001
            failed += 1
            print(f"FAIL  {name:<28} {time.time() - t0:5.1f}s")
            traceback.print_exc()
        finally:
            ctx.cleanup()
    print(f"\n{failed} failed, {skipped} skipped")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
