#!/usr/bin/env python3
"""PLAYBOX_GPU_EVIDENCE_CMD adapter for NVIDIA Confidential Computing (H100/B200).

Reads the 32-byte hex statement binding from PLAYBOX_NONCE, invokes NVIDIA's
attestation SDK (NRAS remote or local nvtrust verifier), and emits the lines
expected by Playbox's CpuGpuProvider / GpuProvider (crates/playbox-core/src/gpu_cc.rs):

    gpu <NRAS overall EAT JWT>
    gpu-detail GPU-0 <detached per-GPU EAT JWT>
"""

from __future__ import annotations

import json
import os
import sys
from typing import Any


def parse_nras_token_payload(raw: Any) -> tuple[str, list[tuple[str, str]]]:
    """Extract (overall_jwt, [(gpu_name, detached_jwt), ...]) from an NRAS/nv-attestation-sdk token."""
    if isinstance(raw, (bytes, bytearray)):
        raw = raw.decode("utf-8")
    if isinstance(raw, str):
        raw = json.loads(raw)

    overall: str | None = None
    details: list[tuple[str, str]] = []

    if isinstance(raw, list):
        for item in raw:
            if isinstance(item, list) and len(item) == 2 and item[0] == "JWT" and isinstance(item[1], str):
                overall = item[1].strip()
            elif isinstance(item, dict):
                for gpu_name, tok in sorted(item.items()):
                    if isinstance(tok, str) and tok.strip():
                        details.append((str(gpu_name).strip(), tok.strip()))
    elif isinstance(raw, dict):
        tok = raw.get("overall") or raw.get("JWT") or raw.get("token")
        if isinstance(tok, str):
            overall = tok.strip()
        sub = raw.get("devices") or raw.get("submods") or {}
        if isinstance(sub, dict):
            for gpu_name, dt in sorted(sub.items()):
                if isinstance(dt, str) and dt.strip():
                    details.append((str(gpu_name).strip(), dt.strip()))

    if not overall:
        raise ValueError("NRAS attestation payload missing overall JWT")
    if not details:
        raise ValueError("NRAS attestation payload missing per-GPU detached JWTs")
    return overall, details


def format_playbox_lines(overall: str, details: list[tuple[str, str]]) -> str:
    lines = [f"gpu {overall}"]
    for name, tok in details:
        lines.append(f"gpu-detail {name} {tok}")
    return "\n".join(lines) + "\n"


def attest_live_gpu(nonce_hex: str) -> tuple[str, list[tuple[str, str]]]:
    from nv_attestation_sdk import attestation  # type: ignore[import-not-found]

    verifier_mode = os.environ.get("PLAYBOX_GPU_VERIFIER", "remote").strip().lower()
    nras_url = os.environ.get("PLAYBOX_NRAS_URL", "https://nras.attestation.nvidia.com/v3/attest/gpu")
    ocsp_url = os.environ.get("PLAYBOX_OCSP_URL", "https://ocsp.ndis.nvidia.com/")
    rim_url = os.environ.get("PLAYBOX_RIM_URL", "https://rim.attestation.nvidia.com/v1/rim/")

    client = attestation.Attestation()
    client.set_name("playbox-tdx-gpu")
    client.set_nonce(nonce_hex)

    if verifier_mode == "local":
        client.add_verifier(attestation.Devices.GPU, attestation.Environment.LOCAL, "", "")
    else:
        client.add_verifier(attestation.Devices.GPU, attestation.Environment.REMOTE, nras_url, "")
        client.set_ocsp_nonce_disabled(False)
        if hasattr(client, "set_claims_version"):
            client.set_claims_version("2.0")
        _ = (ocsp_url, rim_url)

    evidence_list = client.get_evidence()
    if not evidence_list:
        raise RuntimeError("nv_attestation_sdk returned no GPU evidence (is H100 CC mode enabled?)")

    ok = client.attest(evidence_list)
    if not ok:
        raise RuntimeError("NVIDIA GPU attestation returned false")

    token_raw = client.get_token()
    return parse_nras_token_payload(token_raw)


def main() -> int:
    nonce = os.environ.get("PLAYBOX_NONCE", "").strip()
    if len(nonce) != 64:
        print(f"error: PLAYBOX_NONCE must be a 64-char hex string (got len={len(nonce)})", file=sys.stderr)
        return 2
    int(nonce, 16)

    mock_path = os.environ.get("PLAYBOX_GPU_MOCK_TOKEN_JSON", "").strip()
    if mock_path:
        with open(mock_path, "r", encoding="utf-8") as fh:
            overall, details = parse_nras_token_payload(fh.read())
    else:
        overall, details = attest_live_gpu(nonce)

    sys.stdout.write(format_playbox_lines(overall, details))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
