// Runs static/sw.js against mocked browser globals: install/cache, offline fallback, push -> attention fetch -> notifications, click.
const fs = require("fs"), vm = require("vm"), assert = require("assert");
const L = {}, cache = {}, shown = [], opened = [];
const mkCache = n => (cache[n] ||= {m: new Map(), async addAll(u) { u.forEach(x => this.m.set(x, "shell:" + x)); }, async put(k, v) { this.m.set(typeof k === "string" ? k : k.url, v); }, async match(k) { return this.m.get(typeof k === "string" ? k : k.url); }});
let online = true;
const ctx = {
  location: {origin: "http://hub", href: "http://hub/"}, console, URL, Response: class { constructor(b) { this.body = b; this.ok = true; } clone() { return this; } async json() { return JSON.parse(this.body); } static error() { return {error: true}; } },
  caches: {open: async n => mkCache(n), keys: async () => Object.keys(cache), delete: async n => delete cache[n], match: async k => { for (const c of Object.values(cache)) { const v = await c.match(k); if (v) return v; } } },
  fetch: async u => { if (!online) throw new Error("offline"); const p = typeof u === "string" ? u : new URL(u.url).pathname; if (p === "/api/attention") return new ctx.Response(JSON.stringify({items: [{id: "hold:r1:s", room: "r1", title: "Witness paused a write in Lab", body: "tool write_file", urgent: true}, {id: "unread:r2", room: "r2", title: "3 new in Acme", urgent: false}]})); if (p === "/api/rooms") return new ctx.Response('{"rooms":[1]}'); return new ctx.Response("x"); },
  self: null, clients: {claim: async () => {}, matchAll: async () => [], openWindow: async u => opened.push(u)}, skipWaiting: async () => {},
  registration: {showNotification: async (t, o) => shown.push({t, o})}, navigator: {setAppBadge: async n => (ctx.badge = n)},
};
ctx.self = Object.assign(ctx, {addEventListener: (n, f) => (L[n] = f)});
vm.runInContext = vm.runInContext; vm.createContext(ctx);
vm.runInContext(fs.readFileSync(__dirname + "/../static/sw.js", "utf8"), ctx);
const ev = (extra = {}) => { const p = []; return {waitUntil: x => p.push(x), done: () => Promise.all(p), ...extra}; };
(async () => {
  let e = ev(); L.install(e); await e.done(); assert(cache["pb-v2"].m.has("/"), "shell cached");
  let res; e = ev({request: {method: "GET", url: "http://hub/api/rooms"}, respondWith: p => (res = p)}); L.fetch(e); await res; await new Promise(r => setTimeout(r, 10));
  online = false; e = ev({request: {method: "GET", url: "http://hub/api/rooms"}, respondWith: p => (res = p)}); L.fetch(e);
  assert.deepStrictEqual(await (await res).json(), {rooms: [1]}); console.log("PASS offline: last known rooms served from cache");
  e = ev({request: {method: "GET", url: "http://hub/", mode: "navigate"}, respondWith: p => (res = p)}); L.fetch(e);
  assert.strictEqual(await res, "shell:/"); console.log("PASS offline: app shell served on navigation");
  e = ev({request: {method: "GET", url: "http://hub/r/r1/api/state"}, respondWith: () => assert.fail("room data must never be cached")}); L.fetch(e); console.log("PASS room data is never intercepted/cached");
  online = true; e = ev(); L.push(e); await e.done();
  assert.strictEqual(shown.length, 2); assert.strictEqual(shown[0].o.requireInteraction, true); assert.strictEqual(ctx.badge, 2);
  console.log("PASS push (no payload): fetched attention, urgent first + sticky, badge=2");
  const n = {close() {}, data: shown[0].o.data}; e = ev({notification: n}); L.notificationclick(e); await e.done(); assert.deepStrictEqual(opened, ["/r/r1/"]);
  console.log("PASS notification click opens the room");
})().catch(e => { console.error("FAIL", e.message); process.exit(1); });
