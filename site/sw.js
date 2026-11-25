// RePal PWA Offline Service Worker (Auto-Update Ready)
// - Engine shell (HTML, JS, CSS, pal.wasm): network-first when online so new versions
//   apply automatically, falling back to local cache seamlessly when offline.
// - Heavy game assets (/assets/, /replays/): cache-first for instant offline playback
//   on Chromebooks and mobile/desktop PWAs.

const SHELL_CACHE = "repal-shell-dev";
const ASSET_CACHE = "repal-assets-v3";
const KEEP_CACHES = new Set([SHELL_CACHE, ASSET_CACHE]);
const CORE_SHELL = [
  "./",
  "./index.html",
  "./manifest.json",
  "./icons/icon.svg",
  "./css/pal-ui.css",
  "./wasm/pal.wasm",
  "./js/main.js",
  "./js/audio/pal-synth-worklet.js",
];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(SHELL_CACHE)
      .then((cache) => cache.addAll(CORE_SHELL).catch(() => {}))
      .then(() => self.skipWaiting())
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((k) => !KEEP_CACHES.has(k)).map((k) => caches.delete(k)))
      )
      .then(() => self.clients.claim())
  );
});

self.addEventListener("message", (event) => {
  if (event.data && event.data.type === "SKIP_WAITING") {
    self.skipWaiting();
  }
});

// Lazily populated per-cache `origin+pathname -> href` index so stale `?v=` eviction stays O(1)
// per put instead of scanning `cache.keys()` O(n²) across thousands of assets (#211, #233).
const variantIndexes = new Map();

async function getVariantIndex(cacheName, cache) {
  let idx = variantIndexes.get(cacheName);
  if (idx) return idx;
  idx = new Map();
  const keys = await cache.keys();
  for (const k of keys) {
    const ku = new URL(k.url);
    if (ku.searchParams.has("v")) {
      idx.set(ku.origin + ku.pathname, ku.href);
    }
  }
  variantIndexes.set(cacheName, idx);
  return idx;
}

// Evict stale `?v=` variants of the same pathname so asset updates never leak storage (#211, #233).
async function putAndEvictStaleVariants(cacheName, req, res) {
  const cache = await caches.open(cacheName);
  const reqUrl = new URL(req.url);
  if (reqUrl.searchParams.has("v")) {
    const idx = await getVariantIndex(cacheName, cache);
    const key = reqUrl.origin + reqUrl.pathname;
    const prevHref = idx.get(key);
    if (prevHref && prevHref !== reqUrl.href) {
      await cache.delete(prevHref);
    }
    idx.set(key, reqUrl.href);
  }
  await cache.put(req, res);
}

self.addEventListener("fetch", (event) => {
  const req = event.request;
  if (req.method !== "GET") return;
  // Never intercept Range requests (audio/media seeking returns 206 Partial Content, #211).
  if (req.headers.has("range")) return;
  const url = new URL(req.url);
  if (url.protocol !== "http:" && url.protocol !== "https:") return;

  // Bypass `.rpk` asset packs & `pack-manifest.json` so `PackManager` owns OPFS/IDB storage
  // directly without double-caching packs in the Service Worker Cache API (#237).
  if (url.pathname.endsWith(".rpk") || url.pathname.endsWith("/pack-manifest.json")) return;

  const isCodeOrShell =
    req.mode === "navigate" ||
    url.pathname.endsWith(".html") ||
    url.pathname.endsWith(".js") ||
    url.pathname.endsWith(".css") ||
    url.pathname.endsWith(".wasm");

  // Network-first for engine code & shell so PWA auto-updates whenever online,
  // with instant offline cache fallback when disconnected.
  // Never cache non-same-origin responses (prevents poisoning from custom `?assets=` origins, #256).
  const isSameOrigin = url.origin === self.location.origin;
  if (isCodeOrShell) {
    event.respondWith(
      fetch(req)
        .then((res) => {
          if (isSameOrigin && res.status === 200 && res.type === "basic") {
            const copy = res.clone();
            event.waitUntil(putAndEvictStaleVariants(SHELL_CACHE, req, copy));
          }
          return res;
        })
        .catch(() =>
          caches.match(req).then((m) => {
            if (m) return m;
            // Only fall back to index.html for document navigations, never for JS/CSS/WASM (#211).
            return req.mode === "navigate" ? caches.match("./index.html") : Response.error();
          })
        )
    );
    return;
  }

  // Static game assets (/assets/, /replays/): cache-first for same-origin only (status 200).
  if (!isSameOrigin) return;
  event.respondWith(
    caches.match(req).then((cached) => {
      if (cached) return cached;
      return fetch(req).then((res) => {
        if (res.status === 200 && res.type === "basic") {
          const copy = res.clone();
          event.waitUntil(putAndEvictStaleVariants(ASSET_CACHE, req, copy));
        }
        return res;
      });
    })
  );
});
