/* The hub's shared shell: a rooms sidebar (grouped by machine, newest activity first, with state and age) and a status bar. The home page and every
   room cockpit mount the same two pieces, so moving between rooms never means going back to a list. The layout follows the session sidebar and
   status bar of Hermes Agent's desktop app (NousResearch/hermes-agent, MIT). Data comes from the page (rooms) and from /api/system (health). */
(function () {
  const esc = s => String(s == null ? "" : s).replace(/[&<>"']/g, c => ({"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"}[c]));
  const age = ms => { if (!ms) return ""; const s = Math.max(0, (Date.now() - ms) / 1000 | 0); return s < 60 ? "now" : s < 3600 ? `${s / 60 | 0}m` : s < 86400 ? `${s / 3600 | 0}h` : `${s / 86400 | 0}d`; };
  const CSS = `
.rr{display:flex;flex-direction:column;gap:2px;min-height:0;overflow:auto;margin:6px -4px 0;padding:0 4px;flex:1}
.rr h4{display:flex;align-items:center;justify-content:space-between;margin:12px 6px 4px;font:600 11px var(--sans);letter-spacing:.08em;text-transform:uppercase;color:var(--mut)}
.rr h4 a{color:var(--mut);text-decoration:none;font-weight:500;letter-spacing:0;text-transform:none}.rr h4 a:hover{color:var(--ink)}
.rr .g{margin:8px 6px 2px;font:500 12px var(--sans);color:var(--mut);display:flex;align-items:center;gap:6px}
.rr a.rrow{display:flex;align-items:center;gap:9px;padding:5px 8px;border:1px solid transparent;border-radius:7px;color:var(--ink-2);text-decoration:none;font:13px var(--sans);min-width:0}
.rr a.rrow:hover{background:var(--panel-2);color:var(--ink)}
.rr a.rrow[aria-current=page]{background:var(--blue-grad);border-color:var(--blue-edge);color:var(--blue)}
.rr .sd{width:7px;height:7px;border-radius:50%;flex:none;background:var(--dim);border:1px solid var(--line-2)}
.rr .sd.live{background:var(--ok-led);border-color:var(--ok-led)}.rr .sd.hold{background:var(--warn);border-color:var(--warn)}.rr .sd.bad,.rr .sd.down{background:var(--bad);border-color:var(--bad)}
.rr .nm{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.rr .ag{font:11.5px var(--mono);color:var(--mut)}.rr .un{font:700 10px/15px var(--mono);min-width:17px;text-align:center;border-radius:8px;background:var(--sig);color:var(--on-sig);padding:0 4px}
.rr .none{margin:8px 6px;font:12.5px var(--sans);color:var(--mut)}
.pbsb{display:none}
@media (min-width:900px){
  .pbsb{position:fixed;left:0;right:0;bottom:0;height:26px;z-index:35;display:flex;align-items:center;gap:16px;padding:0 14px;background:var(--bg-2);border-top:1px solid var(--line);font:12px var(--sans);color:var(--mut);white-space:nowrap;overflow:hidden}
  .pbsb .it{display:inline-flex;align-items:center;gap:6px;background:none;border:0;color:inherit;font:inherit;padding:0;cursor:default}
  .pbsb button.it{cursor:pointer}.pbsb button.it:hover{color:var(--ink)}
  .pbsb .led{width:7px;height:7px;border-radius:50%;background:var(--dim);border:1px solid var(--line-2)}
  .pbsb .led.ok{background:var(--ok-led);border-color:var(--ok-led)}.pbsb .led.warn{background:var(--warn);border-color:var(--warn)}.pbsb .led.bad{background:var(--bad);border-color:var(--bad)}
  .pbsb .sp{flex:1}.pbsb .mono{font-family:var(--mono);font-size:11.5px}
  body.has-sb{padding-bottom:26px}
  body.has-sb .rail,body.has-sb .pulse,body.has-sb .pane{height:calc(100vh - 26px)!important;max-height:calc(100vh - 26px)}
}`;
  let sb, health, last = {rooms: [], active: null};

  function groups(rooms) {
    const by = new Map();
    for (const r of rooms) { const k = r.machine || "This machine"; (by.get(k) || by.set(k, []).get(k)).push(r); }
    for (const l of by.values()) l.sort((a, b) => ((b.last && b.last.ms) || 0) - ((a.last && a.last.ms) || 0));
    return [...by.entries()];
  }
  function railRooms(rooms, active) {
    if (!rooms.length) return `<div class="rr"><h4>Rooms</h4><div class="none">No rooms yet.</div></div>`;
    const gs = groups(rooms), many = gs.length > 1;
    return `<div class="rr" role="navigation" aria-label="Rooms"><h4>Rooms <a href="/">All</a></h4>${gs.map(([m, l]) => `${many ? `<div class="g">${esc(m)}</div>` : ""}${l.map(r =>
      `<a class="rrow" href="/r/${esc(r.id)}/" ${r.id === active ? 'aria-current="page"' : ""} title="${esc(r.label)}"><i class="sd ${esc(r.state)}"></i><span class="nm">${esc(r.label)}</span>${r.unread ? `<span class="un">${r.unread > 99 ? "99+" : r.unread}</span>` : `<span class="ag">${age(r.last && r.last.ms)}</span>`}</a>`).join("")}`).join("")}</div>`;
  }

  function ensure() {
    if (!document.getElementById("pbsh-css")) { const s = document.createElement("style"); s.id = "pbsh-css"; s.textContent = CSS; document.head.appendChild(s); }
    if (!sb) {
      sb = document.createElement("footer"); sb.className = "pbsb"; sb.setAttribute("role", "contentinfo");
      document.body.appendChild(sb); document.body.classList.add("has-sb");
      sb.addEventListener("click", e => {
        const b = e.target.closest("[data-sb]"); if (!b) return;
        if (b.dataset.sb === "theme" && window.PBTheme) PBTheme.open();
        if (b.dataset.sb === "health") location.href = "/?view=system";
      });
    }
    return sb;
  }
  function paintBar() {
    ensure();
    const rooms = last.rooms, live = rooms.filter(r => r.state === "live").length, h = health;
    const hub = !h ? ["", "Hub"] : h.ok ? ["ok", "Hub ready"] : ["warn", `${h.problems.length} issue${h.problems.length === 1 ? "" : "s"}`];
    const t = h && h.tunnel, theme = window.PBTheme ? (PBTheme.find(PBTheme.state().skin) || {}).label : "";
    sb.innerHTML = `<button class="it" data-sb="health" title="${esc(h && h.problems.length ? h.problems.join("\n") : "Open the System view")}"><i class="led ${hub[0]}"></i>${esc(hub[1])}</button>
      ${t && t.configured ? `<span class="it" title="${esc(t.url || "")}"><i class="led ${t.up ? "ok" : "bad"}"></i>${t.up ? "Public address up" : "Public address down"}</span>` : ""}
      <span class="it">${rooms.length} room${rooms.length === 1 ? "" : "s"}${live ? ` · ${live} live` : ""}</span><span class="sp"></span>
      ${theme ? `<button class="it" data-sb="theme" title="Appearance settings">${esc(theme)}</button>` : ""}<span class="it mono">⌘K</span>${h && h.build ? `<span class="it mono" title="hub build">${esc(h.build)}</span>` : ""}`;
  }
  async function pull() {
    try { const r = await fetch("/api/system", {credentials: "same-origin"}); if (r.ok) { health = (await r.json()).health || null; paintBar(); } } catch {}
  }
  /** Call whenever the page has fresh rooms: paints the sidebar into `el` (if given) and the status bar. */
  function sync(rooms, active, el) {
    last = {rooms: rooms || [], active: active || null};
    if (el) { const keep = el.firstElementChild && el.firstElementChild.scrollTop; el.innerHTML = railRooms(last.rooms, last.active); if (keep && el.firstElementChild) el.firstElementChild.scrollTop = keep; }
    paintBar();
  }
  let timer;
  function start() { if (timer) return; pull(); timer = setInterval(() => { if (!document.hidden) pull(); }, 30000); document.addEventListener("pb:theme", paintBar); }
  window.PBShell = {sync, start, railRooms, age};
})();
