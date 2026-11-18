#!/usr/bin/env python3
"""Make the desktop app report the release's version: copy the root Cargo.toml version (or the one given) into
desktop/Cargo.toml and desktop/src-tauri/tauri.conf.json. Run by the bundle workflow before building; the updater compares these."""
import json, re, sys
from pathlib import Path

DESKTOP = Path(__file__).resolve().parents[1]
ROOT = DESKTOP.parent


def root_version() -> str:
    m = re.search(r'^version\s*=\s*"([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M)
    if not m:
        sys.exit("no version in the root Cargo.toml")
    return m.group(1)


def main(argv) -> int:
    v = argv[0] if argv else root_version()
    if not re.fullmatch(r"\d+\.\d+\.\d+", v):
        sys.exit(f"not a stable version: {v}")
    cargo = DESKTOP / "Cargo.toml"
    s = cargo.read_text()
    s2 = re.sub(r'(\[workspace\.package\]\s*\nversion\s*=\s*")[^"]+(")', rf"\g<1>{v}\g<2>", s, count=1)
    if s2 == s and f'version = "{v}"' not in s:
        sys.exit("could not find the workspace version in desktop/Cargo.toml")
    cargo.write_text(s2)
    conf = DESKTOP / "src-tauri" / "tauri.conf.json"
    c = json.loads(conf.read_text())
    c["version"] = v
    conf.write_text(json.dumps(c, indent=2) + "\n")
    print(f"desktop version -> {v}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
