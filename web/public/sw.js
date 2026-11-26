'use strict';

const version = 'WORKBENCH_CACHE_VERSION';
const cacheName = 'workbench-shell-' + version;
const files = ['/', '/app.js', ...WORKBENCH_BROWSER_MODULES, '/reset.css', '/style.css', '/manifest.webmanifest', '/icon-192.png', '/icon-512.png',
  '/manifest-light.webmanifest', '/manifest-dark.webmanifest',
  '/providers/chatgpt.svg', '/providers/claude.svg', '/providers/opencode.svg', '/providers/trae.png',
  '/vendor/passkeys.js', '/vendor/xterm.js', '/vendor/xterm.css', '/vendor/fit.js', '/vendor/markdown-it.js', '/vendor/readability.js'];
const shell = new Set(files);

self.addEventListener('install', (event) => {
  event.waitUntil(caches.open(cacheName).then((cache) => cache.addAll(files)).then(() => self.skipWaiting()));
});

self.addEventListener('activate', (event) => {
  event.waitUntil(caches.keys().then((keys) => Promise.all(keys
    .filter((key) => key.startsWith('workbench-shell-') && key !== cacheName)
    .map((key) => caches.delete(key)))).then(() => self.clients.claim()));
});

self.addEventListener('message', (event) => {
  if (event.data === 'workbench-version') event.source?.postMessage({ type: 'workbench-version', version });
});

self.addEventListener('fetch', (event) => {
  const { request } = event;
  const url = new URL(request.url);
  if (request.method !== 'GET' || url.origin !== location.origin || !shell.has(url.pathname)) return;
  const navigation = request.mode === 'navigate' && url.pathname === '/';
  if (!navigation && url.search && url.search !== '?v=' + version) return;
  event.respondWith(caches.open(cacheName).then(async (cache) => {
    if (navigation) {
      try {
        const response = await fetch(request, { signal: AbortSignal.timeout(2500) });
        if (response.ok) return response;
      } catch {
        // An installed app can still open its shell while the tunnel is down.
      }
    }
    const cached = await cache.match(url.pathname);
    return cached || (navigation ? Response.error() : fetch(request));
  }));
});
