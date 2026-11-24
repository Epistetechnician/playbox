#!/usr/bin/env python3
"""Self-contained Colab A100 runner: Dense-model (Qwen3-14B) S-ROUTED replication + 50K single-entity dossier stress test.

Run via:
    colab run --gpu A100 --timeout 0 steer/tools/kv_routed_dense_colab.py
"""
from __future__ import annotations

import concurrent.futures
import json
import math
import os
import random
import re
import subprocess
import sys
import time
import urllib.request
from typing import Any


MODEL = os.environ.get("PLAYBOX_DENSE_MODEL", "Qwen/Qwen3-4B")
BASE_URL = "http://127.0.0.1:8000"


def ensure_vllm() -> None:
    try:
        import vllm  # noqa: F401
        print(f"[setup] vllm already installed: {vllm.__version__}", flush=True)
    except ImportError:
        print("[setup] installing vllm...", flush=True)
        subprocess.run([sys.executable, "-m", "pip", "install", "-q", "vllm"], check=True)


def start_vllm(model: str, max_model_len: int = 40960) -> subprocess.Popen:
    cmd = [
        sys.executable, "-m", "vllm.entrypoints.openai.api_server",
        "--model", model,
        "--host", "127.0.0.1",
        "--port", "8000",
        "--enable-prefix-caching",
        "--max-model-len", str(max_model_len),
        "--gpu-memory-utilization", "0.88",
        "--dtype", "bfloat16",
        "--enforce-eager",
    ]
    print(f"[vllm] launching: {' '.join(cmd)}", flush=True)
    log_f = open("/tmp/vllm_dense.log", "w")
    proc = subprocess.Popen(cmd, stdout=log_f, stderr=subprocess.STDOUT)
    t0 = time.time()
    while time.time() - t0 < 600:
        if proc.poll() is not None:
            print("[vllm] exited early! tail:", flush=True)
            print(open("/tmp/vllm_dense.log").read()[-8000:], flush=True)
            raise RuntimeError("vllm exited early")
        try:
            with urllib.request.urlopen(f"{BASE_URL}/v1/models", timeout=3) as r:
                if r.status == 200:
                    print(f"[vllm] ready in {time.time() - t0:.1f}s", flush=True)
                    return proc
        except Exception:
            time.sleep(5)
    raise TimeoutError("vllm did not become ready within 600s")


def chat_completion(system: str, user: str, max_tokens: int = 64) -> dict[str, Any]:
    payload = {
        "model": MODEL,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "temperature": 0.0,
        "max_tokens": max_tokens,
        "chat_template_kwargs": {"enable_thinking": False},
    }
    req = urllib.request.Request(
        f"{BASE_URL}/v1/chat/completions",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
    )
    t0 = time.perf_counter()
    with urllib.request.urlopen(req, timeout=180) as r:
        data = json.loads(r.read().decode())
    dt = time.perf_counter() - t0
    usage = data.get("usage") or {}
    ptd = usage.get("prompt_tokens_details") or {}
    cached = int(ptd.get("cached_tokens") or 0)
    ptoks = int(usage.get("prompt_tokens") or 0)
    text = (data["choices"][0]["message"].get("content") or "").strip()
    return {
        "text": text,
        "latency_s": dt,
        "prompt_tokens": ptoks,
        "cached_tokens": cached,
        "cache_hit_pct": (100.0 * cached / ptoks) if ptoks else 0.0,
    }


def build_crm_corpus(n_entities: int = 8, filler_paras: int = 18, seed: int = 42) -> tuple[dict[str, str], list[dict[str, Any]]]:
    rng = random.Random(seed)
    regions = ["APAC-East", "EMEA-North", "NA-West", "LATAM-South", "EU-Central", "APAC-South", "NA-East", "UK-Core"]
    tiers = ["Enterprise-Platinum", "Growth-Gold", "Strategic-Diamond", "Core-Silver"]
    dossiers: dict[str, str] = {}
    turns: list[dict[str, Any]] = []
    for i in range(n_entities):
        eid = f"ACME-{100 + i}"
        alias = f"account_{i}"
        owner = f"rep_{chr(65 + i)}{10 + i}"
        renewal = f"2026-{(i % 12) + 1:02d}-{(i * 3 + 7) % 28 + 1:02d}"
        arr = f"${(i + 2) * 145},000"
        sla_code = f"SLA-{7000 + i * 37}"
        region = regions[i % len(regions)]
        tier = tiers[i % len(tiers)]
        lines = [
            f"# Customer Dossier: {eid} ({alias})",
            f"- Account ID: {eid}",
            f"- Primary Owner: {owner}",
            f"- Region: {region}",
            f"- Tier: {tier}",
            f"- Annual Recurring Revenue (ARR): {arr}",
            f"- Contract Renewal Date: {renewal}",
            f"- Escalation SLA Code: {sla_code}",
            "",
        ]
        for p in range(filler_paras):
            lines.append(
                f"## Quarterly Operations Log Q{(p % 4) + 1}-{2022 + p // 4} for {eid}\n"
                f"During review cycle #{p + 1}, {eid} completed {12 + (p * 7 + i) % 40} integration checkpoints "
                f"across {region} clusters with {99.1 + ((p + i) % 8) * 0.1:.2f}% uptime and zero P0 regressions. "
                f"Billing audit confirmed invoice batch INV-{i}{p:02d} settled in net-30 terms under {tier} controls.\n"
            )
        dossiers[eid] = "\n".join(lines)
        # Clean + noisy (entity-name-omitted / alias-only) queries:
        turns.extend([
            {"entity": eid, "noisy": False, "q": f"What is the exact Escalation SLA Code for {eid}? Reply with only the code.", "expect": sla_code},
            {"entity": eid, "noisy": False, "q": f"What is the exact Contract Renewal Date for {eid}? Reply with YYYY-MM-DD only.", "expect": renewal},
            {"entity": eid, "noisy": True, "q": f"For {alias} (owned by {owner}), what is the exact ARR? Reply with the dollar amount only.", "expect": arr},
            {"entity": eid, "noisy": True, "q": f"Under {owner}'s {region} account, what is the Escalation SLA Code? Reply with only the code.", "expect": sla_code},
        ])
    rng.shuffle(turns)
    return dossiers, turns


def simple_bm25_chunks(dossiers: dict[str, str], query: str, top_k: int = 3) -> str:
    q_toks = set(re.findall(r"[a-z0-9_-]+", query.lower()))
    scored = []
    for eid, text in dossiers.items():
        for chunk in text.split("\n\n"):
            c_toks = set(re.findall(r"[a-z0-9_-]+", chunk.lower()))
            overlap = len(q_toks & c_toks)
            scored.append((overlap, eid, chunk))
    scored.sort(key=lambda x: x[0], reverse=True)
    return "\n\n".join(c for _, _, c in scored[:top_k])


def run_arm(name: str, dossiers: dict[str, str], turns: list[dict[str, Any]], concurrency: int = 8) -> dict[str, Any]:
    full_ctx = "\n\n---\n\n".join(dossiers.values())
    # Repeat turns 2x to measure warm prefix-cache steady state
    work = turns + turns

    def _one(t: dict[str, Any]) -> dict[str, Any]:
        if name == "S-CTX":
            sys_p = "Answer concisely using the full CRM corpus below.\n\n" + full_ctx
        elif name == "S-RAG":
            sys_p = "Answer concisely using the retrieved CRM snippets below.\n\n" + simple_bm25_chunks(dossiers, t["q"], top_k=3)
        else:  # S-ROUTED
            sys_p = "Answer concisely using the routed entity dossier below.\n\n" + dossiers[t["entity"]]
        res = chat_completion(sys_p, t["q"], max_tokens=32)
        ok = t["expect"].lower() in res["text"].lower()
        return {**res, "correct": ok, "noisy": t["noisy"], "entity": t["entity"]}

    t0 = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as ex:
        rows = list(ex.map(_one, work))
    wall = time.perf_counter() - t0
    clean = [r for r in rows if not r["noisy"]]
    noisy = [r for r in rows if r["noisy"]]
    return {
        "arm": name,
        "turns": len(rows),
        "wall_s": round(wall, 2),
        "turns_per_min": round(len(rows) / (wall / 60.0), 1),
        "accuracy_pct": round(100.0 * sum(r["correct"] for r in rows) / len(rows), 1),
        "clean_acc_pct": round(100.0 * sum(r["correct"] for r in clean) / max(1, len(clean)), 1),
        "noisy_acc_pct": round(100.0 * sum(r["correct"] for r in noisy) / max(1, len(noisy)), 1),
        "mean_prompt_tokens": round(sum(r["prompt_tokens"] for r in rows) / len(rows), 1),
        "mean_cache_hit_pct": round(sum(r["cache_hit_pct"] for r in rows) / len(rows), 1),
        "p95_latency_s": round(sorted(r["latency_s"] for r in rows)[int(0.95 * (len(rows) - 1))], 3),
    }


def run_50k_dossier_stress() -> dict[str, Any]:
    """Single-entity 50K-token dossier: full-entity KV prefix vs sub-dossier adaptive chunk routing."""
    needle_code = "NEEDLE-OMEGA-94821"
    needle_owner = "dr_elena_vance"
    paras = []
    for p in range(440):
        if p == 220:
            paras.append(
                f"## Section {p}: Confidential Escalation Override\n"
                f"For emergency incident recovery on cluster TITAN-9, the authoritative Escalation Override Code is {needle_code}, "
                f"approved by {needle_owner} under policy SEC-409.\n"
            )
        else:
            paras.append(
                f"## Section {p}: Routine Telemetry Archive\n"
                f"Cycle {p:04d} recorded {40 + (p * 13) % 90} nominal heartbeats across shard-{p % 16} "
                f"with median queue depth {(p * 3) % 11} ms, zero packet drops, and checksum CHK-{p * 97:06x}. "
                f"Storage compaction reclaimed {120 + (p * 17) % 500} MiB across replica group RG-{p % 8}.\n"
            )
    big_dossier = "\n\n".join(paras)
    queries = [
        {"q": "What is the authoritative Escalation Override Code for cluster TITAN-9? Reply with only the code.", "expect": needle_code},
        {"q": "Who approved the Escalation Override Code under policy SEC-409? Reply with only the name.", "expect": needle_owner},
        {"q": "Which policy governs the TITAN-9 Escalation Override Code? Reply with only the policy ID.", "expect": "SEC-409"},
        {"q": "Repeat the exact Escalation Override Code for cluster TITAN-9.", "expect": needle_code},
    ]

    # Arm A: Full 50K single-entity KV prefix (cold first turn + warm cached subsequent turns)
    full_rows = []
    t0 = time.perf_counter()
    for q in queries:
        r = chat_completion("Answer concisely from this 50K-token entity dossier.\n\n" + big_dossier, q["q"], max_tokens=32)
        full_rows.append({**r, "correct": q["expect"].lower() in r["text"].lower()})
    full_wall = time.perf_counter() - t0

    # Arm B: Adaptive sub-dossier BM25 chunking (top-5 chunks from the 50K dossier)
    sub_rows = []
    t1 = time.perf_counter()
    for q in queries:
        sub = simple_bm25_chunks({"TITAN-9": big_dossier}, q["q"], top_k=5)
        r = chat_completion("Answer concisely from these routed dossier sections.\n\n" + sub, q["q"], max_tokens=32)
        sub_rows.append({**r, "correct": q["expect"].lower() in r["text"].lower()})
    sub_wall = time.perf_counter() - t1

    return {
        "dossier_chars": len(big_dossier),
        "full_50k_kv": {
            "prompt_tokens": full_rows[0]["prompt_tokens"],
            "cold_latency_s": round(full_rows[0]["latency_s"], 3),
            "warm_mean_latency_s": round(sum(r["latency_s"] for r in full_rows[1:]) / max(1, len(full_rows) - 1), 3),
            "warm_cache_hit_pct": round(sum(r["cache_hit_pct"] for r in full_rows[1:]) / max(1, len(full_rows) - 1), 1),
            "accuracy_pct": round(100.0 * sum(r["correct"] for r in full_rows) / len(full_rows), 1),
            "wall_s": round(full_wall, 2),
        },
        "adaptive_sub_dossier": {
            "mean_prompt_tokens": round(sum(r["prompt_tokens"] for r in sub_rows) / len(sub_rows), 1),
            "mean_latency_s": round(sum(r["latency_s"] for r in sub_rows) / len(sub_rows), 3),
            "accuracy_pct": round(100.0 * sum(r["correct"] for r in sub_rows) / len(sub_rows), 1),
            "wall_s": round(sub_wall, 2),
        },
    }


def main() -> None:
    print(subprocess.check_output(["nvidia-smi", "--query-gpu=name,memory.total", "--format=csv,noheader"]).decode().strip(), flush=True)
    ensure_vllm()
    proc = start_vllm(MODEL, max_model_len=40960)
    try:
        dossiers, turns = build_crm_corpus(n_entities=8, filler_paras=18)
        exp_a = [run_arm(arm, dossiers, turns, concurrency=8) for arm in ("S-RAG", "S-CTX", "S-ROUTED")]
        exp_b = run_50k_dossier_stress()
        out = {
            "schema": "playbox.s_routed_dense_replication.v1",
            "model": MODEL,
            "exp_a_dense_crm": exp_a,
            "exp_b_50k_single_entity": exp_b,
        }
        print("=== RESULT_JSON_START ===", flush=True)
        print(json.dumps(out, indent=2), flush=True)
        print("=== RESULT_JSON_END ===", flush=True)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except Exception:
            proc.kill()


if __name__ == "__main__":
    main()
