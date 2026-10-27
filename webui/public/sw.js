/* EXMACHINA Service Worker：离线壳缓存 + 前端可更新性。
 *
 * 缓存策略（2026-10 修订）：此前全量 cache-first 命中即回，且 sw.js 自身也被缓存，
 * 导致服务端升级（index.html 引用新的 hash 资产）后，浏览器仍从缓存取旧 index.html
 * 与旧资产——「一键更新」后界面长期停留在旧版，需手工清缓存才生效。
 *
 * 现改为：
 *   - 导航请求 / index.html / 其它文档 → network-first（在线永远拿新壳，离线回落缓存）
 *   - /assets/ 下带 hash 的不可变产物 → cache-first（命中即回，省流量）
 *   - sw.js 自身 → 不走缓存（否则 SW 永远无法升级）
 */
const CACHE = "exm-shell-v3";
const SHELL = ["/", "/index.html", "/manifest.webmanifest", "/icon.svg"];

/** 带内容 hash 的构建产物：内容变则文件名变，可长期缓存 */
function isImmutableAsset(pathname) {
  return (
    pathname.startsWith("/assets/") ||
    pathname === "/icon.svg" ||
    /\/icon-\d+\.(png|svg|webp)$/.test(pathname) ||
    pathname === "/manifest.webmanifest"
  );
}

self.addEventListener("install", (e) => {
  e.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (e) => {
  const url = new URL(e.request.url);
  if (e.request.method !== "GET") return;
  // API / WS：永远走网络（实时性与鉴权）
  if (url.pathname.startsWith("/api") || url.pathname === "/ws") return;
  // sw.js 自身：永远走网络，否则本 SW 无法被替换
  if (url.pathname === "/sw.js") return;
  // 跨域资源（远程网关 / CDN）不缓存
  if (url.origin !== location.origin) return;

  const put = (resp) => {
    if (resp && resp.ok) {
      const copy = resp.clone();
      caches.open(CACHE).then((c) => c.put(e.request, copy));
    }
    return resp;
  };

  if (isImmutableAsset(url.pathname)) {
    // 不可变产物：cache-first
    e.respondWith(caches.match(e.request).then((hit) => hit || fetch(e.request).then(put)));
    return;
  }

  // 文档与导航：network-first，离线时回落到缓存的壳
  e.respondWith(
    fetch(e.request)
      .then(put)
      .catch(() => caches.match(e.request).then((hit) => hit || caches.match("/index.html"))),
  );
});