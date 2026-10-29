import json
import subprocess
import sys

import pytest
import torch

from playbox_steer import engine, files, spec as specmod
from playbox_steer.__main__ import main

from conftest import PROMPTS, make_task


def _run(tmp_path, weights, **kw):
    task, text = make_task(tmp_path / "task", weights, **kw)
    out = tmp_path / "out.json"
    code = main(["run", "--weights", str(weights), "--task", str(task), "--out", str(out)])
    return code, json.loads(out.read_text()) if out.exists() else None, task


@pytest.mark.parametrize("arch", ["gpt2_dir", "llama_dir"])
@pytest.mark.parametrize("site", ["resid_post", "attn_out", "mlp_out"])
def test_run_passes_gates_and_steers_on_both_architectures(request, tmp_path, arch, site):
    weights = request.getfixturevalue(arch)
    code, out, _ = _run(tmp_path, weights, site=site, coef_milli=8000)
    assert code == 0, out
    assert out["gates"]["pass"]
    assert out["gates"]["no_op"]["max_delta"] == 0.0 and out["gates"]["parity"]["max_delta"] == 0.0
    assert out["gates"]["reach"]["max_delta"] > 0.0
    assert len(out["results"]) == len(PROMPTS["prompts"])


def test_run_is_byte_deterministic(tmp_path, gpt2_dir):
    outs = []
    for i in range(2):
        code, _, _ = _run(tmp_path / str(i), gpt2_dir)
        assert code == 0
        outs.append((tmp_path / str(i) / "out.json").read_bytes())
    assert outs[0] == outs[1]


def test_noop_spec_changes_nothing_and_reach_is_skipped(tmp_path, gpt2_dir):
    code, out, _ = _run(tmp_path, gpt2_dir, coef_milli=0)
    assert code == 0 and out["gates"]["reach"].get("skipped")
    assert not any(r["changed"] for r in out["results"])


def _projection_after(model, tok, spec, d, norm_ref, prompt, layer):
    cur = {}
    bs = engine.blocks(model)
    mod = engine.site_module(bs[layer], spec.site)
    # Registered after the steerer, so it sees the edited output.
    with engine.Steerer(model, spec, d, norm_ref):
        h = mod.register_forward_hook(lambda _m, _i, o: cur.__setitem__("x", engine._tensor(o).detach()))
        try:
            model(input_ids=tok(prompt, return_tensors="pt").input_ids, use_cache=False)
        finally:
            h.remove()
    return cur["x"][0]


@pytest.mark.parametrize("mode", ["add", "clamp", "ablate"])
def test_mode_semantics(tmp_path, gpt2_dir, mode):
    from dataclasses import replace

    task, text = make_task(tmp_path / "t", gpt2_dir, layers=(2,), mode=mode, coef_milli=0 if mode == "ablate" else 2500, tokens="all")
    spec = specmod.parse(text)
    model, tok = engine.load(gpt2_dir)
    vec, _ = files.read_direction(task / "direction.f32", dim=32)
    d = torch.tensor(vec)
    ps = files.read_prompts(task / "prompts.json")
    norm_ref = engine.calibrate(model, tok, spec, ps)
    clean = _projection_after(model, tok, replace(spec, mode="add", coef_milli=0), d, norm_ref, "The weather today is", 2)
    steered = _projection_after(model, tok, spec, d, norm_ref, "The weather today is", 2)
    s = spec.coef * norm_ref[2]
    if mode == "add":
        assert torch.allclose((steered - clean) @ d, torch.full((clean.shape[0],), s), atol=1e-4)
        # Nothing changes orthogonally to the direction.
        orth = (steered - clean) - ((steered - clean) @ d)[:, None] * d
        assert orth.abs().max() < 1e-5
    elif mode == "clamp":
        assert torch.allclose(steered @ d, torch.full((clean.shape[0],), s), atol=1e-4)
    else:
        assert (steered @ d).abs().max() < 1e-4


def test_tokens_scope(tmp_path, gpt2_dir):
    """`generated` must not touch the prompt pass; `prompt` must not touch decoding."""
    model, tok = engine.load(gpt2_dir)
    task, text = make_task(tmp_path / "t", gpt2_dir, layers=(1,), coef_milli=9000, tokens="generated")
    spec = specmod.parse(text)
    vec, _ = files.read_direction(task / "direction.f32", dim=32)
    d = torch.tensor(vec)
    nr = engine.calibrate(model, tok, spec, files.read_prompts(task / "prompts.json"))
    ids = tok("The weather today is", return_tensors="pt").input_ids
    clean = model(input_ids=ids, use_cache=False).logits
    with engine.Steerer(model, spec, d, nr) as st:
        st.phase = "prefill"
        assert torch.equal(model(input_ids=ids, use_cache=False).logits, clean), "generated scope edited the prompt"
        st.phase = "decode"
        assert not torch.equal(model(input_ids=ids, use_cache=False).logits, clean)
    from dataclasses import replace

    with engine.Steerer(model, replace(spec, tokens="prompt"), d, nr) as st:
        st.phase = "decode"
        assert torch.equal(model(input_ids=ids, use_cache=False).logits, clean), "prompt scope edited decoding"
        st.phase = "prefill"
        assert not torch.equal(model(input_ids=ids, use_cache=False).logits, clean)


def test_generated_scope_changes_text_only_after_prompt(tmp_path, gpt2_dir):
    code, out, _ = _run(tmp_path, gpt2_dir, tokens="generated", coef_milli=20000, layers=(1, 2, 3))
    assert code == 0
    # The first token comes from the prompt's own logits, which `generated` leaves alone.
    for r in out["results"]:
        assert r["steered"][:1] == r["clean"][:1] or not r["steered"]


def test_hash_mismatches_are_refused(tmp_path, gpt2_dir):
    task, _ = make_task(tmp_path / "t", gpt2_dir)
    (task / "direction.f32").write_bytes(files.hash_bytes(b"x").encode()[:32] * 1)  # 32 bytes, wrong content
    out = tmp_path / "o.json"
    assert main(["run", "--weights", str(gpt2_dir), "--task", str(task), "--out", str(out)]) == 2
    task2, _ = make_task(tmp_path / "t2", gpt2_dir)
    p = json.loads((task2 / "prompts.json").read_text())
    p["prompts"][0] = "tampered"
    (task2 / "prompts.json").write_text(json.dumps(p))
    assert main(["run", "--weights", str(gpt2_dir), "--task", str(task2), "--out", str(out)]) == 2


def test_non_unit_direction_is_refused(tmp_path, gpt2_dir):
    task, _ = make_task(tmp_path / "t", gpt2_dir)
    import struct

    (task / "direction.f32").write_bytes(struct.pack("<32f", *([1.0] * 32)))
    assert main(["run", "--weights", str(gpt2_dir), "--task", str(task), "--out", str(tmp_path / "o.json")]) == 2


def test_guard_blocks_a_denied_class_and_allows_otherwise(tmp_path, gpt2_dir):
    model, tok = engine.load(gpt2_dir)
    d = engine.random_direction(32, 5)
    task, _ = make_task(tmp_path / "t", gpt2_dir, direction=d, layers=(2,))
    guards = tmp_path / "g"
    guards.mkdir()
    files.write_direction(guards / "refusal.f32", d)  # the same direction: cosine 1
    (guards / "guards.json").write_text(json.dumps([{"class": "refusal", "site": "resid_post", "layer": 2, "file": "refusal.f32"}]))
    out = tmp_path / "o.json"
    base = ["run", "--weights", str(gpt2_dir), "--task", str(task), "--guards", str(guards), "--out", str(out)]
    assert main(base + ["--deny", "refusal"]) == 3
    assert main(base + ["--deny", "persona"]) == 0
    assert json.loads(out.read_text())["guard_hits"][0]["class"] == "refusal"
    # A guard at another layer does not apply.
    (guards / "guards.json").write_text(json.dumps([{"class": "refusal", "site": "resid_post", "layer": 3, "file": "refusal.f32"}]))
    assert main(base + ["--deny", "refusal"]) == 0
    # An unrelated direction passes the guard.
    other = engine.random_direction(32, 99)
    assert abs(sum(a * b for a, b in zip(d, other))) < 0.5


def test_probe_reads_and_does_not_edit(tmp_path, gpt2_dir):
    task, _ = make_task(tmp_path / "t", gpt2_dir, layers=(1, 2))
    out = tmp_path / "p.json"
    assert main(["probe", "--weights", str(gpt2_dir), "--task", str(task), "--out", str(out)]) == 0
    rows = json.loads(out.read_text())["probe"]
    assert len(rows) == 3 and set(rows[0]["projection"]) == {"1", "2"}
    # The projection of a direction onto itself-scaled activations: re-running is identical.
    out2 = tmp_path / "p2.json"
    main(["probe", "--weights", str(gpt2_dir), "--task", str(task), "--out", str(out2)])
    assert out.read_bytes() == out2.read_bytes()


def test_extract_diffmean_finds_the_separating_direction(tmp_path, gpt2_dir):
    model, tok = engine.load(gpt2_dir)
    pos, neg = ["I love this", "I love that", "love love love"], ["I hate this", "I hate that", "hate hate hate"]
    d = torch.tensor(engine.extract_diffmean(gpt2_dir, pos, neg, "resid_post", 2))
    assert abs(d.norm().item() - 1) < 1e-5
    hp = engine.last_token_states(model, tok, pos, "resid_post", 2)
    hn = engine.last_token_states(model, tok, neg, "resid_post", 2)
    assert (hp @ d).mean() > (hn @ d).mean()


def test_random_direction_is_seeded_unit(tmp_path):
    a, b, c = engine.random_direction(32, 1), engine.random_direction(32, 1), engine.random_direction(32, 2)
    assert a == b and a != c and abs(sum(x * x for x in a) - 1) < 1e-5


def test_layer_out_of_range_and_bad_specs(tmp_path, gpt2_dir):
    code, _, _ = _run(tmp_path, gpt2_dir, layers=(1, 9))
    assert code == 2
    with pytest.raises(ValueError):
        specmod.parse("model x\n")
    base = make_task(tmp_path / "ok", gpt2_dir)[1]
    for bad in [base.replace("layers 1,2", "layers 2,1"), base.replace("mode add", "mode zap"), base + "extra 1\n", base.replace("mode add", "mode ablate")]:
        with pytest.raises(ValueError):
            specmod.parse(bad)
    assert specmod.parse(base).layers == (1, 2)


def test_weights_manifest_is_stable_and_sensitive(tmp_path, gpt2_dir):
    h = files.weights_manifest(gpt2_dir)
    assert h == files.weights_manifest(gpt2_dir)
    (gpt2_dir / ".cache").mkdir(exist_ok=True)
    (gpt2_dir / ".cache" / "x").write_text("ignored")
    assert files.weights_manifest(gpt2_dir) == h
    copy = tmp_path / "c"
    copy.mkdir()
    for p in gpt2_dir.iterdir():
        if p.is_file():
            (copy / p.name).write_bytes(p.read_bytes())
    assert files.weights_manifest(copy) == h
    (copy / "config.json").write_text((copy / "config.json").read_text() + " ")
    assert files.weights_manifest(copy) != h


def test_prompt_set_validation(tmp_path):
    p = tmp_path / "p.json"
    for bad in [{"prompts": []}, {"prompts": [""]}, {"prompts": ["a"], "max_new_tokens": 0}, {"prompts": ["a"], "temperature": 3}, {"prompts": ["a"], "extra": 1}, []]:
        p.write_text(json.dumps(bad))
        with pytest.raises(ValueError):
            files.read_prompts(p)


def test_manifest_cross_check_fixture(tmp_path):
    """The same tree is hashed by crates/playbox-net/src/steer.rs; both must give this value."""
    (tmp_path / "sub").mkdir()
    (tmp_path / ".cache").mkdir()
    (tmp_path / "a.txt").write_bytes(b"alpha")
    (tmp_path / "a-b.txt").write_bytes(b"dash")
    (tmp_path / "sub" / "c.bin").write_bytes(bytes([0, 1, 2, 3]))
    (tmp_path / ".cache" / "ignored").write_bytes(b"x")
    (tmp_path / "link").symlink_to(tmp_path / "a.txt")
    assert files.weights_manifest(tmp_path) == "7bc07aaa2a32a98a50e6abae371e4e48a068edd147265ea594bee1c8f13ec519"
