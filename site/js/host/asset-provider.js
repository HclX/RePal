/**
 * Browser AssetProvider for Full-WASM Rendering (`pal.wasm`).
 *
 * Streams textures, atlas JSONs, map binaries, and TTF fonts on demand from either:
 *   - Static relative path (`./assets`, default for static web hosting / GitHub Pages)
 *   - Configurable remote server / CDN (`window.PAL_ASSET_BASE_URL` or `?assets=https://...`)
 *
 * Uses native browser `createImageBitmap` to decode WebP -> RGBA so `pal.wasm` stays ~400 KB.
 */

export const ASSET_VER = "rust_core_v11";
export const CODE_VER = "rust_core_v11";
export const ASSET_URL_STORAGE_KEY = "pal_remake_asset_base_url";

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

/** Validate an asset base URL: allow relative paths (`./...`, `/...` non-`//`), `https:`, or loopback `http:` (#256). */
export function validateAssetBaseUrl(raw) {
  if (typeof raw !== "string") return null;
  const clean = raw.trim().replace(/\/+$/, "");
  if (!clean || clean.length > 512) return null;
  if (clean.startsWith("./") || (clean.startsWith("/") && !clean.startsWith("//"))) {
    return clean;
  }
  try {
    const u = new URL(clean);
    if (u.protocol === "https:") return clean;
    if (
      u.protocol === "http:" &&
      (u.hostname === "localhost" || u.hostname === "127.0.0.1" || u.hostname === "[::1]")
    ) {
      return clean;
    }
  } catch {}
  return null;
}

export function isCustomAssetOrigin(baseUrl) {
  return Boolean(baseUrl && baseUrl !== "./assets");
}

export function resolveAssetBaseUrl() {
  if (typeof window !== "undefined") {
    const search = window.location?.search;
    if (typeof search === "string" && search.length > 0) {
      const params = new URLSearchParams(search);
      const qs = params.get("assets");
      if (qs === "reset") {
        try {
          localStorage.removeItem(ASSET_URL_STORAGE_KEY);
        } catch {}
      } else if (qs) {
        // Session-only override: NEVER persist query-string `?assets=` into localStorage (#256).
        const valid = validateAssetBaseUrl(qs);
        if (valid) return valid;
      }
    }
    if (window.PAL_ASSET_BASE_URL) {
      const valid = validateAssetBaseUrl(String(window.PAL_ASSET_BASE_URL));
      if (valid) return valid;
    }
    try {
      const saved = localStorage.getItem(ASSET_URL_STORAGE_KEY);
      const valid = validateAssetBaseUrl(saved);
      if (valid) return valid;
      if (saved) localStorage.removeItem(ASSET_URL_STORAGE_KEY);
    } catch {}
  }
  return "./assets";
}

export function withAssetVersion(url) {
  if (!ASSET_VER) return url;
  return url.includes("?") ? `${url}&v=${ASSET_VER}` : `${url}?v=${ASSET_VER}`;
}

export function withCodeVersion(url) {
  if (!CODE_VER) return url;
  return url.includes("?") ? `${url}&v=${CODE_VER}` : `${url}?v=${CODE_VER}`;
}

export class WasmAssetProvider {
  constructor(wasmExports, wasmMemory, baseUrl = resolveAssetBaseUrl(), maxConcurrent = 12) {
    this.w = wasmExports;
    this.mem = wasmMemory;
    this.baseUrl = baseUrl;
    this.packManager = null;
    this.maxConcurrent = Math.max(1, maxConcurrent);
    this.inFlight = new Map();
    this._activeCount = 0;
    this._queue = [];
    this._queuedTags = new Set();
    this._sceneGen = 0;
    this._decodeCanvas = null;
    this._decodeCtx = null;
  }

  /** Drop queued non-UI requests on scene change so stale map chunks don't starve the new scene. */
  cancelScenePending() {
    this._sceneGen++;
    const kept = [];
    this._queuedTags.clear();
    for (const item of this._queue) {
      if (item.key.includes("/ui/")) {
        kept.push(item);
        this._queuedTags.add(item.tag);
      } else {
        const keyLen = this._writeKey(item.key);
        this.w.wasm_mark_asset_failed?.(item.kind, keyLen);
      }
    }
    this._queue = kept;
  }

  hasTexture(key) {
    if (!this.w?.wasm_has_texture) return false;
    const keyLen = this._writeKey(key);
    return this.w.wasm_has_texture(keyLen) !== 0;
  }

  /**
   * Proactively fetch, decode, and upload a texture (e.g. FBP cutscene backdrop,
   * lookahead RNG cutscene frame, or battle spell VFX) so frame 0 renders with zero black flash.
   */
  prefetchTexture(key) {
    if (!key || !this.w) return Promise.resolve(false);
    if (this.hasTexture(key)) return Promise.resolve(true);
    if (typeof createImageBitmap !== "function") return Promise.resolve(false);
    const tag = `1:${key}`;
    if (this.inFlight.has(tag)) return this.inFlight.get(tag);
    const p = this._fetchAndUpload(1, key, tag);
    this.inFlight.set(tag, p);
    return p;
  }

  /**
   * Preload a scene's essential visual textures (HD map meta + chunks, GOP tiles, active MGO
   * sprites) BEFORE `HostReply::SceneLoaded` unblocks core simulation so Frame 0 never renders
   * a black void while the player is already moving/talking.
   */
  async preloadSceneVisuals(mapId, spriteIds = [], hdMode = true) {
    if (!this.w || typeof createImageBitmap !== "function") return;
    const tasks = [];
    const suffix = hdMode ? "4x" : "1x";
    if (mapId > 0) {
      const pad = String(mapId).padStart(3, "0");
      tasks.push(this._fetchAndUpload(2, `graphics/gop/gop_${pad}.json`, `2:graphics/gop/gop_${pad}.json`));
      tasks.push(this.prefetchTexture(`graphics/gop/gop_${pad}_1x.webp`));
      if (hdMode) {
        const metaKey = `graphics/maps_hd/map_${pad}/meta.json`;
        tasks.push(
          (async () => {
            try {
              let bytes = this.packManager ? await this.packManager.readBytes(metaKey) : null;
              if (!bytes && !this.packManager) {
                const resp = await fetch(withAssetVersion(`${this.baseUrl}/${metaKey}`));
                if (resp.ok) bytes = new Uint8Array(await resp.arrayBuffer());
              }
              if (!bytes) return;
              const keyLen = this._writeKey(metaKey);
              const jsonLen = this._writeScratch(bytes);
              this.w.wasm_upload_hd_map_json?.(keyLen, jsonLen);
              const meta = JSON.parse(textDecoder.decode(bytes));
              const chunks = Array.isArray(meta?.chunks) ? meta.chunks : [];
              await Promise.all(
                chunks.map((c) => {
                  const file = c?.file || c?.path;
                  return file ? this.prefetchTexture(`graphics/maps_hd/map_${pad}/${file}`) : null;
                })
              );
            } catch {
              // Fall back cleanly to GOP tiles if map has no HD pack
            }
          })()
        );
      }
    }
    const uniq = Array.from(new Set(spriteIds)).filter((id) => id > 0);
    for (const id of uniq) {
      const pad = String(id).padStart(3, "0");
      tasks.push(this.prefetchTexture(`graphics/mgo/mgo_${pad}_${suffix}.webp`));
    }
    await Promise.allSettled(tasks);
  }

  _writeKey(key) {
    const encoded = textEncoder.encode(key);
    const ptr = this.w.wasm_key_scratch_ptr(encoded.length);
    new Uint8Array(this.mem.buffer, ptr, encoded.length).set(encoded);
    return encoded.length;
  }

  _writeScratch(bytes) {
    const ptr = this.w.wasm_scratch_ptr(bytes.length);
    new Uint8Array(this.mem.buffer, ptr, bytes.length).set(bytes);
    return bytes.length;
  }

  _getDecodeCtx(w, h) {
    if (!this._decodeCanvas) {
      this._decodeCanvas =
        typeof OffscreenCanvas !== "undefined"
          ? new OffscreenCanvas(w, h)
          : document.createElement("canvas");
      this._decodeCtx = this._decodeCanvas.getContext("2d", { willReadFrequently: true });
    }
    if (this._decodeCanvas.width !== w || this._decodeCanvas.height !== h) {
      this._decodeCanvas.width = w;
      this._decodeCanvas.height = h;
    }
    return this._decodeCtx;
  }

  /**
   * Poll all pending missing-asset requests emitted by `pal-render` during the last frame
   * and schedule bounded-concurrency fetches for each.
   */
  pumpRequests() {
    if (!this.w?.wasm_poll_asset_request) return;
    while (true) {
      const kind = this.w.wasm_poll_asset_request();
      if (!kind) break;
      const ptr = this.w.wasm_asset_req_ptr();
      const len = this.w.wasm_asset_req_len();
      const key = textDecoder.decode(new Uint8Array(this.mem.buffer, ptr, len));
      const tag = `${kind}:${key}`;
      if (this.inFlight.has(tag) || this._queuedTags.has(tag)) continue;
      this._queuedTags.add(tag);
      this._queue.push({ kind, key, tag, gen: this._sceneGen });
    }
    this._drainQueue();
  }

  _drainQueue() {
    while (this._activeCount < this.maxConcurrent && this._queue.length > 0) {
      const item = this._queue.shift();
      this._queuedTags.delete(item.tag);
      if (this.inFlight.has(item.tag)) continue;
      this._activeCount++;
      const p = this._fetchAndUpload(item.kind, item.key, item.tag, item.gen).finally(() => {
        this._activeCount--;
        this._drainQueue();
      });
      this.inFlight.set(item.tag, p);
    }
  }

  async _fetchAndUpload(kind, key, tag, gen = this._sceneGen) {
    const url = withAssetVersion(`${this.baseUrl}/${key}`);
    const isUi = key.includes("/ui/");
    try {
      // Optional `.rpk` PackManager fast path (zero network for cached packs, #237).
      let packBytes = null;
      if (this.packManager) {
        packBytes = await this.packManager.readBytes(key);
      }
      let resp = null;
      if (!packBytes) {
        resp = await fetch(url);
        if (resp.status === 404 && (kind === 1 || kind === 4)) {
          const keyLen = this._writeKey(key);
          if (this.w.wasm_mark_asset_missing_permanent) {
            this.w.wasm_mark_asset_missing_permanent(kind, keyLen);
          } else {
            this.w.wasm_mark_asset_failed(kind, keyLen);
          }
          return false;
        }
        if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      }
      if (!isUi && gen !== this._sceneGen) {
        const keyLen = this._writeKey(key);
        this.w.wasm_mark_asset_failed?.(kind, keyLen);
        return false;
      }

      if (kind === 1) {
        if (typeof createImageBitmap !== "function") {
          const keyLen = this._writeKey(key);
          this.w.wasm_mark_asset_missing_permanent?.(kind, keyLen);
          return;
        }
        // Texture (WebP/PNG -> native browser decode -> RGBA upload)
        const blob = packBytes ? new Blob([packBytes]) : await resp.blob();
        const bitmap = await createImageBitmap(blob);
        const w = bitmap.width;
        const h = bitmap.height;
        const ctx = this._getDecodeCtx(w, h);
        ctx.clearRect(0, 0, w, h);
        ctx.drawImage(bitmap, 0, 0);
        bitmap.close?.();
        const imgData = ctx.getImageData(0, 0, w, h);
        const keyLen = this._writeKey(key);
        const rgbaLen = this._writeScratch(imgData.data);
        const pinned = key.includes("/ui/") ? 1 : 0;
        this.w.wasm_upload_texture(keyLen, w, h, rgbaLen, pinned);
        return true;
      } else if (kind === 2) {
        // Atlas JSON metadata
        const bytes = packBytes || new Uint8Array(await resp.arrayBuffer());
        const keyLen = this._writeKey(key);
        const jsonLen = this._writeScratch(bytes);
        this.w.wasm_upload_atlas_json(keyLen, jsonLen);
        return true;
      } else if (kind === 3) {
        // Map binary (.bin)
        const bytes = packBytes || new Uint8Array(await resp.arrayBuffer());
        const keyLen = this._writeKey(key);
        const binLen = this._writeScratch(bytes);
        this.w.wasm_upload_map_bin(keyLen, binLen);
        return true;
      } else if (kind === 4) {
        // Track-B HD map metadata JSON (`maps_hd/map_XXX/meta.json`)
        const bytes = packBytes || new Uint8Array(await resp.arrayBuffer());
        const keyLen = this._writeKey(key);
        const jsonLen = this._writeScratch(bytes);
        this.w.wasm_upload_hd_map_json?.(keyLen, jsonLen);
        return true;
      }
      return false;
    } catch {
      const keyLen = this._writeKey(key);
      this.w.wasm_mark_asset_failed(kind, keyLen);
      return false;
    } finally {
      this.inFlight.delete(tag);
    }
  }

  /**
   * Optional: load a TTF font from PackManager or asset base URL (e.g. `fonts/pal-cjk.ttf`)
   * into the WASM `FontEngine`.
   */
  async loadFont(relativePath, isFallback = false) {
    try {
      let bytes = this.packManager ? await this.packManager.readBytes(relativePath) : null;
      if (!bytes) {
        const resp = await fetch(withAssetVersion(`${this.baseUrl}/${relativePath}`));
        if (!resp.ok) return false;
        bytes = new Uint8Array(await resp.arrayBuffer());
      }
      const len = this._writeScratch(bytes);
      return Boolean(this.w.wasm_upload_font_ttf(len, isFallback ? 1 : 0));
    } catch {
      return false;
    }
  }
}
