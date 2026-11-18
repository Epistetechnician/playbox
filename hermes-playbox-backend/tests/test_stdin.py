"""stdin_data reaches the command that reads it, not only the last statement of a script.
Run with Hermes' python:  PYTHONPATH=~/.hermes/hermes-agent <hermes venv python> -m unittest discover -s hermes-playbox-backend/tests"""
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
import playbox_terminal as pt  # noqa: E402

# the shape of Hermes' atomic file write: cat reads stdin in the middle of the script
SCRIPT = 'set -e; d=$1; tmp="$(mktemp -p "$d" .t.XXXXXX)"; cat > "$tmp"; mv -f "$tmp" "$d/out.txt"; echo done'


class WithStdin(unittest.TestCase):
    def run_sh(self, script, data, close_stdin=False):
        d = tempfile.mkdtemp()
        cmd = pt.with_stdin(script.replace("$1", d), data)
        r = subprocess.run(["sh", "-c", f"( {{ {cmd}\n}} ) 2>&1" + (" 0<&-" if close_stdin else "")], capture_output=True, text=True)
        out = os.path.join(d, "out.txt")
        return r, (open(out).read() if os.path.exists(out) else None)

    def test_cat_in_the_middle_gets_the_data(self):
        data = "".join(f"line {i}\n" for i in range(5000))
        r, got = self.run_sh(SCRIPT, data)
        self.assertIn("done", r.stdout)
        self.assertEqual(got, data)

    def test_replay_with_stdin_closed_still_works(self):
        r, got = self.run_sh(SCRIPT, "hello\n", close_stdin=True)
        self.assertEqual(got, "hello\n")

    def test_data_without_trailing_newline_is_exact(self):
        r, got = self.run_sh(SCRIPT, "no newline at end")
        self.assertEqual(got, "no newline at end")


if __name__ == "__main__":
    unittest.main()
