/**
 * Thin browser adapters around `pal.wasm` (`site/js/host/wasm-runtime.js`).
 *
 * All game logic, world/battle rendering and the 1995 DOS UI live in Rust (`pal_core::game`,
 * `pal-render`, `pal-ui`). These adapters only
 *  - fetch and upload assets the core/renderer ask for (maps, atlases, fonts, cutscene frames),
 *  - present the framebuffer and map pointer coordinates,
 *  - host the few UI actions that need the browser (saves, screenshots, clipboard, toggles).
 */

import { WasmCanvasPresenter } from "./wasm-presenter.js";
import { resolveAssetBaseUrl, withAssetVersion } from "./asset-provider.js";
import { L } from "./l10n.js";

const enc = new TextEncoder();

export function writeKeyStr(wasm, str) {
  const bytes = enc.encode(String(str ?? ""));
  const ptr = wasm.exports.wasm_key_scratch_ptr(bytes.length);
  new Uint8Array(wasm.memoryBuffer, ptr, bytes.length).set(bytes);
  return bytes.length;
}

export function writeScratchStr(wasm, str) {
  const bytes = enc.encode(String(str ?? ""));
  const ptr = wasm.exports.wasm_scratch_ptr(bytes.length);
  new Uint8Array(wasm.memoryBuffer, ptr, bytes.length).set(bytes);
  return bytes.length;
}

/** Copy text to the clipboard on HTTPS/localhost and on plain-HTTP LAN hosts alike. */
export async function copyTextSafe(text) {
  if (navigator.clipboard && window.isSecureContext) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // fall through to the legacy textarea path
    }
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "readonly");
  ta.style.position = "fixed";
  ta.style.top = "-9999px";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.focus();
  ta.select();
  ta.setSelectionRange?.(0, text.length);
  let ok = false;
  try {
    ok = document.execCommand("copy");
  } catch {
    ok = false;
  }
  document.body.removeChild(ta);
  return ok;
}

export const SAVE_PREFIX = "pal_remake_save_v1_slot_";
export const SAVE_SLOT_COUNT = 100;

/** Mirrors `pal_ui::UiAction` codes from `crates/pal-wasm/src/lib.rs::encode_ui_action`. */
export const UI_ACTION = {
  None: 0,
  NewGame: 1,
  LoadSlot: 2,
  SaveSlot: 3,
  ToggleHdMode: 4,
  ToggleMusic: 5,
  ToggleSfx: 6,
  ReturnToTitle: 7,
  ToggleLanguage: 20,
  CopyReplayReport: 21,
  ExportSave: 22,
  ImportSave: 23,
  TakeScreenshot: 24,
  ToggleFullscreen: 25,
  ToggleAgentMode: 26,
  CheckForUpdate: 37,
  DownloadAllAssets: 38,
  RestartRecording: 27,
  ExportReplay: 28,
  OpenReplay: 29,
  ReplayJumpChapter: 30,
  ReplayTakeOver: 31,
  ReplayRestart: 32,
};

export class WasmRendererAdapter {
  constructor(canvas, wasm, baseUrl = resolveAssetBaseUrl()) {
    this.canvas = canvas;
    this.wasm = wasm;
    this.baseUrl = baseUrl;
    this.packManager = null;
    this.presenter = new WasmCanvasPresenter(canvas, wasm.exports, wasm.exports.memory, baseUrl);
    this.hdMode = true;
    this.activeRngFrameImg = null;
    this.mapCache = new Map();
    this.atlasCache = new Map();
    this._clickRipple = null;
  }

  async initFonts() {
    // Primary CJK font plus a Latin/digit fallback so ASCII never renders as boxes (#65, #68, #69).
    await Promise.all([
      this.presenter.assets.loadFont("fonts/pal-cjk.ttf", false),
      this.presenter.assets.loadFont("fonts/pal-latin.ttf", true),
    ]);
  }

  setHdMode(hd) {
    this.hdMode = Boolean(hd);
    this.wasm.exports.wasm_set_hd_mode(this.hdMode ? 1 : 0);
  }

  setTitleFbp(on) {
    this.wasm.exports.wasm_set_fbp_override(on ? 60 : -1);
  }

  /** Fetch `maps/map_XXX.bin`, upload it to the render cache and return the bytes. */
  async ensureMapBinary(mapId) {
    const id = Number(mapId) | 0;
    if (this.mapCache.has(id)) return this.mapCache.get(id);
    const pad = String(id).padStart(3, "0");
    const relKey = `maps/map_${pad}.bin`;
    try {
      let bytes = this.packManager ? await this.packManager.readBytes(relKey) : null;
      if (!bytes) {
        const resp = await fetch(withAssetVersion(`${this.baseUrl}/${relKey}`));
        if (!resp.ok) return null;
        bytes = new Uint8Array(await resp.arrayBuffer());
      }
      this.mapCache.set(id, bytes);
      const keyLen = writeKeyStr(this.wasm, relKey);
      const ptr = this.wasm.exports.wasm_scratch_ptr(bytes.length);
      new Uint8Array(this.wasm.memoryBuffer, ptr, bytes.length).set(bytes);
      this.wasm.exports.wasm_upload_map_bin(keyLen, bytes.length);
      return bytes;
    } catch {
      return null;
    }
  }

  /** Bulk-populate all MGO atlas metadata entries from `graphics/mgo/mgo_index.json` once at boot. */
  preloadMgoIndex(indexObj) {
    if (!indexObj || typeof indexObj !== "object") return 0;
    let count = 0;
    for (const [base, meta] of Object.entries(indexObj)) {
      if (!meta || typeof meta !== "object") continue;
      const cacheKey = `mgo/${base}`;
      const relKey = `graphics/mgo/${base}.json`;
      const text = JSON.stringify(meta);
      this.wasm.exports.wasm_upload_atlas_json(writeKeyStr(this.wasm, relKey), writeScratchStr(this.wasm, text));
      this.atlasCache.set(cacheKey, { meta });
      count++;
    }
    return count;
  }

  /** Fetch an atlas `graphics/<category>/<base>.json`, upload it and return `{ meta }`. */
  async ensureAtlas(category, base) {
    const cacheKey = `${category}/${base}`;
    if (this.atlasCache.has(cacheKey)) return this.atlasCache.get(cacheKey);
    const relKey = `graphics/${category}/${base}.json`;
    try {
      let text = this.packManager ? await this.packManager.readText(relKey) : null;
      if (text == null) {
        const resp = await fetch(withAssetVersion(`${this.baseUrl}/${relKey}`));
        if (!resp.ok) return null;
        text = await resp.text();
      }
      const meta = JSON.parse(text);
      this.wasm.exports.wasm_upload_atlas_json(writeKeyStr(this.wasm, relKey), writeScratchStr(this.wasm, text));
      const entry = { meta };
      this.atlasCache.set(cacheKey, entry);
      return entry;
    } catch {
      return null;
    }
  }

  prefetchTexture(relKey) {
    return this.presenter.assets.prefetchTexture(relKey);
  }

  async ensureFbp(fbpId) {
    const pad = String(Number(fbpId) | 0).padStart(3, "0");
    await this.prefetchTexture(`graphics/fbp/fbp_${pad}_${this.hdMode ? "4x" : "1x"}.webp`);
  }

  addClickRipple(worldX, worldY) {
    if (Number.isFinite(worldX) && Number.isFinite(worldY)) {
      this._clickRipple = { x: Math.round(worldX), y: Math.round(worldY), age: 0 };
    }
  }

  /** Advance presentation-only effects (click ripple) and present one frame. */
  present() {
    const w = this.wasm.exports;
    if (this._clickRipple) {
      this._clickRipple.age += 0.06;
      if (this._clickRipple.age >= 1) {
        this._clickRipple = null;
        w.wasm_set_click_ripple(0, 0, 0, 0);
      } else {
        const r = this._clickRipple;
        w.wasm_set_click_ripple(1, r.x | 0, r.y | 0, Math.round(r.age * 1000) >>> 0);
      }
    }
    this.presenter.presentFrame();
  }

  /**
   * Map a client (CSS pixel) position to 1x world coordinates given the camera origin, honouring
   * 16:10 letterboxing (#52).
   */
  clientToWorld(clientX, clientY, camX = 0, camY = 0) {
    const rect = this.canvas.getBoundingClientRect();
    if (!rect.width || !rect.height) return { screenX: 0, screenY: 0, worldX: 0, worldY: 0 };
    const targetAspect = 320 / 200;
    const boxAspect = rect.width / rect.height;
    let drawW = rect.width;
    let drawH = rect.height;
    let offX = 0;
    let offY = 0;
    if (Math.abs(boxAspect - targetAspect) > 0.01) {
      if (boxAspect > targetAspect) {
        drawW = rect.height * targetAspect;
        offX = (rect.width - drawW) / 2;
      } else {
        drawH = rect.width / targetAspect;
        offY = (rect.height - drawH) / 2;
      }
    }
    const lx = Math.max(0, Math.min(320, ((clientX - rect.left - offX) / drawW) * 320));
    const ly = Math.max(0, Math.min(200, ((clientY - rect.top - offY) / drawH) * 200));
    return { screenX: lx, screenY: ly, worldX: Math.round(camX + lx), worldY: Math.round(camY + ly) };
  }

  /** Play an RNG cutscene frame by frame (the core waits for it to finish). */
  async playRngCutscene(rngId, startFrame = 0, endFrame = 0, speed = 8, isCancelledFn = null) {
    const pad = String(rngId).padStart(2, "0");
    const w = this.wasm.exports;
    try {
      const metaKey = `graphics/rng/rng_${pad}/meta.json`;
      let meta = null;
      if (this.packManager) {
        const raw = await this.packManager.readText(metaKey);
        if (raw) meta = JSON.parse(raw);
      }
      if (!meta) {
        const mResp = await fetch(withAssetVersion(`${this.baseUrl}/${metaKey}`));
        if (!mResp.ok) return;
        meta = await mResp.json();
      }
      const step = meta.frame_step || 1;
      const firstIdx = Math.max(0, Math.floor(startFrame / step));
      const lastIdx =
        endFrame > 0 ? Math.min((meta.exported_frames || 1) - 1, Math.ceil(endFrame / step)) : (meta.exported_frames || 1) - 1;
      const frameDelayMs = Math.max(35, Math.round((1000 / Math.max(4, speed)) * step * 0.55));
      const use4x = Boolean(meta.hd_4x) && this.hdMode;
      const rngKey = (idx) => `graphics/rng/rng_${pad}/f_${String(idx).padStart(3, "0")}_${use4x ? "4x" : "1x"}.webp`;
      this.activeRngFrameImg = true;
      for (let fi = firstIdx; fi <= lastIdx; fi++) {
        if (isCancelledFn?.()) break;
        const t0 = performance.now();
        if (fi + 1 <= lastIdx) this.prefetchTexture(rngKey(fi + 1));
        if (fi + 2 <= lastIdx) this.prefetchTexture(rngKey(fi + 2));
        await this.prefetchTexture(rngKey(fi));
        if (isCancelledFn?.()) break;
        w.wasm_set_rng_frame(1, rngId >>> 0, fi >>> 0);
        this.presenter.presentFrame();
        const waitMs = Math.max(0, Math.round(frameDelayMs - (performance.now() - t0)));
        if (waitMs > 0) await new Promise((r) => setTimeout(r, waitMs));
      }
    } catch {
      // Non-fatal if an RNG pack is unavailable in headless/minimal environments
    } finally {
      this.activeRngFrameImg = null;
      w.wasm_set_rng_frame(0, 0, 0);
    }
  }
}

/**
 * Browser-side glue for `pal-ui`: title/menu views, toasts, save slot summaries, and the UI
 * actions the Rust core forwards to the host (everything else is consumed inside `pal.wasm`).
 */
export class WasmHudAdapter {
  constructor({ wasm, renderer, audio }) {
    this.wasm = wasm;
    this.renderer = renderer;
    this.audio = audio;
    this.lang = "hans";
    this._toastTimer = null;
    this._titleVisible = true;
    // Callbacks wired by PalRemakeApp
    this.onStartNewGame = null;
    this.onReturnToTitle = null;
    this.onLoadSlot = null;
    this.onSaveSlot = null;
    this.onExportSave = null;
    this.onImportSave = null;
    this.onCopyReplayReport = null;
    this.onLanguageChanged = null;
  }

  t(hans, hant) {
    return this.lang === "hant" ? hant || hans || "" : hans || hant || "";
  }

  isTitleVisible() {
    return this._titleVisible;
  }

  isModalOpen() {
    return this.wasm.exports.wasm_ui_is_modal_open() === 1;
  }

  setView(code) {
    this.wasm.exports.wasm_ui_set_view(code >>> 0);
  }

  showTitleScreen() {
    this._titleVisible = true;
    this.renderer.setTitleFbp(true);
    this.setView(1);
  }

  hideTitleScreen() {
    this._titleVisible = false;
    this.renderer.setTitleFbp(false);
    this.setView(0);
  }

  openRootMenu() {
    this._titleVisible = false;
    this.setView(2);
  }

  closeSystemMenu() {
    this.setView(0);
  }

  refreshSaveSlotSummaries() {
    const w = this.wasm.exports;
    const isEn = this.lang === "en";
    const isHans = this.lang === "hans";
    for (let slot = 0; slot < SAVE_SLOT_COUNT; slot++) {
      let detail =
        slot === 0
          ? isEn
            ? "Classic Save #10"
            : isHans
              ? "快速存档 10.rpg"
              : "快速存檔 10.rpg"
          : isEn
            ? "Empty"
            : "空";
      let exists = slot === 0 ? 1 : 0;
      try {
        const raw = localStorage.getItem(`${SAVE_PREFIX}${slot}`);
        if (raw) {
          const parsed = JSON.parse(raw);
          exists = 1;
          const sid = parsed.sceneId || 1;
          const lv = parsed.roles?.[0]?.level ?? 1;
          detail = isEn
            ? `Scene #${sid}  Lv.${lv}`
            : `${parsed.sceneName || (isHans ? `场景#${sid}` : `場景#${sid}`)}  Lv.${lv}`;
        }
      } catch {
        // ignore storage errors
      }
      w.wasm_ui_set_save_slot(slot >>> 0, exists >>> 0, writeKeyStr(this.wasm, detail));
    }
  }

  toggleHdMode() {
    const next = !this.renderer.hdMode;
    this.renderer.setHdMode(next);
    this.audio?.setHdMode?.(next);
    this.showToast(
      this.lang === "en"
        ? next
          ? "Graphics: 4x HD"
          : "Graphics: 1995 Retro"
        : next
          ? L("画质切换：HD 高清", "畫質切換：HD 高清", "")
          : L("画质切换：1995 复古", "畫質切換：1995 復古", "")
    );
  }

  showToast(text, durationMs = 2200) {
    this.wasm.exports.wasm_ui_set_toast(writeKeyStr(this.wasm, text));
    if (this._toastTimer) clearTimeout(this._toastTimer);
    if (durationMs > 0) {
      this._toastTimer = setTimeout(() => this.wasm.exports.wasm_ui_set_toast(0), durationMs);
    }
  }

  /** Execute UI actions the core left for the host. */
  drainActions() {
    const w = this.wasm.exports;
    for (;;) {
      const code = w.wasm_ui_poll_action();
      if (!code) return;
      Promise.resolve(
        this._executeAction(code, w.wasm_ui_last_action_arg0(), w.wasm_ui_last_action_arg1())
      ).catch((err) => console.warn("[WasmHudAdapter] action failed:", code, err));
    }
  }

  async _executeAction(code, arg0) {
    switch (code) {
      case UI_ACTION.NewGame:
        this._titleVisible = false;
        this.renderer.setTitleFbp(false);
        await this.onStartNewGame?.();
        break;
      case UI_ACTION.LoadSlot:
        await this.onLoadSlot?.(arg0);
        break;
      case UI_ACTION.SaveSlot:
        this.onSaveSlot?.(arg0);
        break;
      case UI_ACTION.ToggleHdMode:
        this.toggleHdMode();
        break;
      case UI_ACTION.ToggleMusic:
        this.audio?.toggleMusic?.();
        break;
      case UI_ACTION.ToggleSfx:
        this.audio?.toggleSfx?.();
        break;
      case UI_ACTION.ToggleLanguage:
        this.lang = this.lang === "hant" ? "hans" : this.lang === "hans" ? "en" : "hant";
        try {
          globalThis.localStorage?.setItem("pal_lang", this.lang);
        } catch {}
        this.onLanguageChanged?.(this.lang);
        this.refreshSaveSlotSummaries();
        this.showToast(
          this.lang === "en"
            ? "Language: English"
            : this.lang === "hant"
              ? "文字已切換：繁體中文"
              : "文字已切换：简体中文"
        );
        break;
      case UI_ACTION.CopyReplayReport:
        await this.onCopyReplayReport?.();
        break;
      case UI_ACTION.ExportSave:
        this.onExportSave?.();
        break;
      case UI_ACTION.ImportSave:
        this.onImportSave?.();
        break;
      case UI_ACTION.TakeScreenshot:
        this.takeScreenshot();
        break;
      case UI_ACTION.ToggleFullscreen:
        document.getElementById("btn-stage-fullscreen")?.click();
        break;
      case UI_ACTION.RestartRecording:
        this.onRestartRecording?.();
        break;
      case UI_ACTION.ExportReplay:
        await this.onExportReplay?.();
        break;
      case UI_ACTION.OpenReplay:
        await this.onOpenReplay?.();
        break;
      case UI_ACTION.ReplayJumpChapter:
        this.onReplayJumpChapter?.(arg0);
        break;
      case UI_ACTION.ReplayTakeOver:
        this.onReplayTakeOver?.();
        break;
      case UI_ACTION.ReplayRestart:
        this.onReplayRestart?.();
        break;
      case UI_ACTION.ReturnToTitle:
        await this.onReturnToTitle?.();
        break;
      case UI_ACTION.DownloadAllAssets:
        await this.onDownloadAllAssets?.();
        break;
      case UI_ACTION.CheckForUpdate:
        if ("serviceWorker" in navigator) {
          this.showToast(L("正在检查新版本...", "正在檢查新版本...", "Checking for updates..."));
          navigator.serviceWorker
            .getRegistration()
            .then((reg) => {
              if (!reg) {
                this.showToast(L("当前为网页直连模式，刷新即最新", "目前為網頁直連模式，重新整理即最新", "Web mode: reloading always gets the latest"));
                return;
              }
              return reg.update().then(() => {
                if (reg.waiting || reg.installing) {
                  this.showToast(L("发现新版本，刷新页面即可生效！", "發現新版本，重新整理頁面即可生效！", "New version available, reload the page to apply!"));
                } else {
                  this.showToast(L("已是最新版本", "已是最新版本", "Already up to date"));
                }
              });
            })
            .catch(() => this.showToast(L("检查更新失败", "檢查更新失敗", "Update check failed")));
        } else {
          this.showToast(L("当前为网页直连模式，刷新即最新", "目前為網頁直連模式，重新整理即最新", "Web mode: reloading always gets the latest"));
        }
        break;
      default:
        break;
    }
  }

  takeScreenshot() {
    try {
      const canvas = this.renderer?.canvas;
      if (!canvas || typeof canvas.toBlob !== "function") {
        this.showToast(L("当前环境不支持截图", "目前環境不支援畫面截圖", "Screenshots are not supported here"));
        return;
      }
      canvas.toBlob(async (blob) => {
        if (!blob) return;
        const ts = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
        const filename = `palremake_${ts}.png`;
        downloadBlob(blob, filename);
        let copied = false;
        if (navigator.clipboard?.write && typeof ClipboardItem !== "undefined" && window.isSecureContext) {
          try {
            await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
            copied = true;
          } catch {
            copied = false;
          }
        }
        this.showToast(copied ? `${L("截图已下载并复制到剪贴板", "截圖已下載並複製到剪貼簿", "Screenshot downloaded and copied")} (${filename})` : `${L("截图已保存", "截圖已儲存", "Screenshot saved")}: ${filename}`);
      }, "image/png");
    } catch {
      this.showToast(L("截图失败", "截圖失敗", "Screenshot failed"));
    }
  }
}

export function downloadBlob(blob, filename) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** Let the player pick a local file; resolves to `File` or `null`. */
export function pickFile(accept) {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    input.style.display = "none";
    let settled = false;
    const finish = (file) => {
      if (settled) return;
      settled = true;
      window.removeEventListener?.("focus", onFocus);
      if (input.parentNode) input.parentNode.removeChild(input);
      resolve(file);
    };
    const onFocus = () => {
      setTimeout(() => {
        if (!settled) finish(input.files?.[0] || null);
      }, 300);
    };
    input.onchange = () => finish(input.files?.[0] || null);
    input.oncancel = () => finish(null);
    window.addEventListener?.("focus", onFocus, { once: true });
    document.body.appendChild(input);
    input.click();
  });
}
