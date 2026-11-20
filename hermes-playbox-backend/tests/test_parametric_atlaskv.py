"""Unit tests for Parametric AtlasKV signed shard persistence, quarantine gating, and tamper rejection."""
from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "hermes-playbox-backend"))

from playbox_atlas.kg2kv import Triple  # noqa: E402
from playbox_atlas.parametric import (  # noqa: E402
    KBEncoderProjector,
    ParametricClusterKV,
    _import_torch_and_transformers,
    load_verified_kb_shard,
    save_signed_kb_shard,
)
from playbox_router import load_or_create_signing_key  # noqa: E402


class ParametricAtlasKVUnitTests(unittest.TestCase):
    def test_signed_shard_roundtrip_projector_and_tamper_rejection(self) -> None:
        torch, _ = _import_torch_and_transformers()
        proj = KBEncoderProjector(slots_per_cluster=4)
        k = torch.randn(1, 8, 16, 128)
        v = torch.randn(1, 8, 16, 128)
        kc, vc = proj.compress(k, v)
        self.assertEqual(tuple(kc.shape), (1, 8, 4, 128))
        self.assertEqual(tuple(vc.shape), (1, 8, 4, 128))

        cluster = ParametricClusterKV(
            inter_key="django/utils/autoreload.py",
            root_key="django/utils",
            text="[django/utils/autoreload.py]\n- iter_modules_and_files\n",
            token_count=16,
            slot_count=4,
            layers_kv=[(kc, vc)],
        )
        with tempfile.TemporaryDirectory() as td:
            sk = load_or_create_signing_key(Path(td) / "k.ed25519")
            sdir = Path(td) / "shard"
            manifest = save_signed_kb_shard(
                sdir,
                model_id="Qwen/Qwen3-1.7B-Base",
                base_commit="8007495e6",
                clusters=[cluster],
                quarantined_dropped=3,
                signing_key=sk,
            )
            loaded, meta = load_verified_kb_shard(sdir, expected_pubkey_hex=manifest["pubkey_hex"])
            self.assertEqual(len(loaded), 1)
            self.assertEqual(meta["quarantined_dropped"], 3)
            self.assertEqual(meta["total_kv_slots"], 4)

            # Corrupt 1 byte of kv_clusters.pt -> must raise ValueError
            pt_file = sdir / "kv_clusters.pt"
            buf = bytearray(pt_file.read_bytes())
            buf[-8] ^= 0x55
            pt_file.write_bytes(bytes(buf))
            with self.assertRaises(ValueError):
                load_verified_kb_shard(sdir)


if __name__ == "__main__":
    unittest.main()
