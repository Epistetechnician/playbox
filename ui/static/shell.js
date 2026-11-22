/* The hub's shared shell: a rooms sidebar (grouped by machine, newest activity first, with state and age), a slide-over drawer, a room-surface
   switcher, a universal command palette (⌘K) and a status bar. Hub, Cockpit and Canvas share these controls so moving between rooms and surfaces
   is seamless. Layout follows Hermes Agent's desktop app (NousResearch/hermes-agent, MIT). */
(function () {
  const esc = s => String(s == null ? "" : s).replace(/[&<>"']/g, c => ({"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"}[c]));
  const age = ms => { if (!ms) return ""; const s = Math.max(0, (Date.now() - ms) / 1000 | 0); return s < 60 ? "now" : s < 3600 ? `${s / 60 | 0}m` : s < 86400 ? `${s / 3600 | 0}h` : `${s / 86400 | 0}d`; };
  const CSS = `
.rr{display:flex;flex-direction:column;gap:2px;min-height:0;overflow:auto;margin:6px -4px 0;padding:0 4px;flex:1}
.rr h4{display:flex;align-items:center;justify-content:space-between;margin:12px 6px 4px;font:600 11px var(--sans);letter-spacing:.08em;text-transform:uppercase;color:var(--mut)}
.rr h4 a{color:var(--mut);text-decoration:none;font-weight:500;letter-spacing:0;text-transform:none}.rr h4 a:hover{color:var(--ink)}
.rr .rgrp{margin:8px 6px 2px;font:500 12px var(--sans);color:var(--mut);display:flex;align-items:center;gap:6px}
.rr a.rrow{text-transform:none;letter-spacing:0;display:flex;align-items:center;gap:9px;padding:5px 8px;border:1px solid transparent;border-radius:7px;color:var(--ink-2);text-decoration:none;font:13px var(--sans);min-width:0}
.rr a.rrow:hover{background:var(--panel-2);color:var(--ink)}
.rr a.rrow[aria-current=page]{background:var(--blue-grad);border-color:var(--blue-edge);color:var(--blue)}
.rr .rsd{width:7px;height:7px;border-radius:50%;flex:none;background:var(--dim);border:1px solid var(--line-2)}
.rr .rsd.rs-live{background:var(--ok-led);border-color:var(--ok-led)}.rr .rsd.rs-hold{background:var(--warn);border-color:var(--warn)}.rr .rsd.rs-bad,.rr .rsd.rs-down{background:var(--bad);border-color:var(--bad)}
.rr .rnm small{color:var(--mut);font:11px var(--mono)}.rr .rnm{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.rr .rag{font:11.5px var(--mono);color:var(--mut)}.rr .run{font:700 10px/15px var(--mono);min-width:17px;text-align:center;border-radius:8px;background:var(--sig);color:var(--on-sig);padding:0 4px}
.rr .rnone{margin:8px 6px;font:12.5px var(--sans);color:var(--mut)}
.pbsw{display:inline-flex;align-items:center;border:1px solid var(--line-2);border-radius:8px;background:var(--bg-2);overflow:hidden;vertical-align:middle}
.pbsw a{padding:3px 10px;font:500 12px/1.4 var(--sans);color:var(--ink-2);text-decoration:none;border-right:1px solid var(--line-2);white-space:nowrap}
.pbsw a:last-child{border-right:0}.pbsw a:hover{color:var(--ink);background:var(--panel-2)}
.pbsw a[aria-current=page]{background:var(--blue-grad);color:var(--blue)}
.pbdrawer-bd{position:fixed;inset:0;z-index:120;background:var(--scrim);opacity:0;pointer-events:none;transition:opacity .15s}
.pbdrawer-bd.open{opacity:1;pointer-events:auto}
.pbdrawer{position:fixed;top:0;left:0;bottom:0;width:min(280px,86vw);z-index:121;display:flex;flex-direction:column;padding:12px 12px 10px;background:var(--panel);border-right:1px solid var(--line-2);box-shadow:var(--hard);transform:translateX(-104%);transition:transform .16s ease-out}
.pbdrawer.open{transform:translateX(0)}
.pbdrawer-hd{display:flex;align-items:center;justify-content:space-between;gap:8px;padding:2px 4px 10px;border-bottom:1px solid var(--line)}
.pbdrawer-hd .brand{display:inline-flex;align-items:center;gap:8px;text-decoration:none;color:var(--ink);font:700 15px var(--sans);border:0;padding:0}
.pbdrawer-hd .brand b{width:12px;height:12px;display:inline-block}
.pbdrawer-x{border:0;background:none;color:var(--mut);font-size:18px;width:28px;height:28px;cursor:pointer}.pbdrawer-x:hover{color:var(--ink);background:var(--panel-2)}
.pbdrawer-rooms{flex:1;min-height:0;display:flex;flex-direction:column;overflow:hidden}
.pbdrawer-ft{display:flex;flex-wrap:wrap;gap:6px;padding-top:10px;border-top:1px solid var(--line);font:12px var(--sans)}
.pbdrawer-ft a,.pbdrawer-ft button{display:inline-flex;align-items:center;gap:5px;padding:5px 8px;border:1px solid var(--line-2);background:var(--bg-2);color:var(--ink-2);text-decoration:none;font:500 12px var(--sans);cursor:pointer}
.pbdrawer-ft a:hover,.pbdrawer-ft button:hover{color:var(--ink);border-color:var(--edge)}
.pbdrawer-ft kbd,.pbpal kbd{font:11px var(--mono);color:var(--mut)}
.pbpal{width:min(560px,calc(100vw - 24px));max-height:min(480px,calc(100vh - 64px));margin:10vh auto auto;padding:0;border:1px solid var(--line-2);border-radius:12px;background:var(--panel);color:var(--ink);box-shadow:var(--hard);overflow:hidden}
.pbpal::backdrop{background:var(--scrim)}
.pbpal[open]{display:flex;flex-direction:column}
.pbpal-hd{display:flex;align-items:center;gap:10px;padding:11px 14px;border-bottom:1px solid var(--line);background:var(--bg-2);color:var(--mut)}
.pbpal-hd input{flex:1;min-width:0;border:0;outline:0;background:none;color:var(--ink);font:14px var(--sans);padding:0}
.pbpal ul{list-style:none;margin:0;padding:6px;overflow:auto;flex:1;display:grid;gap:2px;align-content:start}
.pbpal li{display:flex;align-items:center;gap:10px;padding:7px 10px;border:1px solid transparent;border-radius:7px;font:13px var(--sans);color:var(--ink-2);cursor:pointer;min-width:0}
.pbpal li:hover{background:var(--panel-2);color:var(--ink)}
.pbpal li[aria-selected=true]{background:var(--blue-grad);border-color:var(--blue-edge);color:var(--blue)}
.pbpal .pl-lbl{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:var(--ink)}
.pbpal li[aria-selected=true] .pl-lbl{color:var(--blue)}
.pbpal .pl-sub{font:12px var(--sans);color:var(--mut);overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:42%}
.pbpal .pl-empty{padding:16px 12px;color:var(--mut);font:13px var(--sans);text-align:center}
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

  let sb, health, railEl = null, roomsLoaded = false, fetchingRooms = null;
  let dw, dwBd, dwOpen = false;
  let pal, palExtra = [], palItems = [], palSel = 0, palQ = "";
  let timer, keyBound = false;
  let last = {rooms: [], active: null};

  function ensureCss() {
    if (!document.getElementById("pbsh-css") && (document.head || document.documentElement)) {
      const s = document.createElement("style"); s.id = "pbsh-css"; s.textContent = CSS;
      (document.head || document.documentElement).appendChild(s);
    }
  }

  function roomHref(id, overrideView) {
    const v = overrideView || (window.PBTheme && PBTheme.pref("roomview")) || "canvas";
    const enc = encodeURIComponent(id);
    return v === "cockpit" ? `/r/${enc}/` : `/r/${enc}/canvas`;
  }

  function setRoomView(view) {
    if (window.PBTheme) PBTheme.setPref("roomview", view === "cockpit" ? "cockpit" : "canvas");
  }

  function switcherHtml(activeRoomId, currentSurface) {
    if (!activeRoomId) return "";
    ensureCss();
    const enc = encodeURIComponent(activeRoomId);
    return `<span class="pbsw" role="group" aria-label="Room view"><a href="/r/${enc}/canvas" data-rview="canvas"${currentSurface === "canvas" ? ' aria-current="page"' : ""}>Canvas</a><a href="/r/${enc}/" data-rview="cockpit"${currentSurface === "cockpit" ? ' aria-current="page"' : ""}>Cockpit</a></span>`;
  }

  function groups(rooms) {
    const by = new Map();
    for (const r of rooms) { const k = r.machine || "This machine"; (by.get(k) || by.set(k, []).get(k)).push(r); }
    for (const l of by.values()) l.sort((a, b) => ((b.last && b.last.ms) || 0) - ((a.last && a.last.ms) || 0));
    return [...by.entries()];
  }

  function railRooms(rooms, active) {
    if (!rooms.length) return `<div class="rr"><h4>Rooms</h4><div class="rnone">No rooms yet.</div></div>`;
    const gs = groups(rooms), many = gs.length > 1, seen = new Set(), dup = new Set();
    for (const r of rooms) { if (seen.has(r.label)) dup.add(r.label); seen.add(r.label); }
    return `<div class="rr" role="navigation" aria-label="Rooms"><h4>Rooms <a href="/">All</a></h4>${gs.map(([m, l]) => `${many ? `<div class="rgrp">${esc(m)}</div>` : ""}${l.map(r =>
      `<a class="rrow" href="${esc(roomHref(r.id))}" ${r.id === active ? 'aria-current="page"' : ""} title="${esc(r.label)}"><i class="rsd rs-${esc(r.state)}"></i><span class="rnm">${esc(r.label)}${dup.has(r.label) ? `<small> ${esc(r.id.slice(0, 4))}</small>` : ""}</span>${r.unread ? `<span class="run">${r.unread > 99 ? "99+" : r.unread}</span>` : `<span class="rag">${age(r.last && r.last.ms)}</span>`}</a>`).join("")}`).join("")}</div>`;
  }

  function ensureRooms() {
    if (roomsLoaded || last.rooms.length) return Promise.resolve(last.rooms);
    if (fetchingRooms) return fetchingRooms;
    fetchingRooms = fetch("/api/rooms", {credentials: "same-origin"}).then(r => r.ok ? r.json() : null).then(j => {
      if (j && Array.isArray(j.rooms)) {
        roomsLoaded = true;
        sync(j.rooms, last.active, railEl);
      }
      return last.rooms;
    }).catch(() => last.rooms).finally(() => { fetchingRooms = null; });
    return fetchingRooms;
  }

  function activeRoomId() {
    if (last.active) return last.active;
    const m = typeof location !== "undefined" && /^\/r\/([^/]+)/.exec(location.pathname || "");
    return m ? decodeURIComponent(m[1]) : null;
  }

  /* ---- slide-over left drawer ---- */
  function paintDrawerRooms() {
    if (!dw) return;
    const box = dw.querySelector(".pbdrawer-rooms");
    if (box) box.innerHTML = railRooms(last.rooms, activeRoomId());
  }

  function mountDrawer(activeId) {
    ensureCss();
    if (activeId !== undefined && activeId !== null) last.active = activeId;
    if (!dw && document.body) {
      dwBd = document.createElement("div"); dwBd.className = "pbdrawer-bd"; dwBd.id = "pbdrawer-bd";
      dw = document.createElement("aside"); dw.className = "pbdrawer"; dw.id = "pbdrawer"; dw.setAttribute("aria-label", "Rooms");
      dw.innerHTML = `<div class="pbdrawer-hd"><a class="brand" href="/"><b></b><span class="bk">Playbox</span></a><button type="button" class="pbdrawer-x" data-dw="close" aria-label="Close">×</button></div>
        <div class="pbdrawer-rooms"></div>
        <div class="pbdrawer-ft"><a href="/">All rooms</a><button type="button" data-dw="settings">Settings <kbd>⌘,</kbd></button><button type="button" data-dw="palette">Jump <kbd>⌘K</kbd></button></div>`;
      document.body.append(dwBd, dw);
      dwBd.addEventListener("click", () => toggleDrawer(false));
      dw.addEventListener("click", e => {
        const b = e.target.closest("[data-dw]");
        if (b) {
          const act = b.dataset.dw;
          toggleDrawer(false);
          if (act === "settings" && window.PBTheme) PBTheme.open();
          if (act === "palette") openPalette();
          return;
        }
        if (e.target.closest("a")) toggleDrawer(false);
      });
    }
    paintDrawerRooms();
    if (!last.rooms.length && !roomsLoaded) ensureRooms();
    return dw;
  }

  function toggleDrawer(force) {
    if (!dw) mountDrawer(last.active);
    if (!dw) return false;
    dwOpen = typeof force === "boolean" ? force : !dwOpen;
    dw.classList.toggle("open", dwOpen);
    dwBd && dwBd.classList.toggle("open", dwOpen);
    if (dwOpen && !last.rooms.length && !roomsLoaded) ensureRooms();
    return dwOpen;
  }

  /* ---- universal command palette (⌘K) ---- */
  function buildPaletteItems() {
    const out = [];
    for (const x of palExtra || []) {
      if (!x) continue;
      const lbl = x.label || x.title;
      if (!lbl) continue;
      out.push({
        label: lbl, sub: x.hint || x.sub || "", kbd: x.kbd || "", dot: x.dot || "",
        run: x.run || x.action || (() => { if (x.href) location.href = x.href; }),
      });
    }
    const act = activeRoomId();
    if (act) {
      const enc = encodeURIComponent(act);
      const curRoom = last.rooms.find(r => r.id === act);
      const rname = (curRoom && curRoom.label) || act;
      out.push({label: `${rname} — Canvas`, sub: "Room surface", run: () => { setRoomView("canvas"); location.href = `/r/${enc}/canvas`; }});
      out.push({label: `${rname} — Cockpit`, sub: "Room surface", run: () => { setRoomView("cockpit"); location.href = `/r/${enc}/`; }});
      for (const [id, nm] of [["feed", "Feed"], ["work", "Work"], ["notes", "Notes"], ["harnesses", "Harnesses"], ["proof", "Proof"]]) {
        out.push({label: `${rname} — ${nm}`, sub: "Cockpit tab", run: () => {
          const btn = document.querySelector(`[data-tab="${id}"]`);
          if (btn && location.pathname === `/r/${enc}/`) btn.click();
          else location.href = `/r/${enc}/#${id}`;
        }});
      }
    }
    for (const r of last.rooms) {
      out.push({
        label: r.label || r.id,
        sub: [r.machine, r.unread ? `${r.unread} unread` : age(r.last && r.last.ms)].filter(Boolean).join(" · ") || "Room",
        dot: `rs-${r.state || "empty"}`,
        run: () => { location.href = roomHref(r.id); },
      });
    }
    out.push(
      {label: "All rooms", sub: "Hub", run: () => { location.href = "/"; }},
      {label: "Needs you / Attention", sub: "Hub", run: () => { location.href = "/?view=attention"; }},
      {label: "System", sub: "Hub", run: () => { location.href = "/?view=system"; }},
      {label: "Research", sub: "Hub", run: () => { location.href = "/?view=research"; }},
      {label: "Integrations", sub: "Hub", run: () => { location.href = "/?view=integrations"; }},
    );
    if (window.PBTheme) {
      out.push(
        {label: "Settings", sub: "Preferences & Hub", kbd: "⌘,", run: () => PBTheme.open()},
        {label: "Appearance settings", sub: "Theme, fonts, layout", run: () => PBTheme.open("appearance")},
        {label: "Toggle light / dark", sub: `Currently ${PBTheme.mode()}`, run: () => PBTheme.set("mode", PBTheme.mode() === "dark" ? "light" : "dark")},
      );
      for (const t of PBTheme.themes()) {
        out.push({label: `Theme: ${t.label}`, sub: t.desc || "Palette", run: () => PBTheme.set("skin", t.id)});
      }
    }
    return out;
  }

  function paintPalette() {
    if (!pal || !pal.open) return;
    const all = buildPaletteItems();
    const terms = palQ.toLowerCase().split(/\s+/).filter(Boolean);
    palItems = terms.length ? all.filter(it => {
      const hay = (it.label + " " + it.sub).toLowerCase();
      return terms.every(w => hay.includes(w));
    }) : all;
    if (palSel >= palItems.length) palSel = Math.max(0, palItems.length - 1);
    const ul = pal.querySelector("ul");
    if (!ul) return;
    if (!palItems.length) {
      ul.innerHTML = `<div class="pl-empty">No matching rooms or commands.</div>`;
      return;
    }
    ul.innerHTML = palItems.map((it, i) => `<li role="option" data-pi="${i}" aria-selected="${i === palSel}">
      ${it.dot ? `<i class="rr-dot rsd ${esc(it.dot)}" style="width:7px;height:7px;border-radius:50%;flex:none;background:var(--dim);border:1px solid var(--line-2)${it.dot === "rs-live" ? ";background:var(--ok-led);border-color:var(--ok-led)" : it.dot === "rs-hold" ? ";background:var(--warn);border-color:var(--warn)" : it.dot === "rs-bad" || it.dot === "rs-down" ? ";background:var(--bad);border-color:var(--bad)" : ""}"></i>` : ""}
      <span class="pl-lbl">${esc(it.label)}</span>
      ${it.sub ? `<span class="pl-sub">${esc(it.sub)}</span>` : ""}
      ${it.kbd ? `<kbd>${esc(it.kbd)}</kbd>` : ""}
    </li>`).join("");
    const cur = ul.querySelector('[aria-selected="true"]');
    if (cur && cur.scrollIntoView) cur.scrollIntoView({block: "nearest"});
  }

  function runPaletteItem(idx) {
    const it = palItems[idx];
    if (!it) return;
    if (pal && pal.open) pal.close();
    it.run();
  }

  function openPalette(extraItems = []) {
    ensureCss();
    if (!document.body) return;
    palExtra = Array.isArray(extraItems) ? extraItems : [];
    if (!pal) {
      pal = document.createElement("dialog"); pal.className = "pbpal"; pal.setAttribute("aria-label", "Command palette");
      pal.innerHTML = `<div class="pbpal-hd"><span>⌕</span><input type="text" placeholder="Jump to a room, view or command…" autocomplete="off" spellcheck="false"><kbd>esc</kbd></div><ul role="listbox"></ul>`;
      document.body.appendChild(pal);
      const inp = pal.querySelector("input");
      inp.addEventListener("input", () => { palQ = inp.value; palSel = 0; paintPalette(); });
      pal.addEventListener("keydown", e => {
        if (e.key === "ArrowDown") { e.preventDefault(); if (palItems.length) { palSel = (palSel + 1) % palItems.length; paintPalette(); } }
        else if (e.key === "ArrowUp") { e.preventDefault(); if (palItems.length) { palSel = (palSel - 1 + palItems.length) % palItems.length; paintPalette(); } }
        else if (e.key === "Enter") { e.preventDefault(); runPaletteItem(palSel); }
        else if (e.key === "Escape") { e.preventDefault(); pal.close(); }
      });
      pal.addEventListener("click", e => {
        if (e.target === pal) { pal.close(); return; }
        const li = e.target.closest("[data-pi]");
        if (li) runPaletteItem(Number(li.dataset.pi));
      });
    }
    palQ = ""; palSel = 0;
    const inp = pal.querySelector("input");
    if (inp) inp.value = "";
    if (!pal.open) pal.showModal();
    paintPalette();
    if (inp) inp.focus();
    if (!last.rooms.length && !roomsLoaded) ensureRooms().then(paintPalette);
  }

  /* ---- status bar ---- */
  function ensure() {
    ensureCss();
    if (!sb && document.body) {
      sb = document.createElement("footer"); sb.className = "pbsb"; sb.setAttribute("role", "contentinfo");
      document.body.appendChild(sb); document.body.classList.add("has-sb");
      sb.addEventListener("click", e => {
        const b = e.target.closest("[data-sb]"); if (!b) return;
        if (b.dataset.sb === "theme" && window.PBTheme) PBTheme.open("theme");
        if (b.dataset.sb === "palette") { if (typeof window.openPalette === "function") window.openPalette(); else openPalette(); }
        if (b.dataset.sb === "health") location.href = "/?view=system";
        if (b.dataset.sb === "reload") location.reload();
      });
    }
    return sb;
  }

  let seenBuild = null, updateReady = false;
  function paintBar() {
    if (!ensure()) return;
    const rooms = last.rooms, live = rooms.filter(r => r.state === "live").length, h = health;
    const hub = !h ? ["", "Hub"] : h.ok ? ["ok", "Hub ready"] : ["warn", `${h.problems.length} issue${h.problems.length === 1 ? "" : "s"}`];
    const t = h && h.tunnel, theme = window.PBTheme ? (PBTheme.find(PBTheme.state().skin) || {}).label : "";
    sb.innerHTML = `<button class="it" data-sb="health" title="${esc(h && h.problems.length ? h.problems.join("\n") : "Open the System view")}"><i class="led ${hub[0]}"></i>${esc(hub[1])}</button>
      ${t && t.configured ? `<span class="it" title="${esc(t.url || "")}"><i class="led ${t.up ? "ok" : "bad"}"></i>${t.up ? "Public address up" : "Public address down"}</span>` : ""}
      <span class="it">${rooms.length} room${rooms.length === 1 ? "" : "s"}${live ? ` · ${live} live` : ""}</span><span class="sp"></span>
      ${updateReady ? `<button class="it" data-sb="reload" style="color:var(--sig-ink);font-weight:600" title="A newer hub build is live">Hub updated · Reload</button>` : ""}
      ${theme ? `<button class="it" data-sb="theme" title="Change theme">Theme: ${esc(theme)}</button>` : ""}<button class="it mono" data-sb="palette" title="Command palette (⌘K)">⌘K</button>${h && h.build ? `<span class="it mono" title="hub build">${esc(h.build)}</span>` : ""}`;
  }

  async function pull() {
    try {
      const r = await fetch("/api/system", {credentials: "same-origin"});
      if (r.ok) {
        health = (await r.json()).health || null;
        if (health && health.build) {
          if (seenBuild && seenBuild !== health.build) updateReady = true;
          seenBuild = health.build;
        }
        paintBar();
      }
    } catch {}
  }

  /** Call whenever the page has fresh rooms: paints the sidebar into `el` (if given), updates drawer/palette, and paints the status bar. */
  function sync(rooms, active, el) {
    if (Array.isArray(rooms)) roomsLoaded = true;
    last = {rooms: rooms || [], active: active !== undefined ? active : last.active};
    if (el) railEl = el;
    if (railEl) {
      const keep = railEl.firstElementChild && railEl.firstElementChild.scrollTop;
      railEl.innerHTML = railRooms(last.rooms, last.active);
      if (keep && railEl.firstElementChild) railEl.firstElementChild.scrollTop = keep;
    }
    paintDrawerRooms();
    paintPalette();
    paintBar();
  }

  function bindGlobal() {
    if (keyBound || typeof document === "undefined") return;
    keyBound = true;
    document.addEventListener("click", e => {
      const a = e.target.closest && e.target.closest(".pbsw [data-rview]");
      if (a) setRoomView(a.dataset.rview);
    });
    document.addEventListener("keydown", e => {
      if (e.key === "Escape" && dwOpen) { e.preventDefault(); toggleDrawer(false); return; }
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "k") {
        if (e.defaultPrevented) return;
        const pbs = document.querySelector("dialog.pbs[open]");
        if (pbs) return;
        const sh = document.getElementById("sheet");
        if (sh && (sh.open || sh.classList.contains("open") || sh.getAttribute("aria-hidden") === "false")) return;
        e.preventDefault();
        if (pal && pal.open) pal.close();
        else openPalette();
      }
    });
    document.addEventListener("pb:pref", e => {
      if (e.detail && e.detail.k === "roomview") {
        if (railEl) railEl.innerHTML = railRooms(last.rooms, last.active);
        paintDrawerRooms();
      }
    });
  }

  bindGlobal();

  function start() {
    bindGlobal();
    if (timer) return;
    pull();
    timer = setInterval(() => { if (!document.hidden) pull(); }, 30000);
    document.addEventListener("pb:theme", paintBar);
  }

  window.PBShell = {sync, start, railRooms, age, roomHref, setRoomView, switcherHtml, mountDrawer, toggleDrawer, openPalette};
})();
