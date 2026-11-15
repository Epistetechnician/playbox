/* Instrument panels shared by the hub's home page and the room cockpit: a Stats card (key/value rows, a rate sparkline, a stacked breakdown with
   coloured dots), an Activity card (swim-lanes on one time axis) and a Trail (collapsible steps). Pure functions that return HTML strings; the page
   decides what data goes in. Everything dynamic goes through esc(): feed text is untrusted. Uses the page's own colour tokens, so light and dark both work. */
(function () {
  const esc = s => String(s == null ? "" : s).replace(/[&<>"']/g, c => ({"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"}[c]));
  const css = `
:root{--c-you:light-dark(#2563EB,#4DA3FF);--c-peer:var(--sig);--c-join:light-dark(#0E9F6E,#34D399);--c-bad:var(--bad);--c-ev:light-dark(#7C3AED,#A78BFA);--c-idle:light-dark(#8F8C80,#6C6A5F);--c-gen:light-dark(#0E9F6E,#34D399);--c-warn:light-dark(#D97706,#FBBF24)}
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
@media (max-width:520px){.ins .kv{grid-template-columns:auto 1fr}}
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
    if (o.who || o.name) b.push(`<div>${o.who ? `<div class="who">${esc(o.who)}</div>` : ""}${o.name ? `<div class="name">${esc(o.name)}</div>` : ""}</div><hr>`);
    if (o.bar) {
      const tot = o.bar.parts.reduce((a, p) => a + p.w, 0) || 1;
      b.push(`<div class="barrow"><span>${esc(o.bar.label)}</span><div class="bar" role="img" aria-label="${esc(o.bar.parts.map(p => p.t + " " + p.w).join(", "))}">${o.bar.parts.filter(p => p.w > 0).map(p => `<i style="width:${(p.w / tot * 100).toFixed(2)}%;background:${p.c}"></i>`).join("")}</div><span>${esc(o.bar.right || "")}</span></div>`);
    }
    if (o.kv && o.kv.length) b.push(`<div class="kv">${o.kv.map(([k, v]) => `<span class="k">${esc(k)}</span><span class="v">${esc(v)}</span>`).join("")}</div>`);
    if (o.rate) b.push(`<div><div class="rate"><b>${esc(o.rate.now)}</b><span>${esc(o.rate.unit)}</span><span class="avg">avg ${esc(o.rate.avg)} · max ${esc(o.rate.max)}</span></div></div>${spark(o.rate.series || [])}`);
    for (const g of o.groups || []) {
      b.push(`<div class="sec">${esc(g.title)}</div><div class="brk${g.rows.length > 4 ? " two" : ""}">${g.rows.map(r => `<span class="l"><i class="${r.o ? "o" : ""}" style="background:${r.c};color:${r.c}"></i>${esc(r.l)}</span><span class="n">${esc(r.n)}${r.pct != null ? `<small>${esc(r.pct)}</small>` : ""}</span>`).join("")}</div>`);
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
      <div class="axis"><span></span><div><span>${esc(clock(t0))}</span><span>${esc(span(w / 2))}</span><span>${esc(clock(t1))}</span></div><span></span></div>` : `<div class="empty">Nothing to show yet.</div>`, o.open);
  }

  /** Steps log: collapsible rows. steps: [{c, label, text, ms, detail}] newest first. */
  function trail(o) {
    const rows = (o.steps || []).map(s => `<details><summary><i style="background:${s.c}"></i><span>${esc(s.label)}</span><span class="x">${esc(String(s.text || "").slice(0, 90))}</span><span class="t">${s.ms ? esc(clock(s.ms)) : ""}</span></summary>${s.detail ? `<div class="det">${esc(s.detail)}</div>` : ""}</details>`).join("");
    return wrap(o.id || "trail", o.icon, o.title || "Trail", o.meta, rows ? `<div class="steps">${rows}</div>` : `<div class="empty">Nothing yet.</div>`, o.open);
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

  window.PBI = {esc, stats, activity, trail, buckets, span, clock, num, wrap};
})();
