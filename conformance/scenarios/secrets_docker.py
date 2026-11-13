"""A task reads a host-held secret at /run/secrets; the guest never sees the value elsewhere; the audit log has name and hash only."""
import json
import os
import stat
import subprocess

from harness import Skip

TIER = "full"


def _docker_ready():
    try:
        r = subprocess.run(["docker", "image", "inspect", "alpine:3"], capture_output=True, timeout=20)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return r.returncode == 0


def run(ctx):
    if not _docker_ready():
        raise Skip("docker with alpine:3 is not available")
    secrets = ctx.dir("secrets")
    os.chmod(secrets, stat.S_IRWXU)
    (secrets / "API_KEY").write_text("s3cr3t-conformance")
    (secrets / "OTHER").write_text("not-granted")
    repo = ctx.dir("repo")
    for cmd in (["init", "-q", "-b", "main"], ["config", "user.email", "t@e"], ["config", "user.name", "t"]):
        subprocess.run(["git", "-C", str(repo), *cmd], check=True)
    (repo / "a.txt").write_text("a\n")
    subprocess.run(["git", "-C", str(repo), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-q", "-m", "init"], check=True)

    host = ctx.mcp("host", PLAYBOX_SECRETS_DIR=str(secrets))
    created = host.ok("playbox_create", transport="local", name="sec", repo=str(repo), runtime="docker", allowed_secrets=["API_KEY"])
    guest = ctx.mcp("guest")
    guest.ok("playbox_join", link=created["link"], workspace=str(ctx.dir("ws")))

    out = guest.ok("playbox_delegate", base="main", command="cat /run/secrets/API_KEY; echo; ls /run/secrets", secrets=["API_KEY"], timeout_secs=60)
    assert "s3cr3t-conformance" in json.dumps(out), out
    assert "OTHER" not in out.get("stdout", ""), "only the named secret is shown"
    assert "not authorized" in guest.refused("playbox_delegate", base="main", command="true", secrets=["OTHER"])

    log = json.dumps(host.ok("playbox_audit", tail=100))
    assert "API_KEY" in log, "the audit log names the secret"
    assert "s3cr3t-conformance" not in log, "and never holds its value"
