// ohmforge service worker (CP-WEB-M10.2) — makes the fully client-side
// simulator installable and genuinely offline-capable. The wasm bundle is
// self-contained (no CDNs, no backend), so once the app shell + `pkg/` are
// cached the page boots and simulates with the network fully off.
//
// Strategy: precache the shell on install, then serve same-origin GETs
// cache-first (fall back to the network and backfill the cache). A navigation
// that misses the cache offline falls back to the cached `index.html`.
//
// Cache discipline: `CACHE` is versioned. Bump the version when the wasm bundle
// changes so a new build is never served stale; `activate` deletes every older
// ohmforge cache. This file is hand-rolled (house style) and passes `node --check`.

"use strict";

const CACHE = "ohmforge-v1";

// The app shell + engine. These are the exact paths the page and its dynamic
// `import("./pkg/ohmforge_web.js")` fetch, so caching them makes a cold offline
// reload boot identically to an online one.
const SHELL = [
  "./",
  "./index.html",
  "./style.css",
  "./manifest.webmanifest",
  "./ohmforge-mark.svg",
  "./ohmforge-logo.svg",
  "./pkg/ohmforge_web.js",
  "./pkg/ohmforge_web_bg.wasm",
];

// Precache the shell. `pkg/` may not exist yet on a freshly served tree (the
// bundle is built separately), so each asset is cached best-effort — a missing
// one must not fail the whole install and leave the worker uninstalled.
self.addEventListener("install", (event) => {
  event.waitUntil(
    (async () => {
      const cache = await caches.open(CACHE);
      await Promise.all(
        SHELL.map((url) =>
          cache.add(new Request(url, { cache: "reload" })).catch(() => {})
        )
      );
      await self.skipWaiting();
    })()
  );
});

// Drop stale ohmforge caches from older builds, then take control immediately.
self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      const names = await caches.keys();
      await Promise.all(
        names
          .filter((n) => n.startsWith("ohmforge-") && n !== CACHE)
          .map((n) => caches.delete(n))
      );
      await self.clients.claim();
    })()
  );
});

// Cache-first for same-origin GETs; network otherwise. A fetched response is
// backfilled into the cache so a later offline load has it.
self.addEventListener("fetch", (event) => {
  const req = event.request;
  if (req.method !== "GET") return;

  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;

  event.respondWith(
    (async () => {
      const cached = await caches.match(req);
      if (cached) return cached;
      try {
        const res = await fetch(req);
        if (res && res.ok && res.type === "basic") {
          const cache = await caches.open(CACHE);
          cache.put(req, res.clone());
        }
        return res;
      } catch (err) {
        // Offline and uncached: for a navigation, fall back to the app shell so
        // the SPA still boots; otherwise re-raise.
        if (req.mode === "navigate") {
          const shell = await caches.match("./index.html");
          if (shell) return shell;
        }
        throw err;
      }
    })()
  );
});
