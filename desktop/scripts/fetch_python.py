#!/usr/bin/env python3
"""Download the pinned relocatable Python for one target (see desktop/python-runtime.json) and check its SHA-256.

    python3 desktop/scripts/fetch_python.py aarch64-apple-darwin OUT_DIR   ->  prints the archive path and its hash for stage.py

Bumping Python is a reviewed edit of python-runtime.json (take the hashes from the release's SHA256SUMS), never a download-time lookup.
"""
import hashlib, json, sys, urllib.parse, urllib.request
from pathlib import Path

PIN = Path(__file__).resolve().parents[1] / "python-runtime.json"


def main(argv) -> int:
    if len(argv) != 2:
        sys.exit(__doc__)
    target, out = argv[0], Path(argv[1])
    pin = json.loads(PIN.read_text())
    if target not in pin["targets"]:
        sys.exit(f"no pinned Python for {target}; have: {', '.join(pin['targets'])}")
    t = pin["targets"][target]
    tag = pin["source"].rsplit("/", 1)[1]
    url = f"https://github.com/astral-sh/python-build-standalone/releases/download/{tag}/{urllib.parse.quote(t['file'])}"
    out.mkdir(parents=True, exist_ok=True)
    dest = out / t["file"]
    if not dest.is_file():
        with urllib.request.urlopen(url, timeout=120) as r, dest.open("wb") as f:
            while block := r.read(1 << 20):
                f.write(block)
    got = hashlib.sha256(dest.read_bytes()).hexdigest()
    if got != t["sha256"]:
        dest.unlink()
        sys.exit(f"hash mismatch for {t['file']}: pinned {t['sha256']}, got {got}")
    print(dest, t["sha256"])
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
