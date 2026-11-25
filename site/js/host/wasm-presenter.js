/**
 * Thin Canvas2D Framebuffer Presenter & Input Forwarder for Full-WASM Rendering.
 *
 * Zero-copy `Uint8ClampedArray` view over `pal.wasm`'s linear-memory RGBA framebuffer,
 * presented via `ctx.putImageData()`, plus keyboard/pointer forwarding into `pal-ui`.
 */

import { WasmAssetProvider, resolveAssetBaseUrl } from "./asset-provider.js";

export const UI_KEY = {
  Up: 1,
  Down: 2,
  Left: 3,
  Right: 4,
  Confirm: 5,
  Cancel: 6,
  MenuToggle: 7,
  Tab: 8,
  PageUp: 9,
  PageDown: 10,
  Home: 11,
  End: 12,
  BattleRepeat: 13,
  BattleAuto: 14,
  BattleDefend: 15,
  BattleUseItem: 16,
  BattleThrowItem: 17,
  BattleFlee: 18,
  BattleForce: 19,
  BattleStatus: 20,
};

export const POINTER_KIND = {
  Move: 0,
  Down: 1,
  Up: 2,
  RightClick: 3,
};

export function mapDomKeyToUiKey(code) {
  switch (code) {
    case "ArrowUp":
    case "Numpad8":
      return UI_KEY.Up;
    case "ArrowDown":
    case "Numpad2":
      return UI_KEY.Down;
    case "ArrowLeft":
    case "Numpad4":
      return UI_KEY.Left;
    case "ArrowRight":
    case "Numpad6":
      return UI_KEY.Right;
    case "Enter":
    case "NumpadEnter":
    case "Space":
    case "KeyZ":
    case "ControlLeft":
      return UI_KEY.Confirm;
    case "Escape":
    case "Backspace":
    case "KeyX":
    case "Insert":
    case "Numpad0":
      return UI_KEY.Cancel;
    case "AltLeft":
    case "AltRight":
      return UI_KEY.MenuToggle;
    case "PageUp":
    case "Numpad9":
      return UI_KEY.PageUp;
    case "PageDown":
    case "Numpad3":
      return UI_KEY.PageDown;
    case "Home":
    case "Numpad7":
      return UI_KEY.Home;
    case "End":
    case "Numpad1":
      return UI_KEY.End;
    case "KeyR":
      return UI_KEY.BattleRepeat;
    case "KeyA":
      return UI_KEY.BattleAuto;
    case "KeyD":
      return UI_KEY.BattleDefend;
    case "KeyE":
      return UI_KEY.BattleUseItem;
    case "KeyW":
      return UI_KEY.BattleThrowItem;
    case "KeyQ":
      return UI_KEY.BattleFlee;
    case "KeyF":
      return UI_KEY.BattleForce;
    case "KeyS":
      return UI_KEY.BattleStatus;
    default:
      return 0;
  }
}

export class WasmCanvasPresenter {
  constructor(canvas, wasmExports, wasmMemory, baseUrl = resolveAssetBaseUrl()) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d", { alpha: false });
    this.w = wasmExports;
    this.mem = wasmMemory;
    this.assets = new WasmAssetProvider(wasmExports, wasmMemory, baseUrl);
  }

  /**
   * Render one frame inside WASM, pump any newly-requested assets, and blit the
   * linear-memory RGBA framebuffer directly to the HTML5 Canvas.
   */
  presentFrame() {
    if (!this.w?.wasm_render_frame) return false;
    const ptr = this.w.wasm_render_frame();
    const w = this.w.wasm_fb_width();
    const h = this.w.wasm_fb_height();
    if (!ptr || !w || !h) return false;

    if (this.canvas.width !== w || this.canvas.height !== h) {
      this.canvas.width = w;
      this.canvas.height = h;
    }

    const byteLen = w * h * 4;
    const view = new Uint8ClampedArray(this.mem.buffer, ptr, byteLen);
    if (typeof ImageData !== "undefined" && typeof this.ctx?.putImageData === "function") {
      const imgData = new ImageData(view, w, h);
      this.ctx.putImageData(imgData, 0, 0);
    }

    this.assets.pumpRequests();
    return true;
  }

}
