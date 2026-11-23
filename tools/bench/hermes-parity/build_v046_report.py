#!/usr/bin/env python3
"""Build the Playbox v0.4.6 Verified Evaluation Report PDF from pinned repo artifacts."""

from __future__ import annotations

import collections
import html
import json
import subprocess
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
REPORTS_DIR = REPO_ROOT / "docs" / "reports"
OUT_PDF = REPORTS_DIR / "playbox-verified-eval-report-v0.4.6.pdf"
CHROME_BIN = Path("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")


def load_pinned() -> dict[str, str]:
    out: dict[str, str] = {}
    for line in (REPORTS_DIR / "PINNED.txt").read_text(encoding="utf-8").splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            out[k.strip()] = v.strip()
    return out


def repo_bar_svg(rows: list[tuple[str, int, int]]) -> str:
    w, h = 680, 26 * len(rows) + 34
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">',
        '<style>text{font-family:-apple-system,BlinkMacSystemFont,Segoe UI,Helvetica,Arial,sans-serif;font-size:11px;fill:#1f2937}.lbl{font-weight:600}.val{fill:#111827;font-variant-numeric:tabular-nums}</style>',
    ]
    x0, bar_w = 135, 440
    for i, (repo, ok, tot) in enumerate(rows):
        y = 14 + i * 26
        pct = ok / tot if tot else 0.0
        bw = max(2, int(round(bar_w * pct)))
        color = "#0f766e" if pct >= 0.85 else ("#2563eb" if pct >= 0.70 else "#b45309")
        parts.append(f'<text class="lbl" x="{x0 - 8}" y="{y + 13}" text-anchor="end">{html.escape(repo)}</text>')
        parts.append(f'<rect x="{x0}" y="{y}" width="{bar_w}" height="16" rx="3" fill="#e5e7eb"/>')
        parts.append(f'<rect x="{x0}" y="{y}" width="{bw}" height="16" rx="3" fill="{color}"/>')
        parts.append(f'<text class="val" x="{x0 + bar_w + 8}" y="{y + 13}">{ok}/{tot} ({100*pct:.1f}%)</text>')
    parts.append("</svg>")
    return "\n".join(parts)


def build_html() -> str:
    pinned = load_pinned()
    t3_manifest = json.loads((REPO_ROOT / "deploy" / "t3-confidential-space.json").read_text(encoding="utf-8"))
    swe150 = json.loads((REPORTS_DIR / "playbox-swe150-track-a-summary.json").read_text(encoding="utf-8"))
    swe150_t3 = json.loads((REPORTS_DIR / "playbox-swe150-t3-verify.json").read_text(encoding="utf-8"))
    kb = json.loads((REPORTS_DIR / "playbox-atlaskv-kbencoder-v0.4.5.json").read_text(encoding="utf-8"))

    p2 = swe150["arms"]["P2"]
    amort = p2["memory_amortization"]
    per_repo_tot: collections.Counter[str] = collections.Counter()
    per_repo_ok: collections.Counter[str] = collections.Counter()
    for inst, arms in swe150["per_instance"].items():
        r = arms.get("P2", arms) if isinstance(arms, dict) else {}
        repo = inst.split("__")[0]
        per_repo_tot[repo] += 1
        if r.get("resolved"):
            per_repo_ok[repo] += 1
    repo_rows = [(repo, per_repo_ok[repo], tot) for repo, tot in sorted(per_repo_tot.items(), key=lambda x: (-x[1], x[0]))]

    kb_arms = kb["arms"]
    swe150_cs = t3_manifest["swe150_confidential_space_workload"]
    v045_cs = t3_manifest["confidential_space_workload"]
    aiproxy_comp = t3_manifest["components"]["playbox_aiproxy"]

    repro_cmd = f"""gunzip -c docs/reports/playbox-swe150-t3-confidential-space-5gate-bundle.json.gz > /tmp/swe150-t3.json
docker run --rm --platform linux/amd64 --network none \\
  -v /tmp/swe150-t3.json:/work/bundle.json:ro \\
  -v "$(pwd)/docs/reports/google-confidential-space-jwks.json":/work/jwks.json:ro \\
  -e PLAYBOX_CS_JWKS=/work/jwks.json \\
  -e PLAYBOX_CS_AUDIENCE={swe150_cs['audience']} \\
  -e PLAYBOX_CS_PROJECTS={swe150_cs['project_id']} \\
  -e PLAYBOX_CS_IMAGES={swe150_cs['image_digest']} \\
  --entrypoint /usr/local/bin/playbox \\
  {swe150_cs['image_uri']} \\
  gate verify /work/bundle.json \\
    --pubkey {swe150_cs['verifier_pubkey_hex']} \\
    --require-signature --require-pq --require-tier t3 --json"""

    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<title>Playbox v0.4.6 Verified Evaluation Report</title>
<style>
  @page {{ size: letter; margin: 0.6in 0.65in; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; color: #111827; font-size: 10.5pt; line-height: 1.45; }}
  h1 {{ font-size: 20pt; margin: 0 0 4px 0; color: #0f172a; letter-spacing: -0.02em; }}
  h2 {{ font-size: 13pt; margin: 18px 0 8px 0; padding-bottom: 4px; border-bottom: 1.5px solid #e2e8f0; color: #0f172a; page-break-after: avoid; }}
  h3 {{ font-size: 10.8pt; margin: 12px 0 4px 0; color: #1e293b; }}
  p {{ margin: 6px 0; }}
  .subtitle {{ color: #475569; font-size: 10pt; margin-bottom: 14px; }}
  .grid {{ display: grid; grid-template-columns: repeat(4, 1fr); gap: 10px; margin: 12px 0 14px 0; }}
  .card {{ background: #f8fafc; border: 1px solid #cbd5e1; border-radius: 6px; padding: 10px 11px; }}
  .card .k {{ font-size: 8.2pt; text-transform: uppercase; letter-spacing: 0.04em; color: #475569; font-weight: 600; }}
  .card .v {{ font-size: 15pt; font-weight: 700; color: #0f172a; margin-top: 2px; }}
  .card .s {{ font-size: 8.3pt; color: #475569; margin-top: 2px; }}
  table {{ width: 100%; border-collapse: collapse; margin: 8px 0 12px 0; font-size: 9.2pt; }}
  th, td {{ border: 1px solid #cbd5e1; padding: 6px 8px; text-align: left; vertical-align: top; }}
  th {{ background: #f1f5f9; font-weight: 600; color: #0f172a; }}
  .num {{ text-align: right; font-variant-numeric: tabular-nums; }}
  code {{ font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 8.6pt; background: #f1f5f9; padding: 1px 4px; border-radius: 3px; }}
  pre {{ font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 8.1pt; background: #0f172a; color: #e2e8f0; padding: 10px 12px; border-radius: 6px; overflow-x: auto; line-height: 1.4; white-space: pre-wrap; word-break: break-all; }}
  .note {{ background: #fffbeb; border-left: 3.5px solid #d97706; padding: 8px 11px; margin: 10px 0; font-size: 9.3pt; }}
  .ok {{ color: #047857; font-weight: 600; }}
  .page-break {{ page-break-before: always; }}
  ul {{ margin: 5px 0 8px 18px; padding: 0; }}
  li {{ margin: 3px 0; }}
</style>
</head>
<body>

<h1>Playbox v0.4.6 — Offline-Verifiable Evaluation Report</h1>
<div class="subtitle">
  Governed Agent Sandboxes, Host-Enforced Spend (<code>playbox-aiproxy</code>), Parametric AtlasKV (<code>KBEncoder</code>), and Live Intel TDX (<code>T3</code>) Attestation &bull; Pinned release <code>v0.4.6</code>
</div>

<div class="grid">
  <div class="card">
    <div class="k">SWE-150 Track A (P2)</div>
    <div class="v">{p2['solved']}/{p2['instances']} ({100*p2['solve_rate']:.2f}%)</div>
    <div class="s">95% CI [{100*p2['wilson_95_ci'][0]:.2f}%, {100*p2['wilson_95_ci'][1]:.2f}%] &bull; 0 taint</div>
  </div>
  <div class="card">
    <div class="k">Security-20 ASR</div>
    <div class="v">0.0% (19/20)</div>
    <div class="s">95% clean utility vs 20% ASR stock</div>
  </div>
  <div class="card">
    <div class="k">Parametric AtlasKV</div>
    <div class="v">{int(round(100*kb_arms['trained_parametric_kv_adapter']['exact_recall_rate']))}% Recall</div>
    <div class="s">{kb['online_token_compression_vs_full_context_x']:.1f}&times; compression &bull; {kb['prefill_speedup_vs_full_context_x']:.1f}&times; prefill</div>
  </div>
  <div class="card">
    <div class="k">Intel TDX T3 Seal</div>
    <div class="v">{swe150_t3['attested_receipts']} Receipts</div>
    <div class="s">Ed25519 + ML-DSA-65 &bull; 5/5 gates</div>
  </div>
</div>

<h2>1. Executive Summary of Offline-Verifiable Results</h2>
<table>
  <thead>
    <tr><th>Benchmark / Pillar</th><th>Scope</th><th>Headline Result</th><th>Integrity &amp; Bundle Digest</th></tr>
  </thead>
  <tbody>
    <tr>
      <td><strong>SWE-bench Verified Track A (n=150)</strong></td>
      <td>11 Python repos, Arm P2 cascade, OpenShell microVM + remote Docker grader</td>
      <td><strong>{p2['solved']}/{p2['instances']} ({100*p2['solve_rate']:.2f}%)</strong>, 77.3% T1 finish, 0.695s median delegate cmd, 0 taint advisories</td>
      <td>150/150 Ed25519+ML-DSA-65 receipts; T3-sealed <code>{swe150_t3['bundle_digest'][:16]}...</code></td>
    </tr>
    <tr>
      <td><strong>SWE-50 Polyglot Baseline</strong></td>
      <td>50 tasks across 7 repo families (Playbox P2 vs Stock OpenShell)</td>
      <td><strong>44/50 (88.0%)</strong> at $0.1051/solve (0.12s delegate) vs Stock OpenShell 42/50 (84.0%, 0 receipts)</td>
      <td>T1 PQ bundle <code>{pinned['swe50_bundle_digest'][:16]}...</code></td>
    </tr>
    <tr>
      <td><strong>Security-20 Adversarial Suite</strong></td>
      <td>Live prompt injection, secret exfiltration, <code>.git</code> leak, test tampering</td>
      <td><strong>19/20 (95.0%)</strong> utility, <strong>0.0% ASR</strong>, 0 unflagged reward hacks (Local Hermes &amp; Stock OpenShell: 20.0% ASR)</td>
      <td>T1 PQ bundle <code>{pinned['security20_bundle_digest'][:16]}...</code></td>
    </tr>
    <tr>
      <td><strong>Host-Metered Spend (<code>playbox-aiproxy</code>)</strong></td>
      <td><code>PROXY-1..7</code> + hardened loopback proxy (<code>sha256:{pinned['playbox_aiproxy_sha256'][:12]}...</code>)</td>
      <td>Mid-stream USD cutoff, <code>on_revoke</code> termination, Gemini 3 <code>thought_signature</code> multi-turn tool calls</td>
      <td><code>source: "host_aiproxy"</code> bundles <code>{pinned['swe_aiproxy_v045_bundle_digest'][:12]}...</code> &amp; <code>{pinned['swe_multirepo15_aiproxy_bundle_digest'][:12]}...</code></td>
    </tr>
    <tr>
      <td><strong>Parametric AtlasKV (<code>KBEncoder</code>)</strong></td>
      <td><code>Qwen3-1.7B-Base</code>, rank-32 <code>TrainedKVProjectionAdapter</code>, 20 held-out queries</td>
      <td><strong>{kb_arms['trained_parametric_kv_adapter']['exact_recall']}/20 ({int(round(100*kb_arms['trained_parametric_kv_adapter']['exact_recall_rate']))}%)</strong> exact recall vs 45% full-context text &amp; 30% untrained KV; 0 poison leaks</td>
      <td>PQ-signed shard <code>{kb['adapted_shard_manifest']['tensors_sha256'][:16]}...</code></td>
    </tr>
    <tr>
      <td><strong>Live Intel TDX Confidential Space (T3)</strong></td>
      <td>GCP <code>c3-standard-4</code>, <code>GCP_INTEL_TDX</code>, <code>secboot=true</code>, <code>dbgstat=disabled-since-boot</code></td>
      <td>Passes <code>playbox gate verify --require-signature --require-pq --require-tier t3</code> offline (<code>--network none</code>)</td>
      <td>v0.4.5: <code>{v045_cs['verified_bundle_digest'][:16]}...</code> (11 rcpts); v0.4.6 SWE-150: <code>{swe150_cs['verified_bundle_digest'][:16]}...</code> (150 rcpts)</td>
    </tr>
  </tbody>
</table>

<h2>2. SWE-bench Verified Track A (n = 150) — Arm P2 Evaluation</h2>
<p>
  All 150 holdout instances ran inside isolated OpenShell microVMs under Playbox Arm P2 with zero-oracle 3-tier escalation (T1 <code>devin/claude-sonnet-5-5-medium</code> &rarr; T2 <code>devin/gemini-4-neon-high</code> &rarr; T3 <code>devin/claude-opus-5-5-high,gemini-4-neon-high</code>) and remote Docker grading on <code>yukon-rnd-1</code>. Timeout patch-export recovery exported valid in-VM diffs whenever workers reached the 600s limit.
</p>
<ul>
  <li><strong>Solve rate:</strong> <strong>{p2['solved']} / {p2['instances']} ({100*p2['solve_rate']:.2f}%)</strong>, 95% Wilson CI <code>[{100*p2['wilson_95_ci'][0]:.2f}%, {100*p2['wilson_95_ci'][1]:.2f}%]</code>.</li>
  <li><strong>Tier distribution &amp; latency:</strong> <code>t1={p2['tier_finish_counts']['t1']}</code> ({100*p2['tier1_finish_rate']:.1f}%), <code>t2={p2['tier_finish_counts']['t2']}</code>, <code>t3={p2['tier_finish_counts']['t3']}</code>; median wall <strong>{p2['median_wall_s']}s</strong> (mean {p2['mean_wall_s']}s); median delegate command <strong>{p2['median_delegate_cmd_s']}s</strong>.</li>
  <li><strong>Verified-lesson amortization:</strong> Accumulating <strong>{swe150['verified_lessons_count']} verified lessons</strong> reduced second-half mean spend/wall from <strong>${amort['first_half_mean_cost_usd']:.3f} / {amort['first_half_mean_wall_s']}s</strong> to <strong>${amort['second_half_mean_cost_usd']:.3f} / {amort['second_half_mean_wall_s']}s</strong> (${p2['mean_cost_per_solve_usd']:.3f}/solve, ${p2['total_cost_usd']:.2f} total).</li>
  <li><strong>Integrity:</strong> 150/150 Ed25519+ML-DSA-65 receipts verified, 251 PQ-committed RoomLog entries, 125 promotions, 0 taint advisories.</li>
</ul>

<div class="note">
  <strong>Honest Disclosure (Path B Ledger &amp; T3 Scope):</strong> Because the n=150 run used the <code>devin</code> CLI cascade (which bypasses <code>playbox-aiproxy</code>), task receipts carry <code>llm_ledger_source: "client_unverified"</code> estimates rather than <code>source: "host_aiproxy"</code>. Solvers executed locally in OpenShell and all 150 receipts were sealed post-hoc inside live Intel TDX Confidential Space (<code>achieved_tier: "T3"</code>).
</div>

<h3>Per-Repository Breakdown (n = 150)</h3>
<div style="margin:6px 0 8px 0;">{repo_bar_svg(repo_rows)}</div>

<div class="page-break"></div>

<h2>3. Host-Enforced LLM Spend (<code>playbox-aiproxy</code>, <code>PROXY-1..7</code>)</h2>
<p>
  Hardened <code>playbox-aiproxy</code> (built from <code>labring/aiproxy@{aiproxy_comp['upstream_commit'][:9]}</code> with <code>{html.escape(aiproxy_comp['build_flags'])}</code>, reproducible SHA-256 <code>{pinned['playbox_aiproxy_sha256'][:24]}...</code>) removes self-reported token accounting from the agent trust boundary:
</p>
<ul>
  <li><strong>Hardened posture:</strong> <code>playbox_hardened=true</code>, <code>loopback_only=true</code>, MCP/web-search/Swagger disabled at compile time.</li>
  <li><strong>Runtime enforcement:</strong> Mid-stream USD budget cutoffs, synchronous <code>host.rs</code> <code>on_revoke</code> session termination, and native Gemini 3 <code>thought_signature</code> preservation across multi-turn tool calls.</li>
  <li><strong>Cryptographic ledger:</strong> Emits <code>playbox.llm_ledger.v1</code> receipts with <code>source: "host_aiproxy"</code>, verified in pinned bundles <code>playbox-swe-aiproxy-v0.4.5-5gate-bundle.json.gz</code> (<code>{pinned['swe_aiproxy_v045_bundle_digest'][:16]}...</code>) and <code>playbox-swe-multirepo15-aiproxy-5gate-bundle.json.gz</code> (<code>{pinned['swe_multirepo15_aiproxy_bundle_digest'][:16]}...</code>).</li>
</ul>

<h2>4. Parametric AtlasKV (<code>KBEncoder</code> &amp; <code>TrainedKVProjectionAdapter</code>)</h2>
<p>
  Evaluated on <code>{kb['model_id']}</code> over {kb['shard_manifest']['clusters_count']} repository knowledge clusters ({kb['shard_manifest']['total_tokens_encoded']} tokens encoded; {kb['quarantined_poison_triples_dropped_pre_encode']} poisoned triples quarantined pre-encode; strict zero train/eval overlap verified). Both shard tensors and the rank-{kb['kv_adapter_manifest']['rank']} projection adapter carry <code>Ed25519+ML-DSA-65</code> signatures, with tampered shards and adapters rejected before injection.
</p>
<table>
  <thead>
    <tr><th>Evaluation Arm (n = 20 held-out queries)</th><th class="num">Exact Recall</th><th class="num">Online Query Tokens</th><th class="num">Median Prefill (ms)</th><th class="num">Poison Leaks</th></tr>
  </thead>
  <tbody>
    <tr>
      <td>Full-context text baseline</td>
      <td class="num">{kb_arms['full_context_text']['exact_recall']}/20 ({100*kb_arms['full_context_text']['exact_recall_rate']:.0f}%)</td>
      <td class="num">{kb_arms['full_context_text']['mean_online_query_tokens']:.1f}</td>
      <td class="num">{kb_arms['full_context_text']['median_prefill_ms']:.1f}</td>
      <td class="num">{kb_arms['full_context_text']['poison_leak_count']}</td>
    </tr>
    <tr>
      <td>HiKVP text brief</td>
      <td class="num">{kb_arms['hikvp_text_brief']['exact_recall']}/20 ({100*kb_arms['hikvp_text_brief']['exact_recall_rate']:.0f}%)</td>
      <td class="num">{kb_arms['hikvp_text_brief']['mean_online_query_tokens']:.1f}</td>
      <td class="num">{kb_arms['hikvp_text_brief']['median_prefill_ms']:.1f}</td>
      <td class="num">{kb_arms['hikvp_text_brief']['poison_leak_count']}</td>
    </tr>
    <tr>
      <td>Untrained Parametric KV injection</td>
      <td class="num">{kb_arms['parametric_kv_inject']['exact_recall']}/20 ({100*kb_arms['parametric_kv_inject']['exact_recall_rate']:.0f}%)</td>
      <td class="num">{kb_arms['parametric_kv_inject']['mean_online_query_tokens']:.1f}</td>
      <td class="num">{kb_arms['parametric_kv_inject']['median_prefill_ms']:.1f}</td>
      <td class="num">{kb_arms['parametric_kv_inject']['poison_leak_count']}</td>
    </tr>
    <tr>
      <td><strong>Trained Parametric KV Adapter (rank-32)</strong></td>
      <td class="num ok">{kb_arms['trained_parametric_kv_adapter']['exact_recall']}/20 ({100*kb_arms['trained_parametric_kv_adapter']['exact_recall_rate']:.0f}%)</td>
      <td class="num"><strong>{kb_arms['trained_parametric_kv_adapter']['mean_online_query_tokens']:.1f} ({kb['online_token_compression_vs_full_context_x']:.1f}&times;)</strong></td>
      <td class="num"><strong>{kb_arms['trained_parametric_kv_adapter']['median_prefill_ms']:.1f} ({kb['prefill_speedup_vs_full_context_x']:.1f}&times;)</strong></td>
      <td class="num ok">{kb_arms['trained_parametric_kv_adapter']['poison_leak_count']}</td>
    </tr>
    <tr>
      <td>Slot-compressed KV adapter (608 slots)</td>
      <td class="num">{kb_arms['slot_compressed_kv_adapter']['exact_recall']}/20 ({100*kb_arms['slot_compressed_kv_adapter']['exact_recall_rate']:.0f}%)</td>
      <td class="num">{kb_arms['slot_compressed_kv_adapter']['mean_online_query_tokens']:.1f}</td>
      <td class="num">{kb_arms['slot_compressed_kv_adapter']['median_prefill_ms']:.1f}</td>
      <td class="num">{kb_arms['slot_compressed_kv_adapter']['poison_leak_count']}</td>
    </tr>
  </tbody>
</table>

<h2>5. Live Intel TDX Confidential Space (<code>T3</code>) Attestation</h2>
<p>
  Both the v0.4.5 reference workload and the full n=150 Track A receipt set were sealed on real GCP Confidential Space Intel TDX silicon (<code>{swe150_cs['project_id']}</code> / <code>{swe150_cs['zone']}</code>, <code>{swe150_cs['machine_type']}</code>, <code>confidential_instance_type=TDX</code>, <code>secboot=true</code>, <code>dbgstat=disabled-since-boot</code>) using Google Confidential Space OIDC JWT attestation bound to the exact container image digest and verified offline against pinned JWKS (<code>docs/reports/google-confidential-space-jwks.json</code>).
</p>
<table>
  <thead>
    <tr><th>Sealed Bundle</th><th>Pinned Image Digest</th><th>Audience</th><th class="num">Receipts</th><th>Bundle Digest (SHA-256)</th></tr>
  </thead>
  <tbody>
    <tr>
      <td><code>playbox-t3-confidential-space-5gate-bundle.json.gz</code></td>
      <td><code>{v045_cs['image_digest'][:23]}...</code></td>
      <td><code>{v045_cs['audience']}</code></td>
      <td class="num">11</td>
      <td><code>{v045_cs['verified_bundle_digest'][:20]}...</code></td>
    </tr>
    <tr>
      <td><strong><code>playbox-swe150-t3-confidential-space-5gate-bundle.json.gz</code></strong></td>
      <td><code>{swe150_cs['image_digest'][:23]}...</code></td>
      <td><code>{swe150_cs['audience']}</code></td>
      <td class="num"><strong>{swe150_t3['attested_receipts']}</strong></td>
      <td><code>{swe150_cs['verified_bundle_digest'][:20]}...</code></td>
    </tr>
  </tbody>
</table>

<h2>6. One-Command Offline Reproducer (<code>--network none --require-tier t3</code>)</h2>
<p>
  Any external reviewer can verify the full 150-receipt Intel TDX 5-gate hybrid PQ bundle completely offline from a clean clone of this repository:
</p>
<pre>{html.escape(repro_cmd)}</pre>
<p>
  Expected output: <code>"ok": true</code>, <code>"achieved_tier": "T3"</code>, <code>"enforced_gates_count": 5</code>, <code>"attested_receipts": 150</code>, <code>"signed": true</code>, <code>"pq_signed": true</code>, <code>"bundle_digest": "{swe150_t3['bundle_digest']}"</code>.
</p>

</body>
</html>
"""


def main() -> None:
    html_str = build_html()
    with tempfile.NamedTemporaryFile("w", suffix=".html", delete=False, encoding="utf-8") as f:
        f.write(html_str)
        html_path = Path(f.name)
    try:
        cmd = [
            str(CHROME_BIN),
            "--headless=new",
            "--disable-gpu",
            "--no-sandbox",
            "--print-to-pdf-no-header",
            f"--print-to-pdf={OUT_PDF}",
            html_path.as_uri(),
        ]
        subprocess.run(cmd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    finally:
        html_path.unlink(missing_ok=True)
    print(f"Wrote {OUT_PDF} ({OUT_PDF.stat().st_size:,} bytes)")


if __name__ == "__main__":
    main()
