/**
 * Web TV Replay Theater & Couch-Friendly Replay Library (`site/js/host/replay-theater.js`).
 *
 * - Plays segmented `.repal` (`.tar.gz`) and `.json` replays with hard-load on every segment transition.
 * - Always-visible razor-thin (5px) translucent progress bar at the bottom of `#game-stage`.
 * - Auto-hiding (3s) control strip right above the progress bar — automatically switches between
 *   Gamepad glyph hints (`[A] 暂停 [LB/RB] 章节 [LT/RT] 倍速 [Y] 章节 [Start] 菜单`) and clickable buttons.
 * - Integrates natively with `pal-ui`'s exclusive Replay Menu (`Esc`/`Start`) & Chapters list with thumbnails!
 */

import { decodePngToRgba, loadReplayLibrary, parseRepalBytes } from "./repal-loader.js";
import { pickFile } from "./wasm-runtime.js";
import { L } from "./l10n.js";

const IDLE_HIDE_MS = 3000;
// Must stay in sync with `pal_core::game::replay_rec::MIN_COMPAT_REPLAY_VERSION`.
const MIN_COMPAT_REPLAY_VERSION = "0.4.0";

function parseSemverTriple(raw) {
  const clean = String(raw || "").trim().replace(/^v/i, "").split(/[-+]/)[0].trim();
  if (!clean) return null;
  const parts = clean.split(".");
  const maj = Number.parseInt(parts[0], 10);
  const min = Number.parseInt(parts[1] ?? "0", 10);
  const pat = Number.parseInt(parts[2] ?? "0", 10);
  if ([maj, min, pat].some((n) => Number.isNaN(n) || n < 0)) return null;
  return [maj, min, pat];
}

function isReplayEngineCompatible(recorded) {
  const rec = parseSemverTriple(recorded);
  const minReq = parseSemverTriple(MIN_COMPAT_REPLAY_VERSION);
  if (!rec) return false;
  if (!minReq) return true;
  for (let i = 0; i < 3; i++) {
    if (rec[i] !== minReq[i]) return rec[i] > minReq[i];
  }
  return true;
}

export class WebReplayTheater {
  constructor(app) {
    this.app = app;
    this.active = false;
    this.filename = "";
    this.bundle = null;
    this.versionMismatch = false;
    this.segIdx = 0;
    this.segTick = 0;
    this.evIdx = 0;
    this.annIdx = 0;
    this.paused = false;
    this.finished = false;
    this.speed = 1;
    this.heldDir = -1;
    this.steer = null;
    this.lastActivity = 0;
    this.pointerInBottom = false;
    this.dialogHoldUntil = 0;
    this.transitionUntil = 0;
    this.hasActiveThought = false;
    this.lastThoughtAt = 0;
    const thoughtParam = (new URLSearchParams(window.location.search).get("thoughts") || "")
      .trim()
      .toLowerCase();
    this.showThoughts = !(
      thoughtParam === "0" ||
      thoughtParam === "false" ||
      thoughtParam === "off"
    );
    this.app.core?.w?.core_set_show_ai_thought?.(this.showThoughts ? 1 : 0);

    // Library overlay state
    this.libraryOpen = false;
    this.libraryItems = [];
    this.libraryCursor = 0;

    this._mountDom();
  }

  _mountDom() {
    const stage = document.getElementById("game-stage");
    if (!stage) return;

    // Top-center compatibility warning pill for legacy replays (< MIN_COMPAT_REPLAY_VERSION)
    const compatBanner = document.createElement("div");
    compatBanner.id = "repal-compat-banner";
    compatBanner.style.cssText =
      "position:absolute;top:12px;left:50%;transform:translateX(-50%);z-index:26;display:none;pointer-events:none;padding:5px 14px;border-radius:999px;background:rgba(43,26,8,0.88);border:1px solid rgba(240,168,72,0.85);color:#ffd27d;font-size:12px;letter-spacing:0.4px;white-space:nowrap;box-shadow:0 4px 16px rgba(0,0,0,0.45);font-family:'Noto Serif SC',system-ui,sans-serif;";
    stage.appendChild(compatBanner);
    this.elCompatBanner = compatBanner;

    // Bottom theater overlay (always-visible 5px thin bar + auto-hiding spacious 3-zone control strip)
    const barWrap = document.createElement("div");
    barWrap.id = "repal-theater-overlay";
    barWrap.style.cssText =
      "position:absolute;left:0;right:0;bottom:0;z-index:25;display:none;pointer-events:none;font-family:'Noto Serif SC',system-ui,sans-serif;";
    barWrap.innerHTML = `
      <div id="repal-ctrl-strip" style="display:flex;align-items:center;justify-content:space-between;gap:16px;padding:8px 18px;background:rgba(10,20,30,0.86);backdrop-filter:blur(6px);border-top:1px solid rgba(200,164,88,0.55);color:#f2e8d0;font-size:13px;pointer-events:auto;transition:opacity 0.25s ease;">
        <div id="repal-mouse-btns" style="display:flex;align-items:center;justify-content:space-between;flex:1;gap:16px;min-width:0;">
          <div style="display:flex;align-items:center;gap:8px;">
            <button data-act="prev" class="repal-hud-btn repal-icon-btn" title="${L("上一章节", "上一章節", "Previous chapter")} ([)">⏮</button>
            <button data-act="play" class="repal-hud-btn repal-icon-btn repal-play-btn" id="repal-btn-play" title="${L("暂停/播放", "暫停/播放", "Pause/Play")} (Space)">⏸</button>
            <button data-act="next" class="repal-hud-btn repal-icon-btn" title="${L("下一章节", "下一章節", "Next chapter")} (])">⏭</button>
            <button data-act="speed" class="repal-hud-btn" id="repal-btn-speed" style="margin-left:8px;min-width:42px;" title="${L("切换倍速", "切換倍速", "Change speed")} (+/-)">1×</button>
          </div>
          <div id="repal-status-txt" style="color:#d8e8f4;font-variant-numeric:tabular-nums;white-space:nowrap;font-size:12px;letter-spacing:0.5px;">00:00 / 00:00 · 1/1</div>
          <div style="display:flex;align-items:center;gap:10px;">
            <button data-act="thoughts" class="repal-hud-btn" id="repal-btn-thoughts" title="${L("开关 AI 思考字幕", "開關 AI 思考字幕", "Toggle AI thought captions")} (F7)">${L("字幕:开", "字幕:開", "Captions: on")}</button>
            <button data-act="chapters" class="repal-hud-btn" title="${L("章节列表", "章節列表", "Chapter list")} (Tab)">${L("章节", "章節", "Chapters")}</button>
            <button data-act="takeover" class="repal-hud-btn" style="border-color:#62d4a0;background:#1a3b2d;color:#dcf8ea;" title="${L("从此处接管游玩", "從此處接管遊玩", "Take over from here")} (F8)">${L("接管", "接管", "Take over")}</button>
            <button data-act="exit" class="repal-hud-btn" style="border-color:#d96b6b;background:#4a1e24;color:#f6d8d8;" title="${L("退出回放", "退出回放", "Exit replay")} (Esc)">${L("退出", "退出", "Exit")}</button>
          </div>
        </div>
        <div id="repal-gp-hints" style="display:none;align-items:center;justify-content:space-between;flex:1;gap:14px;font-size:13px;color:#f2e8d0;">
          <div style="display:flex;align-items:center;gap:14px;">
            <span><b style="color:#e0b96b;">[A]</b> ${L("暂停/播放", "暫停/播放", "Pause/Play")}</span>
            <span><b style="color:#e0b96b;">[LB/RB]</b> ${L("上/下章", "上/下章", "Prev/Next")}</span>
            <span><b style="color:#e0b96b;">[LT/RT]</b> ${L("倍速", "倍速", "Speed")}</span>
            <span><b style="color:#e0b96b;">[Y]</b> ${L("章节", "章節", "Chapters")}</span>
            <span><b style="color:#e0b96b;">[Start]</b> ${L("菜单/接管", "選單/接管", "Menu/Take over")}</span>
          </div>
          <div id="repal-gp-status-txt" style="color:#d8e8f4;font-variant-numeric:tabular-nums;white-space:nowrap;font-size:12px;">00:00 / 00:00</div>
        </div>
      </div>
      <div id="repal-thin-track" style="height:5px;background:rgba(8,16,24,0.72);cursor:pointer;pointer-events:auto;position:relative;">
        <div id="repal-thin-fill" style="height:100%;width:0%;background:#d8b25c;"></div>
      </div>
    `;
    stage.appendChild(barWrap);

    // Inline minimal button style
    if (!document.getElementById("repal-theater-style")) {
      const st = document.createElement("style");
      st.id = "repal-theater-style";
      st.textContent = `
        .repal-hud-btn {
          background:rgba(22,38,54,0.92);color:#ebe2ce;border:1px solid rgba(200,164,88,0.58);border-radius:6px;
          padding:5px 11px;font-size:12px;cursor:pointer;font-family:inherit;line-height:1.1;transition:background 0.12s ease,border-color 0.12s ease;
        }
        .repal-hud-btn:hover { background:#274058;border-color:#f0cb72; }
        .repal-icon-btn { min-width:34px;padding:5px 8px;font-size:14px;text-align:center; }
        .repal-play-btn { min-width:40px;background:rgba(38,63,86,0.96);border-color:#f0cb72;color:#fff2cc; }
        #repal-library-modal {
          position:absolute;inset:0;z-index:40;display:none;align-items:center;justify-content:center;
          background:rgba(6,10,16,0.82);backdrop-filter:blur(4px);font-family:'Noto Serif SC',system-ui,sans-serif;
        }
        .repal-lib-card {
          width:min(760px,92vw);max-height:84vh;display:flex;flex-direction:column;gap:12px;
          background:#101a24;border:2px solid #c8a458;border-radius:8px;padding:18px 20px;color:#f2e8d0;
          box-shadow:0 16px 48px rgba(0,0,0,0.65);
        }
        .repal-lib-list { overflow-y:auto;max-height:54vh;display:flex;flex-direction:column;gap:8px; }
        .repal-lib-row {
          display:flex;justify-content:space-between;align-items:center;gap:12px;
          padding:10px 14px;border:1px solid rgba(200,164,88,0.3);border-radius:6px;
          background:rgba(24,38,52,0.7);cursor:pointer;
        }
        .repal-lib-row.selected {
          border-color:#f0cb72;background:rgba(200,164,88,0.22);box-shadow:0 0 0 1px #f0cb72;
        }
      `;
      document.head.appendChild(st);
    }

    // Couch-friendly Replay Library Modal
    const modal = document.createElement("div");
    modal.id = "repal-library-modal";
    modal.innerHTML = `
      <div class="repal-lib-card">
        <div style="display:flex;justify-content:space-between;align-items:center;">
          <div style="font-size:18px;color:#f0cb72;font-weight:700;letter-spacing:2px;">${L("录像回放", "錄像回放", "Replay Library")} · RePal</div>
          <div style="font-size:12px;color:#9eb5c4;">↑/↓ ${L("选择", "選擇", "Select")} · A/Enter ${L("播放", "播放", "Play")} · B/Esc ${L("返回", "返回", "Back")}</div>
        </div>
        <div id="repal-lib-list" class="repal-lib-list"></div>
        <div style="display:flex;justify-content:space-between;align-items:center;gap:8px;margin-top:4px;">
          <button id="repal-lib-pick-file" class="repal-hud-btn" style="padding:6px 12px;">${L("打开本地 .repal / .json 文件...", "開啟本地 .repal / .json 檔案...", "Open a local .repal / .json file...")}</button>
          <button id="repal-lib-close" class="repal-hud-btn" style="padding:6px 14px;">${L("关闭", "關閉", "Close")}</button>
        </div>
      </div>
    `;
    stage.appendChild(modal);

    this.elOverlay = barWrap;
    this.elCtrlStrip = barWrap.querySelector("#repal-ctrl-strip");
    this.elMouseBtns = barWrap.querySelector("#repal-mouse-btns");
    this.elGpHints = barWrap.querySelector("#repal-gp-hints");
    this.elStatus = barWrap.querySelector("#repal-status-txt");
    this.elGpStatus = barWrap.querySelector("#repal-gp-status-txt");
    this.elBtnPlay = barWrap.querySelector("#repal-btn-play");
    this.elBtnSpeed = barWrap.querySelector("#repal-btn-speed");
    this.elBtnThoughts = barWrap.querySelector("#repal-btn-thoughts");
    this.elThinTrack = barWrap.querySelector("#repal-thin-track");
    this.elThinFill = barWrap.querySelector("#repal-thin-fill");
    this.elLibModal = modal;
    this.elLibList = modal.querySelector("#repal-lib-list");

    // Full-screen dim + centered CJK text banner for load / chapter jump transitions
    const banner = document.createElement("div");
    banner.id = "repal-transition-banner";
    banner.style.cssText =
      "position:absolute;inset:0;z-index:35;display:none;align-items:center;justify-content:center;background:rgba(6,10,16,0.78);pointer-events:none;font-family:'Noto Serif SC',system-ui,sans-serif;transition:opacity 0.18s ease;";
    banner.innerHTML = `<div id="repal-transition-text" style="padding:14px 36px;background:rgba(14,22,32,0.94);border:2px solid #d8b25c;border-radius:6px;color:#f5d782;font-size:20px;font-weight:700;letter-spacing:2px;box-shadow:0 12px 36px rgba(0,0,0,0.6);"></div>`;
    stage.appendChild(banner);
    this.elBanner = banner;
    this.elBannerText = banner.querySelector("#repal-transition-text");

    barWrap.addEventListener("pointerenter", () => {
      this.pointerInBottom = true;
      this.touchUi();
    });
    barWrap.addEventListener("pointerleave", () => {
      this.pointerInBottom = false;
    });
    barWrap.querySelectorAll("[data-act]").forEach((btn) => {
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        this.app.gamepad?.markInputMode("mouse");
        const act = btn.getAttribute("data-act");
        if (act === "prev") this.prevSegment();
        else if (act === "play") this.togglePlayPause();
        else if (act === "next") this.nextSegment();
        else if (act === "speed") this.cycleSpeed();
        else if (act === "thoughts") this.toggleThoughts();
        else if (act === "chapters") this.openChapters();
        else if (act === "takeover") this.takeOver();
        else if (act === "exit") this.exitToTitle();
      });
    });
    this.elThinTrack.addEventListener("click", (e) => {
      e.stopPropagation();
      const rect = this.elThinTrack.getBoundingClientRect();
      if (rect.width > 0) {
        const ratio = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
        this.seekByRatio(ratio);
      }
    });
    modal.querySelector("#repal-lib-pick-file")?.addEventListener("click", () => this.pickLocalFile());
    modal.querySelector("#repal-lib-close")?.addEventListener("click", () => this.closeLibrary());
  }

  // ---- Replay Library (Couch / TV) ----------------------------------------------------
  isLibraryOpen() {
    return this.libraryOpen;
  }

  async openLibrary() {
    this.libraryOpen = true;
    this.elLibModal.style.display = "flex";
    this.elLibList.innerHTML = `<div style="padding:18px;text-align:center;color:#9eb5c4;">${L("正在加载录像列表...", "正在載入錄像列表...", "Loading replays...")}</div>`;
    this.libraryItems = await loadReplayLibrary();
    this.libraryCursor = 0;
    this._renderLibraryList();
  }

  closeLibrary() {
    this.libraryOpen = false;
    this.elLibModal.style.display = "none";
  }

  moveLibraryCursor(delta) {
    const n = this.libraryItems.length;
    if (!n) return;
    this.libraryCursor = (this.libraryCursor + delta + n) % n;
    this._renderLibraryList();
  }

  async confirmLibrarySelection() {
    const item = this.libraryItems[this.libraryCursor];
    if (!item) return this.pickLocalFile();
    this.closeLibrary();
    try {
      let bytes = item.bytes;
      if (!bytes && item.url) {
        this.app.hud.showToast(`${L("正在加载录像", "正在載入錄像", "Loading replay")}: ${item.title}...`, 2000);
        const resp = await fetch(item.url);
        if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
        bytes = new Uint8Array(await resp.arrayBuffer());
      }
      await this.startFromBytes(item.title, bytes);
    } catch (err) {
      this.app.hud.showToast(`${L("加载录像失败", "載入錄像失敗", "Failed to load replay")}: ${err.message}`);
    }
  }

  async pickLocalFile() {
    const file = await pickFile(".repal,.json,.gz");
    if (!file) return;
    this.closeLibrary();
    try {
      await this.startFromBytes(file.name, new Uint8Array(await file.arrayBuffer()));
    } catch (err) {
      this.app.hud.showToast(`${L("加载失败", "載入失敗", "Load failed")}: ${err.message}`);
    }
  }

  _renderLibraryList() {
    this.elLibList.replaceChildren();
    if (!this.libraryItems.length) {
      const empty = document.createElement("div");
      empty.style.cssText = "padding:20px;text-align:center;color:#9eb5c4;";
      empty.textContent = L("暂无可用录像，可点击下方按钮打开本地 .repal 文件，或将 .repal 拖入游戏窗口。", "暫無可用錄像，可點擊下方按鈕開啟本地 .repal 檔案，或將 .repal 拖入遊戲視窗。", "No replays yet. Open a local .repal file below, or drop one onto the game window.");
      this.elLibList.appendChild(empty);
      return;
    }
    this.libraryItems.forEach((item, idx) => {
      const row = document.createElement("div");
      row.className = `repal-lib-row${idx === this.libraryCursor ? " selected" : ""}`;
      const secs = Math.floor((item.ticks || 0) / 50);
      const dur = item.ticks ? `${String(Math.floor(secs / 60)).padStart(2, "0")}:${String(secs % 60).padStart(2, "0")}` : "";

      // Construct DOM strictly via textContent so untrusted replay/index strings can never inject HTML (#256).
      const left = document.createElement("div");
      const titleEl = document.createElement("div");
      titleEl.style.cssText = `font-size:15px;color:${idx === this.libraryCursor ? "#f5d782" : "#f2e8d0"};font-weight:600;`;
      titleEl.appendChild(document.createTextNode(String(item.title ?? "")));
      if (item.version) {
        const curVer = (this.app.core?.engineVersion?.() || "").replace(/^v/i, "");
        const itemVer = String(item.version).replace(/^v/i, "");
        const mismatch = Boolean(curVer && itemVer && curVer !== itemVer);
        const badge = document.createElement("span");
        badge.style.cssText = mismatch
          ? "display:inline-block;margin-left:6px;padding:1px 5px;border-radius:4px;background:rgba(255,170,102,0.18);color:#ffaa66;font-size:11px;font-weight:500;"
          : "display:inline-block;margin-left:6px;padding:1px 5px;border-radius:4px;background:rgba(200,164,88,0.18);color:#e8c97a;font-size:11px;font-weight:500;";
        badge.textContent = mismatch ? `${String(item.version)} · ${L("旧版", "舊版", "old")}` : String(item.version);
        titleEl.appendChild(badge);
      }
      const subEl = document.createElement("div");
      subEl.style.cssText = "font-size:12px;color:#9eb5c4;margin-top:2px;";
      subEl.textContent = String(item.subtitle || "");
      left.append(titleEl, subEl);

      const right = document.createElement("div");
      right.style.cssText = "text-align:right;font-size:12px;color:#c8a458;white-space:nowrap;";
      const segPart = item.segments ? `${item.segments} ${L("章节", "章節", "chapters")}` : "";
      const durPart = dur ? `${segPart ? " · " : ""}${dur}` : "";
      right.textContent = `${segPart}${durPart}`;

      row.append(left, right);
      row.onclick = () => {
        this.libraryCursor = idx;
        this.confirmLibrarySelection();
      };
      this.elLibList.appendChild(row);
      if (idx === this.libraryCursor) row.scrollIntoView?.({ block: "nearest" });
    });
  }

  // ---- Replay Playback Engine (Hard-Load Segmented) -----------------------------------
  async startFromUrl(url) {
    const resp = await fetch(url);
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const name = url.split("/").pop() || "replay.repal";
    await this.startFromBytes(name, new Uint8Array(await resp.arrayBuffer()));
  }

  async startFromBytes(filename, rawBytes) {
    const bundle = await parseRepalBytes(rawBytes);
    if (!bundle.segments.length) throw new Error(L("录像不含有效分段", "錄像不含有效分段", "Replay has no valid segments"));
    const core = this.app.core;
    const w = core.w;

    this.active = true;
    this.filename = filename;
    this.bundle = bundle;
    const recVer = String(bundle.engine_version || "").replace(/^v/i, "");
    this.versionMismatch = !isReplayEngineCompatible(recVer);
    this.segIdx = 0;
    this.segTick = 0;
    this.evIdx = 0;
    this.paused = false;
    this.finished = false;
    this.speed = 1;
    this.heldDir = -1;
    this.steer = null;
    this.touchUi();

    this.app.inTitle = false;
    this.app.hud.hideTitleScreen();
    w.core_set_in_title(0);
    w.core_set_replay_mode(1);

    // Upload chapter metadata + decoded RGBA thumbnails into `pal-ui`!
    const meta = bundle.segments.map((s, i) => ({
      index: s.index ?? i,
      start_tick: s.start_tick ?? 0,
      scene_id: s.scene_id ?? 0,
      label: s.label || `Chapter ${i + 1}`,
    }));
    w.core_set_replay_chapters(core._scratch(JSON.stringify(meta)));
    bundle.segments.forEach(async (seg, i) => {
      if (!seg.thumb_png) return;
      const rgba = await decodePngToRgba(seg.thumb_png);
      if (rgba && this.active && this.bundle === bundle) {
        w.core_set_replay_chapter_thumb(i, rgba.width, rgba.height, core._scratch(rgba.rgba));
      }
    });

    this.loadSegment(0);
    this.elOverlay.style.display = "block";
    if (this.elCompatBanner) {
      if (this.versionMismatch) {
        this.elCompatBanner.textContent = `⚠ ${L("旧版录像", "舊版錄像", "Old replay")} v${recVer || "?"} (${L("最低兼容", "最低相容", "min compatible")} v${MIN_COMPAT_REPLAY_VERSION}) · ${L("战斗/剧情可能不同步", "戰鬥/劇情可能不同步", "battles/story may desync")}`;
        this.elCompatBanner.style.display = "block";
      } else {
        this.elCompatBanner.style.display = "none";
      }
    }
    const tags = [];
    if (this.versionMismatch) {
      tags.push(`${L("旧版录像", "舊版錄像", "Old replay")} v${recVer || "?"} < v${MIN_COMPAT_REPLAY_VERSION}, ${L("可能不同步", "可能不同步", "may desync")}`);
    }
    if (bundle.debug_mode || bundle.tainted_by_debug) {
      tags.push(L("含调试指令", "含除錯指令", "contains debug commands"));
    }
    const extra = tags.length ? ` | ${tags.join(" / ")}` : "";
    this.app.hud.showToast(
      `${L("开始回放", "開始回放", "Playing")}: ${filename} (${bundle.segments.length} ${L("章节", "章節", "chapters")}${extra})`,
      this.versionMismatch ? 4200 : 2600,
    );
  }

  toggleThoughts() {
    this.touchUi();
    this.showThoughts = !this.showThoughts;
    this.app.core?.w?.core_set_show_ai_thought?.(this.showThoughts ? 1 : 0);
    if (this.elBtnThoughts) {
      this.elBtnThoughts.textContent = this.showThoughts ? L("字幕:开", "字幕:開", "Captions: on") : L("字幕:关", "字幕:關", "Captions: off");
    }
    this.app.hud?.showToast?.(
      this.showThoughts ? L("AI 思考字幕：开 [F7]", "AI 思考字幕：開 [F7]", "AI thought captions: on [F7]") : L("AI 思考字幕：关 [F7]", "AI 思考字幕：關 [F7]", "AI thought captions: off [F7]"),
      1600
    );
  }

  _setThought(text) {
    const core = this.app.core;
    const w = core?.w;
    if (!w?.core_set_ai_thought) return;
    if (text && text.length > 0) {
      w.core_set_ai_thought(core._scratch(text));
      w.core_set_ai_thought_dimmed?.(0);
      this.hasActiveThought = true;
      this.lastThoughtAt = performance.now();
    } else {
      w.core_set_ai_thought(0);
      w.core_set_ai_thought_dimmed?.(0);
      this.hasActiveThought = false;
      this.lastThoughtAt = 0;
    }
  }

  _restoreAnnotationAt(segIdx, targetTick) {
    const seg = this.bundle?.segments?.[segIdx];
    const anns = Array.isArray(seg?.annotations) ? seg.annotations : [];
    let lo = 0;
    let hi = anns.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if ((anns[mid]?.t ?? 0) <= targetTick) lo = mid + 1;
      else hi = mid;
    }
    this.annIdx = lo;
    let active = "";
    if (lo > 0) {
      active = anns[lo - 1]?.text || "";
    } else if (seg) {
      const curScene = seg.scene_id;
      for (let i = segIdx - 1; i >= 0; i--) {
        const prev = this.bundle.segments[i];
        if (!prev || prev.scene_id !== curScene) break;
        const pAnns = Array.isArray(prev.annotations) ? prev.annotations : [];
        if (pAnns.length > 0) {
          active = pAnns[pAnns.length - 1]?.text || "";
          break;
        }
      }
    }
    this._setThought(active);
  }

  loadSegment(idx) {
    const seg = this.bundle?.segments?.[idx];
    if (!seg) return false;
    const core = this.app.core;
    const w = core.w;
    core._sessionGen = (core._sessionGen || 0) + 1;

    let ok = false;
    if (seg.anchor?.type === "new_game") {
      w.core_new_game((seg.anchor.seed ?? 1) >>> 0);
      w.core_set_replay_mode(1);
      ok = true;
    } else if (seg.anchor?.type === "save_json" && seg.anchor.data) {
      const txt = typeof seg.anchor.data === "string" ? seg.anchor.data : JSON.stringify(seg.anchor.data);
      ok = w.core_load_save_replay(core._scratch(txt)) === 1;
    }
    if (ok) {
      this.segIdx = idx;
      this.segTick = 0;
      this.evIdx = 0;
      this.heldDir = -1;
      this.steer = null;
      this.finished = false;
      w.core_set_replay_debug_flags?.(
        this.bundle?.debug_mode ? 1 : 0,
        this.bundle?.tainted_by_debug ? 1 : 0,
      );
      core.pumpCmds();
      w.core_set_show_ai_thought?.(this.showThoughts ? 1 : 0);
      this._restoreAnnotationAt(idx, 0);
    }
    return ok;
  }

  isReplayMenuOpen() {
    return this.app.core.w.core_replay_menu_open() === 1;
  }

  touchUi() {
    this.lastActivity = performance.now();
  }

  openReplayMenu() {
    this.touchUi();
    this.app.hud.openRootMenu();
  }

  openChapters() {
    this.touchUi();
    this.app.core.w.core_open_replay_chapters(this.segIdx >>> 0);
  }

  showTransitionBanner(text, ms = 1500) {
    if (!this.elBanner || !this.elBannerText) return;
    this.elBannerText.textContent = text;
    this.elBanner.style.display = "flex";
    this.elBanner.style.opacity = "1";
    this.transitionUntil = performance.now() + ms;
    if (this._bannerTimer) clearTimeout(this._bannerTimer);
    this._bannerTimer = setTimeout(() => {
      this.elBanner.style.opacity = "0";
      setTimeout(() => {
        if (performance.now() >= this.transitionUntil) {
          this.elBanner.style.display = "none";
        }
      }, 220);
    }, ms);
  }

  restartFromBeginning() {
    this.touchUi();
    if (this.loadSegment(0)) {
      this.paused = false;
      this.app.hud.closeSystemMenu();
      this.showTransitionBanner(L("重新播放录像", "重新播放錄像", "Replaying"), 1500);
    }
  }

  jumpToChapter(idx) {
    this.touchUi();
    if (this.loadSegment(idx)) {
      this.paused = false;
      const seg = this.bundle.segments[idx];
      this.showTransitionBanner(`${L("跳转章节", "跳轉章節", "Chapter")} #${String(idx + 1).padStart(2, "0")} · ${seg.label}`, 1500);
    }
  }

  prevSegment() {
    this.touchUi();
    const target = this.segTick > 50 || this.segIdx === 0 ? this.segIdx : this.segIdx - 1;
    this.jumpToChapter(target);
  }

  nextSegment() {
    this.touchUi();
    if (this.segIdx + 1 < this.bundle.segments.length) {
      this.jumpToChapter(this.segIdx + 1);
    }
  }

  seekByRatio(ratio) {
    this.touchUi();
    const targetTick = Math.floor((this.bundle.total_ticks || 0) * ratio);
    let idx = 0;
    for (let i = 0; i < this.bundle.segments.length; i++) {
      if (this.bundle.segments[i].start_tick <= targetTick) idx = i;
    }
    this.jumpToChapter(idx);
  }

  togglePlayPause() {
    this.touchUi();
    if (this.finished) {
      this.loadSegment(0);
      this.paused = false;
    } else {
      this.paused = !this.paused;
    }
  }

  cycleSpeed() {
    this.touchUi();
    this.speed = this.speed === 1 ? 2 : this.speed === 2 ? 4 : this.speed === 4 ? 8 : 1;
  }

  speedUp() {
    this.touchUi();
    this.speed = this.speed === 1 ? 2 : this.speed === 2 ? 4 : 8;
  }

  speedDown() {
    this.touchUi();
    this.speed = this.speed === 8 ? 4 : this.speed === 4 ? 2 : 1;
  }

  takeOver() {
    if (!this.active) return;
    const wasTainted = Boolean(this.bundle?.debug_mode || this.bundle?.tainted_by_debug);
    this.active = false;
    this.bundle = null;
    this._setThought("");
    this.elOverlay.style.display = "none";
    if (this.elCompatBanner) this.elCompatBanner.style.display = "none";
    const w = this.app.core.w;
    if (w.core_take_over_replay) {
      w.core_take_over_replay(wasTainted ? 1 : 0);
    } else {
      w.core_set_replay_debug_flags?.(0, wasTainted ? 1 : 0);
      w.core_set_replay_mode(0);
      w.core_restart_recording();
    }
    this.app.hud.closeSystemMenu();
    this.app.hud.showToast(L("已停止回放，你可以从此处接管继续游玩！", "已停止回放，你可以從此處接管繼續遊玩！", "Replay stopped. You can take over and keep playing from here!"), 2400);
  }

  exitToTitle() {
    this.active = false;
    this.bundle = null;
    this._setThought("");
    this.elOverlay.style.display = "none";
    if (this.elCompatBanner) this.elCompatBanner.style.display = "none";
    this.app.core.w.core_set_replay_debug_flags?.(0, 0);
    this.app.core.w.core_set_replay_mode(0);
    this.app.hud.onReturnToTitle?.();
  }

  /** Step one deterministic 50 Hz tick (called from `RustCoreHost.frame`). */
  stepOneTick() {
    if (!this.active || this.paused || this.finished || this.isReplayMenuOpen()) return;
    const now = performance.now();
    if (now < this.transitionUntil) return;
    const core = this.app.core;
    const w = core.w;
    if (w.core_is_blocked()) {
      w.core_tick(-1, 0, 0, 0, 0, 0);
      return;
    }
    const seg = this.bundle.segments[this.segIdx];
    if (!seg) {
      this.finished = true;
      this.paused = true;
      w.core_open_replay_finished?.();
      return;
    }

    // Presentation-only dialogue reading pause (1X/2X) without altering deterministic ticks
    const nextEv = seg.events[this.evIdx];
    const nextKind = nextEv ? nextEv.e || nextEv.op : "";
    if (
      this.speed <= 2 &&
      nextEv &&
      (nextEv.t ?? 0) <= this.segTick &&
      (nextKind === "dlg" || nextKind === "dialogue_done" || nextKind === "choice")
    ) {
      if (!this.dialogHoldUntil) {
        this.dialogHoldUntil = now + Math.round(1600 / Math.max(1, this.speed));
        return;
      }
      if (now < this.dialogHoldUntil) return;
      this.dialogHoldUntil = 0;
    }

    const anns = Array.isArray(seg.annotations) ? seg.annotations : [];
    let latestAnn = null;
    while (this.annIdx < anns.length && (anns[this.annIdx]?.t ?? 0) <= this.segTick) {
      latestAnn = anns[this.annIdx++]?.text ?? "";
    }
    if (latestAnn !== null) {
      this._setThought(latestAnn);
    }

    let interact = false;
    let click = null;
    while (this.evIdx < seg.events.length && (seg.events[this.evIdx].t ?? 0) <= this.segTick) {
      const ev = seg.events[this.evIdx++];
      const kind = ev.e || ev.op;
      if (kind === "dir") this.heldDir = ev.d ?? -1;
      else if (kind === "interact") interact = true;
      else if (kind === "click") {
        click = [ev.x | 0, ev.y | 0];
        this.app.renderer?.addClickRipple?.(ev.x | 0, ev.y | 0);
      } else if (kind === "steer") this.steer = [ev.x | 0, ev.y | 0];
      else if (kind === "steer_end") this.steer = null;
      else {
        const toast = this._formatActionToast(kind, ev);
        if (toast) this.app.hud.showToast(toast, 1600);
        w.core_replay_event(core._scratch(JSON.stringify(ev)));
        core.pumpCmds();
        if (w.core_is_blocked()) {
          // Host started an async LoadScene / PlayRng mid-tick; resume remaining same-tick events once unblocked (#243).
          return;
        }
      }
    }

    let flags = 0;
    if (interact) flags |= 1;
    if (click) flags |= 2;
    if (this.steer) flags |= 4;
    w.core_tick(
      this.heldDir,
      flags,
      click ? click[0] : 0,
      click ? click[1] : 0,
      this.steer ? this.steer[0] : 0,
      this.steer ? this.steer[1] : 0
    );
    this.segTick++;

    if (this.evIdx >= seg.events.length && this.segTick >= seg.duration_ticks) {
      if (this.segIdx + 1 < this.bundle.segments.length) {
        const nextIdx = this.segIdx + 1;
        const ns = this.bundle.segments[nextIdx];
        // Only show a transition banner if hard-loading next segment actually corrects a scene/pos drift!
        let drifted = false;
        try {
          if (ns?.anchor?.type === "save_json" && ns.anchor.data) {
            const cur = core.canonicalState();
            const exp = ns.anchor.data;
            const cLead = cur?.party?.[0];
            const eLead = exp?.party?.[0];
            if (
              cur?.sceneId !== exp?.sceneId ||
              (cLead && eLead && (Math.abs(cLead.x - eLead.x) > 24 || Math.abs(cLead.y - eLead.y) > 24))
            ) {
              drifted = true;
            }
          }
        } catch {}
        this.loadSegment(nextIdx);
        if (drifted) {
          this.showTransitionBanner(`${L("章节同步", "章節同步", "Chapter sync")} #${String(nextIdx + 1).padStart(2, "0")} · ${ns.label}`, 1500);
        }
      } else {
        this.finished = true;
        this.paused = true;
        w.core_open_replay_finished?.();
      }
    }
  }

  _formatActionToast(kind, ev) {
    if (kind === "use") return `${L("使用物品", "使用物品", "Use item")} #${ev.id}`;
    if (kind === "equip") return `${L("装备物品", "裝備物品", "Equip item")} #${ev.id}`;
    if (kind === "cast") return `${L("施放仙术", "施放仙術", "Cast spell")} #${ev.id}`;
    if (kind === "b_atk") return L("攻击", "攻擊", "Attack");
    if (kind === "b_mag") return `${L("仙术", "仙術", "Magic")} #${ev.id}`;
    if (kind === "b_item") return `${L("道具", "道具", "Item")} #${ev.id}`;
    if (kind === "b_def") return L("防御", "防禦", "Defend");
    if (kind === "b_flee") return L("逃跑", "逃跑", "Flee");
    if (kind === "buy") return `${L("购入", "購入", "Buy")} #${ev.id}`;
    if (kind === "sell") return `${L("典当", "典當", "Sell")} #${ev.id}`;
    return null;
  }

  /** Update razor-thin progress bar & auto-hiding control strip each animation frame. */
  renderOverlay(now) {
    if (!this.active || !this.bundle) return;
    const menuOpen = this.isReplayMenuOpen();
    this.elOverlay.style.display = menuOpen ? "none" : "block";
    if (this.elCompatBanner) {
      this.elCompatBanner.style.display = !menuOpen && this.versionMismatch ? "block" : "none";
    }
    if (menuOpen) return;

    const seg = this.bundle.segments[this.segIdx];
    const absTick = Math.min(this.bundle.total_ticks || 0, (seg?.start_tick || 0) + this.segTick);
    const pct = this.bundle.total_ticks > 0 ? (absTick / this.bundle.total_ticks) * 100 : 100;
    this.elThinFill.style.width = `${pct.toFixed(2)}%`;

    const showControls =
      this.paused || this.finished || this.pointerInBottom || now - this.lastActivity < IDLE_HIDE_MS;
    this.elCtrlStrip.style.opacity = showControls ? "1" : "0";
    this.elCtrlStrip.style.pointerEvents = showControls ? "auto" : "none";

    const useGp = this.app.gamepad?.lastInputMode === "gamepad";
    this.elMouseBtns.style.display = useGp ? "none" : "flex";
    this.elGpHints.style.display = useGp ? "flex" : "none";

    const secs = Math.floor(absTick / 50);
    const tot = Math.floor((this.bundle.total_ticks || 0) / 50);
    const fmt = (s) => `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
    const dbgBadge = this.bundle.debug_mode || this.bundle.tainted_by_debug ? " · [DEBUG]" : "";
    const statusText = `${fmt(secs)} / ${fmt(tot)} · ${this.segIdx + 1}/${this.bundle.segments.length} · t${absTick}${dbgBadge}`;
    this.elStatus.style.color = this.versionMismatch ? "#ffd27d" : "#d8e8f4";
    this.elStatus.textContent = statusText;
    if (this.elGpStatus) this.elGpStatus.textContent = statusText;
    this.elBtnPlay.textContent = this.finished ? "↻" : this.paused ? "▶" : "⏸";
    this.elBtnPlay.title = `${this.finished ? L("重新播放", "重新播放", "Replay") : this.paused ? L("播放", "播放", "Play") : L("暂停", "暫停", "Pause")} (Space)`;
    this.elBtnSpeed.textContent = `${this.speed}×`;
    if (this.elBtnThoughts) {
      this.elBtnThoughts.textContent = this.showThoughts ? L("字幕:开", "字幕:開", "Captions: on") : L("字幕:关", "字幕:關", "Captions: off");
    }
    if (this.hasActiveThought) {
      this.app.core?.w?.core_set_ai_thought_dimmed?.(now - this.lastThoughtAt >= 12000 ? 1 : 0);
    }
  }
}
