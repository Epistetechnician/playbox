/* Playbox themes: named palettes (each with a dark and a light side), an accent, fonts, interface scale and your own CSS, all kept in this browser.
   The model follows Hermes Agent's desktop themes (NousResearch/hermes-agent, MIT): a theme is a set of colour tokens plus typography, an accent can be
   swapped on top of any theme, and a theme can carry extra CSS. Named palettes (Catppuccin, Nord, Solarized, Everforest, Tokyo Night, GitHub, Gruvbox,
   Dracula) use the colours their authors published; "Playbox" is ours. This file runs in <head>, before first paint, so there is no flash.
   Storage: pb.theme (auto|light|dark, as before), pb.skin, pb.accent, pb.font, pb.mono, pb.scale, pb.css, pb.skins (your own themes). */
(function () {
  const R = document.documentElement;
  const ls = {
    get: (k, d) => { try { const v = localStorage.getItem(k); return v == null ? d : v; } catch { return d; } },
    set: (k, v) => { try { localStorage.setItem(k, v); } catch {} },
    del: k => { try { localStorage.removeItem(k); } catch {} },
  };
  const hex = s => /^#[0-9a-f]{6}$/i.test(String(s));
  const rgb = h => [1, 3, 5].map(i => parseInt(h.slice(i, i + 2), 16));
  const toHex = a => "#" + a.map(v => Math.max(0, Math.min(255, Math.round(v))).toString(16).padStart(2, "0")).join("");
  const mix = (a, b, t) => { const x = rgb(a), y = rgb(b); return toHex(x.map((v, i) => v + (y[i] - v) * t)); };
  const lum = h => { const [r, g, b] = rgb(h).map(v => { v /= 255; return v <= .03928 ? v / 12.92 : ((v + .055) / 1.055) ** 2.4; }); return .2126 * r + .7152 * g + .0722 * b; };

  /* One side of a theme: bg panel ink mut sig blue ok warn bad violet. Everything else is derived from these ten. */
  const P = (bg, panel, ink, mut, sig, blue, ok, warn, bad, violet) => ({bg, panel, ink, mut, sig, blue, ok, warn, bad, violet});
  const THEMES = [
    {id: "playbox", label: "Playbox", desc: "Light olive and blue on a quiet grey-green",
      dark: P("#151713", "#1C1E1A", "#E5E8DD", "#8B9181", "#CBD8A2", "#4C9BE8", "#5CD65C", "#E5A33D", "#FF6B81", "#A78BFA"),
      light: P("#F2F3EE", "#FFFFFF", "#1D2019", "#6B7062", "#6F8236", "#4C8FE0", "#2E7D32", "#B25E09", "#C62840", "#7C3AED")},
    {id: "mono", label: "Mono", desc: "Black, white and one grey. Nothing competes with the text",
      dark: P("#0B0B0B", "#131313", "#ECECEC", "#8C8C8C", "#FFFFFF", "#9DB4D4", "#7FD08A", "#D9B25C", "#F07878", "#B6A6E0"),
      light: P("#F5F5F5", "#FFFFFF", "#141414", "#6A6A6A", "#111111", "#46628C", "#2E7D32", "#8A5A00", "#B3261E", "#6B4FB0")},
    {id: "catppuccin", label: "Catppuccin", desc: "Mocha and Latte",
      dark: P("#1E1E2E", "#181825", "#CDD6F4", "#9399B2", "#CBA6F7", "#89B4FA", "#A6E3A1", "#F9E2AF", "#F38BA8", "#F5C2E7"),
      light: P("#EFF1F5", "#FFFFFF", "#4C4F69", "#6C6F85", "#8839EF", "#1E66F5", "#40A02B", "#DF8E1D", "#D20F39", "#EA76CB")},
    {id: "nord", label: "Nord", desc: "Arctic, north-bluish",
      dark: P("#2E3440", "#3B4252", "#ECEFF4", "#9AA5B8", "#88C0D0", "#81A1C1", "#A3BE8C", "#EBCB8B", "#BF616A", "#B48EAD"),
      light: P("#E5E9F0", "#ECEFF4", "#2E3440", "#5B667A", "#5E81AC", "#5E81AC", "#4F7A3B", "#A87A00", "#B0404A", "#8F6C9A")},
    {id: "solarized", label: "Solarized", desc: "Precision colours for machines and people",
      dark: P("#002B36", "#073642", "#C2CCCC", "#839496", "#2AA198", "#268BD2", "#859900", "#B58900", "#DC322F", "#6C71C4"),
      light: P("#FDF6E3", "#FFFBEF", "#3E555C", "#657B83", "#1F8A82", "#268BD2", "#738500", "#9A7400", "#DC322F", "#6C71C4")},
    {id: "everforest", label: "Everforest", desc: "Green and soft, easy on the eyes",
      dark: P("#2D353B", "#343F44", "#D3C6AA", "#859289", "#A7C080", "#7FBBB3", "#A7C080", "#DBBC7F", "#E67E80", "#D699B6"),
      light: P("#FDF6E3", "#F4F0D9", "#5C6A72", "#829181", "#7B8E00", "#3A94C5", "#8DA101", "#C79000", "#F85552", "#DF69BA")},
    {id: "tokyonight", label: "Tokyo Night", desc: "Night and Day",
      dark: P("#1A1B26", "#1F2335", "#C0CAF5", "#787C99", "#7AA2F7", "#7DCFFF", "#9ECE6A", "#E0AF68", "#F7768E", "#BB9AF7"),
      light: P("#E1E2E7", "#F0F1F5", "#3760BF", "#6172B0", "#2E7DE9", "#007197", "#587539", "#8C6C3E", "#F52A65", "#9854F1")},
    {id: "github", label: "GitHub", desc: "Dark Default and Light Default",
      dark: P("#0D1117", "#161B22", "#E6EDF3", "#8B949E", "#58A6FF", "#388BFD", "#3FB950", "#D29922", "#F85149", "#BC8CFF"),
      light: P("#F6F8FA", "#FFFFFF", "#1F2328", "#59636E", "#0969DA", "#0969DA", "#1A7F37", "#9A6700", "#CF222E", "#8250DF")},
    {id: "gruvbox", label: "Gruvbox", desc: "Retro groove, warm contrast",
      dark: P("#282828", "#32302F", "#EBDBB2", "#A89984", "#B8BB26", "#83A598", "#B8BB26", "#FABD2F", "#FB4934", "#D3869B"),
      light: P("#FBF1C7", "#F9F5D7", "#3C3836", "#7C6F64", "#79740E", "#076678", "#79740E", "#B57614", "#9D0006", "#8F3F71")},
    {id: "dracula", label: "Dracula", desc: "Dark with vivid accents",
      dark: P("#282A36", "#2F3240", "#F8F8F2", "#9AA3C7", "#BD93F9", "#8BE9FD", "#50FA7B", "#FFB86C", "#FF5555", "#FF79C6"),
      light: P("#F8F8F2", "#FFFFFF", "#282A36", "#6272A4", "#7C5CBF", "#1B7FBF", "#2E8B3E", "#B8650A", "#C62B2B", "#C0508F")},
  ];
  const ACCENTS = [["theme", "Theme's own", null], ["olive", "Olive", "#CBD8A2"], ["green", "Green", "#4CC380"], ["cyan", "Cyan", "#38BDD8"], ["blue", "Blue", "#4C9BE8"],
    ["violet", "Violet", "#A78BFA"], ["pink", "Pink", "#EC6BA8"], ["red", "Red", "#F0616D"], ["amber", "Amber", "#E5A33D"], ["mono", "Mono", "#E5E5E5"]];
  const EMOJI = ', "Apple Color Emoji", "Segoe UI Emoji", "Noto Color Emoji"';
  const FONTS = {
    system: ["System UI", 'system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", sans-serif' + EMOJI],
    neo: ["Neo-grotesque", '"Helvetica Neue", "Neue Haas Grotesk", Helvetica, Arial, sans-serif' + EMOJI],
    humanist: ["Humanist", '"Seravek", "Gill Sans Nova", "Noto Sans", "Segoe UI", Optima, Candara, sans-serif' + EMOJI],
    serif: ["Serif", '"Iowan Old Style", "Charter", "Palatino Linotype", Georgia, serif' + EMOJI],
    mono: ["Monospace", 'ui-monospace, "SF Mono", "JetBrains Mono", Menlo, Consolas, monospace' + EMOJI],
  };
  const MONOS = {
    system: ["System mono", 'ui-monospace, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace'],
    jetbrains: ["JetBrains Mono", '"JetBrains Mono", ui-monospace, "SF Mono", Menlo, monospace'],
    plex: ["IBM Plex Mono", '"IBM Plex Mono", ui-monospace, "SF Mono", Menlo, monospace'],
    cascadia: ["Cascadia Code", '"Cascadia Code", "Cascadia Mono", ui-monospace, Consolas, monospace'],
    menlo: ["Menlo", 'Menlo, Monaco, Consolas, monospace'],
  };
  const SCALES = [[.9, "90%"], [1, "100%"], [1.1, "110%"], [1.2, "120%"]];

  function users() { try { const a = JSON.parse(ls.get("pb.skins", "[]")); return Array.isArray(a) ? a.filter(valid) : []; } catch { return []; } }
  function valid(t) { return t && typeof t.id === "string" && typeof t.label === "string" && ["dark", "light"].every(m => t[m] && ["bg", "panel", "ink", "mut", "sig", "blue", "ok", "warn", "bad", "violet"].every(k => hex(t[m][k]))); }
  const all = () => [...THEMES, ...users()];
  const find = id => all().find(t => t.id === id) || THEMES[0];

  /* The mode now: the data-theme attribute (set by the page's own toggle), else the system. */
  const mode = () => R.dataset.theme === "light" || R.dataset.theme === "dark" ? R.dataset.theme : (matchMedia("(prefers-color-scheme:dark)").matches ? "dark" : "light");

  /* Everything the pages' CSS reads, derived from the ten colours. */
  function tokens(p, dark, accent) {
    const sig = accent && hex(accent) ? accent : p.sig;
    const onSig = lum(sig) > .42 ? "#14180C" : "#FFFFFF";
    const toward = dark ? "#FFFFFF" : "#000000";
    return {
      "--bg": p.bg, "--bg-2": mix(p.bg, p.panel, .5), "--panel": p.panel, "--panel-2": mix(p.panel, p.ink, .06), "--field": dark ? mix(p.bg, "#000000", .2) : mix(p.panel, p.bg, .25),
      "--ink": p.ink, "--ink-2": mix(p.ink, p.bg, .24), "--mut": p.mut, "--dim": mix(p.bg, p.ink, .13), "--grid": mix(p.bg, p.ink, .06),
      "--line": mix(p.bg, p.ink, .13), "--line-2": mix(p.bg, p.ink, .23), "--edge": mix(p.bg, p.ink, .4),
      "--sig": sig, "--sig-ink": mix(sig, p.ink, dark ? .2 : .3), "--on-sig": onSig,
      "--blue": mix(p.blue, toward, dark ? .22 : .18), "--blue-edge": p.blue,
      "--ok": p.ok, "--ok-led": p.ok, "--warn": p.warn, "--bad": p.bad, "--violet": p.violet,
      "--scrim": dark ? "rgba(0,0,0,.66)" : "rgba(20,22,18,.45)",
    };
  }

  let customCss;
  function apply() {
    const t = find(ls.get("pb.skin", "playbox")), m = mode(), dark = m === "dark", side = dark ? t.dark : t.light;
    const acc = ls.get("pb.accent", "theme"), swatch = ACCENTS.find(a => a[0] === acc), accentHex = acc.startsWith("custom:") ? acc.slice(7) : swatch && swatch[2];
    const tk = tokens(side, dark, accentHex);
    for (const k in tk) R.style.setProperty(k, tk[k]);
    R.style.colorScheme = m;
    R.dataset.skin = t.id;
    const font = FONTS[ls.get("pb.font", "system")] || FONTS.system, mono = MONOS[ls.get("pb.mono", "system")] || MONOS.system;
    R.style.setProperty("--sans", font[1]); R.style.setProperty("--mono", mono[1]);
    const sc = parseFloat(ls.get("pb.scale", "1")) || 1;
    R.style.zoom = sc === 1 ? "" : String(sc);
    let meta = document.getElementById("themecolor");
    if (!meta) {
      document.querySelectorAll('meta[name="theme-color"]').forEach(x => x.remove());
      meta = document.createElement("meta"); meta.name = "theme-color"; meta.id = "themecolor"; document.head.appendChild(meta);
    }
    meta.content = side.bg;
    const css = ls.get("pb.css", "");
    if (!customCss) { customCss = document.createElement("style"); customCss.id = "pb-custom-css"; }
    customCss.textContent = css.slice(0, 20000);
    if (!customCss.isConnected) (document.head || R).appendChild(customCss);
    document.dispatchEvent(new CustomEvent("pb:theme", {detail: {skin: t.id, mode: m}}));
  }

  const state = () => ({skin: ls.get("pb.skin", "playbox"), mode: ls.get("pb.theme", "auto"), accent: ls.get("pb.accent", "theme"), font: ls.get("pb.font", "system"), mono: ls.get("pb.mono", "system"),
    scale: parseFloat(ls.get("pb.scale", "1")) || 1, css: ls.get("pb.css", "")});
  function set(k, v) {
    if (k === "mode") {
      ls.set("pb.theme", v);
      if (v === "auto") delete R.dataset.theme; else R.dataset.theme = v;
    } else ls.set("pb." + k, String(v));
    apply();
  }

  /* ---- boot ---- */
  { const m = ls.get("pb.theme", "auto"); if (m === "light" || m === "dark") R.dataset.theme = m; }
  apply();
  new MutationObserver(apply).observe(R, {attributes: true, attributeFilter: ["data-theme"]});     // the page's own light/dark toggle
  matchMedia("(prefers-color-scheme:dark)").addEventListener("change", () => { if (!R.dataset.theme) apply(); });
  window.addEventListener("storage", e => { if (e.key && e.key.startsWith("pb.")) apply(); });   // another tab changed it

  /* ---- the settings dialog ---- */
  const esc = s => String(s).replace(/[&<>"']/g, c => ({"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"}[c]));
  const CSS = `
.pbs{width:min(940px,calc(100vw - 24px));height:min(640px,calc(100vh - 40px));max-width:none;padding:0;border:1px solid var(--line-2);border-radius:14px;background:var(--panel);color:var(--ink);box-shadow:0 20px 70px rgba(0,0,0,.45);overflow:hidden}
.pbs::backdrop{background:var(--scrim);backdrop-filter:blur(2px)}
.pbs[open]{display:grid;grid-template-columns:230px 1fr}
.pbs nav{border-right:1px solid var(--line);padding:14px 10px;overflow:auto;background:var(--bg-2)}
.pbs nav h3{margin:2px 8px 10px;font:600 11px var(--sans);letter-spacing:.08em;text-transform:uppercase;color:var(--mut)}
.pbs nav button{display:flex;align-items:center;gap:9px;width:100%;text-align:left;padding:7px 10px;border:1px solid transparent;border-radius:7px;background:none;color:var(--ink-2);font:500 13px var(--sans);cursor:pointer}
.pbs nav button:hover{background:var(--panel-2);color:var(--ink)}
.pbs nav button[aria-current=page]{background:var(--blue-grad);border-color:var(--blue-edge);color:var(--blue)}
.pbs nav .sub{margin-left:12px;font-size:12.5px}
.pbs .main{display:flex;flex-direction:column;min-width:0;min-height:0}
.pbs header{display:flex;align-items:center;gap:8px;padding:14px 20px;border-bottom:1px solid var(--line);font:13px var(--sans);color:var(--mut)}
.pbs header b{color:var(--ink);font-weight:600}
.pbs header .x{margin-left:auto;border:0;background:none;color:var(--mut);font-size:18px;cursor:pointer;border-radius:6px;width:30px;height:30px}
.pbs header .x:hover{background:var(--panel-2);color:var(--ink)}
.pbs .body{padding:18px 20px 24px;overflow:auto;display:grid;gap:18px;align-content:start}
.pbs .row{display:grid;grid-template-columns:1fr auto;gap:6px 18px;align-items:center}
.pbs .row .t{font:500 14px var(--sans)} .pbs .row .d{grid-column:1;font:12.5px/1.4 var(--sans);color:var(--mut)}
.pbs .themes{display:grid;grid-template-columns:repeat(auto-fill,minmax(210px,1fr));gap:12px}
.pbs .tc{display:grid;gap:8px;padding:10px;border:1px solid var(--line-2);border-radius:10px;background:var(--bg-2);cursor:pointer;text-align:left;color:var(--ink);font:inherit}
.pbs .tc:hover{border-color:var(--edge)} .pbs .tc[aria-pressed=true]{border-color:var(--blue-edge);box-shadow:0 0 0 1px var(--blue-edge)}
.pbs .tc .pv{border-radius:7px;padding:9px 10px;font:11.5px/1.5 var(--mono);display:grid;gap:3px}
.pbs .tc .sw{display:flex;gap:4px;margin-top:3px}.pbs .tc .sw i{width:12px;height:12px;border-radius:50%}
.pbs .tc b{font:600 13px var(--sans)} .pbs .tc span.d{font:12px/1.35 var(--sans);color:var(--mut)}
.pbs .seg2{display:inline-flex;border:1px solid var(--line-2);border-radius:8px;overflow:hidden}
.pbs .seg2 button{border:0;background:none;color:var(--ink-2);padding:6px 13px;font:500 12.5px var(--sans);cursor:pointer}
.pbs .seg2 button+button{border-left:1px solid var(--line-2)}
.pbs .seg2 button[aria-pressed=true]{background:var(--blue-grad);color:var(--blue)}
.pbs .acc{display:flex;flex-wrap:wrap;gap:8px}
.pbs .acc button{display:inline-flex;align-items:center;gap:7px;padding:5px 11px 5px 6px;border:1px solid var(--line-2);border-radius:999px;background:none;color:var(--ink-2);font:500 12.5px var(--sans);cursor:pointer}
.pbs .acc button i{width:16px;height:16px;border-radius:50%;border:1px solid var(--line-2)}
.pbs .acc button[aria-pressed=true]{border-color:var(--blue-edge);color:var(--ink);background:var(--blue-grad)}
.pbs select,.pbs textarea,.pbs input[type=text]{background:var(--field);color:var(--ink);border:1px solid var(--line-2);border-radius:8px;padding:7px 10px;font:13px var(--sans)}
.pbs textarea{width:100%;min-height:200px;font:12.5px/1.5 var(--mono);resize:vertical}
.pbs .live{padding:14px;border:1px solid var(--line-2);border-radius:10px;background:var(--bg-2);display:grid;gap:6px}
.pbs .live .h{font:600 17px var(--sans)} .pbs .live p{margin:0;color:var(--ink-2);font:14px/1.5 var(--sans)} .pbs .live code{font:13px var(--mono);color:var(--sig-ink)}
.pbs button.act{padding:7px 14px;border-radius:8px;border:1px solid var(--line-2);background:var(--panel-2);color:var(--ink);font:500 13px var(--sans);cursor:pointer}
.pbs button.act:hover{border-color:var(--edge)}
.pbs .note{font:12.5px/1.45 var(--sans);color:var(--mut)}
@media (max-width:700px){.pbs[open]{grid-template-columns:1fr;grid-template-rows:auto 1fr}.pbs nav{display:flex;gap:6px;overflow-x:auto;border-right:0;border-bottom:1px solid var(--line);padding:8px}.pbs nav h3{display:none}.pbs nav button{width:auto;white-space:nowrap}.pbs nav .sub{margin:0}}
`;
  const SECTIONS = [["theme", "Theme"], ["accent", "Accent"], ["type", "Typography"], ["layout", "Scale and mode"], ["css", "Custom CSS"], ["share", "Import and export"]];
  let dlg, sec = "theme";
  const swatches = (p, dark) => `<span class="sw">${[p.sig, p.blue, p.ok, p.warn, p.bad, p.violet].map(c => `<i style="background:${c}"></i>`).join("")}</span>`;
  function section() {
    const s = state(), cur = find(s.skin), dark = mode() === "dark";
    if (sec === "theme") {
      const card = t => { const p = dark ? t.dark : t.light; return `<button class="tc" data-skin="${esc(t.id)}" aria-pressed="${t.id === s.skin}"><div class="pv" style="background:${p.bg};color:${p.ink};border:1px solid ${mix(p.bg, p.ink, .2)}"><span style="color:${p.mut}">~ playbox up</span><span><span style="color:${p.ok}">●</span> <span style="color:${p.sig}">gpu-lab</span> <span style="color:${p.mut}">3 in room</span></span><span style="color:${p.blue}">&gt; TRY score=6300</span></div><b>${esc(t.label)}</b><span class="d">${esc(t.desc || "Your theme")}</span>${swatches(p, dark)}</button>`; };
      return `<div class="themes">${all().map(card).join("")}</div><p class="note">Each theme has a dark and a light side. Pick the mode under Scale and mode. Your own themes (Import and export) show up here.</p>`;
    }
    if (sec === "accent") return `<div class="row"><div class="t">Primary colour</div><div class="d">Buttons, the brand mark, other members' activity. The theme keeps its own unless you pick one.</div></div>
      <div class="acc">${ACCENTS.map(([id, name, h]) => `<button data-accent="${id}" aria-pressed="${s.accent === id}"><i style="background:${h || (dark ? cur.dark.sig : cur.light.sig)}"></i>${esc(name)}</button>`).join("")}
      <label class="acc" style="gap:8px"><button type="button" data-accent-custom aria-pressed="${s.accent.startsWith("custom:")}"><i style="background:${s.accent.startsWith("custom:") ? s.accent.slice(7) : "var(--line-2)"}"></i>Custom</button><input type="color" id="pbs-color" value="${s.accent.startsWith("custom:") ? s.accent.slice(7) : "#cbd8a2"}" aria-label="Custom primary colour"></label></div>`;
    if (sec === "type") return `<div class="row"><div class="t">Interface font</div><select id="pbs-font">${Object.entries(FONTS).map(([k, v]) => `<option value="${k}" ${s.font === k ? "selected" : ""}>${esc(v[0])}</option>`).join("")}</select><div class="d">Fonts already on this device: nothing is downloaded.</div></div>
      <div class="row"><div class="t">Code and numbers</div><select id="pbs-mono">${Object.entries(MONOS).map(([k, v]) => `<option value="${k}" ${s.mono === k ? "selected" : ""}>${esc(v[0])}</option>`).join("")}</select><div class="d">Used for ids, counts, the activity panels and anything that is code.</div></div>
      <div class="live"><div class="h">Reading sample</div><p>Agents from different owners work in one room. Messages from other members are untrusted text; the witness watches them.</p><p><code>TRY score=6300 vec=[12, 200, 31, 77]</code> 0123456789</p></div>`;
    if (sec === "layout") return `<div class="row"><div class="t">Mode</div><div class="seg2" role="group" aria-label="Mode">${[["auto", "System"], ["light", "Light"], ["dark", "Dark"]].map(([k, l]) => `<button data-mode="${k}" aria-pressed="${s.mode === k}">${l}</button>`).join("")}</div></div>
      <div class="row"><div class="t">Interface scale</div><div class="seg2" role="group" aria-label="Scale">${SCALES.map(([v, l]) => `<button data-scale="${v}" aria-pressed="${s.scale === v}">${l}</button>`).join("")}</div><div class="d">Scales the whole interface, text and spacing together.</div></div>`;
    if (sec === "css") return `<div class="row"><div class="t">Your own CSS</div><div class="d">Applied on every page of this hub, after the theme. Variables you can override: <code>--bg --panel --ink --mut --sig --blue-edge --ok --warn --bad --line</code>.</div></div>
      <textarea id="pbs-css" spellcheck="false" placeholder=":root { --r: 4px; }">${esc(s.css)}</textarea><div class="row"><span class="note" id="pbs-cssn">${s.css.length} of 20000 characters</span><button class="act" data-clearcss>Clear</button></div>`;
    return `<div class="row"><div class="t">Export this look</div><div class="d">The current theme, accent, fonts and scale as one JSON file.</div><button class="act" data-export>Download</button></div>
      <div class="row"><div class="t">Import a theme</div><div class="d">A JSON file with <code>id</code>, <code>label</code> and <code>dark</code> and <code>light</code> palettes (bg, panel, ink, mut, sig, blue, ok, warn, bad, violet as #rrggbb).</div><button class="act" data-import>Choose a file</button></div>
      ${users().length ? `<div class="row"><div class="t">Your themes</div><div class="d">${users().map(t => esc(t.label)).join(", ")}</div><button class="act" data-removeusers>Remove all</button></div>` : ""}<p class="note" id="pbs-msg"></p>`;
  }
  function paint() {
    const name = SECTIONS.find(x => x[0] === sec)[1];
    dlg.innerHTML = `<nav aria-label="Appearance settings"><h3>Appearance</h3>${SECTIONS.map(([k, l]) => `<button class="sub" data-sec="${k}" ${k === sec ? 'aria-current="page"' : ""}>${l}</button>`).join("")}</nav>
      <div class="main"><header>Settings <span>›</span> Appearance <span>›</span> <b>${name}</b><button class="x" data-close aria-label="Close">×</button></header><div class="body">${section()}</div></div>`;
  }
  function open(which) {
    if (!document.getElementById("pbs-css-style")) { const st = document.createElement("style"); st.id = "pbs-css-style"; st.textContent = CSS; document.head.appendChild(st); }
    if (!dlg) {
      dlg = document.createElement("dialog"); dlg.className = "pbs"; dlg.setAttribute("aria-label", "Appearance settings");
      document.body.appendChild(dlg);
      dlg.addEventListener("click", e => {
        const q = s => e.target.closest(s);
        if (e.target === dlg || q("[data-close]")) return dlg.close();
        let b;
        if ((b = q("[data-sec]"))) { sec = b.dataset.sec; return paint(); }
        if ((b = q("[data-skin]"))) { set("skin", b.dataset.skin); return paint(); }
        if ((b = q("[data-accent]"))) { set("accent", b.dataset.accent); return paint(); }
        if (q("[data-accent-custom]")) { set("accent", "custom:" + dlg.querySelector("#pbs-color").value); return paint(); }
        if ((b = q("[data-mode]"))) { set("mode", b.dataset.mode); return paint(); }
        if ((b = q("[data-scale]"))) { set("scale", b.dataset.scale); return paint(); }
        if (q("[data-clearcss]")) { ls.del("pb.css"); apply(); return paint(); }
        if (q("[data-export]")) return exportLook();
        if (q("[data-import]")) return importLook();
        if (q("[data-removeusers]")) { ls.del("pb.skins"); if (!all().some(t => t.id === state().skin)) set("skin", "playbox"); return paint(); }
      });
      dlg.addEventListener("input", e => {
        if (e.target.id === "pbs-css") { ls.set("pb.css", e.target.value.slice(0, 20000)); apply(); const n = dlg.querySelector("#pbs-cssn"); if (n) n.textContent = `${e.target.value.length} of 20000 characters`; }
        if (e.target.id === "pbs-color") { set("accent", "custom:" + e.target.value); }
      });
      dlg.addEventListener("change", e => { if (e.target.id === "pbs-font") set("font", e.target.value); if (e.target.id === "pbs-mono") set("mono", e.target.value); });
    }
    if (which && SECTIONS.some(x => x[0] === which)) sec = which;
    paint();
    if (!dlg.open) dlg.showModal();
  }
  function say(t) { const m = dlg && dlg.querySelector("#pbs-msg"); if (m) m.textContent = t; }
  function exportLook() {
    const s = state(), t = find(s.skin);
    const blob = new Blob([JSON.stringify({id: t.id, label: t.label, desc: t.desc || "", dark: t.dark, light: t.light, accent: s.accent, font: s.font, mono: s.mono, scale: s.scale}, null, 2)], {type: "application/json"});
    const a = document.createElement("a"); a.href = URL.createObjectURL(blob); a.download = `playbox-theme-${t.id}.json`; a.click(); setTimeout(() => URL.revokeObjectURL(a.href), 2000);
  }
  function importLook() {
    const f = document.createElement("input"); f.type = "file"; f.accept = "application/json,.json";
    f.onchange = async () => {
      try {
        const j = JSON.parse(await f.files[0].text());
        const t = {id: String(j.id || "").replace(/[^a-z0-9-]/gi, "").slice(0, 40).toLowerCase(), label: String(j.label || j.id || "").slice(0, 40), desc: String(j.desc || "").slice(0, 120), dark: j.dark, light: j.light};
        if (!t.id || !valid(t)) return say("That file is not a Playbox theme: every colour must be #rrggbb and both dark and light sides are needed.");
        if (THEMES.some(x => x.id === t.id)) t.id = "my-" + t.id;
        const list = users().filter(x => x.id !== t.id).concat(t).slice(-12);
        ls.set("pb.skins", JSON.stringify(list)); set("skin", t.id); paint(); say(`Imported "${t.label}".`);
      } catch { say("Could not read that file."); }
    };
    f.click();
  }

  window.PBTheme = {apply, state, set, open, themes: () => all().map(t => ({id: t.id, label: t.label})), mode, find};
})();
