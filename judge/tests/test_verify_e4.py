"""The E4 verifier against the committed report, untouched and tampered with. Needs the built sidecar and the cached
classifier; skipped without the sidecar."""
import copy
import json
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
REPORT = ROOT / "docs/e4-gpt2-search-evidence.json"
BINARY = ROOT / "target/debug/playbox-mcp"
SCRIPT = ROOT / "judge/experiments/verify_e4.py"
pytestmark = pytest.mark.skipif(not (BINARY.exists() and REPORT.exists()), reason="needs target/debug/playbox-mcp and the committed report")


def run(report: dict, tmp_path, *extra):
    p = tmp_path / "report.json"
    p.write_text(json.dumps(report))
    out = subprocess.run([sys.executable, str(SCRIPT), str(p), *extra], capture_output=True, text=True, cwd=ROOT / "judge")
    return out.returncode, out.stdout + out.stderr


@pytest.fixture(scope="module")
def report():
    return json.loads(REPORT.read_text())


def test_the_committed_report_verifies(report, tmp_path):
    code, out = run(report, tmp_path)
    assert code == 0 and "VERIFIED" in out and out.count("-> OK") == len(report["chain"]) - 1, out


def test_an_edited_output_is_caught_by_the_receipt_hash(report, tmp_path):
    bad = copy.deepcopy(report)
    raw = bad["evidence"]["per_variant"][1]["outputs"][0]
    assert '"steered":"' in raw
    bad["evidence"]["per_variant"][1]["outputs"][0] = raw.replace('"steered":"', '"steered":"Wonderful, wonderful! ', 1)
    code, out = run(bad, tmp_path)
    assert code == 1 and "output bytes match receipt: False" in out and "FAILURE" in out, out


def test_a_mangled_output_fails_cleanly_instead_of_crashing(report, tmp_path):
    bad = copy.deepcopy(report)
    raw = bad["evidence"]["per_variant"][1]["outputs"][0]
    bad["evidence"]["per_variant"][1]["outputs"][0] = raw.replace("steered", "steereD", 1)
    code, out = run(bad, tmp_path)
    assert code == 1 and "cannot be replayed" in out and "Traceback" not in out, out


def test_receipts_swapped_between_variants_are_refused(report, tmp_path):
    bad = copy.deepcopy(report)
    a, b = bad["evidence"]["per_variant"][1], bad["evidence"]["per_variant"][2]
    a["receipts"], b["receipts"] = b["receipts"], a["receipts"]
    code, out = run(bad, tmp_path)
    assert code == 1 and "EVIDENCE REFUSED" in out and "not the run" in out, out


def test_a_host_the_verifier_does_not_pin_is_refused(report, tmp_path):
    code, out = run(report, tmp_path, "--host", "11" * 32)
    assert code == 1 and "does not pin" in out, out


def test_a_different_custodian_makes_the_whole_chain_untrusted(report, tmp_path):
    code, out = run(report, tmp_path, "--custodian", "22" * 32)
    assert code == 1 and "LINEAGE REFUSED" in out, out


def test_evidence_swapped_for_another_variants_is_refused(report, tmp_path):
    bad = copy.deepcopy(report)
    a, b = bad["evidence"]["per_variant"][1], bad["evidence"]["per_variant"][2]
    a["evidence"], b["evidence"] = b["evidence"], a["evidence"]
    code, out = run(bad, tmp_path)
    assert code == 1 and "EVIDENCE REFUSED" in out and "different variant" in out, out
