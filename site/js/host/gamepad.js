/**
 * HTML5 Gamepad API controller for Web & TV browsers (Xbox / PlayStation / Switch Pro / 8BitDo).
 *
 * Standard Gamepad Mapping:
 *   0 = A (Cross / South)     -> Confirm / Interact (Gameplay) | Play/Pause (Replay)
 *   1 = B (Circle / East)     -> Cancel / Back (Gameplay)      | Replay Menu (Replay)
 *   2 = X (Square / West)     -> Toggle Fullscreen (Gameplay)  | Speed cycle (Replay)
 *   3 = Y (Triangle / North)  -> Root Menu (Gameplay)          | Chapters List (Replay)
 *   4 = LB (L1)               -> PageUp (Gameplay)             | Prev Chapter (Replay)
 *   5 = RB (R1)               -> PageDown (Gameplay)           | Next Chapter (Replay)
 *   6 = LT (L2)               -> —                             | Speed Down (Replay)
 *   7 = RT (R2)               -> —                             | Speed Up (Replay)
 *   8 = Select / Share        -> Toggle HD/Retro (#274)
 *   9 = Start / Options       -> Open/Close Menu (Gameplay & Replay)
 *   12..15 = D-Pad Up/Down/Left/Right + Left Stick (axes[0..1], deadzone 0.38)
 */

const DEADZONE = 0.38;
const REPEAT_INITIAL_MS = 280;
const REPEAT_INTERVAL_MS = 95;

export class GamepadController {
  constructor(app) {
    this.app = app;
    this.prevButtons = new Map();
    this.dirRepeat = { dir: null, nextAt: 0 };
    this.heldDir = -1; // PAL95: 0=SW(Down), 1=NW(Left), 2=NE(Up), 3=SE(Right)
    this.connected = false;
    this.lastInputMode = "keyboard"; // "gamepad" | "keyboard" | "mouse"

    window.addEventListener("gamepadconnected", (e) => {
      this.connected = true;
      this.lastInputMode = "gamepad";
      const name = e.gamepad?.id?.split("(")[0]?.trim() || "Gamepad";
      this.app.hud?.showToast?.(`🎮 已连接手柄：${name}`);
    });
    window.addEventListener("gamepaddisconnected", () => {
      this.heldDir = -1;
      this.prevButtons.clear();
    });
  }

  markInputMode(mode) {
    this.lastInputMode = mode;
  }

  getHeldDirection() {
    return this.heldDir;
  }

  _pickGamepad() {
    if (typeof navigator === "undefined" || typeof navigator.getGamepads !== "function") {
      return null;
    }
    const pads = navigator.getGamepads();
    for (let i = 0; i < pads.length; i++) {
      if (pads[i] && pads[i].connected) return pads[i];
    }
    return null;
  }

  _justPressed(gp, idx) {
    const cur = Boolean(gp.buttons[idx]?.pressed || (gp.buttons[idx]?.value ?? 0) > 0.5);
    const prev = Boolean(this.prevButtons.get(idx));
    this.prevButtons.set(idx, cur);
    if (cur) {
      this.lastInputMode = "gamepad";
      if (!prev && !this.app.audio?.unlocked) {
        this.app.audio?.unlockOnGesture?.();
      }
    }
    return cur && !prev;
  }

  poll(now) {
    const gp = this._pickGamepad();
    if (!gp) {
      this.heldDir = -1;
      return;
    }
    this.connected = true;

    // Sample D-pad + Left Stick
    const ax = gp.axes?.[0] ?? 0;
    const ay = gp.axes?.[1] ?? 0;
    const up = Boolean(gp.buttons[12]?.pressed) || ay < -DEADZONE;
    const down = Boolean(gp.buttons[13]?.pressed) || ay > DEADZONE;
    const left = Boolean(gp.buttons[14]?.pressed) || ax < -DEADZONE;
    const right = Boolean(gp.buttons[15]?.pressed) || ax > DEADZONE;

    // Update continuous movement direction for overworld exploration
    if (down) this.heldDir = 0;
    else if (left) this.heldDir = 1;
    else if (up) this.heldDir = 2;
    else if (right) this.heldDir = 3;
    else this.heldDir = -1;
    if (this.heldDir >= 0) this.lastInputMode = "gamepad";

    // Edge-triggered + smooth repeat directional key for menus & library overlay
    const navKey = up ? "ArrowUp" : down ? "ArrowDown" : left ? "ArrowLeft" : right ? "ArrowRight" : null;
    let emitNav = null;
    if (navKey) {
      if (this.dirRepeat.dir !== navKey) {
        this.dirRepeat = { dir: navKey, nextAt: now + REPEAT_INITIAL_MS };
        emitNav = navKey;
      } else if (now >= this.dirRepeat.nextAt) {
        this.dirRepeat.nextAt = now + REPEAT_INTERVAL_MS;
        emitNav = navKey;
      }
    } else {
      this.dirRepeat = { dir: null, nextAt: 0 };
    }

    const a = this._justPressed(gp, 0);
    const b = this._justPressed(gp, 1);
    const x = this._justPressed(gp, 2);
    const y = this._justPressed(gp, 3);
    const lb = this._justPressed(gp, 4);
    const rb = this._justPressed(gp, 5);
    const lt = this._justPressed(gp, 6);
    const rt = this._justPressed(gp, 7);
    const select = this._justPressed(gp, 8);
    const start = this._justPressed(gp, 9);

    if (select) {
      this.app.hud?.toggleHdMode?.();
    }

    const app = this.app;
    const theater = app.replayTheater;

    // 1. In-Game Replay Library Modal (100% couch-navigable)
    if (theater?.isLibraryOpen?.()) {
      if (emitNav === "ArrowUp") theater.moveLibraryCursor(-1);
      else if (emitNav === "ArrowDown") theater.moveLibraryCursor(1);
      else if (a) theater.confirmLibrarySelection();
      else if (b || start) theater.closeLibrary();
      return;
    }

    // 2. Skip RNG cutscene with A / B / Start
    if (app.renderer?.activeRngFrameImg && (a || b || start)) {
      app.cutsceneSkip = true;
      return;
    }

    // 3. Replay Theater Mode
    if (theater?.active) {
      if (theater.isReplayMenuOpen()) {
        if (emitNav) app.core.uiKey(emitNav);
        if (lb) app.core.uiKey("ArrowLeft");
        if (rb) app.core.uiKey("ArrowRight");
        if (a) app.core.uiKey("Enter");
        if (b || start) app.core.uiKey("Escape");
      } else {
        if (a) theater.togglePlayPause();
        if (lb) theater.prevSegment();
        if (rb) theater.nextSegment();
        if (lt) theater.speedDown();
        if (rt || x) theater.speedUp();
        if (y || emitNav === "ArrowUp") theater.openChapters();
        if (start || b) theater.openReplayMenu();
      }
      return;
    }

    // 4. Normal Gameplay UI (Title, Menus, Dialog, Shop, Battle HUD)
    if (app.uiActive()) {
      if (emitNav) app.core.uiKey(emitNav);
      if (lb) app.core.uiKey("PageUp");
      if (rb) app.core.uiKey("PageDown");
      if (a) app.core.uiKey("Enter");
      if (b) app.core.uiKey("Escape");
      if (start) app.core.uiKey("Escape");
      return;
    }

    // 5. Normal Overworld Exploration
    if (a) {
      app.core.queueInteract();
    } else if (start || y) {
      if (!app.coreBusy()) app.core.toggleRootMenu(true);
    } else if (x) {
      document.getElementById("btn-stage-fullscreen")?.click();
    }
  }
}
