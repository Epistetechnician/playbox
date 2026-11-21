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
    TrainedKVProjectionAdapter,
    _import_torch_and_transformers,
    load_verified_kb_shard,
    load_verified_kv_adapter,
    save_signed_kb_shard,
    save_signed_kv_adapter,
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
            self.assertEqual(manifest.get("sig_alg"), "Ed25519+ML-DSA-65")
            loaded, meta = load_verified_kb_shard(
                sdir,
                expected_pubkey_hex=manifest["pubkey_hex"],
                expected_pq_pubkey_hex=manifest["pq_pubkey_hex"],
                require_pq=True,
            )
            self.assertTrue(meta["pq_verified"])
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

    def test_trained_kv_adapter_identity_init_and_signed_roundtrip(self) -> None:
        torch, _ = _import_torch_and_transformers()
        adapter = TrainedKVProjectionAdapter(num_layers=2, num_kv_heads=4, head_dim=16, rank=8, device="cpu")
        k = torch.randn(1, 4, 6, 16)
        v = torch.randn(1, 4, 6, 16)
        # Identity initialization check (`up` = 0, `scale` = 1):
        kp0, vp0 = adapter.project_layer(0, k, v)
        self.assertTrue(torch.allclose(kp0, k, atol=1e-6))
        self.assertTrue(torch.allclose(vp0, v, atol=1e-6))

        # Perturb adapter weights so projection is non-trivial, then bake into clusters & persist:
        with torch.no_grad():
            adapter.net.k_up[0].bias.fill_(0.25)
        cluster = ParametricClusterKV(
            inter_key="pkg/mod/foo.py",
            root_key="pkg/mod",
            text="[pkg/mod/foo.py]\n- fn\n",
            token_count=6,
            slot_count=6,
            layers_kv=[(k.clone(), v.clone()), (k.clone(), v.clone())],
        )
        baked = adapter.apply_to_clusters([cluster])
        self.assertFalse(torch.allclose(baked[0].layers_kv[0][0], k))

        with tempfile.TemporaryDirectory() as td:
            sk = load_or_create_signing_key(Path(td) / "k.ed25519")
            adir = Path(td) / "adapter"
            manifest = save_signed_kv_adapter(
                adir,
                model_id="Qwen/Qwen3-1.7B-Base",
                adapter=adapter,
                train_meta={"epochs": 4, "rank": 8},
                signing_key=sk,
            )
            self.assertEqual(manifest.get("sig_alg"), "Ed25519+ML-DSA-65")
            loaded, meta = load_verified_kv_adapter(
                adir,
                expected_pubkey_hex=manifest["pubkey_hex"],
                expected_pq_pubkey_hex=manifest["pq_pubkey_hex"],
                require_pq=True,
                device="cpu",
            )
            self.assertTrue(meta["pq_verified"])
            self.assertEqual(meta["rank"], 8)
            kp1, _ = loaded.project_layer(0, k, v)
            kp_ref, _ = adapter.project_layer(0, k, v)
            self.assertTrue(torch.allclose(kp1, kp_ref, atol=1e-6))

            # Corrupt 1 byte of kv_adapter.pt -> must fail closed
            pt_file = adir / "kv_adapter.pt"
            buf = bytearray(pt_file.read_bytes())
            buf[-8] ^= 0xAA
            pt_file.write_bytes(bytes(buf))
            with self.assertRaises(ValueError):
                load_verified_kv_adapter(adir)

    def test_plugin_registers_parametric_qa_when_shard_configured(self) -> None:
        import os
        from playbox_atlas import plugin

        registered = {}

        class DummyCtx:
            def register_tool(self, name, **kwargs):
                registered[name] = kwargs

        old = os.environ.get("PLAYBOX_ATLAS_SHARD")
        try:
            os.environ["PLAYBOX_ATLAS_SHARD"] = "/tmp/nonexistent-shard"
            plugin.register(DummyCtx())
            self.assertIn("atlas_query", registered)
            self.assertIn("atlas_parametric_qa", registered)
            res = plugin.query_parametric_atlas("where is foo?")
            self.assertFalse(res["ok"])
        finally:
            if old is None:
                os.environ.pop("PLAYBOX_ATLAS_SHARD", None)
            else:
                os.environ["PLAYBOX_ATLAS_SHARD"] = old


if __name__ == "__main__":
    unittest.main()
