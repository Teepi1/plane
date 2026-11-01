// Plane's service worker: shows push notifications, and keeps the app's own files so it
// still opens (and says so) when the laptop can't be reached.
const CACHE = "plane-v3";
const SHELL = ["./", "index.html", "app.css", "app.js", "manifest.webmanifest", "icons/icon-192.png", "icons/apple-touch-icon.png"];

self.addEventListener("install", e => {
  e.waitUntil(caches.open(CACHE).then(c => c.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", e => {
  e.waitUntil(caches.keys().then(keys => Promise.all(keys.filter(k => k !== CACHE).map(k => caches.delete(k))))
    .then(() => self.clients.claim()));
});

self.addEventListener("fetch", e => {
  const url = new URL(e.request.url);
  if (e.request.method !== "GET" || url.origin !== location.origin || url.pathname.startsWith("/api/")) return;
  // network first, so updates show up at once; the cached copy only when the laptop is away
  e.respondWith(fetch(e.request).then(res => {
    if (res.ok) { const copy = res.clone(); caches.open(CACHE).then(c => c.put(e.request, copy)); }
    return res;
  }).catch(() => caches.match(e.request).then(hit => hit || caches.match("index.html"))));
});

self.addEventListener("push", e => {
  let data = {};
  try { data = e.data ? e.data.json() : {}; } catch { data = { title: "Plane", body: e.data ? e.data.text() : "" }; }
  const title = data.title || "Plane";
  // Tell the laptop it arrived (and whether it showed), so a missing notification can be traced.
  const report = (shown, error) => data.id ? fetch("api/push/seen", {
    method: "POST", headers: { "Content-Type": "application/json", "X-Plane": "1" },
    body: JSON.stringify({ id: data.id, shown, error: error ? String(error) : "" }),
  }).catch(() => {}) : Promise.resolve();
  e.waitUntil(self.registration.showNotification(title, {
    body: data.body || "", tag: data.tag || undefined, renotify: true,
    icon: "icons/icon-192.png", badge: "icons/icon-192.png", data: { url: data.url || "#" },
  }).then(() => report(true), err =>
    // the plainest notification, if the full one was refused
    self.registration.showNotification(title, { body: data.body || "" })
      .then(() => report(true, err), err2 => report(false, err2))));
});

self.addEventListener("notificationclick", e => {
  e.notification.close();
  const target = new URL(e.notification.data && e.notification.data.url || "#", self.registration.scope).href;
  e.waitUntil(self.clients.matchAll({ type: "window", includeUncontrolled: true }).then(list => {
    for (const c of list) {
      if ("focus" in c) { c.postMessage({ open: target }); return c.focus(); }
    }
    return self.clients.openWindow(target);
  }));
});
