#!/usr/bin/env python3
"""Assemble what the desktop bundle carries, in `desktop/stage/`:

    stage/hub/      the hub (a copy of ui/ without tests), run by the bundled Python
    stage/python/   a relocatable CPython, unpacked from an archive whose SHA-256 you pin
    stage/bin/      the `playbox` binary (the hub's CLI and MCP server)

The staged tree is what `tauri build --config tauri.bundle.conf.json` packs into the app's resources. Nothing here downloads:
the Python archive is given on the command line with the hash it must have, so a release records exactly what it shipped.
Standard library only.
"""
import argparse, hashlib, shutil, stat, sys, tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SKIP_DIRS = {"__pycache__", "tests", ".pytest_cache"}
SKIP_FILES = {"e2e.py", "README.md"}


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def stage_hub(src: Path, dst: Path) -> int:
    n = 0
    for p in sorted(src.rglob("*")):
        rel = p.relative_to(src)
        if p.is_dir() or SKIP_DIRS & set(rel.parts) or p.name in SKIP_FILES or p.suffix == ".pyc":
            continue
        out = dst / rel
        out.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(p, out)
        n += 1
    return n


def stage_python(archive: Path, want: str, dst: Path) -> None:
    got = sha256(archive)
    if got != want.lower():
        sys.exit(f"python archive hash mismatch: expected {want}, got {got}")
    dst.mkdir(parents=True)
    with tarfile.open(archive) as t:
        for m in t.getmembers():  # refuse anything that would land outside dst
            target = (dst / m.name).resolve()
            if dst.resolve() not in target.parents and target != dst.resolve():
                sys.exit(f"unsafe path in python archive: {m.name}")
            if m.issym() or m.islnk():
                link = (target.parent / m.linkname).resolve() if m.issym() else (dst / m.linkname).resolve()
                if dst.resolve() not in link.parents and link != dst.resolve():
                    sys.exit(f"unsafe link in python archive: {m.name} -> {m.linkname}")
        t.extractall(dst, filter="data")
    # python-build-standalone archives hold a single top-level `python/` directory: flatten it.
    inner = dst / "python"
    if inner.is_dir() and not (dst / "bin").exists():
        for child in inner.iterdir():
            shutil.move(str(child), dst / child.name)
        inner.rmdir()


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--playbox", type=Path, required=True, help="the release-built playbox binary")
    ap.add_argument("--python-archive", type=Path, help="relocatable CPython .tar.gz (omit only for a development stage)")
    ap.add_argument("--python-sha256", help="the hash that archive must have")
    ap.add_argument("--stage", type=Path, default=ROOT / "desktop" / "stage")
    a = ap.parse_args(argv)

    if bool(a.python_archive) != bool(a.python_sha256):
        sys.exit("--python-archive and --python-sha256 go together")
    if not a.playbox.is_file():
        sys.exit(f"no such binary: {a.playbox}")
    if a.stage.exists():
        shutil.rmtree(a.stage)
    (a.stage / "bin").mkdir(parents=True)

    n = stage_hub(ROOT / "ui", a.stage / "hub")
    name = "playbox.exe" if a.playbox.suffix == ".exe" else "playbox"
    shutil.copyfile(a.playbox, a.stage / "bin" / name)  # contents only: no file flags or xattrs from the build machine
    (a.stage / "bin" / name).chmod(0o755)
    if a.python_archive:
        stage_python(a.python_archive, a.python_sha256, a.stage / "python")
    else:
        print("note: no bundled Python; the app will use the system python3 (development only)", file=sys.stderr)
    print(f"staged {n} hub files, playbox{'' if a.python_archive else ', no python'} -> {a.stage}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
