#!/usr/bin/env python3
"""Generate publication charts for the routed per-account KV + adaptive RAG eval."""
from __future__ import annotations

from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np

OUT_DIR = Path(__file__).resolve().parents[2] / "docs" / "assets" / "kv-routed"
OUT_DIR.mkdir(parents=True, exist_ok=True)

C_CTX = "#8E9AAF"
C_RAG = "#E07A5F"
C_ROUTED = "#2A9D8F"
C_CPU = "#F4A261"

plt.rcParams.update(
    {
        "font.family": "DejaVu Sans",
        "axes.spines.top": False,
        "axes.spines.right": False,
        "axes.grid": True,
        "grid.alpha": 0.25,
        "axes.axisbelow": True,
        "figure.dpi": 180,
    }
)


def make_clean_3arm() -> None:
    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(11.5, 4.4))
    loads = ["n=1", "n=8", "n=32"]
    x = np.arange(len(loads))
    w = 0.25

    ctx_tpm = [43.7, 82.5, 65.3]
    rag_tpm = [88.5, 159.8, 195.9]
    routed_tpm = [144.4, 447.1, 706.5]

    b1 = ax1.bar(x - w, ctx_tpm, w, label="S-CTX (128K Shared KV)", color=C_CTX)
    b2 = ax1.bar(x, rag_tpm, w, label="S-RAG (Inline Search+Rerank)", color=C_RAG)
    b3 = ax1.bar(x + w, routed_tpm, w, label="S-ROUTED (~3.8K KV + Adaptive RAG)", color=C_ROUTED)
    for bars in (b1, b2, b3):
        for bar in bars:
            h = bar.get_height()
            ax1.text(bar.get_x() + bar.get_width() / 2, h + 14, f"{h:.0f}", ha="center", va="bottom", fontsize=8.5, fontweight="bold")
    ax1.set_xticks(x)
    ax1.set_xticklabels(loads, fontsize=10)
    ax1.set_ylabel("Throughput (turns / min)", fontsize=10)
    ax1.set_ylim(0, 810)
    ax1.set_title("Throughput vs Concurrency (higher is better)", fontsize=11, fontweight="bold")
    ax1.legend(frameon=False, fontsize=8.5, loc="upper left")

    metrics = ["p95 q→first\n(all turns)", "p95 q→end\n(all turns)", "p95 q→first\n(turns 2–6)"]
    x2 = np.arange(len(metrics))
    ctx_lat = [11.75, 40.97, 4.50]
    rag_lat = [5.65, 14.33, 5.40]
    routed_lat = [2.01, 4.93, 0.54]

    b4 = ax2.bar(x2 - w, ctx_lat, w, label="S-CTX", color=C_CTX)
    b5 = ax2.bar(x2, rag_lat, w, label="S-RAG", color=C_RAG)
    b6 = ax2.bar(x2 + w, routed_lat, w, label="S-ROUTED", color=C_ROUTED)
    for bars in (b4, b5, b6):
        for bar in bars:
            h = bar.get_height()
            ax2.text(bar.get_x() + bar.get_width() / 2, h + 0.8, f"{h:.2f}s", ha="center", va="bottom", fontsize=8, fontweight="bold")
    ax2.set_xticks(x2)
    ax2.set_xticklabels(metrics, fontsize=9.5)
    ax2.set_ylabel("Pooled p95 Latency (seconds)", fontsize=10)
    ax2.set_ylim(0, 47)
    ax2.set_title("Tail Latency at n=32 (lower is better)", fontsize=11, fontweight="bold")

    fig.suptitle(
        "A100-80GB · Qwen3-30B-A3B-FP8 · 3,456 Measured Turns (2 Counterbalanced Sweeps, 0 Errors)",
        fontsize=11.5,
        fontweight="bold",
        y=1.01,
    )
    fig.tight_layout()
    fig.savefig(OUT_DIR / "01_clean_3arm.png", bbox_inches="tight")
    plt.close(fig)


def make_noisy_routing() -> None:
    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(11.0, 4.3))
    groups = ["Clean CRM\n(n=32)", "Noisy + Implicit\n(Typos + 'this account')"]
    x = np.arange(len(groups))
    w = 0.32

    rag_acc = [99.5, 74.9]
    routed_acc = [97.9, 98.1]
    b1 = ax1.bar(x - w / 2, rag_acc, w, label="S-RAG (Inline)", color=C_RAG)
    b2 = ax1.bar(x + w / 2, routed_acc, w, label="S-ROUTED (Arm C)", color=C_ROUTED)
    for bars in (b1, b2):
        for bar in bars:
            h = bar.get_height()
            ax1.text(bar.get_x() + bar.get_width() / 2, h + 1.2, f"{h:.1f}%", ha="center", va="bottom", fontsize=9.5, fontweight="bold")
    ax1.annotate(
        "−24.6 pp collapse\n(p = 1.44e-20)",
        xy=(1 - w / 2, 74.9),
        xytext=(0.45, 55),
        arrowprops={"arrowstyle": "->", "color": "#B54434", "lw": 1.4},
        fontsize=8.5,
        fontweight="bold",
        color="#B54434",
        bbox={"boxstyle": "round,pad=0.3", "facecolor": "white", "edgecolor": "#B54434", "alpha": 0.95},
    )
    ax1.set_xticks(x)
    ax1.set_xticklabels(groups, fontsize=10)
    ax1.set_ylabel("Accuracy (%)", fontsize=10)
    ax1.set_ylim(45, 110)
    ax1.set_title("Accuracy Under Noisy / Implicit Queries", fontsize=11, fontweight="bold")
    ax1.legend(frameon=True, facecolor="white", framealpha=0.92, edgecolor="none", fontsize=8.5, loc="lower left")

    cats = ["Cat A\n(Account)", "Cat B\n(Look-alike)", "Cat D\n(Policy/Multi)"]
    x2 = np.arange(len(cats))
    rag_cat = [64.1, 96.9, 96.1]
    routed_cat = [97.7, 100.0, 98.0]
    b3 = ax2.bar(x2 - w / 2, rag_cat, w, label="S-RAG (181.7 tpm)", color=C_RAG)
    b4 = ax2.bar(x2 + w / 2, routed_cat, w, label="S-ROUTED (634.5 tpm · 3.49×)", color=C_ROUTED)
    for bars in (b3, b4):
        for bar in bars:
            h = bar.get_height()
            ax2.text(bar.get_x() + bar.get_width() / 2, h + 1.2, f"{h:.1f}%", ha="center", va="bottom", fontsize=8.5, fontweight="bold")
    ax2.set_xticks(x2)
    ax2.set_xticklabels(cats, fontsize=9.5)
    ax2.set_ylabel("Noisy-Run Accuracy by Category (%)", fontsize=10)
    ax2.set_ylim(45, 114)
    ax2.set_title("Category Breakdown (Noisy Run, 768 Turns)", fontsize=11, fontweight="bold")
    ax2.legend(frameon=True, facecolor="white", framealpha=0.92, edgecolor="none", fontsize=8.5, loc="lower left")

    fig.suptitle(
        "Stress Test 1: Turn-1 Typos + Turn 2–6 Pronoun Follow-Ups (A100-80GB, n=32)",
        fontsize=11.5,
        fontweight="bold",
        y=1.01,
    )
    fig.tight_layout()
    fig.savefig(OUT_DIR / "02_noisy_routing.png", bbox_inches="tight")
    plt.close(fig)


def make_evict192_cpu() -> None:
    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(11.5, 4.4))

    arms = ["S-RAG\n(Inline)", "S-ROUTED\n(Arm C)"]
    x = np.arange(len(arms))
    tpms = [107.2, 436.3]
    bars = ax1.bar(x, tpms, width=0.45, color=[C_RAG, C_ROUTED])
    for bar in bars:
        h = bar.get_height()
        ax1.text(bar.get_x() + bar.get_width() / 2, h + 12, f"{h:.1f} tpm", ha="center", va="bottom", fontsize=10, fontweight="bold")
    ax1.annotate("4.07× throughput\n6.66× lower p95 TTFT\n(1.99s vs 13.28s)", xy=(1, 436.3), xytext=(0.08, 340), fontsize=9, fontweight="bold", color="#1D6F63")
    ax1.set_xticks(x)
    ax1.set_xticklabels(arms, fontsize=10)
    ax1.set_ylabel("Throughput (turns / min)", fontsize=10)
    ax1.set_ylim(0, 510)
    ax1.set_title("192-Account Wave Eviction Throughput", fontsize=11, fontweight="bold")

    gpu_pct = [6.7, 52.9]
    cpu_pct = [0.4, 11.9]
    ax2.bar(x, gpu_pct, width=0.45, label="GPU HBM Prefix Hit % of prompt toks", color=C_ROUTED)
    ax2.bar(x, cpu_pct, width=0.45, bottom=gpu_pct, label="CPU Tier Reload % of prompt toks (OffloadingConnector)", color=C_CPU)
    ax2.text(0, 7.1 + 2.2, "7.1% total\n(27.8K CPU toks)", ha="center", va="bottom", fontsize=9, fontweight="bold")
    ax2.text(1, 64.8 + 2.2, "64.8% total (52.9% GPU + 11.9% CPU)\n25.2% of GPU misses · 526.2K CPU toks", ha="center", va="bottom", fontsize=8.5, fontweight="bold")
    ax2.set_xticks(x)
    ax2.set_xticklabels(arms, fontsize=10)
    ax2.set_ylabel("Cached Prompt Tokens (%)", fontsize=10)
    ax2.set_ylim(0, 96)
    ax2.set_title("Verified GPU→CPU KV Offloading (24 GB CPU Tier)", fontsize=11, fontweight="bold")
    ax2.legend(frameon=False, fontsize=8, loc="upper left")

    fig.suptitle(
        "Stress Test 2: 192 Isolated Accounts (~737K KV Tokens) in Waves · 2,304 Turns · 0 Errors",
        fontsize=11.5,
        fontweight="bold",
        y=1.01,
    )
    fig.tight_layout()
    fig.savefig(OUT_DIR / "03_evict192_cpu.png", bbox_inches="tight")
    plt.close(fig)


def make_sota_generalization() -> None:
    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(11.5, 4.4))

    datasets = ["FinanceBench\n(150 Qs · 32 cos · ~5.9K)", "LoCoMo\n(300 Qs · 10 convs · ~18.1K)"]
    x = np.arange(len(datasets))
    w = 0.25

    acc_rag = [42.7, 49.3]
    acc_conv = [44.0, 49.0]
    acc_routed = [64.7, 57.7]

    b1 = ax1.bar(x - w, acc_rag, w, label="S-RAG (Stateless BM25)", color=C_RAG)
    b2 = ax1.bar(x, acc_conv, w, label="S-CONV-RAG (+ LLM Rewriter)", color=C_CPU)
    b3 = ax1.bar(x + w, acc_routed, w, label="S-ROUTED (Entity KV + Fallback)", color=C_ROUTED)
    for bars in (b1, b2, b3):
        for bar in bars:
            h = bar.get_height()
            ax1.text(bar.get_x() + bar.get_width() / 2, h + 1.2, f"{h:.1f}%", ha="center", va="bottom", fontsize=8.5, fontweight="bold")
    ax1.annotate("+22.0 pp vs RAG\n(p = 1.03e-7)", xy=(0 + w, 64.7), xytext=(-0.18, 72), fontsize=8.5, fontweight="bold", color="#1D6F63")
    ax1.annotate("+8.7 pp vs Conv-RAG\n(p = 5.35e-4)", xy=(1 + w, 57.7), xytext=(0.76, 65), fontsize=8.5, fontweight="bold", color="#1D6F63")
    ax1.set_xticks(x)
    ax1.set_xticklabels(datasets, fontsize=9.5)
    ax1.set_ylabel("Accuracy (%)", fontsize=10)
    ax1.set_ylim(0, 102)
    ax1.set_title("Accuracy: Routed KV vs RAG & Conversational RAG", fontsize=11, fontweight="bold")
    ax1.legend(frameon=False, fontsize=8, loc="upper right")

    tpm_rag = [124.7, 205.9]
    tpm_conv = [103.2, 178.0]
    tpm_routed = [147.6, 231.9]

    b1 = ax2.bar(x - w, tpm_rag, w, label="S-RAG", color=C_RAG)
    b2 = ax2.bar(x, tpm_conv, w, label="S-CONV-RAG (+ LLM Rewriter)", color=C_CPU)
    b3 = ax2.bar(x + w, tpm_routed, w, label="S-ROUTED", color=C_ROUTED)
    for bars in (b1, b2, b3):
        for bar in bars:
            h = bar.get_height()
            ax2.text(bar.get_x() + bar.get_width() / 2, h + 4, f"{h:.0f}", ha="center", va="bottom", fontsize=8.5, fontweight="bold")
    ax2.annotate("1.43× vs Conv-RAG\n0.45s follow-up p95", xy=(0 + w, 147.6), xytext=(-0.22, 195), fontsize=8.5, fontweight="bold", color="#1D6F63")
    ax2.annotate("1.30× vs Conv-RAG\n(even at 18.1K toks!)", xy=(1 + w, 231.9), xytext=(0.62, 260), fontsize=8.5, fontweight="bold", color="#1D6F63")
    ax2.set_xticks(x)
    ax2.set_xticklabels(datasets, fontsize=9.5)
    ax2.set_ylabel("Throughput (turns / min, n=8)", fontsize=10)
    ax2.set_ylim(0, 305)
    ax2.set_title("End-to-End Throughput (n=8, A100-40GB + 24 GB CPU Tier)", fontsize=11, fontweight="bold")

    fig.suptitle(
        "Cross-Dataset Generalization: FinanceBench & LoCoMo (Qwen3-30B-A3B-FP8 · 1,350 Turns · 0 Errors)",
        fontsize=11.5,
        fontweight="bold",
        y=1.01,
    )
    fig.tight_layout()
    fig.savefig(OUT_DIR / "04_sota_generalization.png", bbox_inches="tight")
    plt.close(fig)


if __name__ == "__main__":
    make_clean_3arm()
    make_noisy_routing()
    make_evict192_cpu()
    make_sota_generalization()
    print(f"Wrote 4 charts to {OUT_DIR}")
