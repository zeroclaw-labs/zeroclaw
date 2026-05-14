/*
 * ZeroClaw dashboard service worker (M3).
 *
 * Strategy (plan §6 "Service worker cache staleness" row):
 *   - Network-first for HTML and the bootstrap config endpoint so a
 *     reload always picks up a fresh deploy without a waiting-worker
 *     dance.
 *   - Cache-first for hashed asset URLs (the Vite build emits JS/CSS
 *     with content hashes in the filename, so any content change
 *     produces a new URL — cache entries are effectively immutable).
 *
 * Version is the git SHA pinned at build time by whatever ships the
 * dist artifact. Pre-deploy, it stays as the placeholder below; an
 * unset marker just means the cache name stays constant across
 * iterations and relies on Vite's hashed filenames for busting.
 */

/* eslint-disable no-restricted-globals */

const VERSION = "dev";
const CACHE_NAME = `zeroclaw-dashboard-${VERSION}`;

self.addEventListener("install", (event) => {
  event.waitUntil(self.skipWaiting());
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      const keys = await caches.keys();
      await Promise.all(
        keys
          .filter((k) => k.startsWith("zeroclaw-dashboard-") && k !== CACHE_NAME)
          .map((k) => caches.delete(k)),
      );
      await self.clients.claim();
    })(),
  );
});

self.addEventListener("fetch", (event) => {
  const req = event.request;
  if (req.method !== "GET") return;

  const url = new URL(req.url);

  // Network-first: HTML + bootstrap config.
  if (
    req.destination === "document" ||
    url.pathname === "/api/control-ui/config" ||
    url.pathname.endsWith("/api/control-ui/config")
  ) {
    event.respondWith(
      (async () => {
        try {
          const res = await fetch(req);
          const cache = await caches.open(CACHE_NAME);
          cache.put(req, res.clone());
          return res;
        } catch (_err) {
          const cached = await caches.match(req);
          if (cached) return cached;
          throw _err;
        }
      })(),
    );
    return;
  }

  // Cache-first for hashed assets under /assets/.
  if (url.pathname.startsWith("/assets/") || url.pathname.startsWith("/dashboard/assets/")) {
    event.respondWith(
      (async () => {
        const cached = await caches.match(req);
        if (cached) return cached;
        const res = await fetch(req);
        const cache = await caches.open(CACHE_NAME);
        cache.put(req, res.clone());
        return res;
      })(),
    );
  }
});
