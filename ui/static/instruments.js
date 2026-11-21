/* Instrument panels shared by the hub's home page and the room cockpit: a Stats card (key/value rows, a rate sparkline, a stacked breakdown with
   coloured dots), an Activity card (swim-lanes on one time axis) and a Trail (collapsible steps). Pure functions that return HTML strings; the page
   decides what data goes in. Everything dynamic goes through esc(): feed text is untrusted. Uses the page's own colour tokens, so light and dark both work. */
(function () {
  const esc = s => String(s == null ? "" : s).replace(/[&<>"']/g, c => ({"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"}[c]));
  const css = `
:root{--c-you:var(--blue-edge);--c-peer:var(--sig);--c-join:var(--ok-led);--c-bad:var(--bad);--c-ev:var(--violet,#A78BFA);--c-idle:var(--mut);--c-gen:var(--ok-led);--c-warn:var(--warn,#E5A33D)}
.ins{background:var(--panel);border:1px solid var(--line-2);border-radius:12px;overflow:hidden}
.ins>summary{list-style:none;display:flex;align-items:center;gap:10px;padding:12px 14px;cursor:pointer;user-select:none}
.ins>summary::-webkit-details-marker{display:none}
.ins>summary .chev{width:14px;height:14px;stroke:var(--mut);fill:none;stroke-width:1.8;stroke-linecap:round;stroke-linejoin:round;transition:transform .15s}
.ins:not([open])>summary .chev{transform:rotate(-90deg)}
.ins>summary .ttl{font:500 15px var(--sans);color:var(--ink)}
.ins>summary .meta{margin-left:auto;font:500 12.5px var(--mono);color:var(--mut)}
.ins .body{padding:2px 16px 16px;display:grid;gap:14px}
.ins .who{font:12.5px var(--mono);color:var(--mut)}
.ins .name{font:600 18px var(--mono);color:var(--ink);overflow-wrap:anywhere}
.ins hr{border:0;border-top:1px solid var(--line);margin:0}
.ins .barrow{display:flex;align-items:center;gap:12px;font:13px var(--mono);color:var(--ink-2)}
.ins .bar{flex:1;height:7px;border-radius:4px;background:var(--dim);display:flex;overflow:hidden}
.ins .bar i{display:block;height:100%}
.ins .bar i+i{margin-left:1px}
.ins .kv{display:grid;grid-template-columns:auto 1fr auto 1fr;gap:6px 14px;font:13px var(--mono)}
.ins .kv span.k{color:var(--mut)}
.ins .kv span.v{text-align:right;color:var(--ink);overflow-wrap:anywhere}
@media (max-width:520px){.ins .kv{font-size:12px;gap:6px 10px}}
@media (max-width:350px){.ins .kv{grid-template-columns:auto 1fr}}
.ins .rate{display:flex;align-items:baseline;gap:8px}
.ins .rate b{font:600 34px/1 var(--mono);letter-spacing:-.02em;color:var(--ink)}
.ins .rate span{font:13px var(--mono);color:var(--mut)}
.ins .rate .avg{margin-left:auto}
.ins .spark{width:100%;height:76px;display:block;border-radius:6px;background:linear-gradient(var(--field),var(--panel-2));border:1px solid var(--line)}
.ins .spark path.ln{fill:none;stroke:var(--c-you);stroke-width:1.4;vector-effect:non-scaling-stroke;stroke-linejoin:round}
.ins .spark path.ar{fill:var(--c-you);opacity:.12}
.ins .spark line{stroke:var(--line-2);stroke-dasharray:3 3;vector-effect:non-scaling-stroke}
.ins .sec{font:600 11px var(--mono);letter-spacing:.1em;text-transform:uppercase;color:var(--mut);margin:2px 0 -6px}
.ins .brk{display:grid;grid-template-columns:1fr auto;gap:6px 18px;font:13px var(--mono)}
@media (min-width:640px){.ins .brk.two{grid-template-columns:1fr auto 1fr auto}}
.ins .brk .l{display:flex;align-items:center;gap:8px;color:var(--ink-2);min-width:0}
.ins .brk .l i{width:8px;height:8px;border-radius:50%;flex:none}
.ins .brk .l i.o{background:none!important;border:1.5px solid currentColor}
.ins .brk .n{text-align:right;color:var(--ink)}
.ins .brk .n small{color:var(--mut);margin-left:6px}
.ins .lanes{display:grid;grid-template-columns:minmax(64px,auto) 1fr auto;gap:7px 12px;align-items:center;font:12.5px var(--mono)}
.ins .lanes .ln{color:var(--ink-2);text-align:right;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;max-width:130px}
.ins .lanes .ln.h{color:var(--ink);font-weight:600}
.ins .lanes .ct{color:var(--mut);text-align:right}
.ins .lanes .ct.bad{color:var(--bad)}
.ins .lane{position:relative;height:14px;border-radius:7px;background:var(--panel-2);overflow:hidden}
.ins .lane.tall{height:18px}
.ins .lane i{position:absolute;top:0;bottom:0;min-width:3px;border-radius:2px}
.ins .lanes .grp{grid-column:1/-1;margin-top:6px;font:600 11px var(--mono);letter-spacing:.1em;text-transform:uppercase;color:var(--mut)}
.ins .axis{display:grid;grid-template-columns:minmax(64px,auto) 1fr auto;gap:12px;font:12px var(--mono);color:var(--mut)}
.ins .axis div{display:flex;justify-content:space-between}
.ins>summary:focus-visible{outline:2px solid var(--sig);outline-offset:-2px;border-radius:12px}
.ins>summary:hover .ttl{color:var(--sig-ink)}
.ins .line{font:13px/1.45 var(--mono);color:var(--ink-2);margin-top:4px}
.ins .flags{display:flex;flex-wrap:wrap;gap:8px}
.ins .flag{display:inline-flex;align-items:center;gap:7px;padding:4px 11px;border-radius:999px;border:1px solid color-mix(in srgb,var(--fc) 55%,transparent);background:color-mix(in srgb,var(--fc) 12%,transparent);color:var(--ink);font:600 12px var(--mono);cursor:pointer}
.ins .flag i{width:7px;height:7px;border-radius:50%;background:var(--fc)}
.ins .clk{cursor:pointer;border-radius:4px}
.ins .clk:hover{color:var(--ink)}
.ins .clk.on{color:var(--ink);text-decoration:underline;text-underline-offset:3px}
.ins .fl{display:inline-flex;align-items:center;gap:6px;font:600 11.5px var(--mono);padding:2px 9px;border-radius:999px;border:1px solid var(--line-2);color:var(--ink-2);cursor:pointer;background:none}
.since{display:flex;align-items:center;gap:14px;flex-wrap:wrap;padding:12px 16px;border:1px solid var(--line-2);border-left:3px solid var(--sig);border-radius:12px;background:var(--panel)}
.since b{font:600 14px var(--sans);color:var(--ink)}
.since .parts{display:flex;gap:12px;flex-wrap:wrap;font:13px var(--mono);color:var(--ink-2)}
.since .parts span{display:inline-flex;align-items:center;gap:6px}
.since .parts i{width:8px;height:8px;border-radius:50%}
.since button{margin-left:auto;font:600 11.5px var(--mono);letter-spacing:.06em;text-transform:uppercase;padding:6px 12px;border:1px solid var(--line-2);border-radius:8px;background:none;color:var(--ink);cursor:pointer}
.since button:hover{background:var(--panel-2)}
.ins .empty{font:13px var(--mono);color:var(--mut);padding:6px 0}
.ins .steps{display:grid;gap:0;font:13px var(--mono)}
.ins .steps details{border-top:1px solid var(--line)}
.ins .steps details:first-child{border-top:0}
.ins .steps summary{list-style:none;display:flex;align-items:center;gap:10px;padding:8px 2px;cursor:pointer;color:var(--ink-2)}
.ins .steps summary::-webkit-details-marker{display:none}
.ins .steps summary i{width:8px;height:8px;border-radius:50%;flex:none}
.ins .steps summary .t{margin-left:auto;color:var(--mut);white-space:nowrap}
.ins .steps summary .x{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;min-width:0}
.ins .steps .det{padding:2px 2px 10px 18px;color:var(--ink-2);font:13px/1.5 var(--sans);overflow-wrap:anywhere;white-space:pre-wrap}
`;
  if (!document.getElementById("pbi-css")) {
    const st = document.createElement("style");
    st.id = "pbi-css";
    st.textContent = css;
    document.head.appendChild(st);
  }

  const CH = '<svg class="chev" viewBox="0 0 24 24" aria-hidden="true"><path d="M6 9l6 6 6-6"/></svg>';
  const clock = ms => new Date(ms).toLocaleTimeString([], {hour: "2-digit", minute: "2-digit", hour12: false});
  const span = ms => ms < 90e3 ? `${Math.max(1, Math.round(ms / 1e3))}s` : ms < 90 * 60e3 ? `${Math.round(ms / 60e3)}m` : ms < 36 * 3600e3 ? `${(ms / 3600e3).toFixed(1)}h` : `${(ms / 86400e3).toFixed(1)}d`;
  const num = n => n >= 1e6 ? (n / 1e6).toFixed(1) + "M" : n >= 1e4 ? Math.round(n / 1e3) + "k" : String(n);

  /** `open` keeps a panel's state across repaints: pass what the page remembered. */
  function wrap(id, icon, title, meta, body, open) {
    return `<details class="ins" data-ins="${esc(id)}" ${open === false ? "" : "open"}><summary>${CH}${icon || ""}<span class="ttl">${esc(title)}</span><span class="meta">${meta || ""}</span></summary><div class="body">${body}</div></details>`;
  }

  function spark(vals) {
    const n = vals.length, mx = Math.max(1, ...vals);
    if (n < 2) return `<svg class="spark" viewBox="0 0 100 40" preserveAspectRatio="none"></svg>`;
    const pt = (v, i) => `${(i / (n - 1) * 100).toFixed(2)},${(38 - v / mx * 34).toFixed(2)}`;
    const line = vals.map((v, i) => (i ? "L" : "M") + pt(v, i)).join("");
    const avg = vals.reduce((a, b) => a + b, 0) / n;
    const ay = (38 - avg / mx * 34).toFixed(2);
    return `<svg class="spark" viewBox="0 0 100 40" preserveAspectRatio="none" role="img" aria-label="rate over time"><line x1="0" x2="100" y1="${ay}" y2="${ay}"/><path class="ar" d="${line}L100,40L0,40Z"/><path class="ln" d="${line}"/></svg>`;
  }

  /** Stats card. opts: {id, icon, title, meta, who, name, bar:{label, parts:[{c, w, t}], right}, kv:[[k,v],..], rate:{now, unit, avg, max, series}, groups:[{title, rows:[{c,l,n,pct,o}]}], open} */
  function stats(o) {
    const b = [];
    if (o.who || o.name) b.push(`<div>${o.who ? `<div class="who">${esc(o.who)}</div>` : ""}${o.name ? `<div class="name">${esc(o.name)}</div>` : ""}${o.line ? `<div class="line">${esc(o.line)}</div>` : ""}</div>`);
    if (o.flags && o.flags.length) b.push(`<div class="flags">${o.flags.map(f => `<${f.k ? "button" : "span"} class="flag" ${f.k ? `data-kind="${esc(f.k)}"` : ""} style="--fc:${f.c}"><i></i>${esc(f.t)}</${f.k ? "button" : "span"}>`).join("")}</div>`);
    b.push("<hr>");
    if (o.bar) {
      const tot = o.bar.parts.reduce((a, p) => a + p.w, 0) || 1;
      b.push(`<div class="barrow"><span>${esc(o.bar.label)}</span><div class="bar" role="img" aria-label="${esc(o.bar.parts.map(p => p.t + " " + p.w).join(", "))}">${o.bar.parts.filter(p => p.w > 0).map(p => `<i style="width:${(p.w / tot * 100).toFixed(2)}%;background:${p.c}"></i>`).join("")}</div><span>${esc(o.bar.right || "")}</span></div>`);
    }
    if (o.kv && o.kv.length) b.push(`<div class="kv">${o.kv.map(([k, v]) => `<span class="k">${esc(k)}</span><span class="v">${esc(v)}</span>`).join("")}</div>`);
    if (o.rate) b.push(`<div><div class="rate"><b>${esc(o.rate.now)}</b><span>${esc(o.rate.unit)}</span><span class="avg">avg ${esc(o.rate.avg)} · max ${esc(o.rate.max)}</span></div></div>${spark(o.rate.series || [])}`);
    for (const g of o.groups || []) {
      b.push(`<div class="sec">${esc(g.title)}</div><div class="brk${g.rows.length > 4 ? " two" : ""}">${g.rows.map(r => { const k = r.k ? ` data-kind="${esc(r.k)}" role="button" tabindex="0"` : "", on = r.k && r.k === o.kind ? " on" : ""; return `<span class="l${r.k ? " clk" : ""}${on}"${k}><i class="${r.o ? "o" : ""}" style="background:${r.c};color:${r.c}"></i>${esc(r.l)}</span><span class="n${r.k ? " clk" : ""}${on}"${k}>${esc(r.n)}${r.pct != null ? `<small>${esc(r.pct)}</small>` : ""}</span>`; }).join("")}</div>`);
    }
    return wrap(o.id || "stats", o.icon, o.title || "Stats", o.meta, b.join(""), o.open);
  }

  /** Activity card. lanes: [{group?:string} | {label, c, at:[ms..], count, bad, head, tall}], range [t0, t1] in ms. Marks closer than ~0.6% merge into one bar. */
  function activity(o) {
    const [t0, t1] = o.range, w = Math.max(1, t1 - t0);
    const rows = (o.lanes || []).map(l => {
      if (l.group) return `<div class="grp">${esc(l.group)}</div>`;
      const at = (l.at || []).slice().sort((a, b) => a - b), bars = [];
      for (const t of at) {
        const x = Math.min(100, Math.max(0, (t - t0) / w * 100)), last = bars[bars.length - 1];
        if (last && x - last.x2 < 0.6) last.x2 = Math.max(last.x2, x + 0.5); else bars.push({x1: x, x2: x + 0.5});
      }
      const fill = l.parts ? l.parts.map(p => (p.at || []).map(t => `<i style="left:${((t - t0) / w * 100).toFixed(2)}%;width:.6%;background:${p.c}"></i>`).join("")).join("") : bars.map(b => `<i style="left:${b.x1.toFixed(2)}%;width:${(b.x2 - b.x1).toFixed(2)}%;background:${l.c}"></i>`).join("");
      return `<span class="ln ${l.head ? "h" : ""}">${esc(l.label)}</span><div class="lane ${l.tall ? "tall" : ""}">${fill}</div><span class="ct ${l.bad ? "bad" : ""}">${esc(l.count == null ? at.length : l.count)}</span>`;
    }).join("");
    const mid = t0 + w / 2;
    return wrap(o.id || "activity", o.icon, o.title || "Activity", o.meta, o.lanes && o.lanes.length ? `<div class="lanes">${rows}</div>
      <div class="axis"><span></span><div><span>${esc(clock(t0))}</span><span>${esc(clock(mid))}</span><span>${esc(clock(t1))}</span></div><span></span></div>` : `<div class="empty">Nothing to show yet.</div>`, o.open);
  }

  /** Steps log: collapsible rows. steps: [{c, label, text, ms, detail}] newest first. */
  function trail(o) {
    const rows = (o.steps || []).map(s => `<details><summary><i style="background:${s.c}"></i><span>${esc(s.label)}</span><span class="x">${esc(String(s.text || "").slice(0, 90))}</span><span class="t">${s.ms ? esc(clock(s.ms)) : ""}</span></summary>${s.detail ? `<div class="det">${esc(s.detail)}</div>` : ""}</details>`).join("");
    return wrap(o.id || "trail", o.icon, o.title || "Trail", o.meta, rows ? `<div class="steps">${rows}</div>` : `<div class="empty">Nothing yet.</div>`, o.open);
  }

  /** "Since you last looked": a one-line digest with a Mark read button. parts: [{c, t}] */
  function since(o) {
    return `<div class="since" role="status"><b>${esc(o.title)}</b><div class="parts">${o.parts.map(p => `<span><i style="background:${p.c}"></i>${esc(p.t)}</span>`).join("")}</div><button type="button" data-markread="1">${esc(o.btn || "Mark read")}</button></div>`;
  }

  /** Bucket message times into `n` buckets of `size` ms ending at `end`. */
  function buckets(times, end, size, n) {
    const out = new Array(n).fill(0);
    for (const t of times) {
      const i = n - 1 - Math.floor((end - t) / size);
      if (i >= 0 && i < n) out[i]++;
    }
    return out;
  }

  /** Render Model & Spend + Compute/Inference swim-lanes + Model Trail from `/api/rooms/<id>/llm/*` payloads.
      Labels data honestly as `Host-observed (aiproxy)` (or `Receipt-bound` / `T3-attested` when provided). */
  function llmPanels(o) {
    const prov = o.provenanceLabel || "Host-observed (aiproxy)";
    const st = o.stats || {};
    const tot = st.totals || {};
    const byModel = st.by_model || {};
    const cost = Number(tot.cost_usd || 0);
    const cap = Number(o.budgetCapUsd || 0);
    const inTok = Number(tot.input_tokens || 0);
    const cacheTok = Number(tot.cached_read_tokens || 0) + Number(tot.cached_write_tokens || 0);
    const outTok = Number(tot.output_tokens || 0);
    const reasTok = Number(tot.reasoning_tokens || 0);
    const cachePct = inTok > 0 ? Math.round((cacheTok / inTok) * 100) + "%" : "0%";
    const bmap = st.buckets || {};
    const bkeys = Object.keys(bmap).map(Number).sort((a, b) => a - b);
    const series = bkeys.map(k => Number(bmap[k] || 0));
    const maxB = series.length ? Math.max(...series) : 0;
    const avgB = series.length ? (series.reduce((a, b) => a + b, 0) / series.length).toFixed(1) : "0";
    const modelRows = Object.keys(byModel).sort().map(m => ({
      c: "var(--c-you)",
      l: m,
      n: `$${Number(byModel[m].cost_usd || 0).toFixed(4)}`,
      pct: `${num(Number(byModel[m].tokens || 0))} tok`
    }));
    const flags = [{c: "var(--c-ev)", t: prov}];
    if (Number(tot.failovers || 0) > 0) flags.push({c: "var(--c-warn)", t: `${tot.failovers} failover(s)`});
    if (Number(tot.budget_cutoffs || 0) > 0) flags.push({c: "var(--c-bad)", t: `${tot.budget_cutoffs} budget cutoff(s)`});

    const statsHtml = stats({
      id: "llm-stats",
      title: "Model & Spend",
      meta: `$${cost.toFixed(4)} · ${num(inTok + outTok)} tok`,
      flags,
      bar: cap > 0 ? {
        label: "Budget",
        parts: [
          {c: cost >= cap ? "var(--c-bad)" : "var(--c-you)", w: Math.min(cost, cap), t: "spent"},
          {c: "var(--dim)", w: Math.max(0, cap - cost), t: "remaining"}
        ],
        right: `$${cost.toFixed(4)} / $${cap.toFixed(2)}`
      } : null,
      kv: [
        ["Requests", `${tot.requests || 0} (${tot.retries || 0} retries)`],
        ["Cache hit", cachePct],
        ["Median TTFT", `${tot.median_ttft_ms || 0}ms`],
        ["Reasoning tok", num(reasTok)]
      ],
      rate: series.length >= 2 ? {now: String(series[series.length - 1]), unit: "req/5m", avg: avgB, max: String(maxB), series} : null,
      groups: modelRows.length ? [{title: "By model", rows: modelRows}] : [],
      open: o.open
    });

    const spans = (o.activity && o.activity.spans) || [];
    const byMem = {};
    let t0 = Infinity, t1 = -Infinity;
    for (const sp of spans) {
      const m = sp.member_id || "host";
      (byMem[m] = byMem[m] || []).push(sp);
      if (sp.start_ms < t0) t0 = sp.start_ms;
      if (sp.end_ms > t1) t1 = sp.end_ms;
    }
    const lanes = Object.keys(byMem).sort().map(m => {
      const list = byMem[m];
      const bad = list.some(x => x.status >= 400 || x.budget_cutoff);
      return {
        label: m,
        c: bad ? "var(--c-bad)" : "var(--c-ev)",
        at: list.map(x => x.start_ms),
        count: list.length,
        bad
      };
    });
    const actHtml = lanes.length ? activity({
      id: "llm-activity",
      title: "Inference Activity",
      meta: `${spans.length} calls · ${prov}`,
      lanes,
      range: [t0, Math.max(t0 + 1000, t1)],
      open: o.open
    }) : "";

    const entries = (o.logs && o.logs.entries) || [];
    const trailHtml = trail({
      id: "llm-trail",
      title: "Model Trail",
      meta: `${entries.length} recent · metadata-only`,
      steps: entries.map(e => ({
        c: e.budget_cutoff || e.status >= 400 ? "var(--c-bad)" : (e.failover_from ? "var(--c-warn)" : "var(--c-gen)"),
        label: e.upstream_model || "model",
        text: `${e.status} · ${e.ttft_ms || 0}ms ttft · $${Number(e.cost_usd || 0).toFixed(4)}${e.failover_from ? " · failover " + e.failover_from : ""}${e.budget_cutoff ? " · cutoff:" + e.budget_cutoff : ""}`,
        ms: e.ts_ms,
        detail: `req=${e.req_id} member=${e.member_id || "host"} task=${e.task_id || "-"} in=${e.input_tokens} cache=${(e.cached_read_tokens || 0) + (e.cached_write_tokens || 0)} out=${e.output_tokens} reas=${e.reasoning_tokens} latency=${e.latency_ms}ms`
      })),
      open: o.open
    });

    return statsHtml + actHtml + trailHtml;
  }

  window.PBI = {esc, stats, activity, trail, since, buckets, span, clock, num, wrap, llmPanels};
})();
