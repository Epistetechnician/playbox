"""The live room's gate (room_gate.py): what it refuses without a receipt, and that it never lets main drift from the frontier.

The harness and the `playbox review` step are replaced by stand-ins here; the real ones run in scripts/demo_live.py.
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(HERE))
import room_gate as G  # noqa: E402

PENDING = "refs/playbox/members/abc/try"


def git(repo, *a, **kw):
    return subprocess.run(["git", "-c", "user.email=t@t", "-c", "user.name=t", *a], cwd=repo, check=True, capture_output=True, text=True, **kw).stdout.strip()


@pytest.fixture()
def repo(tmp_path):
    r = tmp_path / "repo"
    r.mkdir()
    git(r, "init", "-q", "-b", "main")
    (r / "candidate.py").write_text("LR = 1e-3\n")
    (r / "harness.py").write_text("# host's\n")
    git(r, "add", ".")
    git(r, "commit", "-qm", "base")
    return r


def push(repo, files: dict, base="main", ref=PENDING):
    git(repo, "checkout", "-q", "-b", "tmp", base)
    for name, text in files.items():
        (repo / name).write_text(text)
    git(repo, "add", ".")
    git(repo, "commit", "-qm", "member")
    git(repo, "update-ref", ref, "HEAD")
    git(repo, "checkout", "-q", "main")
    git(repo, "branch", "-q", "-D", "tmp")


def test_a_push_that_changes_only_the_candidate_is_scored(repo):
    push(repo, {"candidate.py": "LR = 3e-3\n"})
    assert G.screen(repo, PENDING) is None


def test_a_push_that_touches_another_file_is_refused(repo):
    push(repo, {"candidate.py": "LR = 3e-3\n", "harness.py": "# mine\n"})
    assert "may change only candidate.py" in G.screen(repo, PENDING)


def test_a_push_that_changes_nothing_is_refused(repo):
    git(repo, "update-ref", PENDING, "main")
    assert "nothing" in G.screen(repo, PENDING)


def test_a_push_not_built_on_the_tip_of_main_is_refused(repo):
    push(repo, {"candidate.py": "LR = 3e-3\n"})
    (repo / "candidate.py").write_text("LR = 2e-3\n")
    git(repo, "commit", "-qam", "main moved")
    assert "not built on the tip of main" in G.screen(repo, PENDING)


def test_an_oversized_candidate_is_refused(repo):
    push(repo, {"candidate.py": "#" * (G.MAX_BYTES + 1) + "\n"})
    assert "bytes" in G.screen(repo, PENDING)


def test_a_refused_push_is_retired_and_never_scored(repo, monkeypatch):
    push(repo, {"harness.py": "# mine\n"})
    calls = []
    monkeypatch.setattr(G, "review", lambda pb, r, verb, ref: calls.append((verb, ref)))
    monkeypatch.setattr(G.harness, "submit", lambda *a, **k: pytest.fail("a refused push must not be scored"))
    assert G.handle(Path("w"), repo, "pb", PENDING, "h", lambda s: None) is None
    assert calls == [("reject", PENDING)]


def test_a_rejection_retires_the_push_and_main_is_untouched(repo, monkeypatch):
    push(repo, {"candidate.py": "LR = 3e-3\n"})
    calls = []
    monkeypatch.setattr(G, "review", lambda pb, r, verb, ref: calls.append(verb))
    monkeypatch.setattr(G.harness, "submit", lambda w, c, author, log: {"verdict": "reject"})
    before = git(repo, "rev-parse", "main")
    assert G.handle(Path("w"), repo, "pb", PENDING, "h", lambda s: None) == {"verdict": "reject"}
    assert calls == ["reject"] and git(repo, "rev-parse", "main") == before


def test_the_gate_stops_if_main_would_drift_from_the_frontier(repo, monkeypatch):
    push(repo, {"candidate.py": "LR = 3e-3\n"})
    monkeypatch.setattr(G, "review", lambda pb, r, verb, ref: git(r, "merge", "-q", "--ff-only", ref) if verb == "accept" else None)
    monkeypatch.setattr(G.harness, "submit", lambda w, c, author, log: {"verdict": "promote"})
    monkeypatch.setattr(G.harness, "_state", lambda w: {"frontier": [{"sha256": "0" * 64}]})
    with pytest.raises(RuntimeError, match="not the harness frontier"):
        G.handle(Path("w"), repo, "pb", PENDING, "h", lambda s: None)


def test_ast_equivalent_and_syntax_broken_pushes_are_refused_without_holdout(repo, tmp_path, monkeypatch):
    # 1. Comment/docstring/formatting-only edit is AST-equivalent to main -> refused pre-holdout
    push(repo, {"candidate.py": '"""docstring."""\n# comment\nLR = 0.001\n'})
    assert "AST-equivalent to the current frontier" in (G.screen(repo, PENDING) or "")

    # 2. Syntax-broken candidate pushed by an untrusted guest (without local hooks) is refused pre-holdout
    git(repo, "checkout", "-q", "-b", "tmp_bad", "main")
    (repo / "candidate.py").write_text("def broken(:\n    pass\n")
    git(repo, "add", ".")
    git(repo, "-c", "core.hooksPath=/dev/null", "commit", "-qm", "guest_bad_syntax")
    git(repo, "update-ref", PENDING, "HEAD")
    git(repo, "checkout", "-q", "main")
    git(repo, "branch", "-q", "-D", "tmp_bad")
    assert "syntax error" in (G.screen(repo, PENDING) or "")

    # 3. Second member pushing an AST-equivalent variant of an already-evaluated candidate is refused pre-holdout
    work = tmp_path / "work"
    work.mkdir()
    scored_calls = []
    monkeypatch.setattr(G, "review", lambda pb, r, verb, ref: None)
    monkeypatch.setattr(
        G.harness,
        "submit",
        lambda w, c, author, log: (scored_calls.append(author) or {"verdict": "reject"}),
    )

    push(repo, {"candidate.py": "def step(x):\n    return x * 2\n"}, ref="refs/playbox/members/alice/v1")
    assert G.handle(work, repo, "pb", "refs/playbox/members/alice/v1", "h1", lambda s: None) == {
        "verdict": "reject"
    }
    assert len(scored_calls) == 1

    push(
        repo,
        {"candidate.py": 'def step(x):\n    """Same AST."""\n    # extra note\n    return (x * 2)\n'},
        ref="refs/playbox/members/bob/v2",
    )
    assert G.handle(work, repo, "pb", "refs/playbox/members/bob/v2", "h2", lambda s: None) is None
    assert len(scored_calls) == 1  # zero extra GPU or holdout evaluations spent!

