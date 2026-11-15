/* Research view: experiments as runs, shown the way a log viewer shows them: a list of runs, then one run with Overview, Results, Usage, Timeline,
   Config and JSON tabs. A run is plain data (ui/research/<id>.json: id, title, kind, status, model, completed, summary, headline[], config{},
   usage{title,unit,items[]}, results{title,columns[],rows[][]}, timeline[], source), so any experiment, from any harness, shows up by adding one file.
   Layout follows the Inspect log viewer's Tasks, Models, Task and Timeline tabs. Everything is escaped; the text is ours, but nothing here trusts it. */
(function () {
  const esc = s => String(s == null ? "" : s).replace(/[&<>"']/g, c => ({"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"}[c]));
  const TABS = [["overview", "Overview"], ["results", "Results"], ["usage", "Usage"], ["timeline", "Timeline"], ["config", "Config"], ["json", "JSON"]];
  const COL = {ok: "var(--ok-led)", warn: "var(--warn)", bad: "var(--bad)", blue: "var(--blue-edge)", mut: "var(--mut)", violet: "var(--violet)"};
  const num = v => { const n = parseFloat(String(v).replace(/[^0-9.\-]/g, "")); return isFinite(n) ? n : null; };
  const fmt = (v, unit) => unit === "USD" ? "$" + Number(v).toFixed(2) : Number(v).toLocaleString();
  const strip = (title, sub, nums, meta) => `<div class="rh"><div class="rh1"><div><div class="rt">${esc(title)}</div><div class="rs">${esc(sub)}</div></div><div class="hns">${nums.map(n => `<div class="hn"><span>${esc(n.k)}</span><b>${esc(n.v)}</b></div>`).join("")}</div></div>
    <div class="rh2">${meta.map(([k, v]) => `<div><span>${esc(k)}</span><b title="${esc(v)}">${esc(v)}</b></div>`).join("")}</div></div>`;
  const badge = s => `<span class="chip ${s === "complete" ? "ok" : s === "failed" ? "bad" : "warn"}">${esc(s)}</span>`;

  function list(runs, sel) {
    const rows = runs.map(r => `<tr data-run="${esc(r.id)}" tabindex="0" role="link" ${r.id === sel ? 'aria-selected="true"' : ""}><td>${badge(r.status)}</td><td class="lk">${esc(r.title)}</td><td>${esc(r.kind)}</td><td class="mut">${esc(r.model)}</td>
      <td>${(r.headline || []).map(h => `<span class="mono mut">${esc(h.k)}</span> <b class="mono">${esc(h.v)}</b>`).join(" &nbsp; ")}</td><td class="mono mut">${esc(r.completed)}</td></tr>`).join("");
    return strip("Research", "experiments run on Playbox, with their results and caveats", [{k: "RUNS", v: String(runs.length)}, {k: "COMPLETE", v: String(runs.filter(r => r.status === "complete").length)}],
      [["Where", "docs/ in the repository"], ["Format", "ui/research/<id>.json"], ["Add one", "drop in a file"], ["Verified", "see each run's source"]])
      + `<div class="card"><h2>Runs</h2><div class="tlt"><table class="rtab"><thead><tr><th>Status</th><th>Run</th><th>Kind</th><th>Model</th><th>Headline</th><th>Completed</th></tr></thead><tbody>${rows}</tbody></table></div></div>`;
  }

  function bar(items, unit) {
    const tot = items.reduce((a, i) => a + i.value, 0) || 1;
    return `<div class="comp"><div class="bar2">${items.filter(i => i.value > 0).map(i => `<i style="width:${i.value / tot * 100}%;background:${COL[i.c] || COL.blue}" title="${esc(i.label)}"></i>`).join("")}</div></div>`;
  }
  function leaders(items, unit) {
    return `<div class="ldr">${items.map(i => `<div><span class="l"><i style="background:${COL[i.c] || COL.blue}"></i>${esc(i.label)}</span><span class="dots"></span><b class="mono">${esc(fmt(i.value, unit))}</b></div>`).join("")}</div>`;
  }
  const kv = o => `<table class="kvt">${Object.entries(o || {}).map(([k, v]) => `<tr><td class="k">${esc(k)}</td><td>${esc(v)}</td></tr>`).join("")}</table>`;
  function table(t) {
    if (!t || !t.rows) return `<p class="mut">No results table.</p>`;
    return `<div class="tlt"><table class="rtab"><thead><tr>${t.columns.map(c => `<th>${esc(c)}</th>`).join("")}</tr></thead><tbody>${t.rows.map(r => `<tr>${r.map((c, i) => `<td class="${num(c) !== null && i > 1 ? "mono" : ""}">${esc(c)}</td>`).join("")}</tr>`).join("")}</tbody></table></div>`;
  }
  function timeline(run, f) {
    const evs = run.timeline || [], kinds = ["all", ...new Set(evs.map(e => e.kind))];
    const rows = evs.filter(e => f === "all" || e.kind === f).map(e => `<div class="tlr"><span><em class="kd k-${e.kind === "finding" ? "join" : "refused"}">${esc(e.kind)}</em></span><span class="tx">${esc(e.text)}</span><span class="mono mut">${esc(e.by)}</span></div>`).join("");
    return `<div class="tlhead"><span class="tlh">History</span>${kinds.map(k => `<button type="button" class="tlc ${f === k ? "on" : ""}" data-rf="${esc(k)}">${esc(k)} ${k === "all" ? evs.length : evs.filter(e => e.kind === k).length}</button>`).join("")}</div><div class="rtl">${rows}</div>`;
  }
  function detail(run, tab, f) {
    const head = `<div class="rback"><button type="button" class="tbtn" data-rback>← All runs</button> <span class="mut mono">${esc(run.id)}</span></div>` + strip(run.title, `${run.kind} · ${run.model}`, run.headline || [],
      [["Status", run.status], ["Completed", run.completed], ["Kind", run.kind], ["Source", run.source || "-"]]);
    const tabs = `<div class="rtabs" role="tablist">${TABS.map(([k, l]) => `<button role="tab" data-rtab="${k}" aria-selected="${tab === k}">${l}</button>`).join("")}</div>`;
    const u = run.usage;
    let body;
    if (tab === "results") body = `<div class="card"><h2>${esc((run.results || {}).title || "Results")}</h2>${table(run.results)}</div>`;
    else if (tab === "usage") body = u ? `<div class="card"><h2>${esc(u.title)}</h2>${bar(u.items, u.unit)}${leaders(u.items, u.unit)}</div>` : `<div class="card empty"><p>No usage data.</p></div>`;
    else if (tab === "timeline") body = `<div class="card"><h2>Findings and caveats</h2>${timeline(run, f)}</div>`;
    else if (tab === "config") body = `<div class="card"><h2>Config</h2>${kv(run.config)}</div>`;
    else if (tab === "json") body = `<pre class="src" style="margin:0">${esc(JSON.stringify(run, null, 2))}</pre>`;
    else body = `<div class="wk"><div class="card"><h2>Summary</h2><p style="margin:0;line-height:1.6">${esc(run.summary)}</p></div>${u ? `<div class="card"><h2>${esc(u.title)}</h2>${bar(u.items, u.unit)}${leaders(u.items, u.unit)}</div>` : ""}</div>
      <div class="card"><h2>${esc((run.results || {}).title || "Results")}</h2>${table(run.results)}</div>
      <div class="wk"><div class="card"><h2>Config</h2>${kv(run.config)}</div><div class="card"><h2>Findings and caveats</h2>${timeline(run, "all")}</div></div>`;
    return head + tabs + body;
  }
  window.PBResearch = {list, detail};
})();
