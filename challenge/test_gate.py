"""Tests for sandboxed candidate execution in challenge/gate.py."""
from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gate  # noqa: E402


def test_scrubbed_env_never_exposes_real_home(tmp_path):
    env = gate.scrubbed_env(tmp_path)
    assert env["HOME"] == str(tmp_path.resolve())
    assert env["HOME"] != str(Path.home())
    assert env["TMPDIR"] == str(tmp_path.resolve())


def test_build_bench_invocation_fails_closed_without_sandbox(monkeypatch, tmp_path):
    cand = tmp_path / "cand.py"
    cand.write_text("x = 1\n")
    mdir = tmp_path / "model"
    mdir.mkdir()

    monkeypatch.setenv("PLAYBOX_GATE_SANDBOX", "docker")
    monkeypatch.delenv("PLAYBOX_GATE_IMAGE", raising=False)
    monkeypatch.delenv("PLAYBOX_ALLOW_UNSAFE_GATE", raising=False)
    with pytest.raises(RuntimeError, match="PLAYBOX_GATE_IMAGE"):
        gate.build_bench_invocation(cand, mdir, 7, tmp_path)

    monkeypatch.setenv("PLAYBOX_GATE_SANDBOX", "unsafe")
    with pytest.raises(RuntimeError, match="refusing unsandboxed"):
        gate.build_bench_invocation(cand, mdir, 7, tmp_path)

    monkeypatch.setenv("PLAYBOX_ALLOW_UNSAFE_GATE", "1")
    cmd, env, rlim = gate.build_bench_invocation(cand, mdir, 7, tmp_path)
    assert cmd[0] == sys.executable
    assert env["HOME"] == str(tmp_path.resolve())
    assert rlim is True


def test_bwrap_and_docker_commands_isolate_network_and_state(monkeypatch, tmp_path):
    cand = tmp_path / "cand.py"
    cand.write_text("x = 1\n")
    mdir = tmp_path / "model"
    mdir.mkdir()
    th = tmp_path / "home"
    th.mkdir()

    monkeypatch.setattr(gate.shutil, "which", lambda name: "/usr/bin/docker" if name == "docker" else None)
    monkeypatch.setenv("PLAYBOX_GATE_SANDBOX", "docker")
    monkeypatch.setenv("PLAYBOX_GATE_IMAGE", "playbox-eval:test")
    cmd, _, rlim = gate.build_bench_invocation(cand, mdir, 11, th)
    assert cmd[:4] == ["docker", "run", "--rm", "--network=none"]
    assert "--read-only" in cmd
    assert "--cap-drop=ALL" in cmd
    assert rlim is False

    bcmd = gate._bwrap_cmd(["python3", "bench.py"], cand, mdir, th)
    assert bcmd[0] == "bwrap"
    assert "--unshare-all" in bcmd
    assert "--die-with-parent" in bcmd


def _hf_style_snapshot(tmp_path):
    blobs = tmp_path / "models--x" / "blobs"
    snap = tmp_path / "models--x" / "snapshots" / "rev1"
    blobs.mkdir(parents=True)
    snap.mkdir(parents=True)
    (blobs / "abc").write_text("WEIGHTS\n")
    (snap / "model.safetensors").symlink_to("../../blobs/abc")
    return snap, blobs


def test_gate_docs_is_the_corpus_bench_reads():
    bench_root = (Path(gate.B) / "bench.py").resolve().parent.parent.parent
    assert gate.DOCS.resolve() == (bench_root / "docs").resolve()
    assert any(gate.DOCS.glob("*.md"))


def test_sandboxes_expose_docs_and_symlinked_model_blobs(monkeypatch, tmp_path):
    cand = tmp_path / "cand.py"
    cand.write_text("x = 1\n")
    snap, blobs = _hf_style_snapshot(tmp_path)
    th = tmp_path / "home"
    th.mkdir()

    bcmd = gate._bwrap_cmd(["python3", "bench.py"], cand, snap, th)
    binds = {bcmd[i + 1] for i, a in enumerate(bcmd) if a == "--ro-bind"}
    assert str(gate.DOCS.resolve()) in binds
    assert str(blobs.resolve()) in binds

    assert str(blobs.resolve()) in gate._seatbelt_profile(cand, snap, th)

    dcmd = gate._docker_cmd(cand, snap, 3, "img")
    assert f"{gate.DOCS.resolve()}:/repo/docs:ro" in dcmd
    assert f"{snap.resolve().parent.parent}:/model:ro" in dcmd
    assert dcmd[dcmd.index("--model") + 1] == "/model/snapshots/rev1"


def test_symlink_hops_follow_symlinked_parent_dirs(tmp_path):
    real = tmp_path / "py-3.12.1" / "bin"
    real.mkdir(parents=True)
    (real / "python").write_text("")
    (tmp_path / "py-3.12").symlink_to("py-3.12.1")
    (tmp_path / "venv").mkdir()
    (tmp_path / "venv" / "python").symlink_to(tmp_path / "py-3.12" / "bin" / "python")
    hops = gate._symlink_hops(str(tmp_path / "venv" / "python"))
    assert [link for _, link in hops] == [tmp_path / "venv" / "python", tmp_path / "py-3.12"]


@pytest.mark.skipif(not (sys.platform.startswith("linux") and shutil.which("bwrap")), reason="needs bubblewrap on Linux")
def test_bwrap_sandbox_can_read_docs_and_symlinked_model(tmp_path):
    cand = tmp_path / "cand.py"
    cand.write_text("x = 1\n")
    snap, _ = _hf_style_snapshot(tmp_path)
    th = tmp_path / "home"
    th.mkdir()
    probe = (
        "import pathlib, sys; "
        f"assert any(pathlib.Path({str(gate.DOCS.resolve())!r}).glob('*.md')), 'docs hidden'; "
        f"assert pathlib.Path({str(snap / 'model.safetensors')!r}).read_text() == 'WEIGHTS\\n', 'model hidden'; "
        "print('OK')"
    )
    cmd = gate._bwrap_cmd([sys.executable, "-c", probe], cand, snap, th)
    r = subprocess.run(cmd, capture_output=True, text=True, env=gate.scrubbed_env(th))
    if r.returncode != 0 and ("Operation not permitted" in r.stderr or "No permissions" in r.stderr):
        pytest.skip("bwrap cannot create namespaces here: " + r.stderr.strip())
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "OK"


@pytest.mark.skipif(sys.platform != "darwin", reason="macOS sandbox-exec test")
def test_macos_sandbox_exec_blocks_network_state_and_outside_writes(tmp_path):
    cand = tmp_path / "cand.py"
    cand.write_text("x = 1\n")
    mdir = tmp_path / "model"
    mdir.mkdir()
    th = tmp_path / "home"
    th.mkdir()
    secret_file = tmp_path / "host_secret.txt"
    secret_file.write_text("TOP_SECRET\n")
    outside_target = tmp_path / "outside_write.txt"

    prof = gate._seatbelt_profile(cand, mdir, th)
    env = gate.scrubbed_env(th)
    probe = f"""
import os, socket, pathlib
assert os.environ["HOME"] == {str(th.resolve())!r}
blocked_read = False
try:
    pathlib.Path({str(secret_file.resolve())!r}).read_text()
except PermissionError:
    blocked_read = True
assert blocked_read, "sandbox allowed reading arbitrary host file"
blocked_write = False
try:
    pathlib.Path({str(outside_target.resolve())!r}).write_text("pwn")
except PermissionError:
    blocked_write = True
assert blocked_write, "sandbox allowed writing outside tmp_home"
blocked_net = False
try:
    s = socket.socket()
    s.settimeout(1.0)
    s.connect(("127.0.0.1", 80))
except PermissionError:
    blocked_net = True
except OSError:
    blocked_net = True
assert blocked_net, "sandbox allowed network socket"
print("SANDBOX_OK")
"""
    r = subprocess.run(
        ["/usr/bin/sandbox-exec", "-p", prof, sys.executable, "-c", probe],
        env=env,
        cwd=str(th),
        capture_output=True,
        text=True,
    )
    assert r.returncode == 0, r.stderr + r.stdout
    assert "SANDBOX_OK" in r.stdout
