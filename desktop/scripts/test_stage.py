import hashlib, io, subprocess, sys, tarfile, tempfile, unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
STAGE = HERE / "stage.py"


def run(*args):
    return subprocess.run([sys.executable, "-I", str(STAGE), *map(str, args)], capture_output=True, text=True)


def archive(path: Path, members: dict) -> str:
    with tarfile.open(path, "w:gz") as t:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            t.addfile(info, io.BytesIO(data))
    return hashlib.sha256(path.read_bytes()).hexdigest()


class Stage(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.bin = self.tmp / "playbox"
        self.bin.write_bytes(b"#!/bin/sh\n")

    def test_stages_hub_without_tests_and_flattens_python(self):
        tgz = self.tmp / "py.tar.gz"
        h = archive(tgz, {"python/bin/python3": b"py"})
        out = self.tmp / "stage"
        r = run("--playbox", self.bin, "--python-archive", tgz, "--python-sha256", h, "--stage", out)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue((out / "hub/playbox_hub.py").is_file())
        self.assertFalse((out / "hub/tests").exists())
        self.assertFalse((out / "hub/e2e.py").exists())
        self.assertTrue((out / "python/bin/python3").is_file())
        self.assertTrue((out / "bin/playbox").is_file())

    def test_wrong_hash_is_refused(self):
        tgz = self.tmp / "py.tar.gz"
        archive(tgz, {"python/bin/python3": b"py"})
        r = run("--playbox", self.bin, "--python-archive", tgz, "--python-sha256", "0" * 64, "--stage", self.tmp / "s")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("hash mismatch", r.stderr)

    def test_path_escape_is_refused(self):
        tgz = self.tmp / "py.tar.gz"
        h = archive(tgz, {"../evil": b"x"})
        r = run("--playbox", self.bin, "--python-archive", tgz, "--python-sha256", h, "--stage", self.tmp / "s")
        self.assertNotEqual(r.returncode, 0)
        self.assertFalse((self.tmp / "evil").exists())

    def test_python_args_go_together(self):
        r = run("--playbox", self.bin, "--python-archive", self.tmp / "x", "--stage", self.tmp / "s")
        self.assertNotEqual(r.returncode, 0)


if __name__ == "__main__":
    unittest.main()
