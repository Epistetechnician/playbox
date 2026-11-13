/* Playbox service worker: offline shell, last-known rooms, and payload-free push.
   A push carries no data. On wake this worker asks the hub (over the signed-in session) what needs attention. */
const VER = "pb-v3";
const SHELL = ["/", "/auth.js", "/space.js", "/manifest.webmanifest", "/icon.svg", "/icon-192.png", "/icon-512.png"];

self.addEventListener("install", e => { e.waitUntil(caches.open(VER).then(c => c.addAll(SHELL)).then(() => self.skipWaiting())); });
self.addEventListener("activate", e => {
  e.waitUntil(caches.keys().then(ks => Promise.all(ks.filter(k => k !== VER && k !== "pb-data").map(k => caches.delete(k)))).then(() => self.clients.claim()));
});

self.addEventListener("fetch", e => {
  const req = e.request, url = new URL(req.url);
  if (req.method !== "GET" || url.origin !== location.origin) return;
  if (url.pathname === "/api/rooms") {                      // network first, remember the last picture of your rooms
    e.respondWith(fetch(req).then(r => { if (r.ok) { const c = r.clone(); caches.open("pb-data").then(d => d.put("/api/rooms", c)); } return r; })
      .catch(() => caches.open("pb-data").then(d => d.match("/api/rooms")).then(r => r || Response.error())));
    return;
  }
  if (url.pathname.startsWith("/api/") || url.pathname.startsWith("/r/")) return;
  if (req.mode === "navigate") {                           // the shell holds no data, so it is safe to serve offline
    e.respondWith(fetch(req).catch(() => caches.match("/").then(r => r || new Response("Offline. Reconnect and reload.", {status: 503, headers: {"Content-Type": "text/plain"}}))));
    return;
  }
  e.respondWith(caches.match(req).then(hit => {
    const net = fetch(req).then(r => { if (r.ok) caches.open(VER).then(c => c.put(req, r.clone())); return r; }).catch(() => hit || Response.error());
    return hit || net;
  }));
});

self.addEventListener("push", e => {
  e.waitUntil((async () => {
    let items = [];
    try { const r = await fetch("/api/attention", {credentials: "same-origin"}); if (r.ok) items = (await r.json()).items || []; } catch {}
    items.sort((a, b) => (b.urgent ? 1 : 0) - (a.urgent ? 1 : 0));
    if (!items.length) return self.registration.showNotification("Playbox", {tag: "pb-generic", body: "Something in your rooms needs a look.", icon: "/icon-192.png", data: {url: "/?view=attention"}});
    await Promise.all(items.slice(0, 3).map(i => self.registration.showNotification(i.title, {
      tag: "pb-" + i.id, body: i.body || "", icon: "/icon-192.png", badge: "/icon-192.png", requireInteraction: !!i.urgent, data: {url: `/r/${i.room}/`}})));
    if (self.navigator.setAppBadge) self.navigator.setAppBadge(items.length).catch(() => {});
  })());
});

self.addEventListener("notificationclick", e => {
  e.notification.close();
  const target = (e.notification.data && e.notification.data.url) || "/";
  e.waitUntil(self.clients.matchAll({type: "window", includeUncontrolled: true}).then(cs => {
    for (const c of cs) if ("focus" in c) { c.navigate(target).catch(() => {}); return c.focus(); }
    return self.clients.openWindow(target);
  }));
});
