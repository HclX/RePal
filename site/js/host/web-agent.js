import { L } from "./l10n.js";
/**
 * Browser Web Agent Bridge (`window.repalAgent` + outbound WebSocket `?agent_ws=...`).
 *
 * Drives the exact same Rust `pal-headless::text_mode::TextEngine` compiled into `pal.wasm`
 * inside the browser's 50 Hz `requestAnimationFrame` loop:
 * - Option 1 (In-Page JS API): `await window.repalAgent.send("look")` (Playwright / Puppeteer / MCP)
 * - Option 2 (Outbound WebSocket): `?agent_ws=ws://127.0.0.1:9999` or `wss://...` (or `☰ → 🤖 连接 Agent`)
 * - Human Takeover: pressing `Esc` immediately cancels any in-flight AI task (`interrupted_by: "human"`).
 */

const enc = new TextEncoder();
const dec = new TextDecoder();

export class WebAgentHost {
  constructor(app) {
    this.app = app;
    this.ws = null;
    this.wsUrl = "";
    this.wsConnected = false;
    this._queue = [];
    this._active = null; // { resolve, reject }
    this._installGlobalApi();
    this._wireMenuButton();
  }

  get w() {
    return this.app.wasm.exports;
  }

  _readAgentBuf(len) {
    if (!len) return null;
    const ptr = this.w.core_agent_buf_ptr();
    const bytes = new Uint8Array(this.w.memory.buffer, ptr, len);
    try {
      return JSON.parse(dec.decode(bytes));
    } catch {
      return { ok: false, error: "invalid JSON from core_agent" };
    }
  }

  isBusy() {
    return !!(this._active || this.w.core_agent_is_busy?.() === 1);
  }

  setObs(mode) {
    const fair = String(mode).toLowerCase() === "fair" || mode === true ? 1 : 0;
    this.w.core_agent_set_obs?.(fair);
  }

  setMinimap(mode) {
    const m = String(mode).toLowerCase();
    const code = m === "always" ? 1 : m === "off" ? 2 : 0;
    this.w.core_agent_set_minimap?.(code);
  }

  greeting() {
    const len = this.w.core_agent_greeting?.() || 0;
    return this._readAgentBuf(len);
  }

  /**
   * Submit one Text Mode command line and resolve with the unified JSON reply object.
   * Serializes concurrent calls in FIFO order.
   */
  send(cmd) {
    return new Promise((resolve, reject) => {
      this._queue.push({ cmd: String(cmd ?? ""), resolve, reject });
      this._pumpQueue();
    });
  }

  _pumpQueue() {
    if (this._active || this._queue.length === 0) return;
    const next = this._queue.shift();
    const bytes = enc.encode(next.cmd);
    const ptr = this.w.wasm_scratch_ptr(Math.max(bytes.length, 1));
    if (bytes.length > 0) {
      new Uint8Array(this.w.memory.buffer, ptr, bytes.length).set(bytes);
    }
    const len = this.w.core_agent_submit(bytes.length);
    this.app.hud?.drainActions?.();
    this.app.core?.pumpCmds?.();
    if (len > 0) {
      const reply = this._readAgentBuf(len);
      next.resolve(reply);
      // Drain next queued command asynchronously so the renderer can present between commands.
      queueMicrotask(() => this._pumpQueue());
    } else {
      this._active = next;
    }
  }

  /** Called once per 50 Hz simulation tick by `RustCoreHost.frame()` while `isBusy()` is true. */
  stepOneTick() {
    if (!this._active && this.w.core_agent_is_busy?.() !== 1) return;
    const len = this.w.core_agent_step();
    this.app.hud?.drainActions?.();
    if (len > 0) {
      const reply = this._readAgentBuf(len);
      const done = this._active;
      this._active = null;
      done?.resolve(reply);
      this._pumpQueue();
    } else {
      this.w.core_agent_tick_with_pending();
      this.app.hud?.drainActions?.();
    }
  }

  /** Cancel any in-flight AI task immediately (e.g. when a human presses `Esc`). */
  cancel() {
    if (!this.isBusy()) return false;
    const len = this.w.core_agent_cancel?.() || 0;
    const reply = this._readAgentBuf(len) || {
      ok: false,
      error_code: "interrupted",
      interrupted_by: "human",
    };
    const done = this._active;
    this._active = null;
    done?.resolve(reply);
    while (this._queue.length > 0) {
      this._queue.shift().resolve(reply);
    }
    this.app.hud?.showToast?.(L("已中断 AI 操作，恢复手动控制", "已中斷 AI 操作，恢復手動控制", "AI control interrupted, manual control restored"), 2200);
    return true;
  }

  // ---- Outbound WebSocket bridge (Option 2) ---------------------------------------------

  connectWs(url) {
    const trimmed = String(url || "").trim();
    if (!trimmed) return;
    if (!/^wss?:\/\//i.test(trimmed)) {
      this.app.hud?.showToast?.(L("Agent 地址必须以 ws:// 或 wss:// 开头", "Agent 位址必須以 ws:// 或 wss:// 開頭", "Agent URL must start with ws:// or wss://"), 3200);
      return;
    }
    this.disconnectWs(true);
    this.wsUrl = trimmed;
    let ws;
    try {
      ws = new WebSocket(trimmed);
    } catch (err) {
      this.app.hud?.showToast?.(`${L("连接 Agent 失败", "連接 Agent 失敗", "Agent connection failed")}: ${err.message}`, 3500);
      return;
    }
    this.ws = ws;
    ws.addEventListener("open", () => {
      if (this.ws !== ws) return;
      this.wsConnected = true;
      this._updateMenuLabel();
      this.app.hud?.showToast?.(`${L("已连接 Agent", "已連接 Agent", "Agent connected")}: ${trimmed}`, 2800);
      const greet = this.greeting();
      if (greet) ws.send(JSON.stringify(greet) + "\n");
    });
    ws.addEventListener("message", (ev) => {
      if (this.ws !== ws || typeof ev.data !== "string") return;
      const lines = ev.data.split(/\r?\n/);
      for (const raw of lines) {
        const line = raw.trim();
        if (!line) continue;
        this.send(line)
          .then((reply) => {
            if (this.ws === ws && ws.readyState === WebSocket.OPEN) {
              ws.send(JSON.stringify(reply) + "\n");
            }
          })
          .catch((err) => {
            if (this.ws === ws && ws.readyState === WebSocket.OPEN) {
              ws.send(JSON.stringify({ ok: false, error: String(err) }) + "\n");
            }
          });
      }
    });
    ws.addEventListener("close", () => {
      if (this.ws !== ws) return;
      const wasConnected = this.wsConnected;
      this.ws = null;
      this.wsConnected = false;
      this._updateMenuLabel();
      if (wasConnected) {
        this.app.hud?.showToast?.(L("Agent WebSocket 连接已断开", "Agent WebSocket 連線已中斷", "Agent WebSocket disconnected"), 2400);
      }
    });
    ws.addEventListener("error", () => {
      if (this.ws !== ws) return;
      this.app.hud?.showToast?.(`${L("无法连接 Agent", "無法連接 Agent", "Cannot connect to Agent")}: ${trimmed}`, 3200);
    });
  }

  disconnectWs(silent = false) {
    if (this.ws) {
      const old = this.ws;
      this.ws = null;
      this.wsConnected = false;
      try {
        old.close();
      } catch {}
      this._updateMenuLabel();
      if (!silent) this.app.hud?.showToast?.(L("已断开 Agent 连接", "已中斷 Agent 連線", "Agent disconnected"), 2000);
    }
  }

  status() {
    return {
      busy: this.isBusy(),
      wsConnected: this.wsConnected,
      wsUrl: this.wsUrl || null,
    };
  }

  _installGlobalApi() {
    const self = this;
    window.repalAgent = {
      send: (cmd) => self.send(cmd),
      greeting: () => self.greeting(),
      setObs: (mode) => self.setObs(mode),
      setMinimap: (mode) => self.setMinimap(mode),
      cancel: () => self.cancel(),
      connectWs: (url) => self.connectWs(url),
      disconnectWs: () => self.disconnectWs(),
      status: () => self.status(),
    };
  }

  _wireMenuButton() {
    const btn = document.getElementById("btn-stage-agent");
    if (!btn) return;
    this._btn = btn;
    btn.addEventListener("click", () => {
      if (this.wsConnected || this.ws) {
        this.disconnectWs();
        return;
      }
      const preset = this.wsUrl || "ws://127.0.0.1:9999";
      const entered = window.prompt(
        L("输入 AI Agent WebSocket 地址（本机用 ws://127.0.0.1:9999，远程/手机需用 wss://...）：", "輸入 AI Agent WebSocket 位址（本機用 ws://127.0.0.1:9999，遠端/手機需用 wss://...）：", "AI Agent WebSocket URL (local: ws://127.0.0.1:9999, remote/phone: wss://...):"),
        preset
      );
      if (entered && entered.trim()) {
        this.connectWs(entered.trim());
      }
    });
    this._updateMenuLabel();
  }

  _updateMenuLabel() {
    if (!this._btn) return;
    this._btn.textContent = this.wsConnected ? "🤖 " + L("断开 Agent", "中斷 Agent", "Disconnect Agent") : "🤖 " + L("连接 Agent", "連接 Agent", "Connect Agent");
  }
}
