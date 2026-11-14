// The "your Cloudflare sign-in expired" notice in static/auth.js, run against a fake page.
const fs = require("fs"), vm = require("vm"), path = require("path");
let mode = "redirect", added = [], handlers = {};
const mk = () => ({style: {}, children: [], setAttribute() {}, append(...c) { this.children.push(...c); }, addEventListener() {}, remove() { added = added.filter(x => x !== this); }});
const body = mk(); body.append = n => { added.push(n); };
const ctx = {window: {isSecureContext: true, PublicKeyCredential: 1, addEventListener: (e, f) => handlers[e] = f},
  document: {hidden: false, body, createElement: mk, addEventListener: (e, f) => handlers[e] = f}, location: {protocol: "https:", reload() {}},
  setInterval() {}, btoa: () => "", atob: () => "", navigator: {},
  fetch: async () => { if (mode === "offline") throw new TypeError("offline"); return mode === "redirect" ? {type: "opaqueredirect", status: 0} : {type: "basic", status: 200}; }};
vm.createContext(ctx);
vm.runInContext(fs.readFileSync(path.join(__dirname, "..", "static", "auth.js"), "utf8"), ctx);
const tick = () => new Promise(r => setTimeout(r, 5));
let bad = 0;
const check = (name, ok) => { console.log((ok ? "PASS " : "FAIL ") + name); if (!ok) bad++; };
(async () => {
  await handlers.focus(); await tick();
  check("an opaque redirect (Access login) shows the notice", added.length === 1);
  await handlers.focus(); await tick();
  check("and only once", added.length === 1);
  mode = "ok"; await handlers.focus(); await tick();
  check("a healthy answer clears it", added.length === 0);
  mode = "offline"; await handlers.focus(); await tick();
  check("being offline is not an expired sign-in", added.length === 0);
  process.exit(bad ? 1 : 0);
})();
