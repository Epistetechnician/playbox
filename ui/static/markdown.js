/* PBMd: a small, strict markdown renderer for text that other members wrote (notes, results, streamed replies).
   Everything is escaped first; only a fixed set of constructs becomes markup: headings, paragraphs, lists, block quotes, tables, rules,
   fenced and inline code, bold, italic, links (http/https only, rel=noopener, shown only once DOMPurify is loaded) and maths ($..$ and $$..$$).
   Images are never fetched from other sites. Heavy parts load on demand from the hub's own /vendor/: KaTeX for maths, Shiki for code colours,
   DOMPurify as the final sanitiser. Until a library has loaded the text still reads fine (maths as source, code uncoloured, links as text),
   and `PBMd.upgrade(el)` finishes the job in place. `streaming: true` renders text that is still arriving: an open code fence, open bold or an
   open $$ block stays as plain text instead of swallowing the rest of the message. */
(function () {
  const esc = s => String(s == null ? "" : s).replace(/[&<>"']/g, c => ({"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"}[c]));
  const LIBS = {
    purify: ["/vendor/purify.min.js", () => window.DOMPurify],
    katex: ["/vendor/katex/katex.min.js", () => window.katex, "/vendor/katex/katex.min.css"],
    shiki: ["/vendor/shiki.bundle.js", () => window.PBShiki],
  };
  const pending = {};
  function load(name) {
    const [src, ok, css] = LIBS[name];
    if (ok()) return Promise.resolve(true);
    if (pending[name]) return pending[name];
    if (css && !document.querySelector(`link[href="${css}"]`)) { const l = document.createElement("link"); l.rel = "stylesheet"; l.href = css; document.head.appendChild(l); }
    return pending[name] = new Promise(res => { const s = document.createElement("script"); s.src = src; s.onload = () => res(!!ok()); s.onerror = () => res(false); document.head.appendChild(s); });
  }

  /* Output of KaTeX and Shiki carries inline styles and MathML that a sanitiser would strip. It is built by those libraries from text that was
     escaped or parsed as plain source (KaTeX with trust off), so it is swapped in after sanitising, behind a marker. */
  let trusted = null;
  const T = h => trusted ? "\u0001" + (trusted.push(h) - 1) + "\u0001" : h;
  function math(tex, display) {
    const k = window.katex;
    if (!k) return `<span class="pbm-math" data-tex="${esc(tex)}" data-display="${display ? 1 : 0}">${esc(display ? "$$" + tex + "$$" : "$" + tex + "$")}</span>`;
    try { return T(k.renderToString(tex, {displayMode: display, throwOnError: false, trust: false, strict: "ignore", maxExpand: 200})); } catch { return esc(tex); }
  }
  function code(src, lang, streamingOpen) {
    const colored = window.PBShiki && lang ? window.PBShiki.html(src, lang) : null;
    if (colored) return T(`<div class="pbm-code" data-lang="${esc(lang)}">${colored}</div>`);
    return `<pre class="pbm-code" data-lang="${esc(lang || "")}" data-src="${lang ? 1 : 0}"><code>${esc(src)}</code></pre>`;
  }
  function inline(t, o) {
    const hold = [];
    const keep = h => `\u0000${hold.push(h) - 1}\u0000`;
    t = t.replace(/`([^`\n]+)`/g, (_, c) => keep(`<code>${esc(c)}</code>`));
    if (o.math !== false) t = t.replace(/\$([^\s$](?:[^$\n]*[^\s$])?)\$(?![0-9])/g, (_, m) => keep(math(m, false)));
    let h = esc(t);
    h = h.replace(/\*\*([^*\n]+)\*\*/g, "<b>$1</b>").replace(/(^|[^*\w])\*([^*\n]+)\*(?!\w)/g, "$1<i>$2</i>").replace(/(^|[^_\w])_([^_\n]+)_(?!\w)/g, "$1<i>$2</i>");
    h = h.replace(/\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)/g, (_, txt, url) => window.DOMPurify ? `<a href="${url}" rel="noopener noreferrer nofollow" target="_blank">${txt}</a>` : txt);
    return h.replace(/\u0000(\d+)\u0000/g, (_, i) => hold[+i]);
  }

  function render(src, o) {
    o = o || {};
    trusted = [];
    const lines = String(src == null ? "" : src).replace(/\r\n?/g, "\n").split("\n"), out = [];
    let para = [], list = null, quote = [], i = 0;
    const flush = () => {
      if (para.length) { out.push(`<p>${inline(para.join(" "), o)}</p>`); para = []; }
      if (list) { out.push(`<${list.t}>${list.items.map(x => `<li>${inline(x, o)}</li>`).join("")}</${list.t}>`); list = null; }
      if (quote.length) { out.push(`<blockquote>${inline(quote.join(" "), o)}</blockquote>`); quote = []; }
    };
    while (i < lines.length) {
      const ln = lines[i];
      let m;
      if ((m = ln.match(/^\s*```\s*([\w+-]*)\s*$/))) {                                  // fenced code
        flush();
        const body = []; i++;
        while (i < lines.length && !/^\s*```\s*$/.test(lines[i])) body.push(lines[i++]);
        const closed = i < lines.length;
        if (closed) i++;
        out.push(code(body.join("\n"), (m[1] || "").toLowerCase(), !closed && o.streaming));      // an open fence while streaming is still a code block, as far as it has got
        continue;
      }
      if (o.math !== false && /^\s*\$\$\s*$/.test(ln)) {                                // display maths over several lines
        let j = i + 1; const body = [];
        while (j < lines.length && !/^\s*\$\$\s*$/.test(lines[j])) body.push(lines[j++]);
        if (j < lines.length) { flush(); out.push(`<div class="pbm-display">${math(body.join("\n"), true)}</div>`); i = j + 1; continue; }
        if (o.streaming) { para.push(esc(ln)); i++; continue; }                          // still arriving: leave as text
      }
      if ((m = ln.match(/^\s*\$\$(.+)\$\$\s*$/)) && o.math !== false) { flush(); out.push(`<div class="pbm-display">${math(m[1], true)}</div>`); i++; continue; }
      if ((m = ln.match(/^(#{1,4})\s+(.*)$/))) { flush(); const n = m[1].length + 1; out.push(`<h${n}>${inline(m[2], o)}</h${n}>`); i++; continue; }
      if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(ln)) { flush(); out.push("<hr>"); i++; continue; }
      if ((m = ln.match(/^\s*(?:[-*+]|(\d+)[.)])\s+(.*)$/))) { const t = m[1] ? "ol" : "ul"; if (!list || list.t !== t) { flush(); list = {t, items: []}; } list.items.push(m[2]); i++; continue; }
      if ((m = ln.match(/^>\s?(.*)$/))) { if (!quote.length) flush(); quote.push(m[1]); i++; continue; }
      if (/^\s*\|.*\|\s*$/.test(ln) && i + 1 < lines.length && /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(lines[i + 1])) {      // table
        flush();
        const cells = s => s.trim().replace(/^\||\|$/g, "").split("|").map(c => c.trim());
        const head = cells(ln); i += 2; const rows = [];
        while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) rows.push(cells(lines[i++]));
        out.push(`<div class="pbm-table"><table><thead><tr>${head.map(c => `<th>${inline(c, o)}</th>`).join("")}</tr></thead><tbody>${rows.map(r => `<tr>${r.map(c => `<td>${inline(c, o)}</td>`).join("")}</tr>`).join("")}</tbody></table></div>`);
        continue;
      }
      if (!ln.trim()) { flush(); i++; continue; }
      if (list) flush();
      para.push(ln.trim()); i++;
    }
    flush();
    let html = out.join("");
    const kept = trusted; trusted = null;
    if (window.DOMPurify) html = window.DOMPurify.sanitize(html, {ADD_ATTR: ["target"], FORBID_ATTR: ["style", "onerror", "onclick"], FORBID_TAGS: ["img", "iframe", "script", "style", "form", "input"]});
    return html.replace(/\u0001(\d+)\u0001/g, (_, i) => kept[+i] || "");
  }

  /** Finish an element rendered before the libraries had loaded: maths, code colours. Safe to call again. */
  function upgrade(el) {
    if (!el) return;
    if (window.katex) el.querySelectorAll(".pbm-math").forEach(n => { n.outerHTML = math(n.dataset.tex, n.dataset.display === "1"); });
    if (window.PBShiki) el.querySelectorAll("pre.pbm-code[data-src='1']").forEach(n => { const h = window.PBShiki.html(n.textContent, n.dataset.lang); if (h) n.outerHTML = `<div class="pbm-code" data-lang="${esc(n.dataset.lang)}">${h}</div>`; });
  }
  /** What this text needs, so a page loads only that: returns a promise that resolves once the libraries are in. */
  function need(src) {
    src = String(src || "");
    const w = [load("purify")];
    if (/\$[^\s$]|\$\$/.test(src)) w.push(load("katex"));
    if (/```\s*[\w+-]+/.test(src)) w.push(load("shiki"));
    return Promise.all(w);
  }
  /** Render into `el`, then load what is missing and render again once. */
  function into(el, src, o) {
    el.innerHTML = render(src, o);
    need(src).then(() => { if (el.isConnected) { el.innerHTML = render(src, o); upgrade(el); } });
  }
  /** Highlight a diff or any code block of known language, plain text when the language is not loaded. Returns a promise of HTML. */
  function highlight(src, lang) {
    return load("shiki").then(ok => (ok && window.PBShiki.html(src, lang)) || `<pre class="pbm-code"><code>${esc(src)}</code></pre>`);
  }
  window.PBMd = {render, upgrade, need, into, highlight, esc};
})();
