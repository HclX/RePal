/**
 * PalRemake entry point (`site/js/main.js`).
 *
 * The whole game runs inside `pal.wasm`: the deterministic Rust core (`pal_core::game`) owns all
 * logic and state, `pal-render` draws, `pal-ui` handles menus. This file boots the wasm module,
 * wires browser input to it and hosts audio, asset loading and saves (see `host/core-host.js`).
 */

import { PalWasmBridge } from "./wasm/wasm-bridge.js";
import { AudioManager } from "./audio/audio-manager.js";
import { ModLoader } from "./mods/mod-loader.js";
import { WasmRendererAdapter, WasmHudAdapter, SAVE_PREFIX } from "./host/wasm-runtime.js";
import {
  withCodeVersion,
  ASSET_URL_STORAGE_KEY,
  isCustomAssetOrigin,
  resolveAssetBaseUrl,
  validateAssetBaseUrl,
} from "./host/asset-provider.js";
import { POINTER_KIND } from "./host/wasm-presenter.js";
import { RustCoreHost, fetchArrayBuffer } from "./host/core-host.js";
import { PackManager, masterKeyHexFromPassphrase } from "./host/pack-manager.js";
import { GamepadController } from "./host/gamepad.js";
import { WebReplayTheater } from "./host/replay-theater.js";
import { WebAgentHost } from "./host/web-agent.js";
import { saveLocalReplay } from "./host/repal-loader.js";
import { L } from "./host/l10n.js";

const DEFAULT_PACK_MANIFEST_URL =
  "https://cdn.jsdelivr.net/gh/HclX/RePal_Packs@packs-v5/pack-manifest.json"; // stamped from assets-lock.json on deploy
const PACK_KEY_STORAGE_KEY = "repal_pack_key_v1";

function promptPackKeyOverlay(overlay, statusEl, message) {
  return new Promise((resolve) => {
    if (statusEl) statusEl.textContent = message;
    const old = document.getElementById("pack-key-setup");
    if (old) old.remove();
    const box = document.createElement("div");
    box.id = "pack-key-setup";
    box.style.cssText =
      "margin-top:14px;display:flex;flex-direction:column;gap:8px;align-items:center;max-width:440px;";
    const row = document.createElement("div");
    row.style.cssText = "display:flex;gap:8px;align-items:center;flex-wrap:wrap;justify-content:center;";
    const inp = document.createElement("input");
    inp.type = "password";
    inp.autocomplete = "off";
    inp.placeholder = "输入资源包口令或 64 位密钥 (passphrase or 64-hex key)";
    inp.style.cssText =
      "width:320px;padding:7px 10px;border-radius:6px;border:1px solid #56c2b1;background:#0d171c;color:#f3ecdc;font-size:13px;font-family:monospace;";
    const btn = document.createElement("button");
    btn.textContent = "解锁并启动 / Unlock";
    btn.style.cssText =
      "padding:7px 14px;border-radius:6px;border:0;background:#e0b96b;color:#142026;font-weight:600;cursor:pointer;";
    const submit = () => {
      const raw = inp.value.trim();
      if (!raw) return;
      box.remove();
      resolve(raw);
    };
    btn.onclick = submit;
    inp.addEventListener("keydown", (e) => {
      if (e.key === "Enter") submit();
    });
    row.append(inp, btn);
    box.append(row);
    overlay?.append(box);
    inp.focus();
  });
}

class PalRemakeApp {
  constructor() {
    this.canvas = document.getElementById("game-canvas");
    this.loadingOverlay = document.getElementById("boot-loading");
    this.loadingBar = document.getElementById("boot-bar-fill");
    this.loadingText = document.getElementById("boot-status");
    this.keysDown = new Set();
    this.touchDir = -1;
    this.touchDpadPointerId = null;
    this.touchUiRepeatTimer = null;
    this.pointerDragActive = false;
    this.pointerClientPos = { x: 0, y: 0 };
    this.inTitle = true;
    this.cutsceneSkip = false;
  }

  setBootProgress(pct, label) {
    if (this.loadingBar) this.loadingBar.style.width = `${Math.round(pct * 100)}%`;
    if (this.loadingText) this.loadingText.textContent = label;
  }

  async boot() {
    try {
      this.setBootProgress(0.1, L(L("正在唤醒 Rust WebAssembly 引擎核心...", "正在喚醒 Rust WebAssembly 引擎核心...", "Starting the Rust WebAssembly engine..."), "正在喚醒 Rust WebAssembly 引擎核心...", "Starting the Rust WebAssembly engine..."));
      this.wasm = new PalWasmBridge();
      await this.wasm.init(withCodeVersion("./wasm/pal.wasm"));

      this.setBootProgress(0.2, L(L("正在挂载 YAML 剧情 Mod 扩展层...", "正在掛載 YAML 劇情 Mod 擴展層...", "Mounting YAML story mods..."), "正在掛載 YAML 劇情 Mod 擴展層...", "Mounting YAML story mods..."));
      this.modLoader = new ModLoader();
      await this.modLoader.init();

      this.renderer = new WasmRendererAdapter(this.canvas, this.wasm);
      this.audio = new AudioManager(this.wasm);
      this.audio.installGestureUnlock();
      this.hud = new WasmHudAdapter({ wasm: this.wasm, renderer: this.renderer, audio: this.audio });

      // Option A (#237): prefer local loose `./assets/` in dev; otherwise mount encrypted `.rpk` packs.
      await this._initPackManagerIfNeeded();

      this.setBootProgress(0.3, L("正在载入 WASM 字型引擎...", L("正在载入 WASM 字型引擎...", "正在載入 WASM 字型引擎...", "Loading fonts..."), "Loading fonts..."));
      await this.renderer.initFonts();

      this.core = new RustCoreHost(this);
      await this.core.init((p, key) => this.setBootProgress(0.3 + p * 0.6, `${L("正在载入游戏数据", "正在載入遊戲資料", "Loading game data")} (${key})...`));

      this.setBootProgress(0.95, L(L("正在载入 4x 高清标题画卷...", "正在載入 4x 高清標題畫卷...", "Loading the HD title screen..."), "正在載入 4x 高清標題畫卷...", "Loading the HD title screen..."));
      await this.renderer.ensureFbp(60);
      this.hud.refreshSaveSlotSummaries();
      this.hud.showTitleScreen();
      this.audio.playMusic(4, true); // 《云谷鹤峰》原版标题曲 (MAINMENU_MUSIC = 4)
      this.packManager?.prefetchBootWarmup?.();

      this.wireHud();
      this.gamepad = new GamepadController(this);
      this.replayTheater = new WebReplayTheater(this);
      this.webAgent = new WebAgentHost(this);
      this.wireInputs();
      this.audio.installGestureUnlock();

      this.setBootProgress(1.0, L(L("准备就绪！", "準備就緒！", "Ready!"), "準備就緒！", "Ready!"));
      setTimeout(() => this.loadingOverlay?.classList.add("hidden"), 180);
      window.__pal = this;

      const activeAssets = resolveAssetBaseUrl();
      if (isCustomAssetOrigin(activeAssets)) {
        this.hud.showToast(`${L("使用自定义资源源", "使用自訂資源來源", "Using custom asset source")}: ${activeAssets}`, 4200);
      }

      if (typeof navigator !== "undefined" && "serviceWorker" in navigator) {
        navigator.serviceWorker
          .register("./sw.js")
          .then((reg) => {
            reg.update().catch(() => {});
            reg.addEventListener("updatefound", () => {
              const nw = reg.installing;
              if (!nw) return;
              nw.addEventListener("statechange", () => {
                if (nw.state === "installed" && navigator.serviceWorker.controller) {
                  window.__pal?.hud?.showToast?.(L("发现新版本，刷新页面即可生效！", "發現新版本，重新整理頁面即可生效！", "New version available, reload the page to apply!"));
                }
              });
            });
          })
          .catch(() => {});
      }

      // Optional URL auto-launch: `?replay=...` or `?agent_ws=ws://127.0.0.1:9999`
      const urlParams = new URLSearchParams(window.location.search);
      const replayParam = urlParams.get("replay");
      if (replayParam) {
        this.replayTheater.startFromUrl(replayParam).catch((err) => {
          this.hud.showToast(`${L("加载录像失败", "載入錄像失敗", "Failed to load replay")}: ${err.message}`);
        });
      }
      const agentWsParam = urlParams.get("agent_ws");
      if (agentWsParam) {
        this.webAgent.connectWs(agentWsParam);
      }

      requestAnimationFrame((t) => this.frameLoop(t));
    } catch (err) {
      console.error("[PalRemake Boot Error]", err);
      if (this.loadingText) {
        const cur = resolveAssetBaseUrl();
        this.loadingText.textContent = `${L("启动失败", "啟動失敗", "Startup failed")}: ${err.message} (${L("当前资源源", "目前資源來源", "asset source")}: ${cur})`;
        if (typeof document !== "undefined" && this.loadingOverlay && !document.getElementById("asset-url-setup")) {
          const box = document.createElement("div");
          box.id = "asset-url-setup";
          box.style.cssText = "margin-top:12px;display:flex;gap:8px;align-items:center;flex-wrap:wrap;justify-content:center;";
          const inp = document.createElement("input");
          inp.type = "text";
          inp.placeholder = L(L("输入资源基地址 (如 https://.../assets 或 ./assets)", "輸入資源基底位址 (如 https://.../assets 或 ./assets)", "Asset base URL (e.g. https://.../assets or ./assets)"), "輸入資源基底位址 (如 https://.../assets 或 ./assets)", "Asset base URL (e.g. https://.../assets or ./assets)");
          inp.value = cur === "./assets" ? "" : cur;
          inp.style.cssText = "width:320px;padding:6px 10px;border-radius:6px;border:1px solid #56c2b1;background:#0d171c;color:#f3ecdc;font-size:13px;";
          const btn = document.createElement("button");
          btn.textContent = L(L("连接资源并重试", "連接資源並重試", "Connect and retry"), "連接資源並重試", "Connect and retry");
          btn.style.cssText = "padding:6px 14px;border-radius:6px;border:0;background:#e0b96b;color:#142026;font-weight:600;cursor:pointer;";
          btn.onclick = () => {
            const v = inp.value.trim().replace(/\/+$/, "");
            try {
              if (!v) {
                localStorage.removeItem(ASSET_URL_STORAGE_KEY);
              } else {
                const valid = validateAssetBaseUrl(v);
                if (!valid) {
                  this.loadingText.textContent = L(L("无效资源地址：仅允许相对路径 (./assets) 或 https:// 地址", "無效資源位址：僅允許相對路徑 (./assets) 或 https:// 位址", "Invalid asset URL: only relative paths (./assets) or https:// are allowed"), "無效資源位址：僅允許相對路徑 (./assets) 或 https:// 位址", "Invalid asset URL: only relative paths (./assets) or https:// are allowed");
                  return;
                }
                localStorage.setItem(ASSET_URL_STORAGE_KEY, valid);
              }
            } catch {}
            window.location.reload();
          };
          box.append?.(inp, btn);
          this.loadingOverlay.append?.(box);
        }
      }
    }
  }

  async _initPackManagerIfNeeded() {
    const isBrowserHttp =
      typeof window !== "undefined" &&
      typeof window.location?.protocol === "string" &&
      (window.location.protocol === "http:" || window.location.protocol === "https:");
    const params = typeof window !== "undefined" ? new URLSearchParams(window.location?.search || "") : null;
    const explicitPacks = params?.get("packs");
    const explicitAssets = params?.get("assets");
    const base = resolveAssetBaseUrl();

    // In headless Node/test harnesses without a real HTTP(S) browser origin, stay in loose mode unless `?packs=` is set.
    if (!explicitPacks && !isBrowserHttp) return;

    // If user explicitly passed `?assets=...` (and not `?packs=`), stick to loose asset mode.
    if (explicitAssets && !explicitPacks) return;

    // Auto-detect local loose `./assets/` during local dev unless `?packs=` is explicitly requested.
    if (!explicitPacks && base === "./assets") {
      try {
        const probe = await fetch("./assets/data/scenes.yaml", { method: "HEAD", cache: "no-cache" });
        if (probe.ok) return;
      } catch {}
    }

    const manifestUrl = explicitPacks || DEFAULT_PACK_MANIFEST_URL;
    this.setBootProgress(0.24, L("正在连接资产包清单...", "正在連接資產包清單...", "Connecting to the asset pack manifest..."));

    let keyHex = "";
    try {
      keyHex = localStorage.getItem(PACK_KEY_STORAGE_KEY) || "";
    } catch {}

    for (;;) {
      try {
        const pm = new PackManager({
          manifestUrl,
          wasmExports: this.wasm.exports,
          wasmMemory: this.wasm.exports.memory,
          masterKeyHex: keyHex || null,
          onManifestStale: () => {
            this.hud?.showToast?.(L(L("资产包已更新，请刷新页面重新加载！", "資產包已更新，請重新整理頁面！", "Asset packs updated, please reload the page!"), "資產包已更新，請重新整理頁面！", "Asset packs updated, please reload the page!"), 6000);
          },
        });
        await pm.init();
        if (keyHex) {
          // Remember the resolved master key (never the passphrase) and ask the browser to keep
          // this origin's storage so TV/console browsers don't evict it (#18).
          try {
            localStorage.setItem(PACK_KEY_STORAGE_KEY, keyHex);
          } catch {}
          navigator.storage?.persist?.().catch(() => {});
        }
        this.packManager = pm;
        this.renderer.packManager = pm;
        this.renderer.presenter.assets.packManager = pm;
        this.audio.packManager = pm;
        this.refreshDownloadAll();
        return;
      } catch (err) {
        const msg = String(err?.message || err);
        const needsKey =
          msg.includes("encrypted") ||
          msg.includes("master key") ||
          msg.includes("OperationError") ||
          msg.includes("decrypt") ||
          msg.includes("wrapped_key");
        if (!needsKey || typeof document === "undefined") {
          throw err;
        }
        try {
          localStorage.removeItem(PACK_KEY_STORAGE_KEY);
        } catch {}
        let promptMsg = keyHex
          ? L("口令或密钥校验失败，请重试", "口令或金鑰校驗失敗，請重試", "Wrong passphrase or key, please retry")
          : L(L("当前资产包已加密，请输入资源包口令或 64 位密钥（仅需输入一次）", "目前資產包已加密，請輸入資源包口令或 64 位金鑰（僅需輸入一次）", "Asset packs are encrypted: enter the pack passphrase or 64-hex key (only needed once)"), "目前資產包已加密，請輸入資源包口令或 64 位金鑰（僅需輸入一次）", "Asset packs are encrypted: enter the pack passphrase or 64-hex key (only needed once)");
        for (;;) {
          const entered = await promptPackKeyOverlay(this.loadingOverlay, this.loadingText, promptMsg);
          if (/^[0-9a-fA-F]{64}$/.test(entered)) {
            keyHex = entered.toLowerCase();
            break;
          }
          this.setBootProgress(0.25, L("正在验证口令...", "正在驗證口令...", "Checking passphrase..."));
          let manifest = null;
          try {
            const r = await fetch(manifestUrl, { cache: "no-cache" });
            manifest = r.ok ? await r.json() : null;
          } catch {}
          const hex = manifest ? await masterKeyHexFromPassphrase(manifest, entered) : null;
          if (hex) {
            keyHex = hex;
            break;
          }
          promptMsg = L("口令不正确，请重试", "口令不正確，請重試", "Wrong passphrase, please retry");
        }
        this.setBootProgress(0.26, L(L("正在解密启动资产包...", "正在解密啟動資產包...", "Decrypting boot packs..."), "正在解密啟動資產包...", "Decrypting boot packs..."));
      }
    }
  }

  /** Push the "download all assets" menu state into the core (#18); hidden without packs. */
  async refreshDownloadAll(progressPermille = -1) {
    const pm = this.packManager;
    const set = this.wasm?.exports?.core_set_download_all;
    if (!pm || !set) return;
    try {
      const missing = await pm.missingPackIds();
      const remaining = missing.reduce((sum, pid) => sum + (pm.manifest.packs[pid]?.size || 0), 0);
      set(1, Math.min(remaining, 0xffffffff) >>> 0, progressPermille | 0);
    } catch {}
  }

  /** Background-download every missing package (play continues meanwhile). */
  async startDownloadAll() {
    const pm = this.packManager;
    if (!pm || this._downloadAllRunning) return;
    this._downloadAllRunning = true;
    const lang = this.hud?.lang;
    const say = (en, hans, hant) => this.hud?.showToast?.(lang === "en" ? en : lang === "hant" ? hant : hans);
    try {
      const missing = await pm.missingPackIds();
      const sizes = missing.map((pid) => pm.manifest.packs[pid]?.size || 0);
      const total = sizes.reduce((a, b) => a + b, 0) || 1;
      let done = 0;
      await this.refreshDownloadAll(0);
      for (let i = 0; i < missing.length; i++) {
        await pm.ensurePack(missing[i]);
        done += sizes[i];
        await this.refreshDownloadAll(Math.round((done / total) * 1000));
      }
      say("All downloaded ✓", "资源已全部下载 ✓", "資源已全部下載 ✓");
    } catch (err) {
      console.warn("[download-all]", err);
      say("Download failed — try again from Options", "资源下载失败，请稍后在选项中重试", "資源下載失敗，請稍後在選項中重試");
    } finally {
      this._downloadAllRunning = false;
      await this.refreshDownloadAll(-1);
    }
  }

  wireHud() {
    const { core, hud } = this;
    hud.onStartNewGame = async () => {
      this.inTitle = false;
      core.newGame();
    };
    hud.onReturnToTitle = async () => {
      this.keysDown.clear();
      this.pointerDragActive = false;
      this.inTitle = true;
      core._bumpSession?.();
      this.wasm.exports.core_set_in_title(1);
      await this.renderer.ensureFbp(60);
      this.audio.playMusic(4, true);
      hud.refreshSaveSlotSummaries();
      hud.showTitleScreen();
    };
    hud.onSaveSlot = (slot) => {
      try {
        core.saveSlot(slot);
        hud.refreshSaveSlotSummaries();
        hud.showToast(
          hud.lang === "en"
            ? `Saved to Slot ${slot + 1}`
            : hud.lang === "hans"
              ? `已保存至进度 ${slot + 1}`
              : `已儲存至進度 ${slot + 1}`
        );
      } catch {
        hud.showToast(hud.lang === "en" ? "Save failed" : "儲存失敗");
      }
    };
    hud.onLoadSlot = async (slot) => {
      let raw = null;
      try {
        raw = localStorage.getItem(`${SAVE_PREFIX}${slot}`);
      } catch {
        raw = null;
      }
      let ok = Boolean(raw) && core.loadSaveText(raw);
      if (!ok && slot === 0) {
        let buf = null;
        if (this.packManager) {
          const u8 = await this.packManager.readBytes("orig_saves/10.rpg");
          if (u8) buf = u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength);
        }
        if (!buf) {
          buf = await fetchArrayBuffer(this.renderer.baseUrl, "orig_saves/10.rpg");
        }
        ok = Boolean(buf) && core.loadRpg(buf);
      }
      if (ok) {
        this.inTitle = false;
        hud.hideTitleScreen();
        hud.showToast(
          hud.lang === "en"
            ? `Loaded Slot ${slot + 1}`
            : hud.lang === "hans"
              ? `已读取进度 ${slot + 1}`
              : `已讀取進度 ${slot + 1}`
        );
      } else {
        if (this.inTitle) hud.showTitleScreen();
        hud.showToast(
          hud.lang === "en"
            ? `Slot ${slot + 1} is empty`
            : hud.lang === "hans"
              ? `进度 ${slot + 1} 尚无存档`
              : `進度 ${slot + 1} 尚無存檔`
        );
      }
    };
    hud.onExportSave = () => core.exportSaveFile();
    hud.onDownloadAllAssets = () => this.startDownloadAll();
    hud.onImportSave = async () => {
      if (await core.importSaveFile()) this.inTitle = false;
    };
    hud.onCopyReplayReport = () => core.openBugReportDialog(L("系统菜单导出录像", "系統選單匯出錄像", "Replay exported from system menu"));
    hud.onLanguageChanged = (lang) => core.setLang(lang);
    hud.onOpenReplay = () => this.replayTheater?.openLibrary();
    hud.onReplayJumpChapter = (idx) => this.replayTheater?.jumpToChapter(idx);
    hud.onReplayTakeOver = () => this.replayTheater?.takeOver();
    hud.onReplayRestart = () => this.replayTheater?.restartFromBeginning();
    hud.onRestartRecording = () => {
      this.wasm.exports.core_restart_recording?.();
      hud.showToast(L("已从此刻重新开始录制！", "已從此刻重新開始錄製！", "Recording restarted from here!"));
    };
    hud.onExportReplay = async () => {
      const len = this.wasm.exports.core_export_replay_v3?.() || 0;
      if (!len) {
        hud.showToast(L("当前没有可导出的录像", "目前沒有可匯出的錄像", "No replay to export yet"));
        return;
      }
      const bytes = core._buf(len).slice();
      const doc = JSON.parse(new TextDecoder().decode(bytes));
      const ts = Date.now();
      const lastSeg = doc.segments?.[doc.segments.length - 1];
      await saveLocalReplay({
        id: `local_${ts}`,
        savedAt: ts,
        title: `${L("本地录像", "本地錄像", "Local replay")} (${new Date(ts).toLocaleString()})`,
        subtitle: lastSeg?.label || "",
        segments: doc.segments?.length || 1,
        ticks: doc.total_ticks || 0,
        bytes,
      });
      hud.showToast(L("已保存录像至本地录像库！", "已儲存錄像至本地錄像庫！", "Replay saved to the local library!"));
    };
  }

  /** Classic DOS PAL diagonal mapping: Down/Num2=SW(0), Left/Num4=NW(1), Up/Num8=NE(2), Right/Num6=SE(3) (#274). */
  getHeldDirection() {
    if (this.touchDir >= 0) return this.touchDir;
    const gpDir = this.gamepad?.getHeldDirection?.() ?? -1;
    if (gpDir >= 0) return gpDir;
    const k = this.keysDown;
    if (k.has("arrowdown") || k.has("numpad2")) return 0;
    if (k.has("arrowleft") || k.has("numpad4")) return 1;
    if (k.has("arrowup") || k.has("numpad8")) return 2;
    if (k.has("arrowright") || k.has("numpad6")) return 3;
    return -1;
  }

  uiActive() {
    return this.hud.isTitleVisible() || this.hud.isModalOpen();
  }

  coreBusy() {
    return this.wasm.exports.core_is_busy() === 1;
  }

  onKeyDown(e) {
    if (e.target && (e.target.tagName === "INPUT" || e.target.tagName === "TEXTAREA")) {
      if (e.key === "Escape") e.target.blur();
      return;
    }
    // Never hijack browser/OS shortcuts (Ctrl/Cmd+R, Ctrl/Cmd+W, Alt+Tab, F12, etc.),
    // while allowing standalone Left Ctrl (SDLPAL Confirm) and Alt (SDLPAL Menu/Cancel, #274).
    const isStandaloneLeftCtrl = e.code === "ControlLeft" && !e.altKey && !e.metaKey;
    const isStandaloneAlt = (e.code === "AltLeft" || e.code === "AltRight") && !e.ctrlKey && !e.metaKey;
    if (e.key === "F12" || e.metaKey) {
      return;
    }
    if ((e.ctrlKey && !isStandaloneLeftCtrl) || (e.altKey && !isStandaloneAlt)) {
      return;
    }
    this.gamepad?.markInputMode("keyboard");
    if (e.key === "Escape" && this.webAgent?.isBusy?.()) {
      e.preventDefault();
      this.webAgent.cancel();
      return;
    }
    // In-game Replay Library keyboard/remote navigation
    if (this.replayTheater?.isLibraryOpen()) {
      e.preventDefault();
      if (e.key === "ArrowUp" || e.code === "Numpad8") this.replayTheater.moveLibraryCursor(-1);
      else if (e.key === "ArrowDown" || e.code === "Numpad2") this.replayTheater.moveLibraryCursor(1);
      else if (e.key === "Enter" || e.code === "NumpadEnter" || e.key === " " || isStandaloneLeftCtrl) {
        this.replayTheater.confirmLibrarySelection();
      } else if (e.key === "Escape" || e.key === "Backspace" || e.code === "Insert" || e.code === "Numpad0") {
        this.replayTheater.closeLibrary();
      }
      return;
    }
    if (e.key === "F2") {
      e.preventDefault();
      if (!e.repeat) this.hud.toggleHdMode();
      return;
    }
    if (e.key === "F8") {
      e.preventDefault();
      if (!e.repeat) {
        if (this.replayTheater?.active) this.replayTheater.takeOver();
        else this.replayTheater?.openLibrary();
      }
      return;
    }
    if (this.replayTheater?.active) {
      if (this.replayTheater.isReplayMenuOpen()) {
        e.preventDefault();
        this.core.uiKey(e.code);
        return;
      }
      e.preventDefault();
      if (e.key === "Escape") { if (!e.repeat) this.replayTheater.openReplayMenu(); }
      else if (e.key === "Tab") { if (!e.repeat) this.replayTheater.openChapters(); }
      else if (e.key === " ") { if (!e.repeat) this.replayTheater.togglePlayPause(); }
      else if (e.key === "+" || e.key === "=") this.replayTheater.speedUp();
      else if (e.key === "-") this.replayTheater.speedDown();
      else if (e.key === "[") this.replayTheater.prevSegment();
      else if (e.key === "]") this.replayTheater.nextSegment();
      else if (e.key === "F7") { if (!e.repeat) this.replayTheater.toggleThoughts(); }
      else if (e.key === "F10") { if (!e.repeat) this.hud.takeScreenshot(); }
      return;
    }
    if (e.key === "F7") {
      e.preventDefault();
      if (!e.repeat) this.replayTheater?.toggleThoughts();
      return;
    }
    if (e.key === "F10") {
      e.preventDefault();
      if (!e.repeat) this.hud.takeScreenshot();
      return;
    }
    if (e.key === "F9") {
      e.preventDefault();
      if (!e.repeat) this.core.openBugReportDialog("F9");
      return;
    }
    if (this.renderer.activeRngFrameImg && (e.key === "Escape" || e.key === " " || e.key === "Enter" || e.code === "NumpadEnter" || isStandaloneLeftCtrl)) {
      e.preventDefault();
      this.cutsceneSkip = true;
      return;
    }
    if (this.uiActive()) {
      e.preventDefault();
      this.keysDown.clear();
      this.core.uiKey(e.code);
      return;
    }
    if (e.key === "Escape" || e.code === "Insert" || e.code === "Numpad0" || isStandaloneAlt) {
      e.preventDefault();
      if (!e.repeat && !this.coreBusy()) {
        this.keysDown.clear();
        this.core.toggleRootMenu(true);
      }
      return;
    }
    if (e.key === " " || e.key === "Enter" || e.code === "NumpadEnter" || e.code === "KeyZ" || isStandaloneLeftCtrl) {
      e.preventDefault();
      if (!e.repeat) this.core.queueInteract();
      return;
    }
    const keyLower = (e.code || e.key || "").toLowerCase();
    if (
      keyLower === "arrowup" ||
      keyLower === "arrowdown" ||
      keyLower === "arrowleft" ||
      keyLower === "arrowright" ||
      keyLower === "numpad2" ||
      keyLower === "numpad4" ||
      keyLower === "numpad6" ||
      keyLower === "numpad8"
    ) {
      e.preventDefault();
    }
    if (!e.repeat) {
      this.keysDown.add(keyLower);
    }
  }

  wireInputs() {
    window.addEventListener("keydown", (e) => this.onKeyDown(e));
    window.addEventListener("keyup", (e) => {
      this.keysDown.delete((e.code || "").toLowerCase());
      this.keysDown.delete((e.key || "").toLowerCase());
    });
    // Never leave movement keys or a pointer drag stuck when focus is lost (#41, #12).
    const release = () => {
      this.keysDown.clear();
      this._releaseTouchDpad();
      this.pointerDragActive = false;
      this.activePointerId = null;
    };
    window.addEventListener("blur", release);
    window.addEventListener("pointerup", (e) => {
      if (this.activePointerId == null || e.pointerId === this.activePointerId) {
        this.pointerDragActive = false;
        this.activePointerId = null;
      }
    });
    window.addEventListener("pointercancel", (e) => {
      if (this.activePointerId == null || e.pointerId === this.activePointerId) {
        this.pointerDragActive = false;
        this.activePointerId = null;
      }
    });
    document.addEventListener?.("visibilitychange", () => document.hidden && release());

    // Wire single-button system dropdown menu (#7)
    const menuWrap = document.getElementById("pal-stage-menu-wrap");
    const menuBtn = document.getElementById("btn-stage-menu");
    const closeStageMenu = () => {
      menuWrap?.classList?.remove("open");
      menuBtn?.setAttribute?.("aria-expanded", "false");
    };
    menuBtn?.addEventListener("click", (e) => {
      e.stopPropagation();
      const open = menuWrap?.classList?.toggle("open");
      menuBtn.setAttribute("aria-expanded", open ? "true" : "false");
    });
    window.addEventListener("pointerdown", (e) => {
      if (menuWrap?.classList?.contains("open") && !menuWrap.contains(e.target)) {
        closeStageMenu();
      }
    });

    // Wire visible Web Replay Library / `.repal` loader button (#2)
    const replayBtn = document.getElementById("btn-stage-replay");
    replayBtn?.addEventListener("click", () => {
      closeStageMenu();
      if (this.replayTheater?.active) this.replayTheater.openReplayMenu();
      else this.replayTheater?.openLibrary();
    });

    // Wire mobile virtual touch controls & toggle (#1)
    this._wireMobileTouchControls(closeStageMenu);

    // Wire in-game bug report toolbar button (F9 shortcut)
    const bugBtn = document.getElementById("btn-stage-bug");
    bugBtn?.addEventListener("click", () => {
      closeStageMenu();
      this.core.openBugReportDialog("Toolbar");
    });

    // Wire fullscreen / cinema-fill button (#17) + landscape orientation lock on mobile
    const enterFullscreenLandscape = () => {
      closeStageMenu();
      const stage = document.getElementById("game-stage") || document.documentElement;
      const tryLock = () => {
        try {
          screen.orientation?.lock?.("landscape")?.catch?.(() => {});
        } catch {}
      };
      if (stage?.requestFullscreen) {
        stage
          .requestFullscreen()
          .then(tryLock)
          .catch(() => {
            document.body?.classList?.toggle("pal-cinema-fill");
            tryLock();
          });
      } else {
        document.body?.classList?.toggle("pal-cinema-fill");
        tryLock();
      }
    };
    const fsBtn = document.getElementById("btn-stage-fullscreen");
    fsBtn?.addEventListener("click", () => {
      closeStageMenu();
      if (document.fullscreenElement) {
        document.exitFullscreen?.().catch?.(() => {});
        document.body?.classList?.remove("pal-cinema-fill");
      } else {
        enterFullscreenLandscape();
      }
    });
    document.getElementById("btn-force-landscape")?.addEventListener("click", enterFullscreenLandscape);

    // Classic desktop right-click: step back in menus, otherwise open the root menu.
    // On touch devices, long-pressing the virtual D-Pad or canvas fires `contextmenu` —
    // suppress the browser menu but NEVER open the in-game menu from a touch long-press
    // (touch devices use the B button or two-finger tap instead).
    let lastTouchTime = 0;
    window.addEventListener(
      "touchstart",
      () => {
        lastTouchTime = Date.now();
      },
      { passive: true, capture: true }
    );
    document.getElementById("game-stage")?.addEventListener("contextmenu", (e) => {
      if (e.target && (e.target.tagName === "INPUT" || e.target.tagName === "TEXTAREA")) return;
      e.preventDefault();
      if (
        e.pointerType === "touch" ||
        e.pointerType === "pen" ||
        this.touchDpadPointerId != null ||
        e.target?.closest?.("#mobile-touch-controls") ||
        Date.now() - lastTouchTime < 1500
      ) {
        return;
      }
      if (this.uiActive()) this.core.uiKey("Escape");
      else if (!this.coreBusy()) this.core.toggleRootMenu(true);
    });

    // Drag-and-drop `.repal` / `.json` replay files directly onto the browser window
    window.addEventListener("dragover", (e) => e.preventDefault());
    window.addEventListener("drop", async (e) => {
      e.preventDefault();
      const file = e.dataTransfer?.files?.[0];
      if (!file) return;
      const lower = file.name.toLowerCase();
      if (lower.endsWith(".repal") || lower.endsWith(".gz") || lower.endsWith(".json")) {
        try {
          await this.replayTheater?.startFromBytes(file.name, new Uint8Array(await file.arrayBuffer()));
        } catch (err) {
          this.hud.showToast(`${L("加载录像失败", "載入錄像失敗", "Failed to load replay")}: ${err.message}`);
        }
      }
    });

    this.canvas.addEventListener("pointermove", (e) => {
      this.gamepad?.markInputMode("mouse");
      this.replayTheater?.touchUi?.();
      if (this.pointerDragActive && this.activePointerId != null && e.pointerId !== this.activePointerId) return;
      this.pointerClientPos = { x: e.clientX, y: e.clientY };
      if (this.uiActive()) this.core.uiPointer(e, POINTER_KIND.Move);
    });

    // Two-finger tap toggles the in-game menu on touch devices (#42).
    this.canvas.addEventListener(
      "touchstart",
      (e) => {
        if (e.touches?.length === 2 && !this.inTitle && !this.coreBusy()) {
          e.preventDefault();
          this.pointerDragActive = false;
          this.activePointerId = null;
          this.core.toggleRootMenu(!this.hud.isModalOpen());
        }
      },
      { passive: false }
    );

    // Click-to-walk (A* in the core), press-and-move steering (#12) and UI clicks.
    this.canvas.addEventListener("pointerdown", (e) => {
      if (typeof e.button === "number" && e.button !== 0) return;
      if (this.renderer.activeRngFrameImg) {
        this.cutsceneSkip = true;
      } else if (this.uiActive()) {
        this.pointerDragActive = false;
        this.activePointerId = null;
        this.core.uiPointer(e, POINTER_KIND.Down);
      } else if (!this.coreBusy()) {
        this.pointerDragActive = true;
        this.activePointerId = e.pointerId ?? null;
        this.pointerClientPos = { x: e.clientX, y: e.clientY };
        const { worldX, worldY } = this.core.clientToWorld(e.clientX, e.clientY);
        this.core.queueClick(worldX, worldY);
      }
    });
  }

  _releaseTouchDpad() {
    this.touchDir = -1;
    this.touchDpadPointerId = null;
    if (this.touchUiRepeatTimer) {
      clearInterval(this.touchUiRepeatTimer);
      this.touchUiRepeatTimer = null;
    }
    const nub = document.getElementById("touch-dpad-nub");
    if (nub) nub.style.transform = "";
    document.querySelectorAll?.(".touch-dpad-arrow.active")?.forEach?.((el) => el.classList.remove("active"));
  }

  _wireMobileTouchControls(onAfterToggle = null) {
    const TOUCH_PREF_KEY = "repal_touch_pad_v1";
    const toggleBtn = document.getElementById("btn-stage-touchpad");
    const isCoarse = typeof window !== "undefined" && window.matchMedia?.("(hover: none) and (pointer: coarse)")?.matches;
    try {
      const saved = localStorage.getItem(TOUCH_PREF_KEY);
      if (saved === "0") document.body?.classList?.add("touch-pad-hidden");
      else if (saved === "1") document.body?.classList?.add("touch-pad-shown");
    } catch {}
    toggleBtn?.addEventListener("click", () => {
      onAfterToggle?.();
      const currentlyHidden =
        document.body.classList.contains("touch-pad-hidden") ||
        (!isCoarse && !document.body.classList.contains("touch-pad-shown"));
      if (currentlyHidden) {
        document.body.classList.remove("touch-pad-hidden");
        document.body.classList.add("touch-pad-shown");
        try { localStorage.setItem(TOUCH_PREF_KEY, "1"); } catch {}
      } else {
        document.body.classList.remove("touch-pad-shown");
        document.body.classList.add("touch-pad-hidden");
        this._releaseTouchDpad();
        try { localStorage.setItem(TOUCH_PREF_KEY, "0"); } catch {}
      }
    });

    const touchRoot = document.getElementById("mobile-touch-controls");
    touchRoot?.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
    });
    // Suppress OS long-press haptic vibration & callout on Android/iOS touch controls (#7)
    const preventNativeLongPress = (el) => {
      el?.addEventListener("touchstart", (e) => e.preventDefault(), { passive: false });
      el?.addEventListener("touchmove", (e) => e.preventDefault(), { passive: false });
      el?.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
      });
    };

    const dpad = document.getElementById("touch-dpad");
    preventNativeLongPress(dpad);
    preventNativeLongPress(document.getElementById("touch-btn-a"));
    preventNativeLongPress(document.getElementById("touch-btn-b"));
    const nub = document.getElementById("touch-dpad-nub");
    const dirToKey = ["ArrowDown", "ArrowLeft", "ArrowUp", "ArrowRight"];

    const updateDpadFromClient = (clientX, clientY, isFreshPress) => {
      if (!dpad) return;
      const rect = dpad.getBoundingClientRect();
      const cx = rect.left + rect.width / 2;
      const cy = rect.top + rect.height / 2;
      const dx = clientX - cx;
      const dy = clientY - cy;
      const dist = Math.hypot(dx, dy);
      const maxR = rect.width * 0.34;
      if (nub) {
        const clamped = Math.min(dist, maxR);
        const angle = Math.atan2(dy, dx);
        nub.style.transform = `translate(${(Math.cos(angle) * clamped).toFixed(1)}px, ${(Math.sin(angle) * clamped).toFixed(1)}px)`;
      }
      if (dist < 10) {
        this.touchDir = -1;
        document.querySelectorAll?.(".touch-dpad-arrow.active")?.forEach?.((el) => el.classList.remove("active"));
        return;
      }
      const nextDir = Math.abs(dx) >= Math.abs(dy) ? (dx > 0 ? 3 : 1) : (dy > 0 ? 0 : 2);
      const changed = nextDir !== this.touchDir;
      this.touchDir = nextDir;
      document.querySelectorAll?.(".touch-dpad-arrow")?.forEach?.((el) => {
        el.classList.toggle("active", Number(el.dataset.dir) === nextDir);
      });
      if ((isFreshPress || changed) && this.uiActive()) {
        this.core.uiKey(dirToKey[nextDir]);
        if (this.touchUiRepeatTimer) clearInterval(this.touchUiRepeatTimer);
        this.touchUiRepeatTimer = setInterval(() => {
          if (this.touchDir >= 0 && this.uiActive()) {
            this.core.uiKey(dirToKey[this.touchDir]);
          }
        }, 190);
      }
    };

    dpad?.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.touchDpadPointerId = e.pointerId;
      dpad.setPointerCapture?.(e.pointerId);
      updateDpadFromClient(e.clientX, e.clientY, true);
    });
    dpad?.addEventListener("pointermove", (e) => {
      if (this.touchDpadPointerId == null || e.pointerId !== this.touchDpadPointerId) return;
      e.preventDefault();
      updateDpadFromClient(e.clientX, e.clientY, false);
    });
    const endDpad = (e) => {
      if (this.touchDpadPointerId != null && e.pointerId === this.touchDpadPointerId) {
        this._releaseTouchDpad();
      }
    };
    dpad?.addEventListener("pointerup", endDpad);
    dpad?.addEventListener("pointercancel", endDpad);

    const btnA = document.getElementById("touch-btn-a");
    btnA?.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      e.stopPropagation();
      btnA.classList.add("pressed");
      if (this.replayTheater?.isLibraryOpen()) {
        this.replayTheater.confirmLibrarySelection();
      } else if (this.renderer.activeRngFrameImg) {
        this.cutsceneSkip = true;
      } else if (this.uiActive()) {
        this.core.uiKey("Enter");
      } else if (!this.coreBusy()) {
        this.core.queueInteract();
      }
    });
    const releaseA = () => btnA?.classList.remove("pressed");
    btnA?.addEventListener("pointerup", releaseA);
    btnA?.addEventListener("pointercancel", releaseA);

    const btnB = document.getElementById("touch-btn-b");
    btnB?.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      e.stopPropagation();
      btnB.classList.add("pressed");
      if (this.replayTheater?.isLibraryOpen()) {
        this.replayTheater.closeLibrary();
      } else if (this.replayTheater?.active && !this.replayTheater.isReplayMenuOpen()) {
        this.replayTheater.openReplayMenu();
      } else if (this.renderer.activeRngFrameImg) {
        this.cutsceneSkip = true;
      } else if (this.uiActive()) {
        this.core.uiKey("Escape");
      } else if (!this.coreBusy()) {
        this.core.toggleRootMenu(true);
      }
    });
    const releaseB = () => btnB?.classList.remove("pressed");
    btnB?.addEventListener("pointerup", releaseB);
    btnB?.addEventListener("pointercancel", releaseB);
  }

  frameLoop(now) {
    this.gamepad?.poll(now);
    this.core.frame(now);
    requestAnimationFrame((t) => this.frameLoop(t));
  }
}

window.addEventListener("DOMContentLoaded", () => {
  new PalRemakeApp().boot();
});
