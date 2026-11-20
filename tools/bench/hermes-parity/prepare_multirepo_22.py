#!/usr/bin/env python3
"""Materialize and gold-calibrate 22 multi-repo evaluation tasks (`n = 28 + 22 = 50` total):
  - 12 Non-Django Python SWE-bench Verified tasks:
      * 1 `pallets/flask` (`flask`, `pallets__flask-4045`)
      * 4 `pytest-dev/pytest` (`pytest-10051`, `pytest-10081`, `pytest-10356`, `pytest-8399`)
      * 4 `sympy/sympy` (`sympy-20154`, `sympy-20428`, `sympy-20590`, `sympy-20801`)
      * 3 `scikit-learn/scikit-learn` (`scikit-learn-25102`, `scikit-learn-25232`, `scikit-learn-25747`)
  - 5 Rust Cargo crate bugfix tasks (`rust-01` .. `rust-05`, `pb-swe-polyglot:arm64`, `cargo test`)
  - 5 TypeScript Node 26 native-TS bugfix tasks (`ts-01` .. `ts-05`, `pb-swe-polyglot:arm64`, `node --test`)

Every task produces a single-commit base archive (`pb_host_tar`, zero `.git` history leak) and is
calibrated via `swe/score.py <inst> calibrate` (`resolved == True` on gold patch, `False` on base).
"""
from __future__ import annotations

import glob
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import pyarrow.parquet as pq

HERE = Path(__file__).resolve().parent
SWE_DIR = Path(os.environ.get("PB_SWE_DIR", os.path.expanduser("~/.playbox-hermes/openshell-parity/swe")))
TARS_DIR = SWE_DIR / "multirepo-tars"
HF_HUB = Path.home() / ".cache/huggingface/hub/datasets--princeton-nlp--SWE-bench_Verified"

PY_SWE_TARGETS = [
    ("pytest-dev/pytest", "pytest-dev__pytest-10051", "pytest-10051"),
    ("pytest-dev/pytest", "pytest-dev__pytest-10081", "pytest-10081"),
    ("pytest-dev/pytest", "pytest-dev__pytest-10356", "pytest-10356"),
    ("sympy/sympy", "sympy__sympy-22080", "sympy-22080"),
    ("sympy/sympy", "sympy__sympy-22456", "sympy-22456"),
    ("sympy/sympy", "sympy__sympy-22714", "sympy-22714"),
    ("sympy/sympy", "sympy__sympy-22914", "sympy-22914"),
    ("scikit-learn/scikit-learn", "scikit-learn__scikit-learn-25102", "scikit-learn-25102"),
    ("scikit-learn/scikit-learn", "scikit-learn__scikit-learn-25232", "scikit-learn-25232"),
    ("scikit-learn/scikit-learn", "scikit-learn__scikit-learn-25747", "scikit-learn-25747"),
    ("sympy/sympy", "sympy__sympy-23262", "sympy-23262"),
]


def _fetch_single_commit_tar(repo_slug: str, base_commit: str, out_tar: Path, *, overlay_sklearn_so: bool = False) -> None:
    if out_tar.is_file() and out_tar.stat().st_size > 10_000:
        return
    out_tar.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="pbx-fetch-") as td:
        rdir = Path(td) / "repo"
        rdir.mkdir()
        subprocess.run(["git", "init", "-q", str(rdir)], check=True)
        subprocess.run(
            ["git", "-C", str(rdir), "fetch", "--depth=1", f"https://github.com/{repo_slug}.git", base_commit],
            check=True,
            capture_output=True,
            timeout=180,
        )
        subprocess.run(["git", "-C", str(rdir), "checkout", "-q", "FETCH_HEAD"], check=True)
        shutil.rmtree(rdir / ".git", ignore_errors=True)
        if repo_slug == "pytest-dev/pytest":
            ver_py = rdir / "src" / "_pytest" / "_version.py"
            if ver_py.parent.is_dir() and not ver_py.exists():
                ver_py.write_text('version = "7.2.0.dev0"\nversion_tuple = (7, 2, 0, "dev0")\n', encoding="utf-8")
        if overlay_sklearn_so:
            # Extract prebuilt arm64 `.so` extensions from `pb-swe-polyglot:arm64` into `sklearn/` so pure-Python edits run natively.
            so_tar = Path(td) / "sklearn_so.tar"
            with so_tar.open("wb") as f_so:
                subprocess.run(
                    [
                        "docker", "run", "--rm", "--network", "none", "--entrypoint", "bash",
                        "pb-swe-polyglot:arm64", "-c",
                        "cd /usr/local/venv/lib/python3.10/site-packages && find sklearn scikit_learn.libs -name '*.so*' | tar -cf - -T -",
                    ],
                    stdout=f_so,
                    check=True,
                )
            subprocess.run(["tar", "-xf", str(so_tar), "-C", str(rdir)], check=True)
        subprocess.run(
            ["tar", "--no-xattrs", "-cf", str(out_tar), "-C", str(rdir), "."],
            env={**os.environ, "COPYFILE_DISABLE": "1"},
            check=True,
        )


def _prepare_python_swe_tasks() -> list[str]:
    pfiles = glob.glob(str(HF_HUB / "**/*.parquet"), recursive=True)
    rows = {r["instance_id"]: r for r in pq.read_table(pfiles[0]).to_pylist()}
    created: list[str] = ["flask"]

    for repo_slug, iid, short_name in PY_SWE_TARGETS:
        r = dict(rows[iid])
        tar_path = TARS_DIR / f"{short_name}.tar"
        is_sk = repo_slug == "scikit-learn/scikit-learn"
        _fetch_single_commit_tar(repo_slug, r["base_commit"], tar_path, overlay_sklearn_so=is_sk)

        f2p = json.loads(r["FAIL_TO_PASS"])
        p2p = json.loads(r["PASS_TO_PASS"])
        test_files = sorted(set(re.findall(r"(?m)^diff --git a/(.+?) b/", r["test_patch"])))
        tfiles_str = " ".join(test_files) if test_files else (f2p[0].split("::")[0] if "::" in f2p[0] else "tests")

        if repo_slug == "sympy/sympy":
            # Map each bare test function name to the matching test file from `test_files`:
            with tempfile.TemporaryDirectory() as td_map:
                subprocess.run(["tar", "-xf", str(tar_path), "-C", td_map, *test_files], check=False)
                subprocess.run(["git", "init", "-q", td_map], check=True)
                subprocess.run(["git", "-C", td_map, "apply", "--whitespace=nowarn", "-"], input=r["test_patch"], text=True, check=False)
                fn_to_file: dict[str, str] = {}
                for tf in test_files:
                    tf_p = Path(td_map) / tf
                    if tf_p.is_file():
                        for m_fn in re.finditer(r"(?m)^def (test_\w+)\s*\(", tf_p.read_text(encoding="utf-8", errors="ignore")):
                            fn_to_file.setdefault(m_fn.group(1), tf)
            default_tf = test_files[0]
            f2p = [f"{fn_to_file.get(fn, default_tf)}::{fn}" if "::" not in fn else fn for fn in f2p]
            p2p = [f"{fn_to_file.get(fn, default_tf)}::{fn}" if "::" not in fn else fn for fn in p2p]

        py_path_tb = "/tmp/tb/src:/tmp/tb" if repo_slug == "pytest-dev/pytest" else "/tmp/tb"
        py_path_sb = "/sandbox/src:/sandbox" if repo_slug == "pytest-dev/pytest" else "/sandbox"

        doc = {
            "instance_id": iid,
            "repo": repo_slug,
            "base_commit": r["base_commit"],
            "problem_statement": r["problem_statement"],
            "patch": r["patch"],
            "test_patch": r["test_patch"],
            "FAIL_TO_PASS": json.dumps(f2p),
            "PASS_TO_PASS": json.dumps(p2p),
            "pb_family": short_name.split("-")[0],
            "pb_image": "pb-swe-polyglot:arm64",
            "pb_parser": "pytest",
            "pb_python": "/usr/local/venv/bin/python",
            "pb_host_tar": str(tar_path),
            "pb_pythonpath": py_path_tb,
            "pb_pythonpath_sandbox": py_path_sb,
            "pb_test_cmd": f"PYTHONPATH={py_path_tb} /usr/local/venv/bin/pytest --color=no -rA -p no:cacheprovider {tfiles_str}",
            "pb_online_cmd": f"PYTHONPATH={py_path_tb} /usr/local/venv/bin/pytest --color=no -q -p no:cacheprovider {tfiles_str}",
            "pb_test_hint": f"cd {{repo}} && PYTHONPATH={py_path_sb} {{py}} -m pytest -q {tfiles_str}",
        }
        (SWE_DIR / f"{short_name}.inst.json").write_text(json.dumps(doc, indent=2), encoding="utf-8")
        created.append(short_name)
    return created


def _make_git_patch(rel_path: str, old_text: str, new_text: str) -> str:
    with tempfile.TemporaryDirectory() as td:
        d = Path(td)
        p = d / rel_path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(old_text, encoding="utf-8")
        subprocess.run(["git", "init", "-q", str(d)], check=True)
        subprocess.run(["git", "-C", str(d), "-c", "user.name=b", "-c", "user.email=b@b", "add", "-A"], check=True)
        subprocess.run(["git", "-C", str(d), "-c", "user.name=b", "-c", "user.email=b@b", "commit", "-q", "-m", "base"], check=True)
        p.write_text(new_text, encoding="utf-8")
        subprocess.run(["git", "-C", str(d), "add", "-N", "."], check=True)
        return subprocess.check_output(["git", "-C", str(d), "diff", "HEAD"], text=True)


def _prepare_rust_tasks() -> list[str]:
    specs = [
        (
            "rust-01",
            "Bounded ring buffer wrap-around `drain_n` order & count",
            "pub struct Ring<T> { buf: Vec<Option<T>>, head: usize, len: usize }\n"
            "impl<T> Ring<T> {\n"
            "    pub fn with_capacity(cap: usize) -> Self { Self { buf: (0..cap).map(|_| None).collect(), head: 0, len: 0 } }\n"
            "    pub fn push(&mut self, item: T) {\n"
            "        let cap = self.buf.len();\n"
            "        if cap == 0 { return; }\n"
            "        if self.len < cap { let idx = (self.head + self.len) % cap; self.buf[idx] = Some(item); self.len += 1; }\n"
            "        else { self.buf[self.head] = Some(item); self.head = (self.head + 1) % cap; }\n"
            "    }\n"
            "    pub fn drain_n(&mut self, n: usize) -> Vec<T> {\n"
            "        let take = n.min(self.len);\n"
            "        let mut out = Vec::with_capacity(take);\n"
            "        for i in 0..take {\n"
            "            if let Some(v) = self.buf[i].take() { out.push(v); }\n"
            "        }\n"
            "        self.len -= take;\n"
            "        out\n"
            "    }\n"
            "}\n",
            "pub struct Ring<T> { buf: Vec<Option<T>>, head: usize, len: usize }\n"
            "impl<T> Ring<T> {\n"
            "    pub fn with_capacity(cap: usize) -> Self { Self { buf: (0..cap).map(|_| None).collect(), head: 0, len: 0 } }\n"
            "    pub fn push(&mut self, item: T) {\n"
            "        let cap = self.buf.len();\n"
            "        if cap == 0 { return; }\n"
            "        if self.len < cap { let idx = (self.head + self.len) % cap; self.buf[idx] = Some(item); self.len += 1; }\n"
            "        else { self.buf[self.head] = Some(item); self.head = (self.head + 1) % cap; }\n"
            "    }\n"
            "    pub fn drain_n(&mut self, n: usize) -> Vec<T> {\n"
            "        let cap = self.buf.len();\n"
            "        if cap == 0 { return Vec::new(); }\n"
            "        let take = n.min(self.len);\n"
            "        let mut out = Vec::with_capacity(take);\n"
            "        for _ in 0..take {\n"
            "            if let Some(v) = self.buf[self.head].take() { out.push(v); }\n"
            "            self.head = (self.head + 1) % cap;\n"
            "            self.len -= 1;\n"
            "        }\n"
            "        out\n"
            "    }\n"
            "}\n",
            "use pb_crate::Ring;\n#[test]\nfn p2p_basic_push() { let mut r = Ring::with_capacity(3); r.push(10); r.push(20); assert_eq!(r.drain_n(1), vec![10]); }\n",
            "use pb_crate::Ring;\n#[test]\nfn p2p_basic_push() { let mut r = Ring::with_capacity(3); r.push(10); r.push(20); assert_eq!(r.drain_n(1), vec![10]); }\n"
            "#[test]\nfn f2p_wrap_and_zero_cap() {\n"
            "    let mut r = Ring::with_capacity(3); r.push(1); r.push(2); r.push(3); r.push(4);\n"
            "    assert_eq!(r.drain_n(2), vec![2, 3]); r.push(5); assert_eq!(r.drain_n(5), vec![4, 5]);\n"
            "    let mut z: Ring<i32> = Ring::with_capacity(0); z.push(1); assert!(z.drain_n(2).is_empty());\n"
            "}\n",
            "`Ring::drain_n(n)` indexes `self.buf[0..take]` instead of advancing `self.head` modulo capacity after overwrite/previous drains, corrupting FIFO order on wrap-around.",
        ),
        (
            "rust-02",
            "HTTP header parser obs-fold continuation & colon whitespace",
            "pub fn parse_headers(raw: &str) -> Result<Vec<(String, String)>, &'static str> {\n"
            "    let mut out = Vec::new();\n"
            "    for line in raw.lines() {\n"
            "        if line.is_empty() { break; }\n"
            "        let (k, v) = line.split_once(':').ok_or(\"malformed\")?;\n"
            "        out.push((k.to_ascii_lowercase(), v.trim().to_string()));\n"
            "    }\n"
            "    Ok(out)\n"
            "}\n",
            "pub fn parse_headers(raw: &str) -> Result<Vec<(String, String)>, &'static str> {\n"
            "    let mut out: Vec<(String, String)> = Vec::new();\n"
            "    for line in raw.split(\"\\r\\n\").flat_map(|s| s.split('\\n')) {\n"
            "        if line.is_empty() { break; }\n"
            "        if line.starts_with(' ') || line.starts_with('\\t') {\n"
            "            let last = out.last_mut().ok_or(\"leading_fold\")?;\n"
            "            if !last.1.is_empty() { last.1.push(' '); }\n"
            "            last.1.push_str(line.trim());\n"
            "            continue;\n"
            "        }\n"
            "        let (k, v) = line.split_once(':').ok_or(\"malformed\")?;\n"
            "        if k.is_empty() || k.chars().any(char::is_whitespace) { return Err(\"bad_name\"); }\n"
            "        out.push((k.to_ascii_lowercase(), v.trim().to_string()));\n"
            "    }\n"
            "    Ok(out)\n"
            "}\n",
            "use pb_crate::parse_headers;\n#[test]\nfn p2p_simple() { assert_eq!(parse_headers(\"Host: example.com\\r\\n\\r\\n\").unwrap(), vec![(\"host\".into(), \"example.com\".into())]); }\n",
            "use pb_crate::parse_headers;\n#[test]\nfn p2p_simple() { assert_eq!(parse_headers(\"Host: example.com\\r\\n\\r\\n\").unwrap(), vec![(\"host\".into(), \"example.com\".into())]); }\n"
            "#[test]\nfn f2p_obs_fold_and_bad_key() {\n"
            "    let h = parse_headers(\"X-Multi: part1\\r\\n\\tpart2\\r\\n\\r\\n\").unwrap();\n"
            "    assert_eq!(h, vec![(\"x-multi\".into(), \"part1 part2\".into())]);\n"
            "    assert!(parse_headers(\"  leading: bad\\r\\n\").is_err());\n"
            "    assert!(parse_headers(\"Bad Key: v\\r\\n\").is_err());\n"
            "}\n",
            "`parse_headers(raw)` fails on RFC 7230 obs-fold continuation lines (starting with SP/HTAB, which should append a single space + trimmed continuation to the previous header value, or error if first line) and fails to reject header names that are empty or contain whitespace.",
        ),
        (
            "rust-03",
            "Cargo SemVer caret `^0.x.y` & `^0.0.z` compatibility matcher",
            "pub fn caret_matches(req: (u64, u64, u64), ver: (u64, u64, u64)) -> bool {\n"
            "    ver >= req && ver.0 == req.0\n"
            "}\n",
            "pub fn caret_matches(req: (u64, u64, u64), ver: (u64, u64, u64)) -> bool {\n"
            "    if ver < req { return false; }\n"
            "    if req.0 > 0 { ver.0 == req.0 }\n"
            "    else if req.1 > 0 { ver.0 == 0 && ver.1 == req.1 }\n"
            "    else { ver == req }\n"
            "}\n",
            "use pb_crate::caret_matches;\n#[test]\nfn p2p_major() { assert!(caret_matches((1, 2, 3), (1, 9, 0))); assert!(!caret_matches((1, 2, 3), (2, 0, 0))); }\n",
            "use pb_crate::caret_matches;\n#[test]\nfn p2p_major() { assert!(caret_matches((1, 2, 3), (1, 9, 0))); assert!(!caret_matches((1, 2, 3), (2, 0, 0))); }\n"
            "#[test]\nfn f2p_zero_major_and_minor() {\n"
            "    assert!(caret_matches((0, 2, 3), (0, 2, 9)));\n"
            "    assert!(!caret_matches((0, 2, 3), (0, 3, 0)));\n"
            "    assert!(caret_matches((0, 0, 3), (0, 0, 3)));\n"
            "    assert!(!caret_matches((0, 0, 3), (0, 0, 4)));\n"
            "}\n",
            "`caret_matches(req, ver)` treats `^0.2.3` as matching `0.3.0` and `^0.0.3` as matching `0.0.4`, violating Cargo SemVer caret rules (`^0.y.z` for `y > 0` requires `ver.0 == 0 && ver.1 == y`; `^0.0.z` requires exact `0.0.z`).",
        ),
        (
            "rust-04",
            "RFC 8785 Canonical JSON control-character & quote escaping",
            "pub fn escape_json_str(s: &str) -> String {\n"
            "    format!(\"\\\"{}\\\"\", s.replace('\"', \"\\\\\\\"\"))\n"
            "}\n",
            "pub fn escape_json_str(s: &str) -> String {\n"
            "    let mut out = String::from(\"\\\"\");\n"
            "    for ch in s.chars() {\n"
            "        match ch {\n"
            "            '\"' => out.push_str(\"\\\\\\\"\"),\n"
            "            '\\\\' => out.push_str(\"\\\\\\\\\"),\n"
            "            '\\u{0008}' => out.push_str(\"\\\\b\"),\n"
            "            '\\u{0009}' => out.push_str(\"\\\\t\"),\n"
            "            '\\u{000A}' => out.push_str(\"\\\\n\"),\n"
            "            '\\u{000C}' => out.push_str(\"\\\\f\"),\n"
            "            '\\u{000D}' => out.push_str(\"\\\\r\"),\n"
            "            c if (c as u32) < 0x20 => out.push_str(&format!(\"\\\\u{:04x}\", c as u32)),\n"
            "            c => out.push(c),\n"
            "        }\n"
            "    }\n"
            "    out.push('\"');\n"
            "    out\n"
            "}\n",
            "use pb_crate::escape_json_str;\n#[test]\nfn p2p_plain() { assert_eq!(escape_json_str(\"hello\"), \"\\\"hello\\\"\"); }\n",
            "use pb_crate::escape_json_str;\n#[test]\nfn p2p_plain() { assert_eq!(escape_json_str(\"hello\"), \"\\\"hello\\\"\"); }\n"
            "#[test]\nfn f2p_rfc8785_escapes() {\n"
            "    assert_eq!(escape_json_str(\"a\\\\b\\\"c\\nd\\u{0000}\\u{001f}\"), \"\\\"a\\\\\\\\b\\\\\\\"c\\\\nd\\\\u0000\\\\u001f\\\"\");\n"
            "    assert_eq!(escape_json_str(\"\\u{0008}\\t\\u{000c}\\r\"), \"\\\"\\\\b\\\\t\\\\f\\\\r\\\"\");\n"
            "}\n",
            "`escape_json_str(s)` fails to escape backslashes (`\\\\`), `\\b`, `\\t`, `\\n`, `\\f`, `\\r`, and other `U+0000..=U+001F` control characters as lowercase `\\uXXXX` per RFC 8785.",
        ),
        (
            "rust-05",
            "Saturating token-bucket rate limiter integer overflow & clock skew",
            "pub struct Bucket { cap: u64, tokens: u64, rate_per_ms: u64, last_ms: u64 }\n"
            "impl Bucket {\n"
            "    pub fn new(cap: u64, rate_per_ms: u64, now_ms: u64) -> Self { Self { cap, tokens: cap, rate_per_ms, last_ms: now_ms } }\n"
            "    pub fn try_acquire(&mut self, cost: u64, now_ms: u64) -> bool {\n"
            "        let added = (now_ms - self.last_ms) * self.rate_per_ms;\n"
            "        self.tokens = (self.tokens + added).min(self.cap);\n"
            "        self.last_ms = now_ms;\n"
            "        if self.tokens >= cost { self.tokens -= cost; true } else { false }\n"
            "    }\n"
            "}\n",
            "pub struct Bucket { cap: u64, tokens: u64, rate_per_ms: u64, last_ms: u64 }\n"
            "impl Bucket {\n"
            "    pub fn new(cap: u64, rate_per_ms: u64, now_ms: u64) -> Self { Self { cap, tokens: cap, rate_per_ms, last_ms: now_ms } }\n"
            "    pub fn try_acquire(&mut self, cost: u64, now_ms: u64) -> bool {\n"
            "        if now_ms > self.last_ms {\n"
            "            let added = (now_ms - self.last_ms).saturating_mul(self.rate_per_ms);\n"
            "            self.tokens = self.tokens.saturating_add(added).min(self.cap);\n"
            "            self.last_ms = now_ms;\n"
            "        }\n"
            "        if self.tokens >= cost { self.tokens -= cost; true } else { false }\n"
            "    }\n"
            "}\n",
            "use pb_crate::Bucket;\n#[test]\nfn p2p_acquire() { let mut b = Bucket::new(10, 1, 0); assert!(b.try_acquire(6, 0)); assert!(!b.try_acquire(6, 0)); }\n",
            "use pb_crate::Bucket;\n#[test]\nfn p2p_acquire() { let mut b = Bucket::new(10, 1, 0); assert!(b.try_acquire(6, 0)); assert!(!b.try_acquire(6, 0)); }\n"
            "#[test]\nfn f2p_skew_and_overflow() {\n"
            "    let mut b = Bucket::new(10, u64::MAX / 2, 100);\n"
            "    assert!(b.try_acquire(10, 100));\n"
            "    assert!(!b.try_acquire(1, 90)); // backward clock skew must not panic or advance last_ms backward\n"
            "    assert!(b.try_acquire(10, 200)); // huge delta * rate must saturate without overflow panic\n"
            "}\n",
            "`Bucket::try_acquire(cost, now_ms)` panics on backward clock skew (`now_ms < self.last_ms`, which should leave `tokens` and `last_ms` unchanged) and panics on integer overflow in `(now_ms - self.last_ms) * self.rate_per_ms + self.tokens`.",
        ),
    ]

    created: list[str] = []
    for name, title, buggy_lib, gold_lib, base_test, full_test, problem in specs:
        tar_path = TARS_DIR / f"{name}.tar"
        TARS_DIR.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory() as td:
            d = Path(td) / "crate"
            (d / "src").mkdir(parents=True)
            (d / "tests").mkdir(parents=True)
            (d / "Cargo.toml").write_text(
                "[package]\nname = \"pb_crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                encoding="utf-8",
            )
            (d / "src" / "lib.rs").write_text(buggy_lib, encoding="utf-8")
            (d / "tests" / "suite.rs").write_text(base_test, encoding="utf-8")
            subprocess.run(
                ["tar", "--no-xattrs", "-cf", str(tar_path), "-C", str(d), "."],
                env={**os.environ, "COPYFILE_DISABLE": "1"},
                check=True,
            )

        gold_patch = _make_git_patch("src/lib.rs", buggy_lib, gold_lib)
        test_patch = _make_git_patch("tests/suite.rs", base_test, full_test)
        f2p_name = [ln.split("fn ")[1].split("(")[0] for ln in full_test.splitlines() if ln.startswith("fn f2p_")][0]
        p2p_name = [ln.split("fn ")[1].split("(")[0] for ln in base_test.splitlines() if ln.startswith("fn p2p_")][0]

        doc = {
            "instance_id": f"playbox__{name}",
            "repo": "playbox/rust-bench",
            "base_commit": f"base-{name}",
            "problem_statement": f"{title}\n\n{problem}",
            "patch": gold_patch,
            "test_patch": test_patch,
            "FAIL_TO_PASS": json.dumps([f2p_name]),
            "PASS_TO_PASS": json.dumps([p2p_name]),
            "pb_family": "rust",
            "pb_image": "pb-swe-polyglot:arm64",
            "pb_parser": "cargo",
            "pb_python": "/usr/local/cargo/bin/cargo",
            "pb_host_tar": str(tar_path),
            "pb_test_cmd": "CARGO_TARGET_DIR=/tmp/cargo-tgt /usr/local/cargo/bin/cargo test --test suite",
            "pb_online_cmd": "CARGO_TARGET_DIR=/tmp/cargo-tgt /usr/local/cargo/bin/cargo test",
            "pb_test_hint": "cd {repo} && CARGO_TARGET_DIR=/tmp/cargo-tgt /usr/local/cargo/bin/cargo test",
        }
        (SWE_DIR / f"{name}.inst.json").write_text(json.dumps(doc, indent=2), encoding="utf-8")
        created.append(name)
    return created


def _prepare_ts_tasks() -> list[str]:
    specs = [
        (
            "ts-01",
            "Deep config merge prototype-pollution & array-clone bug",
            "export function deepMerge(base: Record<string, any>, patch: Record<string, any>): Record<string, any> {\n"
            "  const out: Record<string, any> = { ...base };\n"
            "  for (const k of Object.keys(patch)) {\n"
            "    if (typeof patch[k] === 'object' && patch[k] !== null) {\n"
            "      out[k] = deepMerge(out[k] || {}, patch[k]);\n"
            "    } else {\n"
            "      out[k] = patch[k];\n"
            "    }\n"
            "  }\n"
            "  return out;\n"
            "}\n",
            "const FORBIDDEN = new Set(['__proto__', 'constructor', 'prototype']);\n"
            "function isPlainObj(v: unknown): v is Record<string, any> {\n"
            "  return typeof v === 'object' && v !== null && !Array.isArray(v);\n"
            "}\n"
            "export function deepMerge(base: Record<string, any>, patch: Record<string, any>): Record<string, any> {\n"
            "  const out: Record<string, any> = {};\n"
            "  for (const k of Object.keys(base)) {\n"
            "    if (FORBIDDEN.has(k)) continue;\n"
            "    out[k] = Array.isArray(base[k]) ? [...base[k]] : (isPlainObj(base[k]) ? deepMerge({}, base[k]) : base[k]);\n"
            "  }\n"
            "  for (const k of Object.keys(patch)) {\n"
            "    if (FORBIDDEN.has(k)) continue;\n"
            "    const pv = patch[k];\n"
            "    if (Array.isArray(pv)) out[k] = [...pv];\n"
            "    else if (isPlainObj(pv)) out[k] = deepMerge(isPlainObj(out[k]) ? out[k] : {}, pv);\n"
            "    else out[k] = pv;\n"
            "  }\n"
            "  return out;\n"
            "}\n",
            "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { deepMerge } from '../src/index.ts';\n"
            "test('p2p_nested', () => { assert.deepEqual(deepMerge({ a: { x: 1 } }, { a: { y: 2 } }), { a: { x: 1, y: 2 } }); });\n",
            "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { deepMerge } from '../src/index.ts';\n"
            "test('p2p_nested', () => { assert.deepEqual(deepMerge({ a: { x: 1 } }, { a: { y: 2 } }), { a: { x: 1, y: 2 } }); });\n"
            "test('f2p_arrays_and_proto', () => {\n"
            "  const payload = JSON.parse('{\"__proto__\":{\"polluted\":true},\"tags\":[\"x\",\"y\"]}');\n"
            "  const res = deepMerge({ tags: ['old'] }, payload);\n"
            "  assert.equal(({} as any).polluted, undefined);\n"
            "  assert.equal(Array.isArray(res.tags), true);\n"
            "  assert.deepEqual(res.tags, ['x', 'y']);\n"
            "});\n",
            "`deepMerge(base, patch)` corrupts `Array` values into `{ '0': ... }` plain objects and fails to ignore `__proto__`, `constructor`, and `prototype` keys.",
        ),
        (
            "ts-02",
            "URL route matcher `:param` URI decoding & `*` splat capture",
            "export function matchRoute(pattern: string, path: string): Record<string, string> | null {\n"
            "  const pp = pattern.split('/').filter(Boolean);\n"
            "  const up = path.split('/').filter(Boolean);\n"
            "  if (pp.length !== up.length) return null;\n"
            "  const params: Record<string, string> = {};\n"
            "  for (let i = 0; i < pp.length; i++) {\n"
            "    if (pp[i].startsWith(':')) params[pp[i].slice(1)] = up[i];\n"
            "    else if (pp[i] !== up[i]) return null;\n"
            "  }\n"
            "  return params;\n"
            "}\n",
            "export function matchRoute(pattern: string, path: string): Record<string, string> | null {\n"
            "  const cleanPath = path.split('?')[0];\n"
            "  const pp = pattern.split('/').filter(Boolean);\n"
            "  const up = cleanPath.split('/').filter(Boolean);\n"
            "  const params: Record<string, string> = {};\n"
            "  for (let i = 0; i < pp.length; i++) {\n"
            "    if (pp[i] === '*') {\n"
            "      params['*'] = up.slice(i).map((s) => decodeURIComponent(s)).join('/');\n"
            "      return params;\n"
            "    }\n"
            "    if (i >= up.length) return null;\n"
            "    if (pp[i].startsWith(':')) {\n"
            "      try { params[pp[i].slice(1)] = decodeURIComponent(up[i]); } catch { return null; }\n"
            "    } else if (pp[i] !== up[i]) return null;\n"
            "  }\n"
            "  return pp.length === up.length ? params : null;\n"
            "}\n",
            "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { matchRoute } from '../src/index.ts';\n"
            "test('p2p_param', () => { assert.deepEqual(matchRoute('/users/:id', '/users/42'), { id: '42' }); });\n",
            "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { matchRoute } from '../src/index.ts';\n"
            "test('p2p_param', () => { assert.deepEqual(matchRoute('/users/:id', '/users/42'), { id: '42' }); });\n"
            "test('f2p_decode_query_and_splat', () => {\n"
            "  assert.deepEqual(matchRoute('/files/*', '/files/a/b%20c.txt?v=1'), { '*': 'a/b c.txt' });\n"
            "  assert.deepEqual(matchRoute('/u/:name', '/u/Ada%20Lovelace?tab=bio'), { name: 'Ada Lovelace' });\n"
            "  assert.equal(matchRoute('/u/:name', '/u/%E0%A4%A'), null);\n"
            "});\n",
            "`matchRoute(pattern, path)` fails to strip `?query` strings, fails to `decodeURIComponent` `:param` and `*` segments (returning `null` on malformed URI encoding), and does not support trailing `*` wildcard tail capture (`params['*']`).",
        ),
        (
            "ts-03",
            "TTL LRU Cache stale-entry eviction before capacity eviction",
            "export class TTLCache<K, V> {\n"
            "  private map = new Map<K, { v: V; exp: number }>();\n"
            "  private cap: number;\n"
            "  constructor(cap: number) { this.cap = cap; }\n"
            "  get(k: K, now: number): V | undefined {\n"
            "    const e = this.map.get(k);\n"
            "    if (!e || now > e.exp) return undefined;\n"
            "    return e.v;\n"
            "  }\n"
            "  set(k: K, v: V, ttlMs: number, now: number): void {\n"
            "    if (this.map.size >= this.cap && !this.map.has(k)) {\n"
            "      const first = this.map.keys().next().value;\n"
            "      if (first !== undefined) this.map.delete(first);\n"
            "    }\n"
            "    this.map.set(k, { v, exp: now + ttlMs });\n"
            "  }\n"
            "}\n",
            "export class TTLCache<K, V> {\n"
            "  private map = new Map<K, { v: V; exp: number }>();\n"
            "  private cap: number;\n"
            "  constructor(cap: number) { this.cap = cap; }\n"
            "  get(k: K, now: number): V | undefined {\n"
            "    const e = this.map.get(k);\n"
            "    if (!e) return undefined;\n"
            "    if (now > e.exp) { this.map.delete(k); return undefined; }\n"
            "    this.map.delete(k);\n"
            "    this.map.set(k, e);\n"
            "    return e.v;\n"
            "  }\n"
            "  set(k: K, v: V, ttlMs: number, now: number): void {\n"
            "    if (this.cap <= 0) return;\n"
            "    for (const [ek, ev] of this.map.entries()) {\n"
            "      if (now > ev.exp) this.map.delete(ek);\n"
            "    }\n"
            "    if (this.map.has(k)) this.map.delete(k);\n"
            "    else if (this.map.size >= this.cap) {\n"
            "      const oldest = this.map.keys().next().value;\n"
            "      if (oldest !== undefined) this.map.delete(oldest);\n"
            "    }\n"
            "    this.map.set(k, { v, exp: now + ttlMs });\n"
            "  }\n"
            "}\n",
            "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { TTLCache } from '../src/index.ts';\n"
            "test('p2p_ttl', () => { const c = new TTLCache<string, number>(2); c.set('a', 1, 50, 0); assert.equal(c.get('a', 25), 1); assert.equal(c.get('a', 60), undefined); });\n",
            "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { TTLCache } from '../src/index.ts';\n"
            "test('p2p_ttl', () => { const c = new TTLCache<string, number>(2); c.set('a', 1, 50, 0); assert.equal(c.get('a', 25), 1); assert.equal(c.get('a', 60), undefined); });\n"
            "test('f2p_purge_expired_and_lru_refresh', () => {\n"
            "  const c = new TTLCache<string, number>(2);\n"
            "  c.set('live', 1, 1000, 0);\n"
            "  c.set('short', 2, 10, 0);\n"
            "  assert.equal(c.get('live', 5), 1);\n"
            "  c.set('third', 3, 1000, 20); // 'short' is expired at t=20 so 'live' must NOT be evicted!\n"
            "  assert.equal(c.get('live', 25), 1);\n"
            "  assert.equal(c.get('third', 25), 3);\n"
            "});\n",
            "`TTLCache` fails to refresh LRU recency on `get`/`set` and evicts live entries on `set` when expired entries are still occupying `this.map` (should purge expired entries `now > exp` first and ignore `set` when `cap <= 0`).",
        ),
        (
            "ts-04",
            "Typed EventEmitter `once` re-entrancy & snapshot iteration",
            "export class Emitter {\n"
            "  private listeners = new Map<string, Array<(...a: any[]) => void>>();\n"
            "  on(ev: string, fn: (...a: any[]) => void): void {\n"
            "    const arr = this.listeners.get(ev) || []; arr.push(fn); this.listeners.set(ev, arr);\n"
            "  }\n"
            "  once(ev: string, fn: (...a: any[]) => void): void {\n"
            "    this.on(ev, (...a: any[]) => { fn(...a); this.off(ev, fn); });\n"
            "  }\n"
            "  off(ev: string, fn: (...a: any[]) => void): void {\n"
            "    const arr = this.listeners.get(ev) || [];\n"
            "    this.listeners.set(ev, arr.filter((x) => x !== fn));\n"
            "  }\n"
            "  emit(ev: string, ...args: any[]): void {\n"
            "    for (const fn of (this.listeners.get(ev) || [])) fn(...args);\n"
            "  }\n"
            "}\n",
            "export class Emitter {\n"
            "  private listeners = new Map<string, Array<{ raw: (...a: any[]) => void; call: (...a: any[]) => void }>>();\n"
            "  on(ev: string, fn: (...a: any[]) => void): void {\n"
            "    const arr = this.listeners.get(ev) || []; arr.push({ raw: fn, call: fn }); this.listeners.set(ev, arr);\n"
            "  }\n"
            "  once(ev: string, fn: (...a: any[]) => void): void {\n"
            "    let fired = false;\n"
            "    const entry = {\n"
            "      raw: fn,\n"
            "      call: (...a: any[]) => {\n"
            "        if (fired) return;\n"
            "        fired = true;\n"
            "        const cur = this.listeners.get(ev) || [];\n"
            "        this.listeners.set(ev, cur.filter((e) => e !== entry));\n"
            "        fn(...a);\n"
            "      },\n"
            "    };\n"
            "    const arr = this.listeners.get(ev) || []; arr.push(entry); this.listeners.set(ev, arr);\n"
            "  }\n"
            "  off(ev: string, fn: (...a: any[]) => void): void {\n"
            "    const arr = this.listeners.get(ev) || [];\n"
            "    this.listeners.set(ev, arr.filter((e) => e.raw !== fn && e.call !== fn));\n"
            "  }\n"
            "  emit(ev: string, ...args: any[]): void {\n"
            "    const snap = [...(this.listeners.get(ev) || [])];\n"
            "    for (const e of snap) e.call(...args);\n"
            "  }\n"
            "}\n",
            "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { Emitter } from '../src/index.ts';\n"
            "test('p2p_on_emit', () => { const e = new Emitter(); let n = 0; e.on('x', (v) => { n += v; }); e.emit('x', 3); assert.equal(n, 3); });\n",
            "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { Emitter } from '../src/index.ts';\n"
            "test('p2p_on_emit', () => { const e = new Emitter(); let n = 0; e.on('x', (v) => { n += v; }); e.emit('x', 3); assert.equal(n, 3); });\n"
            "test('f2p_once_remove_and_reentrant', () => {\n"
            "  const e = new Emitter(); let calls = 0;\n"
            "  const fn = () => { calls++; e.emit('ping'); };\n"
            "  e.once('ping', fn);\n"
            "  e.emit('ping');\n"
            "  e.emit('ping');\n"
            "  assert.equal(calls, 1);\n"
            "  let calls2 = 0; const fn2 = () => { calls2++; };\n"
            "  e.once('pong', fn2); e.off('pong', fn2); e.emit('pong');\n"
            "  assert.equal(calls2, 0);\n"
            "});\n",
            "`Emitter.once(ev, fn)` passes a wrapper function to `on()` while calling `off(ev, fn)` with the original `fn` (so `once` listeners are never removed, infinite-loop on re-entrant `emit`, and cannot be cancelled via `off(ev, fn)` before firing).",
        ),
        (
            "ts-05",
            "Bounded concurrency async pool order preservation & error rejection",
            "export async function mapLimit<T, R>(items: T[], limit: number, worker: (x: T, idx: number) => Promise<R>): Promise<R[]> {\n"
            "  const out: R[] = [];\n"
            "  for (let i = 0; i < items.length; i++) {\n"
            "    out.push(await worker(items[i], i));\n"
            "  }\n"
            "  return out;\n"
            "}\n",
            "export async function mapLimit<T, R>(items: T[], limit: number, worker: (x: T, idx: number) => Promise<R>): Promise<R[]> {\n"
            "  if (!Number.isInteger(limit) || limit < 1) throw new RangeError('limit must be >= 1');\n"
            "  if (items.length === 0) return [];\n"
            "  const out = new Array<R>(items.length);\n"
            "  let nextIdx = 0;\n"
            "  let aborted = false;\n"
            "  async function runSlot(): Promise<void> {\n"
            "    while (!aborted) {\n"
            "      const i = nextIdx++;\n"
            "      if (i >= items.length) return;\n"
            "      try { out[i] = await worker(items[i], i); }\n"
            "      catch (err) { aborted = true; throw err; }\n"
            "    }\n"
            "  }\n"
            "  const k = Math.min(limit, items.length);\n"
            "  await Promise.all(Array.from({ length: k }, () => runSlot()));\n"
            "  return out;\n"
            "}\n",
            "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { mapLimit } from '../src/index.ts';\n"
            "test('p2p_basic', async () => { assert.deepEqual(await mapLimit([1, 2, 3], 2, async (x) => x * 2), [2, 4, 6]); });\n",
            "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { mapLimit } from '../src/index.ts';\n"
            "test('p2p_basic', async () => { assert.deepEqual(await mapLimit([1, 2, 3], 2, async (x) => x * 2), [2, 4, 6]); });\n"
            "test('f2p_concurrency_and_validation', async () => {\n"
            "  let active = 0, peak = 0;\n"
            "  const res = await mapLimit([30, 10, 20, 5], 2, async (ms, idx) => {\n"
            "    active++; peak = Math.max(peak, active);\n"
            "    await new Promise((r) => setTimeout(r, ms));\n"
            "    active--; return idx * 10;\n"
            "  });\n"
            "  assert.equal(peak, 2);\n"
            "  assert.deepEqual(res, [0, 10, 20, 30]);\n"
            "  await assert.rejects(() => mapLimit([1], 0, async (x) => x), RangeError);\n"
            "});\n",
            "`mapLimit(items, limit, worker)` executes sequentially (`peak === 1`) instead of running up to `limit` tasks concurrently while preserving input index order, and fails to throw `RangeError` when `limit < 1` or non-integer.",
        ),
    ]

    created: list[str] = []
    for name, title, buggy_src, gold_src, base_test, full_test, problem in specs:
        tar_path = TARS_DIR / f"{name}.tar"
        TARS_DIR.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory() as td:
            d = Path(td) / "pkg"
            (d / "src").mkdir(parents=True)
            (d / "tests").mkdir(parents=True)
            (d / "package.json").write_text(
                json.dumps({"name": name, "version": "0.1.0", "type": "module"}, indent=2) + "\n",
                encoding="utf-8",
            )
            (d / "src" / "index.ts").write_text(buggy_src, encoding="utf-8")
            (d / "tests" / "suite.test.ts").write_text(base_test, encoding="utf-8")
            subprocess.run(
                ["tar", "--no-xattrs", "-cf", str(tar_path), "-C", str(d), "."],
                env={**os.environ, "COPYFILE_DISABLE": "1"},
                check=True,
            )

        gold_patch = _make_git_patch("src/index.ts", buggy_src, gold_src)
        test_patch = _make_git_patch("tests/suite.test.ts", base_test, full_test)
        f2p_name = [ln.split("test('")[1].split("'")[0] for ln in full_test.splitlines() if "test('f2p_" in ln][0]
        p2p_name = [ln.split("test('")[1].split("'")[0] for ln in base_test.splitlines() if "test('p2p_" in ln][0]

        doc = {
            "instance_id": f"playbox__{name}",
            "repo": "playbox/ts-bench",
            "base_commit": f"base-{name}",
            "problem_statement": f"{title}\n\n{problem}",
            "patch": gold_patch,
            "test_patch": test_patch,
            "FAIL_TO_PASS": json.dumps([f2p_name]),
            "PASS_TO_PASS": json.dumps([p2p_name]),
            "pb_family": "ts",
            "pb_image": "pb-swe-polyglot:arm64",
            "pb_parser": "node",
            "pb_python": "node",
            "pb_host_tar": str(tar_path),
            "pb_test_cmd": "node --test --test-reporter=tap tests/suite.test.ts",
            "pb_online_cmd": "node --test tests/suite.test.ts",
            "pb_test_hint": "cd {repo} && node --test tests/suite.test.ts",
        }
        (SWE_DIR / f"{name}.inst.json").write_text(json.dumps(doc, indent=2), encoding="utf-8")
        created.append(name)
    return created


def main() -> None:
    py_tasks = _prepare_python_swe_tasks()
    rs_tasks = _prepare_rust_tasks()
    ts_tasks = _prepare_ts_tasks()
    all_22 = py_tasks + rs_tasks + ts_tasks
    assert len(all_22) == 22, f"expected 22 tasks, got {len(all_22)}"

    scorer = str(HERE / "swe" / "score.py")
    print(f"Calibrating {len(all_22)} multi-repo tasks...", flush=True)
    for name in all_22:
        p_gold = subprocess.run([sys.executable, scorer, name, "calibrate"], capture_output=True, text=True, timeout=300)
        d_gold = json.loads(p_gold.stdout)
        p_none = subprocess.run([sys.executable, scorer, name, "none"], capture_output=True, text=True, timeout=300)
        d_none = json.loads(p_none.stdout)
        print(
            f"  {name:20s} gold_resolved={d_gold['resolved']} (f2p={d_gold['f2p']}/{d_gold['f2p_total']}, p2p={d_gold['p2p']}/{d_gold['p2p_total']}) "
            f"base_resolved={d_none['resolved']}",
            flush=True,
        )
        if not d_gold["resolved"] or d_none["resolved"]:
            raise RuntimeError(f"calibration failed on {name}: gold={d_gold}, base={d_none}")

    # Write canonical 22-task and 50-task manifest lists:
    django_28 = sorted(
        os.path.basename(p).split("__", 1)[1].replace(".inst.json", "")
        for p in glob.glob(str(SWE_DIR / "django__django-*.inst.json"))
    )
    assert len(django_28) == 28
    (SWE_DIR / "multirepo-22.txt").write_text("\n".join(all_22) + "\n", encoding="utf-8")
    (SWE_DIR / "verified-50.txt").write_text("\n".join(django_28 + all_22) + "\n", encoding="utf-8")
    print(f"SUCCESS: 22 multi-repo + 28 Django = {len(django_28) + len(all_22)} calibrated tasks ready!", flush=True)


if __name__ == "__main__":
    main()
