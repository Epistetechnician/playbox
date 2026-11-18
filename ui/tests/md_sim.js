// The strict markdown renderer: escaping, streaming and the no-library fallbacks. Run: node ui/tests/md_sim.js
const fs = require("fs"), vm = require("vm"), assert = require("assert");
const ctx = {window: {}, document: {}}; ctx.window = ctx;
vm.createContext(ctx);
vm.runInContext(fs.readFileSync(__dirname + "/../static/markdown.js", "utf8"), ctx);
const {render, feedHtml} = ctx.PBMd;
const ok = (n, c) => { assert(c, n); console.log("PASS " + n); };
let h = render("# Title\nhello <script>alert(1)</script> **bold** and `a<b`\n- one\n- two\n\n1. x");
ok("headings, lists, bold, code render", h.includes("<h2>Title</h2>") && h.includes("<b>bold</b>") && h.includes("<code>a&lt;b</code>") && h.includes("<ul><li>one</li>") && h.includes("<ol>"));
ok("raw HTML is escaped", !h.includes("<script>") && h.includes("&lt;script&gt;"));
h = render("see [site](https://example.com) and [x](javascript:alert(1)) and ![i](https://evil/x.png)");
ok("without DOMPurify, links are plain text and images never become tags", !h.includes("<a ") && !h.includes("<img") && h.includes("site"));
ctx.DOMPurify = {sanitize: s => s};
h = render("[site](https://example.com) [x](javascript:alert(1)) [y](data:text/html,hi)");
ok("with it, only http(s) links become anchors, with noopener", h.includes('<a href="https://example.com" rel="noopener noreferrer nofollow" target="_blank">site</a>') && !/href="(javascript|data):/.test(h) && (h.match(/<a /g) || []).length === 1);
delete ctx.DOMPurify;
h = render("```python\nprint(1 < 2)\n```\nafter");
ok("a closed fence is a code block and the text after it survives", h.includes('<pre class="pbm-code" data-lang="python"') && h.includes("print(1 &lt; 2)") && h.includes("<p>after</p>"));
h = render("intro\n```js\nlet x = 1", {streaming: true});
ok("streaming: an open fence still renders as code, not as a swallowed page", h.includes("<p>intro</p>") && h.includes("let x = 1") && h.includes("pbm-code"));
h = render("a **half bold and $$ open", {streaming: true});
ok("streaming: open bold and an open $$ stay as plain text", !h.includes("<b>") && h.includes("half bold"));
h = render("inline $x^2$ and cost $5 and $6");
ok("maths without KaTeX shows its source; prices are not maths", h.includes('class="pbm-math"') && h.includes("$5 and $6"));
h = render("| a | b |\n|---|---|\n| 1 | 2 |");
ok("tables render", h.includes("<th>a</th>") && h.includes("<td>2</td>"));
h = render("$$\nE = mc^2\n$$");
ok("display maths block", h.includes("pbm-display"));

// Safe Feed renderer (`PBMd.feedHtml`)
h = feedHtml("hi @alice (@bob-1.2) check `a < b && @noop`:\n```python\nprint('<script>alert(1)</script>')\n```\ndone\nline2");
ok("feedHtml: fenced code blocks render pre.pbm-code", h.includes('<pre class="pbm-code" data-lang="python" data-src="1"><code>print(&#39;&lt;script&gt;alert(1)&lt;/script&gt;&#39;)</code></pre>'));
ok("feedHtml: inline backticks render <code> without @mention inside", h.includes("<code>a &lt; b &amp;&amp; @noop</code>") && !h.includes("@noop</span>"));
ok("feedHtml: @mentions render <span class=\"at\"> and line breaks become <br>", h.includes('<span class="at">@alice</span>') && h.includes('(<span class="at">@bob-1.2</span>)') && h.includes("done<br>line2"));
ctx.DOMPurify = {sanitize: s => s};
h = feedHtml("# NotHeading\n<script>alert(1)</script> <img src=x onerror=alert(1)> [click](javascript:alert(1)) [ok](https://example.com) ```\n<img onerror=alert(2)>\n```");
ok("feedHtml: never produces <a>, <img>, <script> or headings inside or outside code blocks",
   !/<(a|img|script|h[1-6])\b/i.test(h) && !/<[^>]*\bonerror\b/i.test(h) && h.includes("&lt;script&gt;") && h.includes("&lt;img src=x onerror=alert(1)&gt;"));
delete ctx.DOMPurify;
h = feedHtml("streaming\n```js\nconst y = 1 < 2;");
ok("feedHtml: unclosed streaming fence still renders safely as code", h.includes('<pre class="pbm-code" data-lang="js" data-src="1"><code>const y = 1 &lt; 2;</code></pre>'));
ctx.PBShiki = {html: (s, l) => `<pre class="shiki" data-l="${l}"><span>${ctx.PBMd.esc(s)}</span></pre>`};
h = feedHtml("```rust\nfn main() {}\n```");
ok("feedHtml: uses PBShiki when already loaded", h.includes('<div class="pbm-code" data-lang="rust"><pre class="shiki"'));
delete ctx.PBShiki;

console.log("all passed");
