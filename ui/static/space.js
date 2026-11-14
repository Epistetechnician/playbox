/* Playbox space: the rooms as a 3D scene over a generative dither, an ASCII wormhole into a room, and the splat on arrival.
   Pure canvas, no libraries. Everything drawn is a room's name, state or counts; nothing here touches the network. */
const PBSpace = (() => {
  "use strict";
  const reduced = () => matchMedia("(prefers-reduced-motion: reduce)").matches;
  const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
  const lerp = (a, b, t) => a + (b - a) * t;
  const ease = t => t * t * (3 - 2 * t);
  /* 70-step density ramp, light to heavy: the classic ASCII shading ramp, so the tunnel has real tonal depth */
  const RAMP = " .'`^\",:;Il!i><~+_-?][}{1)(|\\/tfjrxnuvczXYUJCLQ0OZmwqpdbkhao*#MW&8%B@$";
  /* a code-editor palette (Tokyo Night on dark, a matching paper variant on light): blue, purple, cyan, green, amber, pink */
  const hex = h => [parseInt(h.slice(1, 3), 16), parseInt(h.slice(3, 5), 16), parseInt(h.slice(5, 7), 16)];
  const SYN_DARK = ["#7aa2f7", "#bb9af7", "#7dcfff", "#9ece6a", "#e0af68", "#f7768e"].map(hex);
  const SYN_LIGHT = ["#2e59c8", "#7847c9", "#0f7290", "#4a7d1f", "#a3640b", "#c4264b"].map(hex);
  const mix = (a, b, t) => [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t)];
  function grad(stops, t) { const x = clamp(t, 0, .9999) * (stops.length - 1), i = Math.floor(x); return mix(stops[i], stops[i + 1], x - i); }

  /* 8x8 Bayer matrix, 0..1 */
  const BAYER = (() => {
    let b = [[0]];
    while (b.length < 8) {
      const n = b.length, m = Array.from({length: n * 2}, () => new Array(n * 2));
      for (let y = 0; y < n; y++) for (let x = 0; x < n; x++) {
        const v = b[y][x] * 4;
        m[y][x] = v; m[y][x + n] = v + 2; m[y + n][x] = v + 3; m[y + n][x + n] = v + 1;
      }
      b = m;
    }
    return b.map(r => r.map(v => (v + .5) / 64));
  })();
  const bay = (x, y) => BAYER[y & 7][x & 7];

  /* theme colours as [r,g,b], resolved through a probe so light-dark() and var() both work */
  let probe = null, palCache = null, palAt = 0;
  function palette() {
    const now = performance.now();
    if (palCache && now - palAt < 1500) return palCache;
    if (!probe) { probe = document.createElement("i"); probe.style.cssText = "display:none"; document.body.appendChild(probe); }
    const get = v => { probe.style.color = `var(${v})`; const m = getComputedStyle(probe).color.match(/[\d.]+/g) || [128, 128, 128]; return [+m[0], +m[1], +m[2]]; };
    palCache = {bg: get("--bg"), panel: get("--panel"), ink: get("--ink"), mut: get("--mut"), line: get("--line-2"), sig: get("--sig"), ok: get("--ok-led"), bad: get("--bad"), on: get("--on-sig")};
    palCache.dark = (palCache.bg[0] * .3 + palCache.bg[1] * .59 + palCache.bg[2] * .11) < 128;
    palCache.syn = palCache.dark ? SYN_DARK : SYN_LIGHT;
    palCache.packed = palCache.syn.map(c => (c[2] << 16) | (c[1] << 8) | c[0]);
    palAt = now;
    return palCache;
  }
  const rgb = (c, a = 1) => `rgba(${c[0] | 0},${c[1] | 0},${c[2] | 0},${a})`;

  function hash(id) { let h = 2166136261; for (const c of String(id)) h = Math.imul(h ^ c.charCodeAt(0), 16777619) >>> 0; return h; }
  function sigBits(id) {      /* the same 5x5 identicon the cards use everywhere else */
    let h = hash(id), bits = 0;
    for (let i = 0; i < 8; i++) { h ^= h << 13; h >>>= 0; h ^= h >>> 17; h ^= h << 5; h >>>= 0; bits = h & 0x7fff; const n = bits.toString(2).replace(/0/g, "").length; if (n >= 6 && n <= 11) break; }
    return bits;
  }

  /* ------------------------------------------------------------------ the 3D room space */
  const st = {yaw: .55, pitch: .42, dist: 3.6, focus: null, fx: 0, fy: 0, fz: 0, ang: {}, pings: {}, hover: null};
  const STATE_TXT = {live: "Active", idle: "Quiet", hold: "Witness hold", bad: "Chain failed", empty: "No room yet", down: "Unreachable"};
  let cur = null;       /* the mounted scene, if any */

  function mount(el, rooms, onOpen) {
    unmount();
    const cv = document.createElement("canvas");
    cv.className = "space-cv"; cv.setAttribute("role", "img");
    cv.setAttribute("aria-label", `Your ${rooms.length} room${rooms.length === 1 ? "" : "s"} as a 3D space. Drag to turn it, scroll to zoom, select a room to look closer. The same rooms are listed after this view.`);
    const panel = document.createElement("div"); panel.className = "space-panel"; panel.hidden = true;
    const hint = document.createElement("div"); hint.className = "space-hint"; hint.textContent = "drag to turn · scroll to zoom · select a room";
    el.replaceChildren(cv, panel, hint);
    const ctx = cv.getContext("2d"), buf = document.createElement("canvas"), bctx = buf.getContext("2d");
    const s = {el, cv, ctx, buf, bctx, panel, rooms, onOpen, W: 0, H: 0, dpr: 1, hits: [], raf: 0, last: performance.now(), t: st.clock || 0, lastBack: 0, pointers: new Map(), drag: null, ro: null, dead: false};
    cur = s;
    for (const k of ["mine", "shared"]) {
      const rs = rooms.filter(r => r.kind === k);
      rs.forEach((r, i) => { if (st.ang[r.id] === undefined) st.ang[r.id] = (i / rs.length) * 6.2832 + (k === "shared" ? .6 : 0); });
    }
    if (st.focus && !rooms.some(r => r.id === st.focus)) st.focus = null;
    resize(s);
    s.ro = new ResizeObserver(() => resize(s)); s.ro.observe(el);
    bind(s);
    drawPanel(s);
    const step = now => {
      if (s.dead) return;
      const dt = Math.min(.1, (now - s.last) / 1000); s.last = now;
      if (!document.hidden) frame(s, dt);
      s.raf = requestAnimationFrame(step);
    };
    frame(s, 0);
    s.raf = requestAnimationFrame(step);
    return s;
  }
  function unmount() { if (!cur) return; cur.dead = true; cancelAnimationFrame(cur.raf); cur.ro && cur.ro.disconnect(); window.removeEventListener("keydown", cur.key); cur = null; }

  function resize(s) {
    const w = Math.max(280, s.el.clientWidth), h = clamp(Math.round(w * .72), 360, Math.round(innerHeight * .74));
    s.dpr = Math.min(2, devicePixelRatio || 1); s.W = w; s.H = h;
    s.cv.width = w * s.dpr; s.cv.height = h * s.dpr; s.cv.style.height = h + "px";
    s.cell = w < 520 ? 2 : 3;
    s.bw = Math.ceil(w / s.cell); s.bh = Math.ceil(h / s.cell); s.buf.width = s.bw; s.buf.height = s.bh; s.img = s.bctx.createImageData(s.bw, s.bh); s.u32 = new Uint32Array(s.img.data.buffer); s.bAt = -1;
    frame(s, 0);
  }

  function worldPos(r) {
    const R = r.kind === "mine" ? 1.15 : 2.15, a = st.ang[r.id] ?? 0, hh = (hash(r.id) % 1000) / 1000;
    return [R * Math.cos(a), (hh - .5) * .8, R * Math.sin(a)];
  }
  function project(s, p) {
    const x = p[0] - st.fx, y = p[1] - st.fy, z = p[2] - st.fz, cy = Math.cos(st.yaw), sy = Math.sin(st.yaw), cp = Math.cos(st.pitch), sp = Math.sin(st.pitch);
    const x1 = x * cy + z * sy, z1 = -x * sy + z * cy, y2 = y * cp - z1 * sp, z2 = y * sp + z1 * cp, d = z2 + st.dist;
    if (d < .25) return null;
    const f = Math.min(s.W, s.H) * .95, k = f / d;
    return {x: s.W / 2 + x1 * k, y: s.H / 2 - y2 * k, k, d};
  }

  function backdrop(s, t, pal) {
    /* A flowing, domain-warped field quantised by an 8x8 Bayer matrix into a few translucent syntax-coloured levels.
       Most pixels stay fully transparent; the dither only shows where the field is high, and fades toward the middle so the rooms read clearly. */
    const {bw, bh, u32} = s, P = pal.packed, n = P.length, aspect = bh / bw;
    for (let y = 0; y < bh; y++) {
      const ny = y / bw, vy = y / bh - .5;
      for (let x = 0; x < bw; x++) {
        const nx = x / bw;
        const qx = Math.sin(ny * 6.1 + t * .21) + Math.sin(nx * 4.3 - t * .17), qy = Math.cos(nx * 6.7 - t * .19) + Math.cos(ny * 4.1 + t * .13);
        const v1 = .5 + .5 * Math.sin(nx * 9 + qx * 1.4 + ny * 7 + qy * 1.2 + t * .25), v2 = .5 + .5 * Math.sin(nx * 21 - qy * 2.3 + ny * 5 + t * .11);
        let v = v1 * .68 + v2 * .32;
        const dx = nx - .5, d = Math.sqrt(dx * dx + vy * vy * aspect * aspect * 2.2);
        v *= .12 + d * 1.7;
        const th = bay(x, y), lev = v * 3.0 + (th - .5) * 1.25;
        if (lev < 1) { u32[y * bw + x] = 0; continue; }
        const ci = ((Math.floor(v1 * 4.2 + nx * 3.2 + ny * 2.4 + t * .05) % n) + n) % n, a = clamp(.1 + (lev - 1) * .08, .1, .3) * 255 | 0;
        u32[y * bw + x] = (a << 24) | P[ci];
      }
    }
    s.bctx.putImageData(s.img, 0, 0);
  }

  function frame(s, dt) {
    if (!s.W) return;
    s.t += dt; st.clock = s.t;
    const rm = reduced(), pal = palette(), {ctx, W, H} = s;
    for (const r of s.rooms) st.ang[r.id] = (st.ang[r.id] ?? 0) + (rm || st.focus ? 0 : dt * (r.kind === "mine" ? .06 : -.035));
    if (!rm && !s.drag && !st.focus) st.yaw += dt * .02;
    /* camera tween toward the focused room */
    const fr = st.focus && s.rooms.find(r => r.id === st.focus), tp = fr ? worldPos(fr) : [0, 0, 0], td = fr ? 1.7 : st.userDist ?? 3.6, k = rm ? 1 : Math.min(1, dt * 5.5);
    st.fx = lerp(st.fx, tp[0], k); st.fy = lerp(st.fy, tp[1], k); st.fz = lerp(st.fz, tp[2], k); st.dist = lerp(st.dist, td, k);

    ctx.setTransform(s.dpr, 0, 0, s.dpr, 0, 0);
    ctx.clearRect(0, 0, W, H);
    if (rm ? s.bAt < 0 : s.t - s.bAt > .06) { backdrop(s, rm ? 3 : s.t, pal); s.bAt = s.t; }
    ctx.imageSmoothingEnabled = false;
    ctx.drawImage(s.buf, 0, 0, W, H);

    const fog = d => clamp(1.25 - (d - 2.2) * .16, .25, 1);
    ctx.lineWidth = 1;
    /* floor grid */
    ctx.strokeStyle = rgb(pal.syn[0], .28);
    for (let g = -3; g <= 3; g += .75) for (const horiz of [0, 1]) {
      const a = project(s, horiz ? [-3, -.7, g] : [g, -.7, -3]), b = project(s, horiz ? [3, -.7, g] : [g, -.7, 3]);
      if (!a || !b) continue;
      ctx.globalAlpha = fog((a.d + b.d) / 2) * .7; ctx.beginPath(); ctx.moveTo(a.x, a.y); ctx.lineTo(b.x, b.y); ctx.stroke();
    }
    /* orbit rings */
    for (const [R, label] of [[1.15, "MINE"], [2.15, "SHARED"]]) {
      ctx.strokeStyle = rgb(pal.syn[R > 2 ? 1 : 2], .75); ctx.setLineDash([2, 5]); ctx.beginPath();
      let first = true, dsum = 0, n = 0;
      for (let i = 0; i <= 72; i++) { const a = i / 72 * 6.2832, p = project(s, [R * Math.cos(a), 0, R * Math.sin(a)]); if (!p) { first = true; continue; } dsum += p.d; n++; first ? ctx.moveTo(p.x, p.y) : ctx.lineTo(p.x, p.y); first = false; }
      ctx.globalAlpha = n ? fog(dsum / n) : 0; ctx.stroke(); ctx.setLineDash([]);
      const lp = project(s, [R, 0, 0]); if (lp && n) { ctx.globalAlpha = .9; ctx.fillStyle = rgb(pal.syn[R > 2 ? 1 : 2]); ctx.font = "600 9px ui-monospace,Menlo,monospace"; ctx.fillText(label, lp.x + 6, lp.y - 6); }
    }
    ctx.globalAlpha = 1;

    /* hub at the origin + a plumb line to the floor */
    const hub = project(s, [0, 0, 0]);
    const items = [];
    for (const r of s.rooms) { const p = worldPos(r), q = project(s, p); if (q) items.push({r, q, p}); }
    items.sort((a, b) => b.q.d - a.q.d);
    if (hub) {
      for (const it of items) {          /* edges first, far to near */
        const col = it.r.state === "hold" ? pal.sig : it.r.state === "live" ? pal.ok : it.r.state === "bad" ? pal.bad : pal.mut;
        ctx.strokeStyle = rgb(col, .8); ctx.globalAlpha = fog(it.q.d) * (it.r.state === "live" || it.r.state === "hold" ? .9 : .45); ctx.lineWidth = it.r.state === "live" ? 1.5 : 1;
        ctx.beginPath(); ctx.moveTo(hub.x, hub.y); ctx.lineTo(it.q.x, it.q.y); ctx.stroke();
      }
      ctx.globalAlpha = 1; ctx.lineWidth = 1;
      const h = 11 * Math.min(1.8, hub.k / 80);
      ctx.fillStyle = rgb(pal.ink); ctx.fillRect(hub.x - h, hub.y - h, h * 2, h * 2);
      ctx.fillStyle = rgb(pal.sig); ctx.fillRect(hub.x - h * .32, hub.y - h * .32, h * .64, h * .64);
      if (hub.k > 60) { ctx.fillStyle = rgb(pal.ink); ctx.font = "600 9px ui-monospace,Menlo,monospace"; ctx.textAlign = "center"; ctx.fillText("THIS HUB", hub.x, hub.y + h + 13); ctx.textAlign = "left"; }
    }
    /* rooms, far to near */
    s.hits = [];
    for (const it of items) {
      const {r, q} = it, half = clamp(.26 * q.k, 9, 70), x0 = q.x - half, y0 = q.y - half, a = fog(q.d), sel = st.focus === r.id, hov = st.hover === r.id;
      const col = r.state === "hold" ? pal.sig : r.state === "live" ? pal.ok : r.state === "bad" ? pal.bad : r.state === "idle" ? pal.mut : pal.line;
      ctx.globalAlpha = a;
      /* shadow on the floor */
      const fl = project(s, [it.p[0], -.7, it.p[2]]);
      if (fl) { ctx.strokeStyle = rgb(pal.mut, .5); ctx.setLineDash([1, 3]); ctx.beginPath(); ctx.moveTo(q.x, q.y + half); ctx.lineTo(fl.x, fl.y); ctx.stroke(); ctx.setLineDash([]); ctx.fillStyle = rgb(pal.mut, .35); ctx.fillRect(fl.x - half * .5, fl.y - 1, half, 2); }
      /* halo for active and held rooms */
      if (r.state === "live" || r.state === "hold") {
        const ph = ((s.t * (r.state === "hold" ? 1 : .42)) % 1), g = half * (1 + ph * .55);
        ctx.strokeStyle = rgb(col, 1 - ph); ctx.lineWidth = 1.5; ctx.strokeRect(q.x - g, q.y - g, g * 2, g * 2);
      }
      const pg = st.pings[r.id]; if (pg) { const u = (s.t - pg) / 1.3; if (u > 1) delete st.pings[r.id]; else { const g = half * (1 + u * 2.6); ctx.strokeStyle = rgb(pal.sig, 1 - u); ctx.lineWidth = 2; ctx.strokeRect(q.x - g, q.y - g, g * 2, g * 2); } }
      ctx.lineWidth = sel || hov ? 3 : r.state === "live" || r.state === "hold" ? 2.5 : 1.5;
      ctx.fillStyle = rgb(pal.panel); ctx.strokeStyle = rgb(col); if (r.state === "empty" || r.state === "down") ctx.setLineDash([3, 3]);
      ctx.fillRect(x0, y0, half * 2, half * 2); ctx.strokeRect(x0, y0, half * 2, half * 2); ctx.setLineDash([]);
      /* identicon */
      const bits = sigBits(r.id), u = half * 2 * .78 / 5, ox = q.x - u * 2.5, oy = q.y - u * 2.5;
      ctx.fillStyle = rgb(pal.ink, .92);
      for (let rr = 0; rr < 5; rr++) for (let c = 0; c < 3; c++) if (bits >> (rr * 3 + c) & 1) { ctx.fillRect(ox + (c) * u, oy + rr * u, u + .5, u + .5); if (c < 2) ctx.fillRect(ox + (4 - c) * u, oy + rr * u, u + .5, u + .5); }
      if (r.unread) { const bw = 16; ctx.fillStyle = rgb(pal.sig); ctx.fillRect(x0 + half * 2 - bw / 2, y0 - 7, bw, 14); ctx.fillStyle = rgb(pal.on); ctx.font = "700 9px ui-monospace,Menlo,monospace"; ctx.textAlign = "center"; ctx.fillText(r.unread > 9 ? "9+" : String(r.unread), x0 + half * 2, y0 + 3); ctx.textAlign = "left"; }
      if (half > 13) {
        const label = (r.label.length > 18 ? r.label.slice(0, 17) + "…" : r.label).toUpperCase();
        ctx.font = `600 ${clamp(half * .3, 9, 13)}px ui-monospace,Menlo,monospace`; ctx.textAlign = "center";
        ctx.strokeStyle = rgb(pal.bg, .9); ctx.lineWidth = 3; ctx.strokeText(label, q.x, y0 + half * 2 + 14); ctx.fillStyle = rgb(pal.ink); ctx.fillText(label, q.x, y0 + half * 2 + 14); ctx.textAlign = "left";
      }
      ctx.globalAlpha = 1;
      s.hits.push({id: r.id, x: q.x, y: q.y, h: half * 1.1});
    }
    s.hits.reverse();      /* nearest first for hit tests */
    ctx.lineWidth = 1;
  }

  function hit(s, x, y) { return s.hits.find(h => Math.abs(x - h.x) <= h.h && Math.abs(y - h.y) <= h.h)?.id || null; }
  function focus(s, id) { st.focus = id; if (!id) st.userDist = st.userDist ?? 3.6; drawPanel(s); }

  function drawPanel(s) {
    const r = st.focus && s.rooms.find(x => x.id === st.focus), p = s.panel;
    if (!r) { p.hidden = true; p.replaceChildren(); return; }
    p.hidden = false; p.replaceChildren();
    const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text !== undefined) e.textContent = text; return e; };
    const head = el("div", "sp-head"); head.append(el("b", "", r.label), el("span", "chip " + (r.state === "live" ? "ok" : r.state === "hold" || r.state === "bad" ? "warn" : ""), STATE_TXT[r.state] || r.state));
    const meta = el("div", "sp-meta", `${r.kind === "mine" ? "Your agents" : "Shared"}${r.machine ? " · " + r.machine : ""} · ${r.members || 0} member${r.members === 1 ? "" : "s"}${r.unread ? " · " + r.unread + " new" : ""}${r.chain ? (r.chain.ok ? " · history verified" : " · HISTORY FAILED") : ""}`);
    p.append(head, meta);
    if (r.last && r.last.text) p.append(el("div", "sp-last", "“" + r.last.text.slice(0, 140) + "”"));
    const row = el("div", "sp-row");
    const go = el("button", "btn primary sm", "Enter room"); go.type = "button"; go.addEventListener("click", () => s.onOpen(r, s.hitRect(r)));
    const back = el("button", "btn sm", "Back"); back.type = "button"; back.addEventListener("click", () => focus(s, null));
    row.append(go, back); p.append(row);
  }

  function bind(s) {
    s.hitRect = r => { const h = s.hits.find(x => x.id === r.id); const b = s.cv.getBoundingClientRect(); return h ? {x: b.left + h.x, y: b.top + h.y} : {x: b.left + b.width / 2, y: b.top + b.height / 2}; };
    const pt = e => { const b = s.cv.getBoundingClientRect(); return [e.clientX - b.left, e.clientY - b.top]; };
    s.cv.addEventListener("pointerdown", e => {
      s.cv.setPointerCapture(e.pointerId); s.pointers.set(e.pointerId, pt(e));
      s.drag = {x: e.clientX, y: e.clientY, moved: 0, id: e.pointerId, pinch: 0};
    });
    s.cv.addEventListener("pointermove", e => {
      const [x, y] = pt(e);
      if (!s.pointers.has(e.pointerId)) { const id = hit(s, x, y); if (id !== st.hover) { st.hover = id; s.cv.style.cursor = id ? "pointer" : "grab"; } return; }
      const prev = s.pointers.get(e.pointerId); s.pointers.set(e.pointerId, [x, y]);
      if (s.pointers.size === 2) {        /* pinch zoom */
        const [a, b] = [...s.pointers.values()], d = Math.hypot(a[0] - b[0], a[1] - b[1]);
        if (s.drag.pinch) { st.userDist = clamp((st.userDist ?? st.dist) * s.drag.pinch / d, 2.2, 8); if (st.focus) focus(s, null); }
        s.drag.pinch = d; s.drag.moved = 99; return;
      }
      if (!s.drag) return;
      const dx = x - prev[0], dy = y - prev[1]; s.drag.moved += Math.abs(dx) + Math.abs(dy);
      if (s.drag.moved > 5) { st.yaw -= dx * .006; st.pitch = clamp(st.pitch + dy * .005, -.2, 1.25); s.cv.style.cursor = "grabbing"; }
    });
    const up = e => {
      const d = s.drag; s.pointers.delete(e.pointerId); if (s.pointers.size) return; s.drag = null; s.cv.style.cursor = "";
      if (!d || d.moved > 5 || e.type === "pointercancel") return;
      const [x, y] = pt(e), id = hit(s, x, y);
      if (id && id === st.focus) { const r = s.rooms.find(q => q.id === id); s.onOpen(r, s.hitRect(r)); }      /* a second tap on the focused room goes in */
      else focus(s, id);
    };
    s.cv.addEventListener("pointerup", up); s.cv.addEventListener("pointercancel", up);
    s.cv.addEventListener("wheel", e => { e.preventDefault(); if (st.focus) focus(s, null); st.userDist = clamp((st.userDist ?? st.dist) * Math.exp(e.deltaY * .0012), 2.2, 8); }, {passive: false});
    s.cv.addEventListener("dblclick", e => e.preventDefault());
    s.key = e => {
      if (!st.focus || /INPUT|TEXTAREA|SELECT/.test(document.activeElement?.tagName || "") || document.querySelector("dialog[open]")) return;
      if (e.key === "Escape") { focus(s, null); }
      else if (e.key === "Enter" && !(document.activeElement && document.activeElement.closest("button,a"))) { const r = s.rooms.find(q => q.id === st.focus); if (r) s.onOpen(r, s.hitRect(r)); }
    };
    window.addEventListener("keydown", s.key);
  }

  function ping(id) { if (cur) st.pings[id] = cur.t; }

  /* ------------------------------------------------------------------ ASCII dither tunnel, used by the wormhole and the splat */
  function tunnel(ctx, W, H, o) {
    const pal = palette(), cw = W < 520 ? 6 : 7, ch = W < 520 ? 10 : 12, cols = Math.ceil(W / cw), rows = Math.ceil(H / ch), M = Math.max(W, H);
    const cx = o.cx * W, cy = o.cy * H, p = o.p, time = o.time, d = o.dissolve || 0;
    ctx.font = `500 ${ch - 1}px ui-monospace,Menlo,monospace`; ctx.textBaseline = "top";
    const stops = [pal.syn[0], pal.syn[1], pal.syn[5], pal.syn[4]];
    const bgc = rgb(pal.bg);
    for (let j = 0; j < rows; j++) for (let i = 0; i < cols; i++) {
      const x = i * cw + cw / 2, y = j * ch + ch / 2, dx = (x - cx) / M * 2, dy = (y - cy) / M * 2, r = Math.hypot(dx, dy), th = bay(i, j);
      const cover = p * 2.4;
      if (r > cover + (th - .5) * .35) continue;                 /* not opened yet: the page shows through */
      const a = Math.atan2(dy, dx);
      const v = -Math.log(r + .03) * 1.1 + time * (1 + p * 6), u = a / 6.2832 * 8 + v * .22;
      const s1 = Math.pow(.5 + .5 * Math.sin(u * 6.2832), 2.4), s2 = Math.pow(.5 + .5 * Math.sin(v * 6.2832), 1.7);     /* spokes and rings of the tube wall */
      let b = clamp(.12 + (s1 * .55 + s2 * .75) * (.3 + .7 * Math.exp(-r * 1.4)) + .85 * Math.exp(-r * 5.5) + p * .12, 0, 1);
      b = clamp((b - .16) * 1.55, 0, 1); b = b * b * (3 - 2 * b);      /* stretch the tones so the tube wall reads as rings and spokes, not noise */
      let ox = 0, oy = 0, alpha = 1;
      if (d > 0) {                                                  /* splat: dissolve outward from the centre, chars thrown off */
        const gone = th * .5 + Math.min(1, r) * .42 + (Math.sin(i * 12.9 + j * 78.2) * .5 + .5) * .08;
        if (gone < d * 1.25) continue;
        const k = clamp((gone - d * 1.25) * 4, 0, 1);
        if (k < 1) { const ux = dx / (r || 1), uy = dy / (r || 1), thr = (1 - k) * (1 - k) * 46 * (.5 + th); ox = ux * thr; oy = uy * thr; alpha = k; }
      }
      const idx = Math.floor(clamp(b + (th - .5) * .045, 0, .999) * RAMP.length);
      if (d <= 0 || alpha >= 1) { ctx.fillStyle = bgc; ctx.fillRect(i * cw, j * ch, cw + .5, ch + .5); }
      if (idx === 0) continue;
      const hot = r < .1 + p * .16, col = hot ? mix(pal.syn[4], pal.dark ? [255, 255, 255] : [60, 30, 0], clamp(b, 0, 1) * .6) : grad(stops, b * .8 + (s1 * .25) + (v * .05 % 1) * .15);
      ctx.fillStyle = rgb(col, alpha); ctx.fillText(RAMP[idx], i * cw + ox + 1, j * ch + oy);
    }
  }

  function overlay() {
    const cv = document.createElement("canvas"), dpr = Math.min(2, devicePixelRatio || 1);
    cv.style.cssText = "position:fixed;inset:0;width:100vw;height:100vh;z-index:9999;pointer-events:auto;visibility:visible;background:transparent";
    cv.width = innerWidth * dpr; cv.height = innerHeight * dpr;
    const ctx = cv.getContext("2d"); ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    document.body.appendChild(cv);
    return {cv, ctx, W: innerWidth, H: innerHeight};
  }

  /* pulled into a room: the tunnel opens at the card and sweeps over the whole page, then `done` navigates */
  function wormhole(from, done) {
    if (reduced()) return done(false);
    const o = overlay(), t0 = performance.now(), dur = 1050;
    let time = 0, last = t0;
    const cx = clamp(from.x / o.W, 0, 1), cy = clamp(from.y / o.H, 0, 1);
    let finished = false;
    const fin = () => { if (finished) return; finished = true; done({cx: .5, cy: .5, time}); };
    const step = now => {
      const t = clamp((now - t0) / dur, 0, 1), p = ease(t);
      time += (now - last) / 1000; last = now;
      const mx = lerp(cx, .5, p), my = lerp(cy, .5, p);
      o.ctx.clearRect(0, 0, o.W, o.H);
      tunnel(o.ctx, o.W, o.H, {cx: mx, cy: my, p, time});
      if (t < 1) requestAnimationFrame(step); else fin();
    };
    requestAnimationFrame(step);
    setTimeout(fin, dur + 400);      /* a background tab throttles rAF: do not hang */
  }

  /* arriving in a room: the page was hidden by the head snippet; draw the last tunnel frame, then splat it away */
  function splat(state) {
    const reveal = () => document.documentElement.classList.remove("pb-splat");
    if (reduced() || !state) return reveal();
    const o = overlay(), t0 = performance.now(), dur = 900, time0 = state.time || 0;
    tunnel(o.ctx, o.W, o.H, {cx: .5, cy: .5, p: 1, time: time0});
    reveal();
    const step = now => {
      const t = clamp((now - t0) / dur, 0, 1);
      o.ctx.clearRect(0, 0, o.W, o.H);
      if (t < 1) { tunnel(o.ctx, o.W, o.H, {cx: .5, cy: .5, p: 1, time: time0 + t * 1.2, dissolve: ease(t)}); requestAnimationFrame(step); }
      else o.cv.remove();
    };
    requestAnimationFrame(step);
    setTimeout(() => o.cv.remove(), dur + 1500);
  }

  /* hub -> room: wormhole, remember where we were going, navigate */
  function enter(room, from) {
    const go = state => { try { if (state) sessionStorage.setItem("pb.splat", JSON.stringify({...state, at: Date.now()})); } catch {} location.href = `/r/${room.id}/`; };
    wormhole(from || {x: innerWidth / 2, y: innerHeight / 2}, go);
  }
  function arrive() {
    let state = null;
    try { const v = sessionStorage.getItem("pb.splat"); sessionStorage.removeItem("pb.splat"); const j = v && JSON.parse(v); if (j && Date.now() - j.at < 10000) state = j; } catch {}
    splat(state);
  }

  return {mount, unmount, ping, enter, arrive, reduced};
})();
