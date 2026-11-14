"""The command line's promises: version, completions, doctor --json, typed arguments, exit codes, quiet pipes."""
import json

TIER = "fast"
# REQ: CLI-1


def run(ctx):
    v = ctx.cli("--version")
    assert v.returncode == 0 and v.stdout.startswith("playbox "), v

    z = ctx.cli("completions", "zsh")
    assert z.returncode == 0 and z.stdout.startswith("#compdef playbox")
    assert "e1-merge" in z.stdout and "decision" in z.stdout, "typed values must reach the completions"

    d = ctx.cli("doctor", "--json")
    assert d.returncode in (0, 1), d
    rep = json.loads(d.stdout)
    assert {"version", "home", "missing", "checks"} <= rep.keys()
    assert all({"section", "check", "ok", "detail"} <= c.keys() for c in rep["checks"])
    assert (d.returncode == 1) == (rep["missing"] > 0), "exit status must say whether anything is missing"

    for bad in (["memory", "remember", "bogus", "t", "b"], ["experiment", "nope"], ["pin", "frob"], ["nosuch"]):
        r = ctx.cli(*bad)
        assert r.returncode == 2 and "error:" in r.stderr, (bad, r)

    r = ctx.cli("verify", str(ctx.root / "missing.log"))
    assert r.returncode == 1 and "cannot verify" in r.stderr and "caused by" in r.stderr, r
    assert "panicked" not in r.stderr

    # A closed pipe ends quietly, not with a panic.
    import subprocess

    from harness import exe
    p = subprocess.Popen([exe("playbox"), "completions", "zsh"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    p.stdout.read(10)
    p.stdout.close()
    err = p.stderr.read().decode()
    p.wait(timeout=10)
    assert "panicked" not in err, err
