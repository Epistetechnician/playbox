import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common  # noqa: E402
import compose  # noqa: E402

BASE = "\n".join(f"line{i}" for i in range(1, 21)) + "\n"


def edit(text, n, new):
    return text.replace(f"line{n}\n", new + "\n")


def test_disjoint_edits_both_land():
    r = compose.compose(BASE, [("a", edit(BASE, 2, "A")), ("b", edit(BASE, 18, "B"))])
    assert r["merged"] == ["a", "b"] and not r["skipped"]
    assert "A\n" in r["text"] and "B\n" in r["text"]


def test_conflict_is_skipped_not_fatal():
    r = compose.compose(BASE, [("a", edit(BASE, 5, "A")), ("b", edit(BASE, 5, "B")), ("c", edit(BASE, 19, "C"))])
    assert r["merged"] == ["a", "c"]
    assert r["skipped"] == [{"name": "b", "why": "conflict"}]
    assert "B\n" not in r["text"]


def test_noop_candidate_skipped():
    assert compose.compose(BASE, [("a", BASE)])["merged"] == []


def test_custom_merger_is_used():
    r = compose.compose(BASE, [("a", "x")], merger=lambda b, c, t: c + t)
    assert r["text"] == BASE + "x"


def test_receipt_is_signed_and_chained(tmp_path, monkeypatch):
    monkeypatch.setattr(common, "STATE", tmp_path)
    base, a, b = tmp_path / "base.py", tmp_path / "a.py", tmp_path / "b.py"
    base.write_text(BASE)
    a.write_text(edit(BASE, 2, "A"))
    b.write_text(edit(BASE, 18, "B"))
    log = tmp_path / "c.jsonl"
    out = tmp_path / "out.py"
    body = compose.compose_files(base, [a, b], out, log)
    compose.compose_files(base, [a], tmp_path / "out2.py", log)
    assert common.verify_log(log) == 2
    assert body["domain"] == "playbox.compose.v1"
    assert set(body["parents"]) == {str(a), str(b)}
    assert json.loads(log.read_text().splitlines()[0])["body"]["candidate"] == common.sha256_bytes(out.read_bytes())
    assert json.loads(log.read_text().splitlines()[0])["body"]["domain"] == "playbox.compose.v1"


def test_crlf_raw_bytes_and_duplicate_candidates(tmp_path, monkeypatch):
    monkeypatch.setattr(common, "STATE", tmp_path)
    crlf_base = "\r\n".join(f"line{i}" for i in range(1, 10)) + "\r\n"
    base, a = tmp_path / "base.py", tmp_path / "a.py"
    base.write_bytes(crlf_base.encode("utf-8"))
    a.write_bytes(crlf_base.replace("line2\r\n", "A\r\n").encode("utf-8"))
    out = tmp_path / "out.py"
    log = tmp_path / "c.jsonl"
    body = compose.compose_files(base, [a, a], out, log)
    assert body["base"] == common.sha256_bytes(base.read_bytes())
    assert body["parents"] == {str(a): common.sha256_bytes(a.read_bytes())}
    assert body["candidate"] == common.sha256_bytes(out.read_bytes())
    assert any(s["why"] == "duplicate candidate" for s in body["skipped"])
