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
import { PackManager } from "./host/pack-manager.js";
import { GamepadController } from "./host/gamepad.js";
import { WebReplayTheater } from "./host/replay-theater.js";
import { saveLocalReplay } from "./host/repal-loader.js";

const DEFAULT_PACK_MANIFEST_URL =
  "https://cdn.jsdelivr.net/gh/HclX/RePal_Packs@packs-v2/pack-manifest.json";
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
    inp.placeholder = "输入 64 位资产包密钥 (Enter 64-hex pack key)";
    inp.style.cssText =
      "width:320px;padding:7px 10px;border-radius:6px;border:1px solid #56c2b1;background:#0d171c;color:#f3ecdc;font-size:13px;font-family:monospace;";
    const btn = document.createElement("button");
    btn.textContent = "解锁并启动 / Unlock";
    btn.style.cssText =
      "padding:7px 14px;border-radius:6px;border:0;background:#e0b96b;color:#142026;font-weight:600;cursor:pointer;";
    const submit = () => {
      const clean = inp.value.trim().toLowerCase();
      if (!/^[0-9a-f]{64}$/.test(clean)) {
        if (statusEl) statusEl.textContent = "密钥格式无效：需要 64 位十六进制字符 (64 hex chars)";
        return;
      }
      try {
        localStorage.setItem(PACK_KEY_STORAGE_KEY, clean);
      } catch {}
      box.remove();
      resolve(clean);
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
      this.setBootProgress(0.1, "正在唤醒 Rust WebAssembly 引擎核心...");
      this.wasm = new PalWasmBridge();
      await this.wasm.init(withCodeVersion("./wasm/pal.wasm"));

      this.setBootProgress(0.2, "正在挂载 YAML 剧情 Mod 扩展层...");
      this.modLoader = new ModLoader();
      await this.modLoader.init();

      this.renderer = new WasmRendererAdapter(this.canvas, this.wasm);
      this.audio = new AudioManager(this.wasm);
      this.audio.installGestureUnlock();
      this.hud = new WasmHudAdapter({ wasm: this.wasm, renderer: this.renderer, audio: this.audio });

      // Option A (#237): prefer local loose `./assets/` in dev; otherwise mount encrypted `.rpk` packs.
      await this._initPackManagerIfNeeded();

      this.setBootProgress(0.3, "正在載入 WASM 字型引擎...");
      await this.renderer.initFonts();

      this.core = new RustCoreHost(this);
      await this.core.init((p, key) => this.setBootProgress(0.3 + p * 0.6, `正在载入游戏数据 (${key})...`));

      this.setBootProgress(0.95, "正在载入 4x 高清标题画卷...");
      await this.renderer.ensureFbp(60);
      this.hud.refreshSaveSlotSummaries();
      this.hud.showTitleScreen();
      this.audio.playMusic(4, true); // 《云谷鹤峰》原版标题曲 (MAINMENU_MUSIC = 4)
      this.packManager?.prefetchBootWarmup?.();

      this.wireHud();
      this.gamepad = new GamepadController(this);
      this.replayTheater = new WebReplayTheater(this);
      this.wireInputs();
      this.audio.installGestureUnlock();

      this.setBootProgress(1.0, "准备就绪！");
      setTimeout(() => this.loadingOverlay?.classList.add("hidden"), 180);
      window.__pal = this;

      const activeAssets = resolveAssetBaseUrl();
      if (isCustomAssetOrigin(activeAssets)) {
        this.hud.showToast(`使用自定义资源源：${activeAssets}`, 4200);
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
                  window.__pal?.hud?.showToast?.("发现新版本，刷新页面即可生效！");
                }
              });
            });
          })
          .catch(() => {});
      }

      // Optional URL auto-launch: `?replay=./replays/06_ch2_suzhou_full_to_hidden_dragon_cave.repal`
      const replayParam = new URLSearchParams(window.location.search).get("replay");
      if (replayParam) {
        this.replayTheater.startFromUrl(replayParam).catch((err) => {
          this.hud.showToast(`加载录像失败：${err.message}`);
        });
      }

      requestAnimationFrame((t) => this.frameLoop(t));
    } catch (err) {
      console.error("[PalRemake Boot Error]", err);
      if (this.loadingText) {
        const cur = resolveAssetBaseUrl();
        this.loadingText.textContent = `启动失败: ${err.message} (当前资源源: ${cur})`;
        if (typeof document !== "undefined" && this.loadingOverlay && !document.getElementById("asset-url-setup")) {
          const box = document.createElement("div");
          box.id = "asset-url-setup";
          box.style.cssText = "margin-top:12px;display:flex;gap:8px;align-items:center;flex-wrap:wrap;justify-content:center;";
          const inp = document.createElement("input");
          inp.type = "text";
          inp.placeholder = "输入资源基地址 (如 https://.../assets 或 ./assets)";
          inp.value = cur === "./assets" ? "" : cur;
          inp.style.cssText = "width:320px;padding:6px 10px;border-radius:6px;border:1px solid #56c2b1;background:#0d171c;color:#f3ecdc;font-size:13px;";
          const btn = document.createElement("button");
          btn.textContent = "连接资源并重试";
          btn.style.cssText = "padding:6px 14px;border-radius:6px;border:0;background:#e0b96b;color:#142026;font-weight:600;cursor:pointer;";
          btn.onclick = () => {
            const v = inp.value.trim().replace(/\/+$/, "");
            try {
              if (!v) {
                localStorage.removeItem(ASSET_URL_STORAGE_KEY);
              } else {
                const valid = validateAssetBaseUrl(v);
                if (!valid) {
                  this.loadingText.textContent = "无效资源地址：仅允许相对路径 (./assets) 或 https:// 地址";
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
    this.setBootProgress(0.24, "正在连接加密资产包清单 (jsDelivr CDN)...");

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
            this.hud?.showToast?.("资产包已更新，请刷新页面重新加载！", 6000);
          },
        });
        await pm.init();
        this.packManager = pm;
        this.renderer.packManager = pm;
        this.renderer.presenter.assets.packManager = pm;
        this.audio.packManager = pm;
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
        const promptMsg = keyHex
          ? "密钥校验失败，请重新输入 64 位资产包密钥 (Invalid key, please retry)"
          : "当前资产包已加密，请输入 64 位资产包密钥（仅需输入一次）";
        keyHex = await promptPackKeyOverlay(this.loadingOverlay, this.loadingText, promptMsg);
        this.setBootProgress(0.26, "正在解密启动资产包...");
      }
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
    hud.onImportSave = async () => {
      if (await core.importSaveFile()) this.inTitle = false;
    };
    hud.onCopyReplayReport = () => core.openBugReportDialog("系統選單匯出錄像");
    hud.onLanguageChanged = (lang) => core.setLang(lang);
    hud.onOpenReplay = () => this.replayTheater?.openLibrary();
    hud.onReplayJumpChapter = (idx) => this.replayTheater?.jumpToChapter(idx);
    hud.onReplayTakeOver = () => this.replayTheater?.takeOver();
    hud.onReplayRestart = () => this.replayTheater?.restartFromBeginning();
    hud.onRestartRecording = () => {
      this.wasm.exports.core_restart_recording?.();
      hud.showToast("已从此刻重新开始录制！");
    };
    hud.onExportReplay = async () => {
      const len = this.wasm.exports.core_export_replay_v3?.() || 0;
      if (!len) {
        hud.showToast("当前没有可导出的录像");
        return;
      }
      const bytes = core._buf(len).slice();
      const doc = JSON.parse(new TextDecoder().decode(bytes));
      const ts = Date.now();
      const lastSeg = doc.segments?.[doc.segments.length - 1];
      await saveLocalReplay({
        id: `local_${ts}`,
        savedAt: ts,
        title: `本地录像 (${new Date(ts).toLocaleString()})`,
        subtitle: lastSeg?.label || "",
        segments: doc.segments?.length || 1,
        ticks: doc.total_ticks || 0,
        bytes,
      });
      hud.showToast("已保存录像至本地录像库（IndexedDB）！");
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

    // Wire visible Web Replay Library / `.repal` loader button (#2)
    const replayBtn = document.getElementById("btn-stage-replay");
    replayBtn?.addEventListener("click", () => {
      if (this.replayTheater?.active) this.replayTheater.openReplayMenu();
      else this.replayTheater?.openLibrary();
    });

    // Wire mobile virtual touch controls & toggle (#1)
    this._wireMobileTouchControls();

    // Wire in-game bug report toolbar button (F9 shortcut)
    const bugBtn = document.getElementById("btn-stage-bug");
    bugBtn?.addEventListener("click", () => {
      this.core.openBugReportDialog("Toolbar");
    });

    // Wire fullscreen / cinema-fill button (#17) + landscape orientation lock on mobile
    const enterFullscreenLandscape = () => {
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
          this.hud.showToast(`加载录像失败：${err.message}`);
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

  _wireMobileTouchControls() {
    const TOUCH_PREF_KEY = "repal_touch_pad_v1";
    const toggleBtn = document.getElementById("btn-stage-touchpad");
    const isCoarse = typeof window !== "undefined" && window.matchMedia?.("(hover: none) and (pointer: coarse)")?.matches;
    try {
      const saved = localStorage.getItem(TOUCH_PREF_KEY);
      if (saved === "0") document.body?.classList?.add("touch-pad-hidden");
      else if (saved === "1") document.body?.classList?.add("touch-pad-shown");
    } catch {}
    toggleBtn?.addEventListener("click", () => {
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

    const dpad = document.getElementById("touch-dpad");
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
