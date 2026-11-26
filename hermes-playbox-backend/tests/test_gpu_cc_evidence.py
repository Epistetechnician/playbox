"""Unit tests for scripts/gpu-cc-evidence.py (PLAYBOX_GPU_EVIDENCE_CMD)."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "gpu-cc-evidence.py"


def _load_mod():
    spec = importlib.util.spec_from_file_location("gpu_cc_evidence", SCRIPT)
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_parse_nras_list_format():
    mod = _load_mod()
    payload = [["JWT", "hdr.overall.sig"], {"GPU-0": "hdr.det0.sig", "GPU-1": "hdr.det1.sig"}]
    overall, details = mod.parse_nras_token_payload(json.dumps(payload))
    assert overall == "hdr.overall.sig"
    assert details == [("GPU-0", "hdr.det0.sig"), ("GPU-1", "hdr.det1.sig")]
    out = mod.format_playbox_lines(overall, details)
    assert out == "gpu hdr.overall.sig\ngpu-detail GPU-0 hdr.det0.sig\ngpu-detail GPU-1 hdr.det1.sig\n"


def test_cli_mock_token_and_nonce_validation(tmp_path: Path):
    tok = tmp_path / "nras.json"
    tok.write_text(json.dumps([["JWT", "a.b.c"], {"GPU-0": "d.e.f"}]), encoding="utf-8")

    bad = subprocess.run([sys.executable, str(SCRIPT)], env={**os.environ, "PLAYBOX_NONCE": "short"}, capture_output=True, text=True)
    assert bad.returncode == 2

    good = subprocess.run(
        [sys.executable, str(SCRIPT)],
        env={**os.environ, "PLAYBOX_NONCE": "ab" * 32, "PLAYBOX_GPU_MOCK_TOKEN_JSON": str(tok)},
        capture_output=True,
        text=True,
    )
    assert good.returncode == 0
    assert good.stdout == "gpu a.b.c\ngpu-detail GPU-0 d.e.f\n"
