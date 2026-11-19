"""Tests for sandboxed candidate execution in challenge/gate.py."""
from __future__ import annotations

import os
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
