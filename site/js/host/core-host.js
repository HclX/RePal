/**
 * `?core=rust` host: drives the deterministic Rust game core (`pal_core::game::Game` inside
 * `pal.wasm`). All game logic — scripts, battles, menus, state — runs in Rust; this file only
 *  - feeds YAML tables / script chapters / mod overrides into the core,
 *  - advances it in fixed 20 ms logic ticks with the sampled input,
 *  - services host commands (audio, scene asset loading, cutscenes, title screen),
 *  - persists saves in `localStorage`,
 *  - records every input against the logic tick counter for frame-exact replays.
 */

import { parseYaml } from "../vendor/yaml.js";
import { resolveAssetBaseUrl, withAssetVersion } from "./asset-provider.js";
import { mapDomKeyToUiKey, POINTER_KIND } from "./wasm-presenter.js";
import { copyTextSafe, downloadBlob, pickFile, SAVE_PREFIX } from "./wasm-runtime.js";
import { L } from "./l10n.js";
import { BUG_REPORT_SEGMENTS, buildRepalArchive, trimBundleToLastSegments } from "./repal-loader.js";

export const TICK_MS = 20;
const enc = new TextEncoder();
const dec = new TextDecoder();
const TABLES = ["scenes", "events", "roles", "items", "magic", "enemies", "shops"];
const REPLY = { dialogue: 1, choice: 2, modal: 3, sceneLoaded: 4, done: 5 };

async function fetchYaml(baseUrl, rel, packManager = null) {
  if (packManager) {
    const text = await packManager.readText(rel);
    if (text != null) return parseYaml(text);
  }
  const resp = await fetch(withAssetVersion(`${baseUrl}/${rel}`));
  if (!resp.ok) throw new Error(`Failed to fetch ${rel} (${resp.status})`);
  return parseYaml(await resp.text());
}

export class RustCoreHost {
  constructor(app) {
    this.app = app;
    this.wasm = app.wasm;
    this.w = app.wasm.exports;
    this.acc = 0;
    this.lastNow = 0;
    this.pending = { interact: false, click: null };
    this._cmdQueue = Promise.resolve();
    this.seed = 0;
    this.anchor = null;
    this.replay = null;
    this.log = [];
  }

  // ---- wasm string helpers -------------------------------------------------------------
  _key(str) {
    const b = enc.encode(String(str ?? ""));
    const p = this.w.wasm_key_scratch_ptr(b.length);
    new Uint8Array(this.wasm.memoryBuffer, p, b.length).set(b);
    return b.length;
  }
  _scratch(bytesOrStr) {
    const b = typeof bytesOrStr === "string" ? enc.encode(bytesOrStr) : bytesOrStr;
    const p = this.w.wasm_scratch_ptr(b.length);
    new Uint8Array(this.wasm.memoryBuffer, p, b.length).set(b);
    return b.length;
  }
  _buf(len) {
    return new Uint8Array(this.wasm.memoryBuffer, this.w.core_buf_ptr(), len);
  }
  _bufText(len) {
    return dec.decode(this._buf(len));
  }
  engineVersion() {
    if (typeof this.w.core_engine_version !== "function") return "";
    const n = this.w.core_engine_version();
    return n ? this._bufText(n) : "";
  }

  // ---- boot ----------------------------------------------------------------------------
  async init(onProgress) {
    const base = resolveAssetBaseUrl();
    const pm = this.app.packManager || null;
    const total = TABLES.length + 11;
    let done = 0;
    // Fetch all 7 tables and 11 script chapters in parallel to avoid 18 serial network RTTs,
    // then feed them into the core in deterministic order (`tables -> core_start -> chapters`).
    const tablePromises = TABLES.map(async (t) => {
      const doc = await fetchYaml(base, `data/${t}.yaml`, pm);
      onProgress?.(++done / total, t);
      return [t, doc];
    });
    const chapterPromises = Array.from({ length: 11 }, (_, ch) =>
      fetchYaml(base, `data/scripts/scripts_${String(ch).padStart(2, "0")}.yaml`, pm).then((doc) => {
        onProgress?.(++done / total, `scripts_${ch}`);
        return [ch, doc];
      })
    );
    const mgoIndexPromise = fetch(withAssetVersion(`${base}/graphics/mgo/mgo_index.json`))
      .then((resp) => (resp.ok ? resp.json() : null))
      .catch(() => null);
    const [tables, chapters, mgoIndex] = await Promise.all([
      Promise.all(tablePromises),
      Promise.all(chapterPromises),
      mgoIndexPromise,
    ]);
    if (mgoIndex) {
      this.app.renderer?.preloadMgoIndex?.(mgoIndex);
    }
    for (const [t, doc] of tables) {
      const nLen = this._key(t);
      if (!this.w.core_load_table(nLen, this._scratch(JSON.stringify(doc)))) {
        throw new Error(`core_load_table(${t}) failed`);
      }
    }
    this.w.core_start();
    if (mgoIndex) {
      for (const [base, meta] of Object.entries(mgoIndex)) {
        const id = Number.parseInt(base.slice(4), 10);
        const frames = meta?.frames?.length || 0;
        if (Number.isFinite(id) && frames > 0) {
          this.w.core_set_sprite_frames(id, frames);
        }
      }
    }
    for (const [ch, doc] of chapters) {
      if (this.w.core_load_chapter(ch, this._scratch(JSON.stringify({ scripts: doc?.scripts || {} }))) < 0) {
        throw new Error(`core_load_chapter(${ch}) failed`);
      }
    }
    this.syncMods();
    // Load optional decoupled English translation overlays (`data/i18n/en/*.json`).
    if (typeof this.w.core_load_i18n_en === "function") {
      // Pack mode: read every `data/i18n/en/*.json` from the packs (the live site has no loose
      // files, so a bare fetch 404'd and Web silently showed no English text). Loose mode cannot
      // list directories over HTTP, so probe the known file names and ignore 404s.
      const I18N_DIR = "data/i18n/en/";
      const packKeys = pm?.listKeys ? pm.listKeys(I18N_DIR).filter((k) => k.endsWith(".json")) : [];
      const looseNames = [
        "tables.json",
        "scripts_ch1.json",
        ...Array.from({ length: 11 }, (_, i) => `scripts_${String(i).padStart(2, "0")}.json`),
      ];
      const i18nDocs = await Promise.all(
        (packKeys.length ? packKeys : looseNames.map((n) => I18N_DIR + n)).map(async (key) => {
          try {
            if (pm) {
              const text = await pm.readText(key);
              if (text != null) return text;
            }
            const resp = await fetch(withAssetVersion(`${base}/${key}`));
            return resp.ok ? await resp.text() : null;
          } catch {
            return null;
          }
        })
      );
      for (const raw of i18nDocs) {
        if (raw) {
          this.w.core_load_i18n_en(this._scratch(raw));
        }
      }
    }
    try {
      const savedLang = globalThis.localStorage?.getItem("pal_lang");
      if (savedLang === "hans" || savedLang === "hant" || savedLang === "en") {
        this.app.hud.lang = savedLang;
      }
    } catch {}
    this.setLang(this.app.hud.lang);
    this.app.hud.refreshSaveSlotSummaries?.();
    this.w.core_set_in_title(1);
  }

  setLang(lang) {
    const code = lang === "en" ? 2 : lang === "hans" ? 0 : 1;
    this.w.core_set_lang(code);
  }

  syncMods() {
    this.w.core_clear_overrides();
    for (const [label, steps] of this.app.modLoader?.activeOverrides || []) {
      const ok = this.w.core_load_override(this._key(label), this._scratch(JSON.stringify(steps)));
      if (!ok) console.warn("[RustCoreHost] Mod script override rejected:", label);
    }
  }

  _bumpSession() {
    this._sessionGen = (this._sessionGen || 0) + 1;
    this.anchor = null;
    this.replay = null;
    this.log = [];
  }

  // ---- lifecycle -----------------------------------------------------------------------
  newGame(seed = (Date.now() & 0x7fffffff) >>> 0) {
    this._bumpSession();
    this.seed = seed >>> 0;
    this.anchor = { type: "new_game", seed: this.seed };
    this.app.inTitle = false;
    this.w.core_set_in_title(0);
    this.w.core_new_game(this.seed);
    this.app.audio.stopMusic?.();
  }

  saveSlot(slot) {
    this.w.core_split_segment_manual?.();
    const nLen = this._key(`slot ${slot + 1}`);
    const len = this.w.core_save(nLen, this._scratch(new Date().toISOString()));
    const text = this._bufText(len);
    localStorage.setItem(`${SAVE_PREFIX}${slot}`, text);
    return JSON.parse(text);
  }

  loadSaveText(text) {
    const ok = this.w.core_load_save(this._scratch(text)) === 1;
    if (ok) {
      this._bumpSession();
      try {
        this.anchor = { type: "save_json", data: JSON.parse(text) };
      } catch {
        this.anchor = null;
      }
      this.app.inTitle = false;
    }
    return ok;
  }

  loadRpg(bytes) {
    const u8 = new Uint8Array(bytes);
    const ok = this.w.core_load_rpg(this._scratch(u8)) === 1;
    if (ok) {
      this._bumpSession();
      this.anchor = { type: "rpg_base64", data: bytesToBase64(u8) };
      this.app.inTitle = false;
    }
    return ok;
  }

  canonicalState() {
    return JSON.parse(this._bufText(this.w.core_canonical_state()));
  }

  // ---- input ---------------------------------------------------------------------------
  /** Open/close the in-game root menu and record it for deterministic replay. */
  toggleRootMenu(open = true) {
    this.log.push({ t: this.w.core_tick_count(), m: open ? 1 : 0 });
    if (open) this.app.hud.openRootMenu();
    else this.app.hud.closeSystemMenu();
  }

  /** Forward a UI key (menus, dialogue, battle HUD) and record it for replay. */
  uiKey(domCode) {
    const key = mapDomKeyToUiKey(domCode);
    if (!key) return false;
    this.log.push({ t: this.w.core_tick_count(), k: key });
    this.w.wasm_ui_on_key(key);
    this.app.hud.drainActions();
    return true;
  }

  uiPointer(domEvent, kind) {
    let x, y;
    if (typeof this.app.renderer?.clientToWorld === "function") {
      const p = this.app.renderer.clientToWorld(domEvent.clientX, domEvent.clientY, 0, 0);
      x = Math.max(0, Math.min(319, Math.floor(p.worldX)));
      y = Math.max(0, Math.min(199, Math.floor(p.worldY)));
    } else {
      const rect = this.app.canvas.getBoundingClientRect();
      if (!rect.width || !rect.height) return;
      x = Math.max(0, Math.min(319, Math.floor(((domEvent.clientX - rect.left) / rect.width) * 320)));
      y = Math.max(0, Math.min(199, Math.floor(((domEvent.clientY - rect.top) / rect.height) * 200)));
    }
    if (kind !== POINTER_KIND.Move) this.log.push({ t: this.w.core_tick_count(), p: [kind, x, y] });
    this.w.wasm_ui_on_pointer(kind, x, y);
    this.app.hud.drainActions();
  }

  clientToWorld(clientX, clientY) {
    const cam = this.w.core_camera_pos();
    return this.app.renderer.clientToWorld(clientX, clientY, cam >>> 16, cam & 0xffff);
  }

  queueInteract() {
    this.pending.interact = true;
  }

  queueClick(worldX, worldY) {
    this.pending.click = { x: worldX | 0, y: worldY | 0 };
    this.app.renderer.addClickRipple(worldX, worldY);
  }

  _sampleInput() {
    const app = this.app;
    let held = app.getHeldDirection();
    let steer = null;
    if (held < 0 && app.pointerDragActive) {
      const p = this.clientToWorld(app.pointerClientPos.x, app.pointerClientPos.y);
      steer = { x: p.worldX, y: p.worldY };
    }
    let flags = 0;
    if (this.pending.interact) flags |= 1;
    const c = this.pending.click;
    if (c) flags |= 2;
    if (steer) flags |= 4;
    this.pending = { interact: false, click: null };
    return [held, flags, c?.x | 0, c?.y | 0, steer?.x | 0, steer?.y | 0];
  }

  // ---- frame loop ----------------------------------------------------------------------
  frame(now) {
    if (!this.lastNow) this.lastNow = now;
    this.acc = Math.min(this.acc + (now - this.lastNow), TICK_MS * 8);
    this.lastNow = now;
    while (this.acc >= TICK_MS) {
      this.acc -= TICK_MS;
      const theater = this.app.replayTheater;
      if (theater?.active) {
        const steps = theater.speed || 1;
        for (let s = 0; s < steps; s++) {
          theater.stepOneTick();
          this.pumpCmds();
        }
      } else if (this.replay) {
        this._replayStep();
        this.pumpCmds();
      } else if (this.app.webAgent?.isBusy?.()) {
        this.app.webAgent.stepOneTick();
        this.pumpCmds();
      } else {
        if (this.w.core_is_blocked()) {
          this.w.core_tick(-1, 0, 0, 0, 0, 0);
        } else {
          const input = this._sampleInput();
          if (input[0] >= 0 || input[1]) this.log.push({ t: this.w.core_tick_count(), i: input });
          this.w.core_tick(...input);
        }
        this.pumpCmds();
      }
    }
    this.app.renderer.present();
    this.app.replayTheater?.renderOverlay?.(now);
  }

  pumpCmds() {
    for (;;) {
      const len = this.w.core_poll_cmd();
      if (!len) return;
      let cmd;
      try {
        cmd = JSON.parse(this._bufText(len));
      } catch {
        continue;
      }
      this._enqueueCmd(cmd);
    }
  }

  /** Run async host commands serially so blocking replies (load_scene, rng) never race or deadlock. */
  _enqueueCmd(cmd) {
    const gen = this._sessionGen || 0;
    this._cmdQueue = this._cmdQueue
      .then(() => this.handleCmd(cmd, gen))
      .catch((err) => console.warn("[RustCoreHost] HostCmd failed:", cmd?.cmd, err));
  }

  async handleCmd(cmd, gen = this._sessionGen || 0) {
    if (gen !== (this._sessionGen || 0)) return;
    const app = this.app;
    switch (cmd.cmd) {
      case "music":
        app.audio.playMusic(cmd.track, cmd.loop);
        break;
      case "music_stop":
        app.audio.stopMusic();
        break;
      case "sfx":
        app.audio.playSfx(cmd.id);
        break;
      case "load_scene": {
        app.renderer?.presenter?.assets?.cancelScenePending?.();
        let loadingEl = null;
        const showTimer = setTimeout(() => {
          if (typeof document === "undefined") return;
          loadingEl = document.getElementById("repal-scene-loading");
          if (!loadingEl) {
            loadingEl = document.createElement("div");
            loadingEl.id = "repal-scene-loading";
            loadingEl.style.cssText =
              "position:fixed;left:50%;bottom:24px;transform:translateX(-50%);z-index:45;padding:7px 16px;border-radius:999px;background:rgba(10,18,26,0.88);border:1px solid rgba(200,164,88,0.65);color:#f2e8d0;font-size:13px;letter-spacing:0.5px;pointer-events:none;box-shadow:0 6px 20px rgba(0,0,0,0.5);font-family:'Noto Serif SC',system-ui,sans-serif;";
            loadingEl.textContent = L("正在载入场景…", "正在載入場景…", "Loading scene…");
            document.body?.appendChild(loadingEl);
          }
          loadingEl.style.display = "block";
        }, 220);
        try {
          if (app.packManager) {
            await app.packManager.ensureScenePacks(cmd.scene ?? 0, cmd.map > 0 ? cmd.map : null);
          }
          await this.loadSceneAssets(cmd.map, gen);
        } catch (err) {
          console.warn("[RustCoreHost] loadSceneAssets failed:", cmd.map, err);
        } finally {
          clearTimeout(showTimer);
          const el = loadingEl || (typeof document !== "undefined" ? document.getElementById("repal-scene-loading") : null);
          if (el) el.style.display = "none";
          if (gen === (this._sessionGen || 0)) {
            this.w.core_reply(REPLY.sceneLoaded, 0);
          }
        }
        break;
      }
      case "load_chapter":
        if (gen === (this._sessionGen || 0)) {
          this.w.core_reply(REPLY.done, 0); // all chapters are preloaded at boot
        }
        break;
      case "rng":
        try {
          if (app.packManager) {
            await app.packManager.ensureRngPack(cmd.id);
          }
          await app.renderer.playRngCutscene(
            cmd.id,
            cmd.start,
            cmd.end,
            cmd.speed,
            () => app.cutsceneSkip || gen !== (this._sessionGen || 0)
          );
        } catch (err) {
          console.warn("[RustCoreHost] playRngCutscene failed:", cmd.id, err);
        } finally {
          app.cutsceneSkip = false;
          if (gen === (this._sessionGen || 0)) {
            this.w.core_reply(REPLY.done, 0);
          }
        }
        break;
      case "title":
        this._bumpSession();
        app.hud.onReturnToTitle?.();
        break;
      case "game_over":
        this._bumpSession();
        app.hud.showToast(L("胜败乃兵家常事，请重新来过", "勝敗乃兵家常事，請重新來過", "Victory and defeat are part of war. Try again!"), 4000);
        app.hud.onReturnToTitle?.();
        break;
      default:
        break;
    }
  }

  async loadSceneAssets(mapId, gen = this._sessionGen || 0) {
    const r = this.app.renderer;
    if (mapId > 0) {
      const bytes = await r.ensureMapBinary(mapId);
      if (gen !== (this._sessionGen || 0)) return;
      if (bytes) this.w.core_set_map(this._scratch(bytes));
    }
    const n = this.w.core_scene_sprite_ids();
    const ids = Array.from(new Uint16Array(this._buf(n * 2).slice().buffer));
    await Promise.all([
      ...ids.map(async (id) => {
        const atlas = await r.ensureAtlas("mgo", `mgo_${String(id).padStart(3, "0")}`);
        if (gen !== (this._sessionGen || 0)) return;
        const frames = atlas?.meta?.frames?.length || 0;
        if (frames > 0) this.w.core_set_sprite_frames(id, frames);
      }),
      r.presenter?.assets?.preloadSceneVisuals?.(mapId, ids, r.hdMode !== false),
    ]);
  }

  // ---- save files & diagnostics -----------------------------------------------------------
  exportSaveFile() {
    const len = this.w.core_save(this._key("export"), this._scratch(new Date().toISOString()));
    const blob = new Blob([this._bufText(len)], { type: "application/json;charset=utf-8" });
    downloadBlob(blob, `pal_remake_save_${Date.now()}.json`);
    this.app.hud.showToast(L("已导出存档 JSON 至本地下载！", "已匯出存檔 JSON 至本地下載！", "Save exported as JSON to your downloads!"));
  }

  async importSaveFile() {
    const file = await pickFile(".json,.rpg");
    if (!file) return false;
    const ok = file.name.toLowerCase().endsWith(".rpg") ? this.loadRpg(await file.arrayBuffer()) : this.loadSaveText(await file.text());
    this.app.hud.showToast(ok ? `${L("已导入存档", "已匯入存檔", "Save imported")}: ${file.name}` : L("导入失败：无效的存档文件", "匯入失敗：無效的存檔檔案", "Import failed: invalid save file"));
    if (ok) this.app.hud.hideTitleScreen();
    return ok;
  }

  /** Deterministic bug report: replay log + canonical state (paste into an issue). */
  diagnosticReport(note = "") {
    return JSON.stringify({ kind: "PalRemake_CoreDiag_v1", note, replay: this.exportReplay(), state: this.canonicalState() });
  }

  async copyDiagnosticReport(note = "") {
    const text = this.diagnosticReport(note);
    const ok = await copyTextSafe(text);
    if (!ok) console.log(text);
    this.app.hud.showToast(ok ? "🐞 " + L("已复制确定性录像与状态快照", "已複製確定性錄像與狀態快照", "Copied replay and state snapshot") : "🐞 " + L("剪贴板不可用，已输出到控制台", "剪貼簿不可用，已輸出到主控台", "Clipboard unavailable, printed to the console"));
  }

  /**
   * Open the in-game bug report dialog (screenshot preview + description + single `.repal`
   * export (replay + screenshot + state) + clipboard screenshot copy + prefilled GitHub Issue link). Falls back to
   * `copyDiagnosticReport()` in headless/Node environments without `<dialog>`.
   */
  async openBugReportDialog(note = "") {
    const dlg = typeof document !== "undefined" ? document.getElementById("bug-report-dialog") : null;
    if (!dlg || typeof dlg.showModal !== "function") {
      return this.copyDiagnosticReport(note);
    }

    // Snapshot state + replay + screenshot immediately at trigger time
    const state = this.canonicalState();
    const tick = this.w.core_tick_count?.() ?? 0;
    const sceneId = state?.sceneId ?? 0;
    const lead = state?.party?.[0];
    const posStr = lead ? `(${lead.x}, ${lead.y})` : "(?, ?)";
    const modeStr = this.app.renderer?.hdMode ? "4x HD" : "1x Retro";
    const anchorType = this.anchor?.type || "new_game";
    const stamp = `pal_bug_sc${sceneId}_t${tick}`;

    let shotBlob = null;
    const canvas = this.app.renderer?.canvas;
    if (canvas && typeof canvas.toBlob === "function") {
      shotBlob = await new Promise((resolve) => canvas.toBlob((b) => resolve(b), "image/png"));
    }

    const preview = document.getElementById("bug-shot-preview");
    let previewUrl = null;
    if (preview) {
      if (shotBlob) {
        previewUrl = URL.createObjectURL(shotBlob);
        preview.src = previewUrl;
        preview.style.display = "block";
      } else {
        preview.removeAttribute("src");
        preview.style.display = "none";
      }
    }

    const metaEl = document.getElementById("bug-meta-summary");
    if (metaEl) {
      metaEl.textContent = [
        `场景: Scene #${sceneId}  坐标: ${posStr}`,
        `画质: ${modeStr}  逻辑帧: ${tick}`,
        `起点: ${anchorType}  录制步数: ${this.log.length}`,
      ].join("\n");
    }

    const titleEl = document.getElementById("bug-title-input");
    const descEl = document.getElementById("bug-desc-input");
    if (titleEl) titleEl.value = `[Bug] Scene #${sceneId} ${posStr}: `;
    if (descEl) descEl.value = "";

    const exportBundle = async (openGithub) => {
      const desc = (descEl?.value || "").trim();
      const title = (titleEl?.value || `[Bug] Scene #${sceneId} ${posStr}`).trim();
      // 1. Download ONE `.repal` archive (replay + screenshot + state snapshot), the same format
      // desktop F9 writes, so the issue gets a single attachment. Falls back to the legacy
      // separate .png + .json when the browser lacks CompressionStream or the v3 export fails.
      let repalFile = null;
      try {
        const len = this.w.core_export_replay_v3?.() || 0;
        if (!len) throw new Error("no v3 replay");
        // Only the last checkpoints: the bug is near the end, and each segment replays on its own.
        const bundle = trimBundleToLastSegments(
          JSON.parse(new TextDecoder().decode(this._buf(len).slice())),
          BUG_REPORT_SEGMENTS,
        );
        const png = shotBlob ? new Uint8Array(await shotBlob.arrayBuffer()) : null;
        const bytes = await buildRepalArchive(bundle, { endScreenshotPng: png, state, note: desc || note || title });
        // `.repal.gz`: GitHub issue attachments accept `.gz` (not `.repal`); RePal opens both.
        repalFile = `${stamp}.repal.gz`;
        downloadBlob(new Blob([bytes], { type: "application/gzip" }), repalFile);
      } catch (err) {
        console.warn("[bugreport] .repal export failed, falling back to .png + .json", err);
      }
      const jsonFile = `${stamp}.json`;
      const pngFile = `${stamp}.png`;
      if (!repalFile) {
        if (shotBlob) downloadBlob(shotBlob, pngFile);
        const reportJson = this.diagnosticReport(desc || note || title);
        downloadBlob(new Blob([reportJson], { type: "application/json;charset=utf-8" }), jsonFile);
      }
      const files = repalFile || `${pngFile} + ${jsonFile}`;

      // 2. Copy screenshot PNG to clipboard so user can press Ctrl+V directly in GitHub Issue
      let copiedImg = false;
      if (shotBlob && navigator.clipboard?.write && typeof ClipboardItem !== "undefined" && window.isSecureContext) {
        try {
          await navigator.clipboard.write([new ClipboardItem({ "image/png": shotBlob })]);
          copiedImg = true;
        } catch {
          copiedImg = false;
        }
      }

      if (openGithub) {
        const body = [
          "### 问题描述",
          desc || "（请补充现象与复现步骤）",
          "",
          "### 自动采集状态",
          `- Scene: \`#${sceneId}\` | Pos: \`${posStr}\` | Mode: \`${modeStr}\` | Tick: \`${tick}\``,
          `- Replay Anchor: \`${anchorType}\` | Logged Inputs: \`${this.log.length}\``,
          `- URL: ${location.href}`,
          "",
          "### 附件（已自动下载到本地）",
          copiedImg ? "- 截图：已复制到剪贴板，按 Ctrl+V / Cmd+V 直接粘贴到此处" : "",
          repalFile
            ? `- 录像包：\`${repalFile}\`（含录像、截图与状态快照；请拖入此处，本地可用 \`repal --replay ${repalFile}\` 100% 复现）`
            : `- 截图：\`${pngFile}\`；确定性录像：\`${jsonFile}\`（请拖入此处，本地可用 \`repal --replay ${jsonFile}\` 100% 复现）`,
        ].join("\n");
        const url = `https://github.com/HclX/RePal/issues/new?labels=bug&title=${encodeURIComponent(title)}&body=${encodeURIComponent(body)}`;
        window.open(url, "_blank", "noopener,noreferrer");
      }

      dlg.close();
      this.app.hud.showToast(
        openGithub
          ? copiedImg
            ? `${L("已下载", "已下載", "Downloaded")} ${files} (${L("截图已复制，Ctrl+V 贴图", "截圖已複製，Ctrl+V 貼上", "screenshot copied, Ctrl+V to paste")})`
            : `${L("已下载", "已下載", "Downloaded")} ${files}, ${L("并打开 GitHub Issue", "並開啟 GitHub Issue", "opened a GitHub issue")}`
          : `${L("已下载", "已下載", "Downloaded")} ${files}`
      );
    };

    const downloadPngOnly = async () => {
      const pngFile = `${stamp}.png`;
      if (shotBlob) {
        downloadBlob(shotBlob, pngFile);
      }
      if (shotBlob && navigator.clipboard?.write && typeof ClipboardItem !== "undefined" && window.isSecureContext) {
        try {
          await navigator.clipboard.write([new ClipboardItem({ "image/png": shotBlob })]);
        } catch {}
      }
      dlg.close();
      this.app.hud.showToast(shotBlob ? `${L("已下载截图", "已下載截圖", "Screenshot downloaded")}: ${pngFile}` : L("截图不可用", "截圖不可用", "Screenshot unavailable"));
    };

    const closeBtn = document.getElementById("bug-close-btn");
    const dlPngBtn = document.getElementById("bug-download-png-btn");
    const dlOnlyBtn = document.getElementById("bug-download-only-btn");
    const ghBtn = document.getElementById("bug-submit-gh-btn");
    if (closeBtn) closeBtn.onclick = () => dlg.close();
    if (dlPngBtn) dlPngBtn.onclick = () => downloadPngOnly();
    if (dlOnlyBtn) dlOnlyBtn.onclick = () => exportBundle(false);
    if (ghBtn) ghBtn.onclick = () => exportBundle(true);
    dlg.onclose = () => {
      if (previewUrl) URL.revokeObjectURL(previewUrl);
      this.app.keysDown?.clear?.();
    };

    this.app.keysDown?.clear?.();
    dlg.showModal();
    descEl?.focus();
  }

  // ---- deterministic replay ------------------------------------------------------------
  exportReplay() {
    return {
      kind: "PalRemake_CoreReplay_v1",
      seed: this.seed,
      anchor: this.anchor,
      log: this.log.slice(),
    };
  }

  startReplay(rep) {
    const anchor = rep?.anchor;
    if (anchor?.type === "save_json" && anchor.data) {
      const text = typeof anchor.data === "string" ? anchor.data : JSON.stringify(anchor.data);
      this.loadSaveText(text);
    } else if (anchor?.type === "rpg_base64" && typeof anchor.data === "string") {
      this.loadRpg(base64ToBytes(anchor.data));
    } else {
      this.newGame(anchor?.seed ?? rep.seed);
    }
    this.replay = { log: rep.log.slice(), idx: 0, recorded: rep.log };
    this.log = [];
  }

  _replayStep() {
    const rp = this.replay;
    if (this.w.core_is_blocked()) {
      this.w.core_tick(-1, 0, 0, 0, 0, 0);
      return;
    }
    const now = this.w.core_tick_count();
    let input = [-1, 0, 0, 0, 0, 0];
    while (rp.idx < rp.log.length && rp.log[rp.idx].t <= now) {
      const e = rp.log[rp.idx++];
      if (e.m !== undefined) {
        if (e.m) this.app.hud.openRootMenu();
        else this.app.hud.closeSystemMenu();
      } else if (e.k) {
        this.w.wasm_ui_on_key(e.k);
        this.app.hud.drainActions();
      } else if (e.p) {
        this.w.wasm_ui_on_pointer(e.p[0], e.p[1], e.p[2]);
        this.app.hud.drainActions();
      } else if (e.i && e.t === now) {
        input = e.i;
      }
    }
    this.w.core_tick(...input);
    if (rp.idx >= rp.log.length && !this.w.core_is_busy()) {
      this.replay = null;
      this.log = rp.recorded.slice();
    }
  }
}

function bytesToBase64(u8) {
  if (typeof Buffer !== "undefined") return Buffer.from(u8).toString("base64");
  let s = "";
  const chunk = 0x8000;
  for (let i = 0; i < u8.length; i += chunk) {
    s += String.fromCharCode(...u8.subarray(i, i + chunk));
  }
  return btoa(s);
}

function base64ToBytes(b64) {
  if (typeof Buffer !== "undefined") return new Uint8Array(Buffer.from(b64, "base64"));
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export async function fetchArrayBuffer(baseUrl, rel) {
  const resp = await fetch(withAssetVersion(`${baseUrl}/${rel}`));
  return resp.ok ? resp.arrayBuffer() : null;
}

export { parseYaml };
