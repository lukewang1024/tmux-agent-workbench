'use strict';

const cacheName = 'workbench-shell-WORKBENCH_CACHE_VERSION';
const files = ['/', '/app.js', '/style.css', '/manifest.webmanifest', '/icon-192.png', '/icon-512.png',
  '/vendor/passkeys.js', '/vendor/xterm.js', '/vendor/xterm.css', '/vendor/fit.js'];
const shell = new Set(files);

self.addEventListener('install', (event) => {
  event.waitUntil(caches.open(cacheName).then((cache) => cache.addAll(files)));
});

self.addEventListener('activate', (event) => {
  event.waitUntil(caches.keys().then((keys) => Promise.all(keys
    .filter((key) => key.startsWith('workbench-shell-') && key !== cacheName)
    .map((key) => caches.delete(key)))).then(() => self.clients.claim()));
});

self.addEventListener('fetch', (event) => {
  const { request } = event;
  const url = new URL(request.url);
  if (request.method !== 'GET' || url.origin !== location.origin || url.search || !shell.has(url.pathname)) return;
  event.respondWith(caches.open(cacheName).then(async (cache) => {
    const cached = await cache.match(url.pathname);
    return cached || fetch(request);
  }));
});
