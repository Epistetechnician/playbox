import json

import pytest

from playbox_judge.replay import max_abs_diff, output_matches, parse_variant_scores, per_prompt_rows


def test_variant_scores_are_read_back_from_millionths():
    text = "playbox.variant.v1\nparent -\nevaluator ab\npatch cd\nscores 3437488,-1250000,0\ncost 3\nsigner 00\nsig 00\n"
    assert parse_variant_scores(text) == [3.437488, -1.25, 0.0]
    assert parse_variant_scores("x\nscores -\n") == []
    with pytest.raises(ValueError):
        parse_variant_scores("x\ncost 1\n")


def test_output_hash_is_blake3_of_the_exact_bytes():
    import blake3
    raw = '{"results":[]}\n'
    h = blake3.blake3(raw.encode()).hexdigest()
    assert output_matches(raw, h) and not output_matches(raw + " ", h) and not output_matches(raw, "00" * 32)


def test_rows_and_comparison():
    assert per_prompt_rows(json.dumps({"results": [{"steered": "a", "clean": "b"}]}))[0]["steered"] == "a"
    for bad in ('{"results": []}', "{}", '{"results": 3}'):
        with pytest.raises(ValueError):
            per_prompt_rows(bad)
    assert max_abs_diff([1.0, 2.0], [1.0005, 1.9995]) == pytest.approx(0.0005)
    assert max_abs_diff([], []) == 0.0
    with pytest.raises(ValueError):
        max_abs_diff([1.0], [1.0, 2.0])
