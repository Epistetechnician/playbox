import json

from playbox_world.claude_code import convert


def test_convert_maps_blocks_to_event_kinds(tmp_path):
    rows = [
        {"type": "user", "timestamp": "2026-10-28T10:00:00Z", "message": {"content": "add a parser <system-reminder>x</system-reminder>"}},
        {"type": "assistant", "timestamp": "2026-10-28T10:00:05Z", "message": {"content": [
            {"type": "thinking", "thinking": ""},
            {"type": "thinking", "thinking": "plan the lexer"},
            {"type": "text", "text": "on it"},
            {"type": "tool_use", "name": "Bash", "input": {"command": "git commit -m x"}},
            {"type": "tool_use", "name": "Read", "input": {"file_path": "a.rs"}},
        ]}},
        {"type": "user", "timestamp": "2026-10-28T10:00:06Z", "message": {"content": [{"type": "tool_result", "content": "ok"}]}},
        {"type": "system", "timestamp": "2026-10-28T10:00:07Z"},
    ]
    p = tmp_path / "t.jsonl"
    p.write_text("\n".join(json.dumps(r) for r in rows) + "\nnot json\n")
    out = list(convert(str(p)))
    assert [(e["agent"], e["kind"]) for e in out] == [
        ("user", "message"), ("claude", "reasoning"), ("claude", "message"), ("claude", "commit"), ("claude", "action"),
    ]
    assert out[0]["text"] == "add a parser"
    assert out[4]["text"] == "Read a.rs"
