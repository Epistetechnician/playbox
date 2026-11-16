/* Playbox settings: one screen for everything you can change, in a tree (group > page) with a breadcrumb, search and a content pane of rows
   (name and description on the left, control on the right). Opened from the Hub tab, the status bar, the command palette or ⌘, / Ctrl+,.
   Appearance and Interface settings are kept in this browser (PBTheme); Hub and Safety pages read and change the hub itself through its API,
   with the same passkey step-up as the rest of the app. Layout of the screen follows the settings dialog of Hermes Agent's desktop app (MIT). */
(function () {
  const esc = s => String(s == null ? "" : s).replace(/[&<>"']/g, c => ({"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"}[c]));
  const T = () => window.PBTheme;
  const ago = ms => { if (!ms) return "never"; const s = Math.max(0, (Date.now() - ms) / 1000 | 0); return s < 60 ? "just now" : s < 3600 ? `${s / 60 | 0} min ago` : s < 86400 ? `${s / 3600 | 0} h ago` : `${s / 86400 | 0} d ago`; };

  const CSS = `
.pbs{width:min(1040px,calc(100vw - 24px));height:min(700px,calc(100vh - 40px));max-width:none;padding:0;border:1px solid var(--line-2);background:var(--panel);color:var(--ink);box-shadow:0 20px 70px rgba(0,0,0,.45);overflow:hidden}
.pbs::backdrop{background:var(--scrim)}
.pbs[open]{display:grid;grid-template-columns:260px minmax(0,1fr)}
.pbs nav{border-right:1px solid var(--line);padding:12px 8px;overflow:auto;background:var(--bg-2);display:flex;flex-direction:column;gap:2px}
.pbs .sq{display:flex;align-items:center;gap:8px;margin:0 4px 10px;padding:6px 10px;border:1px solid var(--line-2);background:var(--field);color:var(--mut);font:13px var(--sans)}
.pbs .sq input{flex:1;min-width:0;border:0;outline:0;background:none;color:var(--ink);font:13px var(--sans);padding:0}
.pbs .sq kbd{font:11px var(--mono);color:var(--mut);border:1px solid var(--line-2);padding:0 5px}
.pbs .gh{display:flex;align-items:center;gap:9px;width:100%;text-align:left;padding:7px 10px;border:1px solid transparent;background:none;color:var(--ink);font:500 13.5px var(--sans);cursor:pointer}
.pbs .gh:hover{background:var(--panel-2)}.pbs .gh .ch{margin-left:auto;color:var(--mut);transition:transform .12s}.pbs .gh[aria-expanded=true] .ch{transform:rotate(90deg)}
.pbs .gh svg{width:16px;height:16px;stroke:var(--mut);fill:none;stroke-width:1.6;stroke-linecap:square;flex:none}
.pbs .pg{display:flex;align-items:center;width:100%;text-align:left;margin-left:14px;max-width:calc(100% - 14px);padding:6px 10px;border:1px solid transparent;background:none;color:var(--ink-2);font:13px var(--sans);cursor:pointer}
.pbs .pg:hover{background:var(--panel-2);color:var(--ink)}.pbs .pg[aria-current=page]{background:var(--blue-grad);border-color:var(--blue-edge);color:var(--blue)}
.pbs .pg small{margin-left:auto;color:var(--mut);font-size:11.5px}
.pbs .main{display:flex;flex-direction:column;min-width:0;min-height:0}
.pbs header{display:flex;align-items:center;gap:8px;padding:12px 20px;border-bottom:1px solid var(--line);font:13px var(--sans);color:var(--mut)}
.pbs header b{color:var(--ink);font-weight:600}.pbs header .x{margin-left:auto;border:0;background:none;color:var(--mut);font-size:18px;cursor:pointer;width:30px;height:30px}.pbs header .x:hover{background:var(--panel-2);color:var(--ink)}
.pbs .body{padding:18px 24px 28px;overflow:auto;display:grid;gap:0;align-content:start}
.pbs .lead{margin:0 0 14px;color:var(--mut);font:13px/1.5 var(--sans);max-width:680px}
.pbs .row{display:grid;grid-template-columns:minmax(0,1fr) auto;gap:4px 24px;align-items:center;padding:14px 0;border-bottom:1px solid var(--line)}
.pbs .row:last-child{border-bottom:0}.pbs .row .t{font:500 14px var(--sans)}.pbs .row .d{grid-column:1;font:12.5px/1.45 var(--sans);color:var(--mut);max-width:560px}.pbs .row .c{grid-row:1/span 2;grid-column:2;justify-self:end;display:flex;gap:8px;align-items:center;flex-wrap:wrap}
.pbs .row.full{grid-template-columns:1fr}.pbs .row.full .c{grid-row:auto;grid-column:1;justify-self:stretch}
.pbs .themes{display:grid;grid-template-columns:repeat(auto-fill,minmax(210px,1fr));gap:12px}
.pbs .tc{display:grid;gap:8px;padding:10px;border:1px solid var(--line-2);background:var(--bg-2);cursor:pointer;text-align:left;color:var(--ink);font:inherit}
.pbs .tc:hover{border-color:var(--edge)}.pbs .tc[aria-pressed=true]{border-color:var(--blue-edge);box-shadow:0 0 0 1px var(--blue-edge)}
.pbs .tc .pv{padding:9px 10px;font:11.5px/1.5 var(--mono);display:grid;gap:3px}.pbs .tc .sw{display:flex;gap:4px}.pbs .tc .sw i{width:12px;height:12px}
.pbs .tc b{font:600 13px var(--sans)}.pbs .tc span.d{font:12px/1.35 var(--sans);color:var(--mut)}
.pbs .seg2{display:inline-flex;border:1px solid var(--line-2)}.pbs .seg2 button{border:0;background:none;color:var(--ink-2);padding:6px 13px;font:500 12.5px var(--sans);cursor:pointer}
.pbs .seg2 button+button{border-left:1px solid var(--line-2)}.pbs .seg2 button[aria-pressed=true]{background:var(--blue-grad);color:var(--blue)}
.pbs .acc{display:flex;flex-wrap:wrap;gap:8px}.pbs .acc button{display:inline-flex;align-items:center;gap:7px;padding:5px 11px 5px 6px;border:1px solid var(--line-2);background:none;color:var(--ink-2);font:500 12.5px var(--sans);cursor:pointer}
.pbs .acc button i{width:16px;height:16px;border:1px solid var(--line-2)}.pbs .acc button[aria-pressed=true]{border-color:var(--blue-edge);color:var(--ink);background:var(--blue-grad)}
.pbs select,.pbs textarea,.pbs input[type=text]{background:var(--field);color:var(--ink);border:1px solid var(--line-2);padding:7px 10px;font:13px var(--sans)}
.pbs select{min-width:190px}.pbs textarea{width:100%;min-height:220px;font:12.5px/1.5 var(--mono);resize:vertical}
.pbs .sw2{position:relative;width:38px;height:20px;border:1px solid var(--line-2);background:var(--dim);cursor:pointer;padding:0}.pbs .sw2::after{content:"";position:absolute;top:2px;left:2px;width:14px;height:14px;background:var(--mut);transition:left .12s}
.pbs .sw2[aria-checked=true]{background:var(--blue-grad);border-color:var(--blue-edge)}.pbs .sw2[aria-checked=true]::after{left:20px;background:var(--blue)}
.pbs .live{padding:14px;border:1px solid var(--line-2);background:var(--bg-2);display:grid;gap:6px}.pbs .live .h{font:600 17px var(--sans)}.pbs .live p{margin:0;color:var(--ink-2);font:14px/1.5 var(--sans)}.pbs .live code{font:13px var(--mono);color:var(--sig-ink)}
.pbs button.act{padding:6px 14px;border:1px solid var(--line-2);background:var(--panel-2);color:var(--ink);font:500 13px var(--sans);cursor:pointer}.pbs button.act:hover{border-color:var(--edge)}.pbs button.act.danger{color:var(--bad);border-color:var(--bad);background:none}
.pbs .note{font:12.5px/1.45 var(--sans);color:var(--mut);margin:12px 0 0}.pbs .chip{display:inline-flex}
.pbs table.kt{width:100%;border-collapse:collapse;font:13px var(--sans)}.pbs table.kt td{padding:8px 4px;border-bottom:1px solid var(--line);vertical-align:top}.pbs table.kt td:first-child{width:34%;font:12.5px var(--mono);color:var(--mut)}
.pbs .li{display:flex;align-items:center;gap:12px;padding:10px 0;border-bottom:1px solid var(--line)}.pbs .li:last-child{border:0}.pbs .li .snm{flex:1;min-width:0;display:block}.pbs .li small{display:block;color:var(--mut);font-size:12px}
.pbs kbd{font:12px var(--mono);border:1px solid var(--line-2);padding:1px 7px;background:var(--bg-2)}
@media (max-width:760px){.pbs[open]{grid-template-columns:1fr;grid-template-rows:auto 1fr}.pbs nav{max-height:190px;border-right:0;border-bottom:1px solid var(--line)}.pbs .row{grid-template-columns:1fr}.pbs .row .c{grid-row:auto;grid-column:1;justify-self:start}}`;

  const IC = {
    appearance: '<path d="M12 3a9 9 0 1 0 0 18c1.5 0 2-1 1.5-2s0-2 1.5-2h2a3 3 0 0 0 3-3c0-6-4-11-8-11z"/>', interface: '<rect x="3" y="4" width="18" height="16"/><path d="M3 9h18M9 9v11"/>',
    hub: '<rect x="3" y="4" width="18" height="6"/><rect x="3" y="14" width="18" height="6"/><path d="M7 7h.01M7 17h.01"/>', safety: '<path d="M12 3l8 3v6c0 4.5-3.2 7.9-8 9-4.8-1.100-8-4.500-8-9V6z"/>',
    notifications: '<path d="M6 17V11a6 6 0 0 1 12 0v6l1.500 2h-15z"/><path d="M10 21h4"/>', keys: '<rect x="2" y="6" width="20" height="12"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>', about: '<circle cx="12" cy="12" r="9"/><path d="M12 11v6M12 7.500v.01"/>'};

  /* ---------- controls ---------- */
  const row = (t, d, c, full) => `<div class="row${full ? " full" : ""}"><div class="t">${t}</div>${d ? `<div class="d">${d}</div>` : ""}<div class="c">${c}</div></div>`;
  const seg = (kind, key, opts, cur, label) => `<div class="seg2" role="group" aria-label="${esc(label || key)}">${opts.map(([v, l]) => `<button type="button" data-${kind}="${key}" data-v="${esc(v)}" aria-pressed="${String(cur) === String(v)}">${esc(l)}</button>`).join("")}</div>`;
  const sel = (kind, key, opts, cur) => `<select data-${kind}-sel="${key}">${opts.map(([v, l]) => `<option value="${esc(v)}" ${String(cur) === String(v) ? "selected" : ""}>${esc(l)}</option>`).join("")}</select>`;
  const tog = (key, on) => `<button type="button" class="sw2" role="switch" aria-checked="${on}" data-tog="${key}" aria-label="${esc(key)}"></button>`;

  /* ---------- hub data ---------- */
  let hub = {me: null, sys: null, err: null};
  async function call(method, path, body) {
    const init = {method, headers: {"Content-Type": "application/json"}, credentials: "same-origin"};
    if (body !== undefined) init.body = JSON.stringify(body);
    const r = window.PB && PB.guarded ? await PB.guarded(path, init) : await fetch(path, init);
    let j = null; try { j = await r.json(); } catch {}
    if (!r.ok) throw new Error((j && j.error) || `The hub answered ${r.status}`);
    return j;
  }
  async function loadHub(force) {
    if (!force && hub.me && Date.now() - (hub.at || 0) < 8000) return;
    hub.at = Date.now();
    try { hub.me = await call("GET", "/api/me"); hub.err = null; } catch (e) { hub.err = e.message; }
    try { hub.sys = await call("GET", "/api/system"); } catch {}
    if (page().group === "notifications" || cur === "approvals") await loadNotif();
    if (open_ && page().needsHub) paint();
  }
  let nt = {n: null, ap: null, push: null, err: null};
  async function loadNotif() {
    try { nt.n = await call("GET", "/api/settings/notify"); nt.err = null; } catch (e) { nt.err = e.message; }
    try { nt.ap = (await call("GET", "/api/approvals")).approvals; } catch { nt.ap = nt.ap || []; }
    await pushProbe();
  }
  const pushOk = () => "serviceWorker" in navigator && "PushManager" in window && "Notification" in window && window.isSecureContext;
  async function pushProbe() {
    if (!pushOk()) { nt.push = {state: "unsupported"}; return; }
    try {
      const reg = await Promise.race([navigator.serviceWorker.ready, new Promise((_, no) => setTimeout(() => no(new Error("no service worker")), 2500))]);
      nt.reg = reg; const sub = await reg.pushManager.getSubscription();
      nt.push = {state: Notification.permission === "denied" ? "blocked" : sub ? "on" : "off", sub};
    } catch { nt.push = {state: "unavailable"}; }
  }
  async function togglePush() {
    await pushProbe();
    if (!nt.push || !nt.reg) return;
    if (nt.push.sub) { await call("POST", "/api/push/unsubscribe", {endpoint: nt.push.sub.endpoint}); await nt.push.sub.unsubscribe(); return; }
    if (await Notification.requestPermission() !== "granted") throw new Error("Permission not granted");
    const sub = await nt.reg.pushManager.subscribe({userVisibleOnly: true, applicationServerKey: PB.unb64u(hub.me.push_key)});
    await call("POST", "/api/push/subscribe", {endpoint: sub.endpoint});
  }
  const fmtS = v => v % 3600 === 0 ? `${v / 3600} h` : v % 60 === 0 ? `${v / 60} min` : `${v} s`;

  /* ---------- pages ---------- */
  const THEME_PREVIEW = (t, dark) => { const p = dark ? t.dark : t.light; return `<button type="button" class="tc" data-k="skin" data-v="${esc(t.id)}" aria-pressed="${T().state().skin === t.id}"><div class="pv" style="background:${p.bg};color:${p.ink};border:1px solid ${T().mix(p.bg, p.ink, .2)}"><span style="color:${p.mut}">~ playbox up</span><span><span style="color:${p.ok}">●</span> <span style="color:${p.sig}">gpu-lab</span> <span style="color:${p.mut}">3 in room</span></span><span style="color:${p.blue}">&gt; TRY score=6300</span></div><b>${esc(t.label)}</b><span class="d">${esc(t.desc || "Your theme")}</span><span class="sw">${[p.sig, p.blue, p.ok, p.warn, p.bad, p.violet].map(c => `<i style="background:${c}"></i>`).join("")}</span></button>`; };
  const PAGES = {
    theme: {label: "Theme", group: "appearance", words: "colour palette dark light catppuccin nord solarized", render() {
      const dark = T().mode() === "dark";
      return `<p class="lead">Each theme has a dark and a light side; the mode is chosen under Layout. Text colours are adjusted automatically so they stay readable.</p><div class="themes">${T().themes().map(t => THEME_PREVIEW(T().find(t.id), dark)).join("")}</div>`; }},
    accent: {label: "Accent", group: "appearance", words: "primary colour button brand", render() {
      const s = T().state(), cur = T().find(s.skin), dark = T().mode() === "dark", custom = s.accent.startsWith("custom:") ? s.accent.slice(7) : "";
      return row("Primary colour", "Buttons, the brand mark, other members' activity. The theme keeps its own unless you pick one.", "") + `<div class="acc">${T().ACCENTS.map(([id, name, h]) => `<button type="button" data-k="accent" data-v="${id}" aria-pressed="${s.accent === id}"><i style="background:${h || (dark ? cur.dark.sig : cur.light.sig)}"></i>${esc(name)}</button>`).join("")}
        <button type="button" data-custom aria-pressed="${!!custom}"><i style="background:${custom || "var(--line-2)"}"></i>Custom</button><input type="color" id="pbs-color" value="${custom || "#cbd8a2"}" aria-label="Custom primary colour"></div>`; }},
    typography: {label: "Typography", group: "appearance", words: "font mono code sans serif", render() {
      const s = T().state();
      return row("Interface font", "Fonts already on this device: nothing is downloaded.", sel("set", "font", Object.entries(T().FONTS).map(([k, v]) => [k, v[0]]), s.font))
        + row("Code and numbers", "Ids, counts, the activity panels and anything that is code.", sel("set", "mono", Object.entries(T().MONOS).map(([k, v]) => [k, v[0]]), s.mono))
        + `<div class="live"><div class="h">Reading sample</div><p>Agents from different owners work in one room. Messages from other members are untrusted text; the witness watches them.</p><p><code>TRY score=6300 vec=[12, 200, 31, 77]</code> 0123456789</p></div>`; }},
    layout: {label: "Layout", group: "appearance", words: "mode dark light density compact scale zoom style refined expressive", render() {
      const s = T().state();
      return row("Mode", "Follow the system or force light or dark.", seg("set", "mode", [["auto", "System"], ["light", "Light"], ["dark", "Dark"]], s.mode))
        + row("Style", "Refined is flat and plain, like a log viewer. Expressive adds glows, shadows, the dot-matrix wordmark and digits.", seg("set", "style", [["refined", "Refined"], ["expressive", "Expressive"]], s.style))
        + row("Density", "Compact tightens rows, buttons and cards to show more at once.", seg("set", "density", [["comfortable", "Comfortable"], ["compact", "Compact"]], s.density))
        + row("Interface scale", "Scales the whole interface, text and spacing together.", seg("set", "scale", T().SCALES.map(([v, l]) => [v, l]), s.scale)); }},
    css: {label: "Custom CSS", group: "appearance", words: "stylesheet override variables", render() {
      const s = T().state();
      return row("Your own CSS", "Applied on every page of this hub, after the theme. Variables: <code>--bg --panel --ink --mut --sig --blue-edge --ok --warn --bad --line</code>.", "", true)
        + `<textarea id="pbs-css" spellcheck="false" placeholder=":root { --sans: Georgia, serif; }">${esc(s.css)}</textarea><div class="row"><span class="note" id="pbs-cssn" style="margin:0">${s.css.length} of 20000 characters</span><span class="c"><button class="act" data-clearcss>Clear</button></span></div>`; }},
    share: {label: "Import and export", group: "appearance", words: "json file theme share backup reset", render() {
      return row("Export this look", "The current theme, accent, fonts, style and scale as one JSON file.", `<button class="act" data-export>Download</button>`)
        + row("Import a theme", "A JSON file with <code>id</code>, <code>label</code> and <code>dark</code> and <code>light</code> palettes (bg, panel, ink, mut, sig, blue, ok, warn, bad, violet as #rrggbb).", `<button class="act" data-import>Choose a file</button>`)
        + (T().users().length ? row("Your themes", T().users().map(t => esc(t.label)).join(", "), `<button class="act" data-removeusers>Remove all</button>`) : "")
        + row("Reset appearance and interface", "Back to the defaults. Your rooms, devices and the hub itself are not touched.", `<button class="act danger" data-reset>Reset</button>`) + `<p class="note" id="pbs-msg"></p>`; }},

    navigation: {label: "Navigation", group: "interface", words: "sidebar status bar landing start page", render() {
      const P = T().pref, views = [["last", "The last one I used"], ["rooms", "Rooms"], ["attention", "Attention"], ["system", "System"], ["research", "Research"]];
      return row("Rooms sidebar", "The list of rooms down the left side of the home page and of every room.", tog("sidebar", P("sidebar") === "on"))
        + row("Status bar", "Hub health, the public address, the room count and the theme along the bottom of the screen on desktop.", tog("statusbar", P("statusbar") === "on"))
        + row("Open on", "Which hub view to land on when you open the app.", sel("pref", "landing", views, P("landing"))); }},
    time: {label: "Time and motion", group: "interface", words: "clock 24 hour animation reduce motion timezone", render() {
      const P = T().pref;
      return row("24-hour clock", "The clock in the top strip of the home page.", tog("clock24", P("clock24") === "1"))
        + row("Reduce motion", "Stops animations and transitions. Turn on if motion bothers you.", seg("pref", "motion", [["full", "Full"], ["reduce", "Reduced"]], P("motion"))); }},

    devices: {label: "Devices", group: "hub", needsHub: true, words: "passkey pair remove device sign in", render() {
      if (hub.err && !hub.me) return `<p class="lead">${esc(hub.err)}</p>`;
      const m = hub.me; if (!m) return `<p class="lead">Loading…</p>`;
      return `<p class="lead">Each device unlocks this hub with its own passkey. Removing one signs it out and its passkey stops working.</p>`
        + (m.devices || []).map(d => `<div class="li"><div class="snm">${esc(d.name)} ${d.current ? '<span class="chip ok">this device</span>' : ""}<small>paired ${ago(d.created * 1000)} · used ${ago(d.last * 1000)}</small></div>${d.current ? "" : `<button class="act danger" data-rmdev="${esc(d.id)}">Remove</button>`}</div>`).join("")
        + `<div class="row"><div class="t">Add a device</div><div class="d">Makes a one-time pairing link for a phone or another computer. It needs a fresh passkey touch.</div><div class="c"><button class="act" data-pair>Add a device</button></div></div><p class="note" id="pbs-msg"></p>`; }},
    sessions: {label: "Sessions", group: "hub", needsHub: true, words: "signed in sign out", render() {
      const m = hub.me; if (!m) return `<p class="lead">${esc(hub.err || "Loading…")}</p>`;
      return `<p class="lead">Browsers and apps currently signed in to this hub.</p>` + ((m.sessions || []).map(x => `<div class="li"><div class="snm">${esc(x.name)} ${x.current ? '<span class="chip">this session</span>' : ""} ${x.weak ? '<span class="chip warn" title="Plain HTTP session">no passkey</span>' : ""}<small>started ${ago(x.created * 1000)} · active ${ago(x.last * 1000)}</small></div>${x.current ? "" : `<button class="act" data-rmses="${esc(x.key)}">End</button>`}</div>`).join("") || `<p class="note">No other sessions.</p>`) + `<p class="note" id="pbs-msg"></p>`; }},
    network: {label: "Network", group: "hub", needsHub: true, words: "address tunnel cloudflare https tailscale", render() {
      const h = hub.sys && hub.sys.health, t = h && h.tunnel, secure = location.protocol === "https:" || ["localhost", "127.0.0.1", "[::1]"].includes(location.hostname);
      return `<table class="kt"><tr><td>address</td><td>${esc(location.host)}</td></tr><tr><td>connection</td><td><span class="chip ${secure ? "ok" : "warn"}">${location.protocol === "https:" ? "HTTPS" : secure ? "local" : "plain HTTP"}</span></td></tr>`
        + (t ? `<tr><td>public address</td><td>${t.configured ? `<span class="chip ${t.up ? "ok" : "bad"}">${t.up ? "up" : "down"}</span> ${esc(t.url || "")}` : "not configured"}</td></tr>` : "")
        + (h ? `<tr><td>hub up since</td><td>${esc(ago(h.started_ms))}</td></tr><tr><td>build</td><td>${esc(h.build || "")}</td></tr>` : "") + `</table><p class="note">The public address is a Cloudflare tunnel with Access in front. Change it by reinstalling the service (<code>ui/service/install-macos.sh</code>).</p>`; }},

    posture: {label: "Posture", group: "safety", needsHub: true, words: "security controls passkey step-up access weak", render() {
      const sys = hub.sys; if (!sys) return `<p class="lead">${esc(hub.err || "Loading…")}</p>`;
      const ok = sys.posture.filter(c => c.level === "ok").length;
      return `<p class="lead">${ok} of ${sys.posture.length} controls hold. The full workflow and live trail are in the System view.</p>` + sys.posture.map(c => `<div class="li"><span class="chip ${c.level === "ok" ? "ok" : c.level === "warn" ? "warn" : "bad"}">${esc(c.level)}</span><div class="snm">${esc(c.label)}<small>${esc(c.detail)}${c.fix ? " · " + esc(c.fix) : ""}</small></div></div>`).join("") + `<p class="note"><a href="/?view=system" style="color:var(--blue)">Open the System view</a></p>`; }},

    push: {label: "Push", group: "notifications", needsHub: true, words: "notifications phone browser subscribe test buzz", render() {
      const p = nt.push, n = nt.n;
      if (!p || !n) return `<p class="lead">${esc(nt.err || "Loading…")}</p>`;
      const label = {unsupported: "Not supported here", unavailable: "Unavailable in this browser", blocked: "Blocked in browser settings", on: "Turn off on this device", off: "Turn on for this device"}[p.state];
      return `<p class="lead">A push buzzes your phone or computer when the witness pauses a write, a history check fails, a call waits for you or a room needs you. It carries no content: the device wakes, then asks this hub directly.</p>`
        + row("This device", p.state === "on" ? "Notifications are on here." : p.state === "unsupported" ? "Push needs HTTPS (or localhost) and a browser with service workers." : "Notifications are off here.", `<button class="act" data-pushtog ${["unsupported", "unavailable", "blocked"].includes(p.state) ? "disabled" : ""}>${label}</button>`)
        + row("Devices subscribed", "Across this hub, every browser and phone that has turned notifications on.", `<span class="chip">${n.push_subs}</span>`)
        + row("Send a test", "Wakes every subscribed device now. A silent test confirms the path works.", `<button class="act" data-pushtest ${n.push_subs ? "" : "disabled"}>Send test</button>`) + `<p class="note" id="pbs-msg"></p>`; }},
    alerts: {label: "Alerts", group: "notifications", needsHub: true, words: "down unreachable threshold urgent tunnel public address minutes", render() {
      const n = nt.n; if (!n) return `<p class="lead">${esc(nt.err || "Loading…")}</p>`;
      const opts = [60, 120, 300, 600, 900, 1800, 3600].filter(v => v !== n.down_alert_s); opts.push(n.down_alert_s); opts.sort((a, b) => a - b);
      return `<p class="lead">A room or the public address that is unreachable is shown quietly at first, since it usually retries by itself. After the time below it turns urgent and pushes to your devices.</p>`
        + row("Urgent after", `How long something may stay down before it wakes you. ${n.custom ? "Set on this hub." : `Using the default (${fmtS(n.default_s)}, or PLAYBOX_DOWN_ALERT_S).`} Changing it needs a fresh passkey touch.`,
              sel("alert", "down", opts.map(v => [v, fmtS(v)]), n.down_alert_s) + (n.custom ? `<button class="act" data-alertreset>Use default</button>` : "")) + `<p class="note" id="pbs-msg"></p>`; }},
    mute: {label: "Rooms", group: "notifications", needsHub: true, words: "mute silence room push quiet", render() {
      const n = nt.n; if (!n) return `<p class="lead">${esc(nt.err || "Loading…")}</p>`;
      return `<p class="lead">A muted room never sends a push, but its items still show in Attention. That includes pauses and failed history checks, so mute only rooms you watch another way.</p>`
        + (n.rooms.map(r => row(esc(r.label), r.muted ? "Muted: no push." : "Pushes on.", tog("mute:" + r.id, !r.muted))).join("") || `<p class="note">No rooms yet.</p>`) + `<p class="note" id="pbs-msg"></p>`; }},

    approvals: {label: "Approvals", group: "safety", needsHub: true, words: "approve deny capability call owner PLAYBOX_APPROVERS waiting", render() {
      const ap = nt.ap; if (!ap) return `<p class="lead">${esc(nt.err || "Loading…")}</p>`;
      return `<p class="lead">Calls that wait for the room owner, on rooms hosted on this machine. Approving or denying needs a fresh passkey touch and is written to the audit trail.</p>`
        + (ap.map(c => `<div class="li"><div class="snm"><b>${esc(c.cap)}</b> in ${esc(c.label)}<small>asked by ${esc(c.member)} · ${esc(Object.entries(c.args || {}).map(([k, v]) => k + "=" + v).join(" ").slice(0, 160))}</small></div><button class="act" data-apv="approve" data-room="${esc(c.room || c.host)}" data-call="${esc(c.call)}">Approve</button><button class="act danger" data-apv="deny" data-room="${esc(c.room || c.host)}" data-call="${esc(c.call)}">Deny</button></div>`).join("") || `<p class="note">Nothing is waiting.</p>`)
        + `<details style="margin-top:18px"><summary class="note" style="cursor:pointer">How to turn approvals on</summary><p class="note">A capability marked <code>"approval": true</code> in the room's capabilities file waits here for you, and expires after 15 minutes if unanswered. To make an agent's sensitive tools need a human capability, start it with <code>PLAYBOX_APPROVERS</code> set to your approver key (<code>playbox approver-key</code>). Rooms hosted by an older Playbox build only list calls in their own console until that host is updated.</p></details><p class="note" id="pbs-msg"></p>`; }},

    keys: {label: "Keyboard shortcuts", group: "keys", words: "shortcut hotkey command palette", render() {
      const K = [["⌘K or /", "Search rooms and commands"], ["⌘,", "Open these settings"], ["J / K", "Move through rooms (home page)"], ["Enter", "Open the selected room"], ["Esc", "Close a dialog, or leave a Canvas panel"]];
      return `<table class="kt">${K.map(([k, d]) => `<tr><td><kbd>${esc(k)}</kbd></td><td>${esc(d)}</td></tr>`).join("")}</table><p class="note">On Windows and Linux, use Ctrl in place of ⌘.</p>`; }},

    about: {label: "About", group: "about", needsHub: true, words: "version build licence libraries credits", render() {
      const h = hub.sys && hub.sys.health;
      return `<table class="kt"><tr><td>hub build</td><td>${esc((h && h.build) || "-")}</td></tr><tr><td>themes</td><td>${T().themes().length}</td></tr></table>
        <p class="lead" style="margin-top:18px">Playbox is the connecting layer for people, agents and tools that already have their own interfaces: it does not replace them, it lets them work in one room under limits the host sets, with a signed record.</p>
        <table class="kt"><tr><td>KaTeX 0.16.11</td><td>maths in notes · MIT</td></tr><tr><td>Shiki 1.29.2</td><td>code colours · MIT</td></tr><tr><td>DOMPurify 3.2.4</td><td>link sanitiser · Apache-2.0 or MPL-2.0</td></tr><tr><td>Hermes Agent desktop</td><td>theme model and settings layout · MIT</td></tr><tr><td>mlsweep, Inspect log viewer</td><td>layout ideas for panels and run views</td></tr></table>`; }},
  };
  const GROUPS = [["appearance", "Appearance"], ["interface", "Interface"], ["hub", "Hub"], ["notifications", "Notifications"], ["safety", "Safety"], ["keys", "Keyboard shortcuts"], ["about", "About"]];
  const ids = Object.keys(PAGES), first = g => ids.find(i => PAGES[i].group === g);

  /* ---------- dialog ---------- */
  let dlg, cur = "theme", q = "", openG = new Set(["appearance"]), open_ = false;
  const page = () => PAGES[cur];
  function paint() {
    if (!dlg) return;
    const p = page(), g = GROUPS.find(x => x[0] === p.group), body = dlg.querySelector(".body"), keep = body ? body.scrollTop : 0;
    const match = id => !q || (PAGES[id].label + " " + PAGES[id].words + " " + PAGES[id].group).toLowerCase().includes(q);
    const nav = GROUPS.map(([gid, gl]) => {
      const pages = ids.filter(i => PAGES[i].group === gid && match(i));
      if (!pages.length) return "";
      const single = ids.filter(i => PAGES[i].group === gid).length === 1, expanded = q ? true : openG.has(gid);
      return `<button type="button" class="gh" data-g="${gid}" aria-expanded="${expanded}"><svg viewBox="0 0 24 24">${IC[gid]}</svg>${esc(gl)}<span class="ch">›</span></button>` + (expanded ? pages.map(i => `<button type="button" class="pg" data-p="${i}" ${i === cur ? 'aria-current="page"' : ""}>${esc(PAGES[i].label)}</button>`).join("") : "");
    }).join("");
    dlg.innerHTML = `<nav aria-label="Settings"><label class="sq" for="pbs-q">⌕<input id="pbs-q" type="text" placeholder="Search settings" value="${esc(q)}" autocomplete="off"><kbd>⌘K</kbd></label>${nav || '<p class="note" style="padding:8px">No match.</p>'}</nav>
      <div class="main"><header>Settings <span>›</span> ${esc(g[1])} <span>›</span> <b>${esc(p.label)}</b><button type="button" class="x" data-close aria-label="Close">×</button></header><div class="body">${p.render()}</div></div>`;
    const nb = dlg.querySelector(".body"); if (nb) nb.scrollTop = keep;
    if (q) { const i = dlg.querySelector("#pbs-q"); i.focus(); i.setSelectionRange(q.length, q.length); }
  }
  function say(t) { const m = dlg && dlg.querySelector("#pbs-msg"); if (m) m.textContent = t; }
  function openDlg(path) {
    if (!document.getElementById("pbs-css-style")) { const st = document.createElement("style"); st.id = "pbs-css-style"; st.textContent = CSS; document.head.appendChild(st); }
    if (!dlg) {
      dlg = document.createElement("dialog"); dlg.className = "pbs"; dlg.setAttribute("aria-label", "Settings"); document.body.appendChild(dlg);
      dlg.addEventListener("close", () => { open_ = false; });
      dlg.addEventListener("click", async e => {
        const t = e.target, c = s => t.closest(s);
        if (t === dlg || c("[data-close]")) return dlg.close();
        let b;
        if ((b = c("[data-g]"))) { openG.has(b.dataset.g) ? openG.delete(b.dataset.g) : openG.add(b.dataset.g); const f = first(b.dataset.g); if (openG.has(b.dataset.g) && PAGES[cur].group !== b.dataset.g) cur = f; return paint(); }
        if ((b = c("[data-p]"))) { cur = b.dataset.p; openG.add(PAGES[cur].group); if (page().needsHub) loadHub(true); return paint(); }
        if ((b = c("[data-k]"))) { T().set(b.dataset.k, b.dataset.v); return paint(); }
        if ((b = c("[data-set]"))) { T().set(b.dataset.set, b.dataset.v); return paint(); }
        if ((b = c("[data-pref]"))) { T().setPref(b.dataset.pref, b.dataset.v); return paint(); }
        if ((b = c("[data-tog]")) && !b.dataset.tog.startsWith("mute:")) { const k = b.dataset.tog, on = b.getAttribute("aria-checked") === "true"; T().setPref(k, k === "clock24" ? (on ? "0" : "1") : (on ? "off" : "on")); return paint(); }
        if (c("[data-custom]")) { T().set("accent", "custom:" + dlg.querySelector("#pbs-color").value); return paint(); }
        if (c("[data-clearcss]")) { T().set("css", ""); return paint(); }
        if (c("[data-export]")) return T().exportLook();
        if (c("[data-import]")) return T().importLook(m => { paint(); say(m); });
        if (c("[data-removeusers]")) { T().removeUsers(); return paint(); }
        if (c("[data-reset]")) { if (confirm("Reset appearance and interface settings to the defaults?")) { T().resetAll(); paint(); } return; }
        if (c("[data-pushtog]")) { try { await togglePush(); await loadNotif(); await loadHub(true); paint(); say("Done."); } catch (er) { say(er.message); } return; }
        if (c("[data-pushtest]")) { try { const j = await call("POST", "/api/push/test", {}); say(`Sent to ${j.sent} of ${j.subscriptions} device(s).`); } catch (er) { say(er.message); } return; }
        if (c("[data-alertreset]")) { try { nt.n = await call("POST", "/api/settings/notify", {down_alert_s: null}); paint(); } catch (er) { say(er.message); } return; }
        if ((b = c("[data-apv]"))) { try { const j = await call("POST", `/api/approvals/${encodeURIComponent(b.dataset.room)}/${encodeURIComponent(b.dataset.call)}`, {decision: b.dataset.apv}); await loadNotif(); paint(); say(j.message || "Done."); } catch (er) { say(er.message); } return; }
        if ((b = c("[data-tog^='mute:']"))) { const id = b.dataset.tog.slice(5), on = b.getAttribute("aria-checked") === "true"; try { await call("PATCH", "/api/rooms/" + encodeURIComponent(id), {muted: on}); await loadNotif(); paint(); } catch (er) { say(er.message); } return; }
        if ((b = c("[data-rmdev]"))) { if (!confirm("Remove this device? It will be signed out and its passkey stops working here.")) return; try { await call("DELETE", "/api/devices/" + encodeURIComponent(b.dataset.rmdev)); await loadHub(true); paint(); } catch (er) { say(er.message); } return; }
        if ((b = c("[data-rmses]"))) { try { await call("DELETE", "/api/sessions/" + encodeURIComponent(b.dataset.rmses)); await loadHub(true); paint(); } catch (er) { say(er.message); } return; }
        if (c("[data-pair]")) { dlg.close(); if (window.pairDevice) window.pairDevice(); else location.href = "/?view=hub"; return; }
      });
      dlg.addEventListener("input", e => {
        if (e.target.id === "pbs-q") { q = e.target.value.trim().toLowerCase(); const hit = ids.find(i => (PAGES[i].label + " " + PAGES[i].words).toLowerCase().includes(q)); if (q && hit && !(PAGES[cur].label + " " + PAGES[cur].words).toLowerCase().includes(q)) cur = hit; return paint(); }
        if (e.target.id === "pbs-css") { T().set("css", e.target.value.slice(0, 20000)); const n = dlg.querySelector("#pbs-cssn"); if (n) n.textContent = `${e.target.value.length} of 20000 characters`; }
        if (e.target.id === "pbs-color") T().set("accent", "custom:" + e.target.value);
      });
      dlg.addEventListener("change", e => {
        const d = e.target.dataset; if (!d) return;
        if (d.setSel) { T().set(d.setSel, e.target.value); paint(); }
        if (d.alertSel) { call("POST", "/api/settings/notify", {down_alert_s: Number(e.target.value)}).then(j => { nt.n = j; paint(); }).catch(er => { say(er.message); loadNotif().then(paint); }); return; }
        if (d.prefSel) { T().setPref(d.prefSel, e.target.value); paint(); }
      });
      dlg.addEventListener("keydown", e => { if ((e.metaKey || e.ctrlKey) && e.key === "k") { e.preventDefault(); const i = dlg.querySelector("#pbs-q"); i && i.focus(); } });
    }
    if (path) { const id = path.includes("/") ? path.split("/")[1] : path; if (PAGES[id]) cur = id; else if (first(path)) cur = first(path); }
    q = ""; openG.add(PAGES[cur].group); open_ = true;
    paint();
    if (!dlg.open) dlg.showModal();
    if (page().needsHub || true) loadHub(false);
  }
  window.PBSettings = {open: openDlg, pages: () => ids.map(i => ({id: i, label: PAGES[i].label, group: PAGES[i].group}))};
})();
