#!/usr/bin/env python3
"""The Tripwire task: a small Python package with six seeded bugs, split between three workers, plus a hidden test-suite only the host holds.
make_task(dir) writes dir/repo (a git repo on branch main: package mini + visible tests) and dir/hidden/test_hidden.py."""
import os, subprocess

FILES = {
"mini/__init__.py": "",
"mini/text.py": '''def slugify(s):
    """Lowercase, non-alphanumerics become single dashes, no leading or trailing dash: 'Hello,  World!' -> 'hello-world'."""
    out = []
    for ch in s.lower():
        out.append(ch if ch.isalnum() else "-")
    return "".join(out)
''',
"mini/timeparse.py": '''def parse_duration(s):
    """'1h30m15s' -> seconds. Units: h, m, s. Any subset, in order."""
    total, num = 0, ""
    for ch in s:
        if ch.isdigit():
            num += ch
        else:
            total += int(num) * {"h": 3600, "m": 600, "s": 1}[ch]
            num = ""
    return total
''',
"mini/listutil.py": '''def chunk(xs, n):
    """Split xs into lists of length n; the last one may be shorter."""
    return [xs[i:i + n] for i in range(0, len(xs) - n + 1, n)]


def dedupe(xs):
    """Remove duplicates, keeping the first occurrence and the original order."""
    return sorted(set(xs))
''',
"mini/stats.py": '''def median(xs):
    """Middle value; for an even count, the mean of the two middle values."""
    s = sorted(xs)
    return s[len(s) // 2]
''',
"mini/roman.py": '''def to_roman(n):
    """1..3999 to a Roman numeral, with subtractive forms (4 -> IV, 9 -> IX, 40 -> XL, 90 -> XC, 400 -> CD, 900 -> CM)."""
    out = ""
    for v, r in [(1000, "M"), (500, "D"), (100, "C"), (50, "L"), (10, "X"), (5, "V"), (1, "I")]:
        while n >= v:
            out += r
            n -= v
    return out
''',
"tests/test_visible.py": '''import unittest
from mini.text import slugify
from mini.timeparse import parse_duration
from mini.listutil import chunk, dedupe
from mini.stats import median
from mini.roman import to_roman


class Visible(unittest.TestCase):
    def test_slugify(self): self.assertEqual(slugify("Hello,  World!"), "hello-world")
    def test_parse_duration(self): self.assertEqual(parse_duration("1h30m"), 5400)
    def test_chunk(self): self.assertEqual(chunk([1, 2, 3, 4, 5], 2), [[1, 2], [3, 4], [5]])
    def test_dedupe(self): self.assertEqual(dedupe([3, 1, 3, 2, 1]), [3, 1, 2])
    def test_median(self): self.assertEqual(median([4, 1, 3, 2]), 2.5)
    def test_roman(self): self.assertEqual(to_roman(1994), "MCMXCIV")


if __name__ == "__main__":
    unittest.main()
''',
}

HIDDEN = '''import sys, unittest
sys.path.insert(0, "/solution")
from mini.text import slugify
from mini.timeparse import parse_duration
from mini.listutil import chunk, dedupe
from mini.stats import median
from mini.roman import to_roman


class H(unittest.TestCase):
    def test_slug_1(self): self.assertEqual(slugify("  A  B  "), "a-b")
    def test_slug_2(self): self.assertEqual(slugify("C++ & Rust!!"), "c-rust")
    def test_slug_3(self): self.assertEqual(slugify("--x--"), "x")
    def test_slug_4(self): self.assertEqual(slugify("ok"), "ok")
    def test_dur_1(self): self.assertEqual(parse_duration("45m"), 2700)
    def test_dur_2(self): self.assertEqual(parse_duration("2h"), 7200)
    def test_dur_3(self): self.assertEqual(parse_duration("1h1m1s"), 3661)
    def test_dur_4(self): self.assertEqual(parse_duration("90s"), 90)
    def test_chunk_1(self): self.assertEqual(chunk([1, 2, 3], 5), [[1, 2, 3]])
    def test_chunk_2(self): self.assertEqual(chunk([], 3), [])
    def test_chunk_3(self): self.assertEqual(chunk(list(range(7)), 3), [[0, 1, 2], [3, 4, 5], [6]])
    def test_chunk_4(self): self.assertEqual(chunk([1, 2, 3, 4], 2), [[1, 2], [3, 4]])
    def test_dedupe_1(self): self.assertEqual(dedupe([5, 5, 4, 5, 3]), [5, 4, 3])
    def test_dedupe_2(self): self.assertEqual(dedupe([]), [])
    def test_dedupe_3(self): self.assertEqual(dedupe(["b", "a", "b"]), ["b", "a"])
    def test_dedupe_4(self): self.assertEqual(dedupe([1, 2, 3]), [1, 2, 3])
    def test_median_1(self): self.assertEqual(median([1, 2, 3, 4]), 2.5)
    def test_median_2(self): self.assertEqual(median([9]), 9)
    def test_median_3(self): self.assertEqual(median([5, 1, 3]), 3)
    def test_median_4(self): self.assertEqual(median([10, 20]), 15)
    def test_roman_1(self): self.assertEqual(to_roman(4), "IV")
    def test_roman_2(self): self.assertEqual(to_roman(9), "IX")
    def test_roman_3(self): self.assertEqual(to_roman(444), "CDXLIV")
    def test_roman_4(self): self.assertEqual(to_roman(3999), "MMMCMXCIX")


if __name__ == "__main__":
    unittest.main()
'''
TOTAL = 24
MODULES = {1: "mini/text.py and mini/timeparse.py (slugify, parse_duration)", 2: "mini/listutil.py (chunk, dedupe)", 3: "mini/stats.py and mini/roman.py (median, to_roman)"}


def make_task(d):
    r = os.path.join(d, "repo")
    for p, c in FILES.items():
        os.makedirs(os.path.dirname(os.path.join(r, p)), exist_ok=True)
        open(os.path.join(r, p), "w").write(c)
    g = lambda *a: subprocess.run(["git", "-C", r, *a], check=True, capture_output=True)
    g("init", "-q", "-b", "main"); g("add", "-A")
    g("-c", "user.name=owner", "-c", "user.email=o@x", "commit", "-qm", "tripwire base")
    os.makedirs(os.path.join(d, "hidden"), exist_ok=True)
    open(os.path.join(d, "hidden", "test_hidden.py"), "w").write(HIDDEN)
    return r


def score_tree(tree, hidden_dir):
    """Run the hidden tests against a directory holding package mini. Returns (passed, total). No network."""
    r = subprocess.run(["docker", "run", "--rm", "--network", "none", "-v", f"{tree}:/solution:ro", "-v", f"{hidden_dir}:/hidden:ro", "-w", "/hidden",
                        "python:3.12-alpine", "python", "-m", "unittest", "test_hidden", "-v"], capture_output=True, text=True, timeout=120)
    out = r.stderr + r.stdout
    return sum(1 for ln in out.splitlines() if ln.rstrip().endswith("... ok")), TOTAL
