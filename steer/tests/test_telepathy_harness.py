"""Harness plumbing: process-group timeout, answer parsing, retry-only-on-infra, statistics, resume merge."""
import importlib.util
import json
import os
import time
from pathlib import Path

import pytest

from playbox_steer import telepathy_harness as H

TOPICS = ["ocean", "space", "music"]


def test_timeout_kills_the_whole_group(tmp_path):
    marker = tmp_path / "alive"
    t = time.time()
    with pytest.raises(H.InfraError, match="timed out"):
        H.run_agent(["sh", "-c", f"(sleep 3; touch {marker}) & sleep 30"], tmp_path, timeout=1)
    assert time.time() - t < 5
    time.sleep(3.5)
    assert not marker.exists()  # the background child died with its group


def test_run_agent_returns_output(tmp_path):
    assert H.run_agent(["sh", "-c", "echo hi; echo oops >&2; exit 3"], tmp_path, 10) == (3, "hi\n", "oops\n")


@pytest.mark.parametrize("text,want", [
    ("blah\nANSWER: ocean", "ocean"), ("**ANSWER: Space.**", "space"), ("ANSWER: `none`", "none"),
    ("ANSWER: ocean\nlater\nANSWER: music", "music"), ("ANSWER: pizza", None), ("no answer here", None), ("", None)])
def test_parse_answer(text, want):
    assert H.parse_answer(text, TOPICS) == want


def test_vector_ok(tmp_path):
    f = tmp_path / "v.pt"
    assert not H.vector_ok(f)
    f.write_bytes(b"x" * 10)
    assert not H.vector_ok(f)
    f.write_bytes(b"x" * 4000)
    assert H.vector_ok(f)


def test_wilson_and_binomial():
    lo, hi = H.wilson(50, 88)
    assert 0.46 < lo < 0.47 and 0.66 < hi < 0.67
    assert H.wilson(0, 0) == (0.0, 1.0)
    assert H.binom_sf(0, 10, 0.3) == pytest.approx(1.0)
    assert H.binom_sf(10, 10, 0.5) == pytest.approx(0.5**10)
    assert H.binom_sf(50, 88, 1 / 21) < 1e-30


def _r(i, truth, guess, src="a", dst="b", **kw):
    return {"i": i, "src": src, "dst": dst, "truth": truth, "guess": guess, "ok": guess == truth, **kw}


def test_summarize_excludes_infra_and_splits_controls():
    rs = [_r(0, "ocean", "ocean"), _r(1, "space", "music"), _r(2, "none", "none"), _r(3, "none", "ocean"),
          {"i": 4, "src": "a", "dst": "b", "truth": "ocean", "error": "agent timed out after 600 s", "ok": False}]
    s = H.summarize(rs, 3)
    assert s["concept"]["n"] == 2 and s["concept"]["correct"] == 1
    assert s["control"]["n"] == 2 and s["false_positive_rate"] == 0.5
    assert s["infra_failures"] == 1 and s["chance"] == 0.25


def test_merge_keeps_clean_over_error():
    old = [_r(0, "ocean", "ocean"), {"i": 1, "error": "x", "ok": False}]
    new = [{"i": 0, "error": "y", "ok": False}, _r(1, "space", "space")]
    m = H.merge_results(old, new)
    assert [("error" in r) for r in m] == [False, False]


def _driver(monkeypatch, tmp_path):
    monkeypatch.setenv("TELEPATHY_OUT", str(tmp_path / "o"))
    monkeypatch.setenv("TELEPATHY_AUTOSTART", "0")
    monkeypatch.setenv("TELEPATHY_CONCEPTS", ",".join(TOPICS))
    monkeypatch.setenv("TELEPATHY_ATTEMPTS", "3")
    spec = importlib.util.spec_from_file_location("telepathy_hermes", Path(__file__).resolve().parents[1] / "tools/telepathy_hermes.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    monkeypatch.setattr(m.time, "sleep", lambda *_: None)
    monkeypatch.setattr(m.H, "daemon_up", lambda *_: True)
    monkeypatch.setattr(m.H, "wait_for_memory", lambda *_: None)
    return m


def test_trial_retries_infra_but_never_a_wrong_answer(monkeypatch, tmp_path):
    m = _driver(monkeypatch, tmp_path)
    calls = []

    def fake(prompt, name, cwd=None):
        calls.append(name)
        if name.startswith("A_"):
            if name == "A_0":  # first attempt: agent says 'sent' but writes nothing
                return "sent"
            Path(prompt.split("--out ")[1].split()[0]).write_bytes(b"x" * 4000)
            return "sent"
        return "ANSWER: music" if name == "B_0.1" else "ANSWER: ocean"

    monkeypatch.setattr(m, "hermes", fake)
    r = m.trial(0, "a", "b", "ocean")
    # attempt 1: no vector -> retried; attempt 2: valid answer 'music' (wrong) is kept, not retried
    assert r["guess"] == "music" and r["ok"] is False and r["attempts"] == 2
    assert calls == ["A_0", "A_0.1", "B_0.1"]


def test_trial_gives_up_as_infra(monkeypatch, tmp_path):
    m = _driver(monkeypatch, tmp_path)
    monkeypatch.setattr(m, "hermes", lambda *a, **k: "sent")
    with pytest.raises(m.H.InfraError, match="no vector"):
        m.trial(1, "a", "b", "ocean")


def test_stale_vector_does_not_pass(monkeypatch, tmp_path):
    m = _driver(monkeypatch, tmp_path)
    stale = m.SHARED / "t2" / "thought.pt"
    stale.parent.mkdir(parents=True)
    stale.write_bytes(b"x" * 4000)
    monkeypatch.setattr(m, "hermes", lambda *a, **k: "sent")
    with pytest.raises(m.H.InfraError, match="no vector"):
        m.trial(2, "a", "b", "space")
