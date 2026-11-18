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

  const cr = (a, b) => { const x = lum(a), y = lum(b); return (Math.max(x, y) + .05) / (Math.min(x, y) + .05); };
  /* Nudge `c` toward `toward` until it reads at `ratio` against `bg`: a theme's published colours stay as close as possible, but text stays legible. */
  const ensure = (c, bg, ratio, toward) => { let t = 0, x = c; while (cr(x, bg) < ratio && t < 1) { t += .05; x = mix(c, toward, t); } return x; };

  /* Everything the pages' CSS reads, derived from the ten colours. */
  function tokens(p0, dark, accent) {
    const body = dark ? "#FFFFFF" : "#000000", p = {...p0};
    p.ink = ensure(p.ink, p.bg, 7, body);
    p.mut = ensure(p.mut, p.panel, 4.6, body); p.mut = ensure(p.mut, p.bg, 4.5, body);
    for (const k of ["blue", "ok", "bad", "warn", "violet"]) p[k] = ensure(p[k], p.panel, 4.5, body);
    const dk = "#14180C", wh = "#FFFFFF", sig0 = accent && hex(accent) ? accent : p0.sig;
    let sig = sig0, st = 0;
    while (Math.max(cr(dk, sig), cr(wh, sig)) < 4.5 && st < 1) { st += .04; sig = mix(sig0, dark ? "#FFFFFF" : "#000000", st); }      // a button label must be readable on the primary colour
    const onSig = cr(dk, sig) >= cr(wh, sig) ? dk : wh;
    const sigInk = ensure(sig, p.panel, 4.5, body);
    return {
      "--bg": p.bg, "--bg-2": mix(p.bg, p.panel, .5), "--panel": p.panel, "--panel-2": mix(p.panel, p.ink, .06), "--field": dark ? mix(p.bg, "#000000", .2) : mix(p.panel, p.bg, .25),
      "--ink": p.ink, "--ink-2": mix(p.ink, p.bg, .24), "--mut": p.mut, "--dim": mix(p.bg, p.ink, .13), "--grid": mix(p.bg, p.ink, .06),
      "--line": mix(p.bg, p.ink, .13), "--line-2": mix(p.bg, p.ink, .23), "--edge": mix(p.bg, p.ink, .4),
      "--sig": sig, "--sig-ink": sigInk, "--on-sig": onSig,
      "--blue": p.blue, "--blue-edge": p0.blue,
      "--ok": p.ok, "--ok-led": p0.ok, "--warn": p.warn, "--bad": p.bad, "--violet": p.violet,
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
    R.dataset.style = ls.get("pb.style", "refined"); R.dataset.density = ls.get("pb.density", "comfortable");
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
    scale: parseFloat(ls.get("pb.scale", "1")) || 1, css: ls.get("pb.css", ""), style: ls.get("pb.style", "refined"), density: ls.get("pb.density", "comfortable")});
  function set(k, v) {
    if (k === "mode") {
      ls.set("pb.theme", v);
      if (v === "auto") delete R.dataset.theme; else R.dataset.theme = v;
    } else ls.set("pb." + k, String(v));
    apply();
  }

  /* ---- boot ---- */
  if (typeof navigator !== "undefined" && /PlayboxDesktop/.test(navigator.userAgent)) {
    R.dataset.desktop = /Mac/i.test(navigator.userAgent) ? "macos" : "other";
  }
  { const m = ls.get("pb.theme", "auto"); if (m === "light" || m === "dark") R.dataset.theme = m; }
  apply();
  new MutationObserver(apply).observe(R, {attributes: true, attributeFilter: ["data-theme"]});     // the page's own light/dark toggle
  matchMedia("(prefers-color-scheme:dark)").addEventListener("change", () => { if (!R.dataset.theme) apply(); });
  window.addEventListener("storage", e => { if (e.key && e.key.startsWith("pb.")) apply(); });   // another tab changed it

  /* ---- preferences (interface behaviour, kept in this browser like the look) ---- */
  const PREFS = {sidebar: "on", statusbar: "on", motion: "full", clock24: "1", landing: "last", roomview: "canvas", tz: "local"};
  const pref = k => ls.get("pb.pref." + k, PREFS[k]);
  function setPref(k, v) { ls.set("pb.pref." + k, String(v)); applyPrefs(); document.dispatchEvent(new CustomEvent("pb:pref", {detail: {k, v}})); }
  function applyPrefs() { R.dataset.sidebar = pref("sidebar"); R.dataset.statusbar = pref("statusbar"); R.dataset.motion = pref("motion"); R.dataset.roomview = pref("roomview"); }
  applyPrefs();

  /* ---- export and import of a look ---- */
  function exportLook() {
    const s = state(), t = find(s.skin);
    const blob = new Blob([JSON.stringify({id: t.id, label: t.label, desc: t.desc || "", dark: t.dark, light: t.light, accent: s.accent, font: s.font, mono: s.mono, scale: s.scale, style: s.style, density: s.density}, null, 2)], {type: "application/json"});
    const a = document.createElement("a"); a.href = URL.createObjectURL(blob); a.download = `playbox-theme-${t.id}.json`; a.click(); setTimeout(() => URL.revokeObjectURL(a.href), 2000);
  }
  /** Ask for a file, then call done(message). */
  function importLook(done) {
    const f = document.createElement("input"); f.type = "file"; f.accept = "application/json,.json";
    f.onchange = async () => {
      try {
        const j = JSON.parse(await f.files[0].text());
        const t = {id: String(j.id || "").replace(/[^a-z0-9-]/gi, "").slice(0, 40).toLowerCase(), label: String(j.label || j.id || "").slice(0, 40), desc: String(j.desc || "").slice(0, 120), dark: j.dark, light: j.light};
        if (!t.id || !valid(t)) return done("That file is not a Playbox theme: every colour must be #rrggbb and both dark and light sides are needed.");
        if (THEMES.some(x => x.id === t.id)) t.id = "my-" + t.id;
        const list = users().filter(x => x.id !== t.id).concat(t).slice(-12);
        ls.set("pb.skins", JSON.stringify(list)); set("skin", t.id); done(`Imported "${t.label}".`);
      } catch { done("Could not read that file."); }
    };
    f.click();
  }
  function removeUsers() { ls.del("pb.skins"); if (!all().some(t => t.id === state().skin)) set("skin", "playbox"); }
  function resetAll() { for (const k of ["skin", "accent", "font", "mono", "scale", "css", "style", "density", "skins"]) ls.del("pb." + k); for (const k in PREFS) ls.del("pb.pref." + k); apply(); applyPrefs(); }

  /* The settings screen lives in /settings.js and loads the first time it is asked for. */
  let loading;
  function open(path) {
    if (window.PBSettings) return window.PBSettings.open(path);
    loading = loading || new Promise((res, rej) => { const sc = document.createElement("script"); sc.src = "/settings.js"; sc.onload = res; sc.onerror = rej; document.head.appendChild(sc); });
    return loading.then(() => window.PBSettings.open(path)).catch(() => {});
  }
  document.addEventListener("keydown", e => { if ((e.metaKey || e.ctrlKey) && e.key === ",") { e.preventDefault(); open(); } });

  window.PBTheme = {apply, state, set, open, themes: () => all().map(t => ({id: t.id, label: t.label, desc: t.desc || "", dark: t.dark, light: t.light, mine: !THEMES.some(x => x.id === t.id)})), mode, find, pref, setPref, exportLook, importLook, removeUsers, resetAll, mix, ACCENTS, FONTS, MONOS, SCALES, users: () => users()};
})();
