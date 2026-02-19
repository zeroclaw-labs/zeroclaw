// ClawSuite Service Worker — DISABLED
// Unregisters itself and clears all caches to prevent stale asset issues
// after Docker image updates or reverse proxy deployments.

self.addEventListener('install', () => {
  self.skipWaiting()
})

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches.keys().then((names) =>
      Promise.all(names.map((name) => caches.delete(name)))
    ).then(() => self.clients.claim())
    .then(() => {
      // Tell all open tabs to reload so they get fresh assets
      self.clients.matchAll({ type: 'window' }).then((clients) => {
        clients.forEach((client) => client.navigate(client.url))
      })
    })
  )
})

// Don't intercept any fetches — let the browser/server handle everything
