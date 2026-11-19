"""Unit and algebraic invariant tests for `playbox_steer.commuting_hub`."""
from __future__ import annotations

import torch

from playbox_steer import commuting_hub as CH
from playbox_steer import hub as H


def test_zero_beta_gamma_matches_static_fit_hub_identically() -> None:
    g = torch.Generator().manual_seed(7)
    s_mid = {
        "a": torch.randn(80, 48, generator=g),
        "b": torch.randn(80, 64, generator=g),
    }
    s_late = {
        "a": torch.randn(80, 48, generator=g),
        "b": torch.randn(80, 64, generator=g),
    }
    h_static = H.fit_hub(s_mid, "anc", k=16, lam=0.1)
    h_comm0 = CH.fit_commuting_hub(s_mid, s_late, "anc", k=16, lam=0.1, beta=0.0, gamma=0.0)

    probe = torch.randn(10, 48, generator=g)
    out_static = h_static.send("a", "b", probe)
    out_comm0 = h_comm0.send("a", "b", probe)
    # SVD column signs are arbitrary, and `enc @ dec = (X^T ... G) (G^T X)` cancels column signs!
    assert torch.allclose(out_static, out_comm0, atol=1e-4)


def test_commuting_hub_suppresses_transient_decaying_modes_and_packs_pbl1() -> None:
    """When mid-layer states mix persistent concepts with higher-variance transient modes that
    lie in the nullspace of `T : h_mid -> h_late`, static `fit_hub` wastes rank-k capacity on the
    transient modes and loses downstream signal, whereas `fit_commuting_hub` recovers the persistent
    downstream concept cleanly and packs standard 64-byte `PBL1` messages.
    """
    gen = torch.Generator().manual_seed(42)
    n_anc, n_test, k_true = 160, 40, 8
    z_all = torch.randn(n_anc + n_test, k_true, generator=gen)
    # High-variance mid-layer nuisance shared across models at mid-layer ONLY, killed by T_j!
    nuisance = 2.5 * torch.randn(n_anc + n_test, k_true, generator=gen)

    s_mid, s_late, test_mid, test_late, trans_true = {}, {}, {}, {}, {}
    for idx, (name, width) in enumerate((("m1", 64), ("m2", 80), ("m3", 96))):
        g_m = torch.Generator().manual_seed(100 + idx)
        # Orthonormal frame: first `k_true` cols propagate to `late`; next `k_true` cols die (`T=0`)
        q, _ = torch.linalg.qr(torch.randn(width, 2 * k_true, generator=g_m))
        p_persist = q[:, :k_true]  # width x k_true
        p_decay = q[:, k_true:]  # width x k_true
        q_late, _ = torch.linalg.qr(torch.randn(width, k_true, generator=g_m))

        t_op = p_persist @ q_late.T  # width x width: maps p_persist -> q_late, annihilates p_decay!
        xm = z_all @ p_persist.T + nuisance @ p_decay.T + 0.05 * torch.randn(n_anc + n_test, width, generator=g_m)
        xl = z_all @ q_late.T + 0.05 * torch.randn(n_anc + n_test, width, generator=g_m)

        s_mid[name] = xm[:n_anc]
        s_late[name] = xl[:n_anc]
        test_mid[name] = xm[n_anc:]
        test_late[name] = xl[n_anc:]
        trans_true[name] = t_op

    # Rank budget k = k_true forces static hub to pick the 2.5x larger nuisance subspace!
    h_static = H.fit_hub(s_mid, "anc", k=k_true, lam=0.1)
    h_comm = CH.fit_commuting_hub(s_mid, s_late, "anc", k=k_true, lam=0.1, beta=1.0, gamma=2.0)

    # Wire-format compatibility (`PBL1` pack/unpack)
    enc_vec = h_comm.encode("m1", test_mid["m1"][0])
    blob = H.pack(h_comm, "m1", enc_vec)
    head, unpacked = H.unpack(h_comm, blob)
    assert head["k"] == k_true

    # Downstream cosine after applying receiver's true forward operator `T_m2`
    dm_src = test_mid["m1"] - test_mid["m1"].mean(0)
    dl_true = test_late["m2"] - test_late["m2"].mean(0)

    pred_late_static = h_static.send("m1", "m2", dm_src) @ trans_true["m2"]
    pred_late_comm = h_comm.send("m1", "m2", dm_src) @ trans_true["m2"]

    cos_static = torch.nn.functional.cosine_similarity(pred_late_static, dl_true, dim=-1).mean().item()
    cos_comm = torch.nn.functional.cosine_similarity(pred_late_comm, dl_true, dim=-1).mean().item()
    assert cos_comm > 0.85
    assert cos_comm > cos_static + 0.45
