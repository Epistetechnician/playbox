"""Transition-commuting shared latent hub ("rulial particles") across N model families.

Static `fit_hub` (`hub.py`) aligns mid-layer states `X_mid` alone: two receiver directions with
similar mid-layer reconstruction error can have very different survival through the receiver's
remaining transformer blocks `T_j : h_{mid} -> h_{late}`.

`fit_commuting_hub` preserves the exact `Hub` / `Spoke` / `PBL1` / `PBL2` 64-byte wire contract
while enforcing dynamical commutation across `(X_mid, X_late)`:

1. Trajectory GCCA: builds orthonormal hub coordinates `G` (`G^T G = n I_k`) from stacked whitened
   `(U_mid^{(i)}, sqrt(beta) * U_late^{(i)})` across all N families, selecting invariants that
   persist from injection depth (`frac_mid`) to readout depth (`frac_late`).
2. Sobolev / Jacobian-commuting decoder: fits each receiver's `dec_j : R^k -> R^{d_j}` to minimize
       ||G D - X_mid||_F^2 + gamma * ||G D T_hat_j - X_late||_F^2 + lam * n * ||D||_F^2
   where `T_hat_j` is the empirical anchor transition operator `X_mid -> X_late`. Since
   `G^T G = n I_k`, the Sylvester system decouples into one strictly SPD solve:
       ((1 + lam) I_d + gamma * T_hat_j T_hat_j^T) D^T = (1 / n) (X_mid^T + gamma * T_hat_j X_late^T) G
   At `beta = 0, gamma = 0`, `fit_commuting_hub` reduces identically to `fit_hub`.
"""
from __future__ import annotations

import json
import os
import random
from pathlib import Path

import torch

from . import hub as H
from . import telepathy as T
from .engine import blocks
from .telepathy_data import FIT_TOPICS, NEUTRAL, PROMPTS, TEST_CONCEPTS


def _fit_transition_operator(x_mid: torch.Tensor, x_late: torch.Tensor, lam_t: float = 0.2) -> torch.Tensor:
    """Dual ridge transition `T_hat : R^d -> R^d` mapping centered `x_mid` to centered `x_late`."""
    xm = x_mid.double()
    xl = x_late.double()
    gram = xm @ xm.T
    scale = gram.diagonal().mean().clamp_min(1e-12)
    reg = gram + lam_t * scale * torch.eye(xm.shape[0], dtype=torch.float64)
    return xm.T @ torch.linalg.solve(reg, xl)


def _solve_commuting_decoder(
    g: torch.Tensor,
    x_mid: torch.Tensor,
    x_late: torch.Tensor,
    *,
    lam: float,
    gamma: float,
    lam_t: float = 0.2,
) -> torch.Tensor:
    """Exact SPD normal-equation solver for `dec : R^k -> R^d` (`gamma=0` matches `_ridge_primal`)."""
    gd = g.double()
    xm = x_mid.double()
    n = float(gd.shape[0])
    if gamma <= 0.0:
        return H._ridge_primal(g, x_mid, lam)

    xl = x_late.double()
    t_hat = _fit_transition_operator(xm, xl, lam_t=lam_t)  # d x d
    d = xm.shape[1]
    lhs = (1.0 + lam) * torch.eye(d, dtype=torch.float64) + gamma * (t_hat @ t_hat.T)
    rhs = ((xm.T + gamma * (t_hat @ xl.T)) @ gd) / n  # d x k
    dec_t = torch.linalg.solve(lhs, rhs)  # d x k
    return dec_t.T.float()  # k x d


def add_commuting_spoke(
    hub: H.Hub,
    name: str,
    states_mid: torch.Tensor,
    states_late: torch.Tensor,
    *,
    gamma: float = 1.0,
    lam_t: float = 0.2,
) -> H.Spoke:
    """Attach a new model family to an existing hub with `O(1)` spoke fit and commutation loss."""
    if states_mid.shape[0] != hub.g.shape[0] or states_late.shape[0] != hub.g.shape[0]:
        raise ValueError("anchor row count mismatch between states_mid/states_late and hub.g")
    mean_mid = states_mid.mean(0)
    xm = states_mid - mean_mid
    xl = states_late - states_late.mean(0)
    enc = H._ridge_dual(xm, hub.g, hub.lam)
    dec = _solve_commuting_decoder(hub.g, xm, xl, lam=hub.lam, gamma=gamma, lam_t=lam_t)
    sp = H.Spoke(mean=mean_mid, enc=enc, dec=dec)
    hub.spokes[name] = sp
    return sp


def fit_commuting_hub(
    states_mid: dict[str, torch.Tensor],
    states_late: dict[str, torch.Tensor],
    anchors: str,
    *,
    k: int = 64,
    lam: float = 0.1,
    beta: float = 0.5,
    gamma: float = 1.0,
    lam_t: float = 0.2,
    r: int | None = None,
) -> H.Hub:
    """Fit trajectory-GCCA + transition-commuting `Hub` compatible with `PBL1` / `PBL2`."""
    if set(states_mid.keys()) != set(states_late.keys()):
        raise ValueError("states_mid and states_late must have identical model keys")
    n = next(iter(states_mid.values())).shape[0]
    for m in states_mid:
        if states_mid[m].shape[0] != n or states_late[m].shape[0] != n:
            raise ValueError(f"model {m!r} anchor rows do not match n={n}")

    r_eff = r or min(n - 1, 256)
    blocks_list: list[torch.Tensor] = []
    w_late = float(beta) ** 0.5 if beta > 0.0 else 0.0

    for m in states_mid:
        xm = (states_mid[m] - states_mid[m].mean(0)).double()
        um, svm, _ = torch.linalg.svd(xm, full_matrices=False)
        keep_m = int((svm > svm[0] * 1e-6).sum())
        blocks_list.append(um[:, : min(r_eff, keep_m)])

        if w_late > 0.0:
            xl = (states_late[m] - states_late[m].mean(0)).double()
            ul, svl, _ = torch.linalg.svd(xl, full_matrices=False)
            keep_l = int((svl > svl[0] * 1e-6).sum())
            blocks_list.append(w_late * ul[:, : min(r_eff, keep_l)])

    u_all, _, _ = torch.linalg.svd(torch.cat(blocks_list, dim=1), full_matrices=False)
    k_eff = min(k, u_all.shape[1])
    g = (u_all[:, :k_eff] * (n**0.5)).float()

    hub = H.Hub(k=k_eff, anchors=anchors, g=g, lam=lam)
    for m in states_mid:
        add_commuting_spoke(
            hub,
            m,
            states_mid[m].float(),
            states_late[m].float(),
            gamma=gamma,
            lam_t=lam_t,
        )
    return hub


def dual_layer_states(
    model,
    tok,
    texts: list[str],
    layer_mid: int,
    layer_late: int,
) -> tuple[torch.Tensor, torch.Tensor]:
    """Extract mean non-BOS block outputs at `layer_mid` and `layer_late` in one forward pass."""
    rows_m, rows_l, cur = [], [], {}
    hm = blocks(model)[layer_mid].register_forward_hook(
        lambda _m, _i, out: cur.__setitem__(
            "m", (out[0] if isinstance(out, tuple) else out).detach()
        )
    )
    hl = blocks(model)[layer_late].register_forward_hook(
        lambda _m, _i, out: cur.__setitem__(
            "l", (out[0] if isinstance(out, tuple) else out).detach()
        )
    )
    try:
        for t in texts:
            ids = tok(t, return_tensors="pt").input_ids[:, :64].to(model.device)
            model(input_ids=ids, use_cache=False)
            xm = cur["m"][0].cpu().float()
            xl = cur["l"][0].cpu().float()
            rows_m.append(xm[1:].mean(0) if xm.shape[0] > 1 else xm[0])
            rows_l.append(xl[1:].mean(0) if xl.shape[0] > 1 else xl[0])
    finally:
        hm.remove()
        hl.remove()
    return torch.stack(rows_m), torch.stack(rows_l)


def steered_late_delta(
    model,
    tok,
    prompts: list[str],
    layer_mid: int,
    layer_late: int,
    vec_mid: torch.Tensor,
) -> torch.Tensor:
    """Measure actual nonlinear downstream shift `h_late(steered at mid) - h_late(clean)`."""
    clean_rows, steered_rows, cur = [], [], {}
    hl = blocks(model)[layer_late].register_forward_hook(
        lambda _m, _i, out: cur.__setitem__(
            "l", (out[0] if isinstance(out, tuple) else out).detach()
        )
    )
    try:
        for p in prompts:
            ids = tok(p, return_tensors="pt").input_ids[:, :64].to(model.device)
            model(input_ids=ids, use_cache=False)
            xl0 = cur["l"][0].cpu().float()
            clean_rows.append(xl0[1:].mean(0) if xl0.shape[0] > 1 else xl0[0])

            def _inject(_m, _i, out):
                if isinstance(out, tuple):
                    return (out[0] + vec_mid.to(out[0].device, out[0].dtype),) + tuple(out[1:])
                return out + vec_mid.to(out.device, out.dtype)

            hm = blocks(model)[layer_mid].register_forward_hook(_inject)
            try:
                model(input_ids=ids, use_cache=False)
            finally:
                hm.remove()
            xl1 = cur["l"][0].cpu().float()
            steered_rows.append(xl1[1:].mean(0) if xl1.shape[0] > 1 else xl1[0])
    finally:
        hl.remove()
    return (torch.stack(steered_rows) - torch.stack(clean_rows)).mean(0)


def _paired_bootstrap_ci(
    diffs: list[float], n_boot: int = 4000, seed: int = 20261119
) -> tuple[float, float, float]:
    rng = random.Random(seed)
    n = len(diffs)
    if n == 0:
        return 0.0, 0.0, 0.0
    mean_d = sum(diffs) / n
    draws = sorted(
        sum(diffs[rng.randrange(n)] for _ in range(n)) / n for _ in range(n_boot)
    )
    lo = draws[int(0.025 * n_boot)]
    hi = draws[int(0.975 * n_boot)]
    return round(mean_d, 4), round(lo, 4), round(hi, 4)


def run_paired_commutation_benchmark(
    models: dict[str, str],
    *,
    frac_mid: float = 0.45,
    frac_late: float = 0.85,
    k: int = 64,
    coef: float = 0.5,
    gen: int = 24,
    seed: int = 0,
) -> dict[str, object]:
    """Paired cross-family benchmark: `fit_commuting_hub` vs static `fit_hub` on unseen concepts.

    Hyperparameters `(beta, gamma)` are selected strictly on the `FIT_TOPICS` train/val split
    (`426` fit / `150` val anchor sentences) by downstream linear-transition cosine before locking
    and evaluating once on `TEST_CONCEPTS` (`0` overlap with `FIT_TOPICS`).
    """
    torch.set_grad_enabled(False)
    all_fit = [s for topic in FIT_TOPICS.values() for s in topic]
    rng = random.Random(seed)
    rng.shuffle(all_fit)
    val_texts, train_texts = all_fit[:150], all_fit[150:]
    concepts = list(TEST_CONCEPTS.keys())
    lex = {c: set(TEST_CONCEPTS[c]["lexicon"]) for c in concepts}

    info: dict[str, dict[str, object]] = {}
    for name, path in models.items():
        m, tok = T.load(path)
        l_mid = T.layer_of(m, frac_mid)
        l_late = max(l_mid + 1, T.layer_of(m, frac_late))
        tr_m, tr_l = dual_layer_states(m, tok, train_texts, l_mid, l_late)
        va_m, va_l = dual_layer_states(m, tok, val_texts, l_mid, l_late)
        neu_m, neu_l = dual_layer_states(m, tok, NEUTRAL, l_mid, l_late)

        thoughts_mid: dict[str, torch.Tensor] = {}
        thoughts_late: dict[str, torch.Tensor] = {}
        for c in concepts:
            cm, cl = dual_layer_states(m, tok, TEST_CONCEPTS[c]["sentences"], l_mid, l_late)
            thoughts_mid[c] = cm.mean(0) - neu_m.mean(0)
            thoughts_late[c] = cl.mean(0) - neu_l.mean(0)

        info[name] = {
            "layer_mid": l_mid,
            "layer_late": l_late,
            "train_mid": tr_m,
            "train_late": tr_l,
            "val_mid": va_m,
            "val_late": va_l,
            "norm_mid": T.mean_norm(m, tok, NEUTRAL + PROMPTS, l_mid),
            "thoughts_mid": thoughts_mid,
            "thoughts_late": thoughts_late,
            "t_hat": _fit_transition_operator(tr_m - tr_m.mean(0), tr_l - tr_l.mean(0)).float(),
            "hidden": int(m.config.hidden_size),
        }
        del m

    # Tune (beta, gamma) strictly on FIT_TOPICS validation anchors (never seeing TEST_CONCEPTS)
    digest = H.anchor_digest(train_texts)
    tr_mid_map = {n: info[n]["train_mid"] for n in models}  # type: ignore[misc]
    tr_late_map = {n: info[n]["train_late"] for n in models}  # type: ignore[misc]

    best_cfg = (0.5, 1.0)
    best_val_score = -1e9
    val_grid_scores: dict[str, float] = {}
    for beta in (0.25, 0.5, 1.0):
        for gamma in (0.25, 0.5, 1.0, 2.0):
            cand_hub = fit_commuting_hub(
                tr_mid_map, tr_late_map, digest, k=k, beta=beta, gamma=gamma
            )
            pair_vals: list[float] = []
            for src in models:
                vm_s = info[src]["val_mid"]  # type: ignore[assignment]
                assert isinstance(vm_s, torch.Tensor)
                dm_s = vm_s - vm_s.mean(0)
                for dst in models:
                    if src == dst:
                        continue
                    vl_d = info[dst]["val_late"]  # type: ignore[assignment]
                    t_d = info[dst]["t_hat"]  # type: ignore[assignment]
                    assert isinstance(vl_d, torch.Tensor) and isinstance(t_d, torch.Tensor)
                    pred_mid = cand_hub.send(src, dst, dm_s)
                    pred_late = pred_mid @ t_d
                    cos_l = (
                        torch.nn.functional.cosine_similarity(
                            pred_late, vl_d - vl_d.mean(0), dim=-1
                        )
                        .mean()
                        .item()
                    )
                    pair_vals.append(cos_l)
            sc = sum(pair_vals) / len(pair_vals)
            val_grid_scores[f"beta={beta},gamma={gamma}"] = round(sc, 4)
            if sc > best_val_score:
                best_val_score = sc
                best_cfg = (beta, gamma)

    static_hub = H.fit_hub(tr_mid_map, digest, k=k)
    comm_hub = fit_commuting_hub(
        tr_mid_map, tr_late_map, digest, k=k, beta=best_cfg[0], gamma=best_cfg[1]
    )

    late_cos_static: list[float] = []
    late_cos_comm: list[float] = []
    late_cos_diffs: list[float] = []
    late_nn_static: list[float] = []
    late_nn_comm: list[float] = []
    gen_acc_static: list[float] = []
    gen_acc_comm: list[float] = []
    gen_acc_diffs: list[float] = []
    per_pair: dict[str, dict[str, float]] = {}

    for dst, dst_path in models.items():
        mb, tb = T.load(dst_path)
        l_mid = int(info[dst]["layer_mid"])  # type: ignore[arg-type]
        l_late = int(info[dst]["layer_late"])  # type: ignore[arg-type]
        norm_b = float(info[dst]["norm_mid"])  # type: ignore[arg-type]
        dst_late_bank = torch.stack(
            [info[dst]["thoughts_late"][c] for c in concepts]  # type: ignore[index]
        )
        dst_late_unit = torch.nn.functional.normalize(dst_late_bank, dim=-1)

        for src in models:
            if src == dst:
                continue
            pkey = f"{src}->{dst}"
            p_cos_s, p_cos_c, p_gen_s, p_gen_c = [], [], [], []
            p_nn_s, p_nn_c = [], []

            for c_idx, c in enumerate(concepts):
                d_src = info[src]["thoughts_mid"][c]  # type: ignore[index]
                true_late = info[dst]["thoughts_late"][c]  # type: ignore[index]

                # Exact 64-byte PBL1 int8 quantization on the wire for BOTH arms
                raw_s, sc_s = H.quantize(static_hub.encode(src, d_src))
                v_static = static_hub.decode(dst, H.dequantize(raw_s, sc_s))
                v_static = v_static / v_static.norm().clamp_min(1e-12) * coef * norm_b

                raw_c, sc_c = H.quantize(comm_hub.encode(src, d_src))
                v_comm = comm_hub.decode(dst, H.dequantize(raw_c, sc_c))
                v_comm = v_comm / v_comm.norm().clamp_min(1e-12) * coef * norm_b

                # 1. Real nonlinear transformer propagation layer_mid -> layer_late
                delta_late_s = steered_late_delta(mb, tb, PROMPTS[:4], l_mid, l_late, v_static)
                delta_late_c = steered_late_delta(mb, tb, PROMPTS[:4], l_mid, l_late, v_comm)

                cs = float(
                    torch.nn.functional.cosine_similarity(delta_late_s, true_late, dim=0).item()
                )
                cc = float(
                    torch.nn.functional.cosine_similarity(delta_late_c, true_late, dim=0).item()
                )
                p_cos_s.append(cs)
                p_cos_c.append(cc)
                late_cos_static.append(cs)
                late_cos_comm.append(cc)
                late_cos_diffs.append(cc - cs)

                # Nearest-concept retrieval at downstream layer_late
                nn_s = int(torch.argmax(dst_late_unit @ torch.nn.functional.normalize(delta_late_s, dim=0)).item())
                nn_c = int(torch.argmax(dst_late_unit @ torch.nn.functional.normalize(delta_late_c, dim=0)).item())
                p_nn_s.append(1.0 if nn_s == c_idx else 0.0)
                p_nn_c.append(1.0 if nn_c == c_idx else 0.0)
                late_nn_static.append(p_nn_s[-1])
                late_nn_comm.append(p_nn_c[-1])

                # 2. Real greedy text generation + held-out lexicon decoding across PROMPTS
                hits_s = sum(
                    T.lexicon_id(T.generate(mb, tb, l_mid, v_static, p, gen), lex)[0] == c
                    for p in PROMPTS
                ) / len(PROMPTS)
                hits_c = sum(
                    T.lexicon_id(T.generate(mb, tb, l_mid, v_comm, p, gen), lex)[0] == c
                    for p in PROMPTS
                ) / len(PROMPTS)
                p_gen_s.append(hits_s)
                p_gen_c.append(hits_c)
                gen_acc_static.append(hits_s)
                gen_acc_comm.append(hits_c)
                gen_acc_diffs.append(hits_c - hits_s)

            per_pair[pkey] = {
                "downstream_late_cos_static": round(sum(p_cos_s) / len(p_cos_s), 4),
                "downstream_late_cos_commuting": round(sum(p_cos_c) / len(p_cos_c), 4),
                "downstream_nn_top1_static": round(sum(p_nn_s) / len(p_nn_s), 4),
                "downstream_nn_top1_commuting": round(sum(p_nn_c) / len(p_nn_c), 4),
                "lexicon_gen_acc_static": round(sum(p_gen_s) / len(p_gen_s), 4),
                "lexicon_gen_acc_commuting": round(sum(p_gen_c) / len(p_gen_c), 4),
            }
        del mb

    cos_diff_mean, cos_ci_lo, cos_ci_hi = _paired_bootstrap_ci(late_cos_diffs)
    gen_diff_mean, gen_ci_lo, gen_ci_hi = _paired_bootstrap_ci(gen_acc_diffs)

    return {
        "claim_ceiling": "LocalDevelopmentUnreviewed",
        "wire_bytes_per_thought": k,
        "models": {n: info[n]["hidden"] for n in models},
        "selected_hyperparams_on_val": {"beta": best_cfg[0], "gamma": best_cfg[1]},
        "val_grid_scores": val_grid_scores,
        "n_ordered_pairs": len(per_pair),
        "n_unseen_test_concepts": len(concepts),
        "n_paired_concept_transfers": len(late_cos_diffs),
        "summary": {
            "downstream_late_cos_static": round(sum(late_cos_static) / len(late_cos_static), 4),
            "downstream_late_cos_commuting": round(sum(late_cos_comm) / len(late_cos_comm), 4),
            "downstream_late_cos_lift": cos_diff_mean,
            "downstream_late_cos_lift_95ci": [cos_ci_lo, cos_ci_hi],
            "downstream_nn_top1_static": round(sum(late_nn_static) / len(late_nn_static), 4),
            "downstream_nn_top1_commuting": round(sum(late_nn_comm) / len(late_nn_comm), 4),
            "lexicon_gen_acc_static": round(sum(gen_acc_static) / len(gen_acc_static), 4),
            "lexicon_gen_acc_commuting": round(sum(gen_acc_comm) / len(gen_acc_comm), 4),
            "lexicon_gen_acc_lift": gen_diff_mean,
            "lexicon_gen_acc_lift_95ci": [gen_ci_lo, gen_ci_hi],
            "chance_top1": round(1.0 / len(concepts), 4),
        },
        "per_pair": per_pair,
    }


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser()
    ap.add_argument("--model", action="append", required=True, metavar="NAME=PATH")
    ap.add_argument("--frac-mid", type=float, default=0.45)
    ap.add_argument("--frac-late", type=float, default=0.85)
    ap.add_argument("--k", type=int, default=64)
    ap.add_argument("--coef", type=float, default=0.5)
    ap.add_argument("--gen", type=int, default=24)
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    os.environ.setdefault("HF_HUB_OFFLINE", "1")
    model_map = dict(m.split("=", 1) for m in args.model)
    out = run_paired_commutation_benchmark(
        model_map,
        frac_mid=args.frac_mid,
        frac_late=args.frac_late,
        k=args.k,
        coef=args.coef,
        gen=args.gen,
    )
    txt = json.dumps(out, indent=2) + "\n"
    if args.out is not None:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(txt, encoding="utf-8")
    print(txt)


if __name__ == "__main__":
    main()
