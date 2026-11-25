/**
 * Browser-host regressions (audio, YAML parser, input, coordinate mapping, clipboard) for the
 * Rust-core site. Game-logic regressions live in `crates/pal-core/tests/regressions_*.rs`.
 */
import assert from "node:assert/strict";
import { bootPalApp } from "../lib/headless-harness.mjs";
import { AudioManager } from "../../site/js/audio/audio-manager.js";
import { parseYaml } from "../../site/js/vendor/yaml.js";
import { copyTextSafe } from "../../site/js/host/wasm-runtime.js";

const { app, winListeners, getClipboardText } = await bootPalApp({ bootTimeoutMs: 60000 });
const { renderer } = app;
assert.ok(app.core, "Rust core host must be active");

// ---- #24: AudioManager pendingTrack must play after first gesture unlocks ctx ----
{
  let loadedCount = 0;
  const fakeWasm = { synthLoadMidi: () => { loadedCount++; }, synthInit: () => {}, synthSetHdMode: () => {} };
  const am = new AudioManager(fakeWasm);
  await am.playMusic(2, true);
  assert.deepEqual(am.pendingTrack, { trackId: 2, loop: true }, "#24: pre-gesture track queued");
  assert.equal(am.currentTrackId, 0, "#24: currentTrackId not prematurely set before ctx exists");
  am.ctx = { state: "running", resume: async () => {} };
  await am.playMusic(am.pendingTrack.trackId, am.pendingTrack.loop);
  assert.equal(am.currentTrackId, 2, "#24: queued track plays on unlock instead of early-returning");
  console.log("  PASS #24: AudioManager pendingTrack plays cleanly after gesture unlock");
}

// ---- #33: AudioManager safe native-rate AudioContext, persistent unlock & SFX deduplication ----
{
  const origAudioCtx = globalThis.window.AudioContext;
  // 1. Throwing AudioContext must not crash ensureContext
  globalThis.window.AudioContext = class { constructor() { throw new Error("NotAllowed"); } };
  const amFail = new AudioManager({ synthInit: () => {}, synthSetHdMode: () => {}, synthSetVolume: () => {} });
  assert.equal(amFail.ensureContext(), false, "#33: ensureContext catches AudioContext constructor errors");

  // 2. Native sample rate (no hardcoded 44100 arg), stays unlocked=false while suspended, unlocks on running
  let ctorArgs = null;
  let initRate = 0;
  let resumeCalls = 0;
  globalThis.window.AudioContext = class {
    constructor(...a) {
      ctorArgs = a;
      this.sampleRate = 48000;
      this.state = "suspended";
      this.destination = {};
    }
    createGain() { return { gain: { value: 1 }, connect() {} }; }
    createScriptProcessor() { return { connect() {}, onaudioprocess: null }; }
    createBufferSource() { return { buffer: null, connect() {}, start() {} }; }
    createBiquadFilter() { return { type: "", frequency: { value: 0 }, connect() {} }; }
    async decodeAudioData() { return { duration: 0.1 }; }
    async resume() {
      resumeCalls++;
      if (resumeCalls >= 2) this.state = "running";
    }
  };
  const am = new AudioManager({
    synthInit: (r) => { initRate = r; },
    synthSetHdMode: () => {},
    synthSetVolume: () => {},
    synthLoadMidi: () => {},
  });
  assert.equal(am.ensureContext(), true);
  assert.equal(ctorArgs.length, 0, "#33: AudioContext constructed with native device sampleRate");
  assert.equal(initRate, 48000, "#33: synthInit receives native 48000Hz rate");
  assert.equal(am.unlocked, false, "#33: unlocked stays false while AudioContext is suspended");
  await am.unlockOnGesture();
  assert.equal(am.unlocked, true, "#33: unlockOnGesture sets unlocked=true once state becomes running");

  // 3. In-flight SFX fetch deduplication
  let fetchCount = 0;
  const origFetch = globalThis.fetch;
  globalThis.fetch = async (url, ...rest) => {
    if (!String(url).includes("sfx_009")) {
      return origFetch(url, ...rest);
    }
    fetchCount++;
    await new Promise((r) => setTimeout(r, 15));
    return { ok: true, arrayBuffer: async () => new ArrayBuffer(8) };
  };
  await Promise.all([am.playSfx(9), am.playSfx(9), am.playSfx(9)]);
  globalThis.fetch = origFetch;
  globalThis.window.AudioContext = origAudioCtx;
  assert.equal(fetchCount, 1, "#33: concurrent playSfx(9) must deduplicate into 1 fetch");
  console.log("  PASS #33: AudioManager native rate, safe ctor, persistent unlock & SFX dedup verified");
}

// ---- #49 & #20: YAML parser rejects prototype pollution (block + flow) & handles \\" escapes ----
{
  const parsed = parseYaml(
    'safe: 1\n__proto__:\n  polluted: true\nconstructor:\n  bad: 1\nflow: { __proto__: { hacked: 1 }, ok: 2 }\nesc: "a\\\\" # trailing comment\n'
  );
  assert.equal({}.polluted, undefined, "#49: Object.prototype must not be polluted by block mapping");
  assert.equal({}.hacked, undefined, "#20: Object.prototype must not be polluted by flow object");
  assert.equal(parsed.safe, 1, "#49: safe YAML keys still parse");
  assert.equal(parsed.flow.ok, 2, "#20: safe keys in flow object still parse");
  assert.equal(parsed.esc, "a\\", "#20: string ending in escaped backslash must not swallow closing quote");
  console.log("  PASS #49/#20: YAML prototype-pollution & escaped-quote guards verified");
}

// ---- #41: window blur clears stuck movement keys ----
{
  app.keysDown.add("arrowup");
  for (const fn of winListeners.get("blur") || []) fn();
  assert.equal(app.keysDown.size, 0, "#41: window blur must clear keysDown");
  console.log("  PASS #41: blur handler clears stuck keysDown");
}

// ---- #52: clientToWorld accounts for 16:10 letterboxing ----
{
  const origRect = renderer.canvas.getBoundingClientRect;
  renderer.canvas.getBoundingClientRect = () => ({ left: 0, top: 0, width: 1600, height: 800 });
  const pt = renderer.clientToWorld(800, 400, 100, 50);
  assert.equal(Math.round(pt.screenX), 160);
  assert.equal(Math.round(pt.screenY), 100);
  assert.equal(pt.worldX, 260, "#52: camera origin is added");
  assert.equal(Math.round(renderer.clientToWorld(160, 0).screenX), 0, "#52: letterboxed left edge maps to 0");
  renderer.canvas.getBoundingClientRect = origRect;
  console.log("  PASS #52: clientToWorld letterbox coordinate mapping verified");
}

// ---- #76: LAN-safe clipboard helper ----
{
  assert.equal(await copyTextSafe("diag-text"), true);
  assert.equal(getClipboardText(), "diag-text", "#76: copyTextSafe writes the clipboard");
  console.log("  PASS #76: copyTextSafe verified");
}

// ---- HostCmd deadlock guard: failed load_scene / rng still sends core_reply ----
{
  const replies = [];
  const origW = app.core.w;
  const origLoadScene = app.core.loadSceneAssets;
  const origPlayRng = app.renderer.playRngCutscene;
  app.core.w = { ...origW, core_reply: (kind, arg) => replies.push([kind, arg]) };
  app.core.loadSceneAssets = async () => { throw new Error("network down"); };
  app.renderer.playRngCutscene = async () => { throw new Error("rng decode error"); };
  app.core._enqueueCmd({ cmd: "load_scene", map: 99 });
  app.core._enqueueCmd({ cmd: "rng", id: 0, start: 0, end: 1, speed: 10 });
  await app.core._cmdQueue;
  app.core.w = origW;
  app.core.loadSceneAssets = origLoadScene;
  app.renderer.playRngCutscene = origPlayRng;
  assert.deepEqual(replies, [[4, 0], [5, 0]], "failed load_scene and rng must still reply to prevent core deadlock");
  console.log("  PASS: async HostCmd deadlock guard & serial queue verified");
}

// ---- Audit #4: ToggleMusic / ToggleSfx act independently ----
{
  const { UI_ACTION } = await import("../../site/js/host/wasm-runtime.js");
  let musicToggled = 0;
  let sfxToggled = 0;
  const origAudio = app.hud.audio;
  app.hud.audio = {
    toggleMusic: () => { musicToggled++; },
    toggleSfx: () => { sfxToggled++; },
    toggleMute: () => { throw new Error("ToggleMusic must not call toggleMute"); },
  };
  await app.hud._executeAction(UI_ACTION.ToggleMusic, 0);
  await app.hud._executeAction(UI_ACTION.ToggleSfx, 0);
  app.hud.audio = origAudio;
  assert.equal(musicToggled, 1, "ToggleMusic must call audio.toggleMusic()");
  assert.equal(sfxToggled, 1, "ToggleSfx must call audio.toggleSfx()");
  console.log("  PASS: ToggleMusic & ToggleSfx independent dispatch verified");
}

// ---- Audit #17: core.uiPointer() respects 16:10 letterboxing ----
{
  const origRect = renderer.canvas.getBoundingClientRect;
  const origW = app.core.w;
  const ptrCalls = [];
  renderer.canvas.getBoundingClientRect = () => ({ left: 0, top: 0, width: 1600, height: 800 });
  app.core.w = {
    ...origW,
    wasm_ui_on_pointer: (k, x, y) => { ptrCalls.push([k, x, y]); return 0; },
    core_tick_count: () => 0,
  };
  // With 1600x800 (2:1) and 16:10 content (1280x800), left pillarbox is 160px -> clientX=160 maps to x=0
  app.core.uiPointer({ clientX: 160, clientY: 400 }, 1);
  app.core.uiPointer({ clientX: 800, clientY: 400 }, 1);
  app.core.w = origW;
  renderer.canvas.getBoundingClientRect = origRect;
  assert.equal(ptrCalls[0][1], 0, "left edge of 16:10 content maps to logical x=0");
  assert.equal(ptrCalls[1][1], 160, "center of 16:10 content maps to logical x=160");
  assert.equal(ptrCalls[1][2], 100, "center of 16:10 content maps to logical y=100");
  console.log("  PASS: core.uiPointer() letterbox mapping verified");
}

// ---- Audit #13: PalSynthWorkletProcessor throttles refill requests via pendingFrames ----
{
  const fs = await import("node:fs");
  const vm = await import("node:vm");
  const code = fs.readFileSync(new URL("../../site/js/audio/pal-synth-worklet.js", import.meta.url), "utf8");
  let ProcessorCls = null;
  const posted = [];
  class BaseProcessor {
    constructor() {
      this.port = { onmessage: null, postMessage: (m) => posted.push(m) };
    }
  }
  vm.runInNewContext(code, {
    AudioWorkletProcessor: BaseProcessor,
    registerProcessor: (_name, cls) => { ProcessorCls = cls; },
    Float32Array,
  });
  const proc = new ProcessorCls();
  const out = [[new Float32Array(128), new Float32Array(128)]];
  for (let i = 0; i < 10; i++) proc.process([], out);
  assert.equal(posted.length, 2, "worklet must cap in-flight refill requests (8192 / 4096 = 2) instead of posting every quantum");
  assert.equal(posted[0].need, 4096, "each worklet refill requests 4096 frames");
  proc.port.onmessage({ data: { l: new Float32Array(2048), r: new Float32Array(2048) } });
  assert.equal(proc.available, 2048);
  assert.equal(proc.pendingFrames, 6144);
  console.log("  PASS: PalSynthWorkletProcessor pendingFrames throttling verified");
}

// ---- #274: Original SDLPAL keybindings (Arrows/Numpad, no WASD movement, F2 HD toggle, battle shortcuts) ----
{
  const { mapDomKeyToUiKey, UI_KEY } = await import("../../site/js/host/wasm-presenter.js");
  assert.equal(mapDomKeyToUiKey("ArrowUp"), UI_KEY.Up);
  assert.equal(mapDomKeyToUiKey("Numpad8"), UI_KEY.Up);
  assert.equal(mapDomKeyToUiKey("ArrowDown"), UI_KEY.Down);
  assert.equal(mapDomKeyToUiKey("Numpad2"), UI_KEY.Down);
  assert.equal(mapDomKeyToUiKey("ArrowLeft"), UI_KEY.Left);
  assert.equal(mapDomKeyToUiKey("Numpad4"), UI_KEY.Left);
  assert.equal(mapDomKeyToUiKey("ArrowRight"), UI_KEY.Right);
  assert.equal(mapDomKeyToUiKey("Numpad6"), UI_KEY.Right);
  assert.equal(mapDomKeyToUiKey("ControlLeft"), UI_KEY.Confirm);
  assert.equal(mapDomKeyToUiKey("NumpadEnter"), UI_KEY.Confirm);
  assert.equal(mapDomKeyToUiKey("Insert"), UI_KEY.Cancel);
  assert.equal(mapDomKeyToUiKey("Numpad0"), UI_KEY.Cancel);
  assert.equal(mapDomKeyToUiKey("AltLeft"), UI_KEY.MenuToggle);
  assert.equal(mapDomKeyToUiKey("PageUp"), UI_KEY.PageUp);
  assert.equal(mapDomKeyToUiKey("Numpad9"), UI_KEY.PageUp);
  assert.equal(mapDomKeyToUiKey("PageDown"), UI_KEY.PageDown);
  assert.equal(mapDomKeyToUiKey("Numpad3"), UI_KEY.PageDown);
  assert.equal(mapDomKeyToUiKey("Home"), UI_KEY.Home);
  assert.equal(mapDomKeyToUiKey("End"), UI_KEY.End);
  assert.equal(mapDomKeyToUiKey("KeyR"), UI_KEY.BattleRepeat);
  assert.equal(mapDomKeyToUiKey("KeyA"), UI_KEY.BattleAuto);
  assert.equal(mapDomKeyToUiKey("KeyD"), UI_KEY.BattleDefend);
  assert.equal(mapDomKeyToUiKey("KeyE"), UI_KEY.BattleUseItem);
  assert.equal(mapDomKeyToUiKey("KeyW"), UI_KEY.BattleThrowItem);
  assert.equal(mapDomKeyToUiKey("KeyQ"), UI_KEY.BattleFlee);
  assert.equal(mapDomKeyToUiKey("KeyF"), UI_KEY.BattleForce);
  assert.equal(mapDomKeyToUiKey("KeyS"), UI_KEY.BattleStatus);
  assert.equal(mapDomKeyToUiKey("Tab"), 0, "#274: Tab must not map in gameplay UI");

  // Movement: WASD must NOT move; Arrows & Numpad 2/4/8/6 MUST move
  app.keysDown.clear();
  for (const k of ["keyw", "keya", "keys", "keyd", "w", "a", "s", "d"]) {
    app.keysDown.clear();
    app.keysDown.add(k);
    assert.equal(app.getHeldDirection(), -1, `#274: ${k} must not move party`);
  }
  for (const [k, dir] of [["arrowdown", 0], ["numpad2", 0], ["arrowleft", 1], ["numpad4", 1], ["arrowup", 2], ["numpad8", 2], ["arrowright", 3], ["numpad6", 3]]) {
    app.keysDown.clear();
    app.keysDown.add(k);
    assert.equal(app.getHeldDirection(), dir, `#274: ${k} must map to dir ${dir}`);
  }
  app.keysDown.clear();

  // F2 toggles HD mode (and Tab does NOT toggle HD mode in gameplay)
  let hdToggles = 0;
  const origToggleHd = app.hud.toggleHdMode;
  app.hud.toggleHdMode = () => { hdToggles++; };
  const mkKey = (key, code) => ({ key, code, ctrlKey: false, altKey: false, metaKey: false, preventDefault() {} });
  app.onKeyDown(mkKey("F2", "F2"));
  assert.equal(hdToggles, 1, "#274: F2 must toggle HD mode");
  app.onKeyDown(mkKey("Tab", "Tab"));
  assert.equal(hdToggles, 1, "#274: Tab must not toggle HD mode");
  app.hud.toggleHdMode = origToggleHd;
  console.log("  PASS #274: SDLPAL keybindings, Numpad movement, no WASD & F2 HD toggle verified");
}

console.log("ALL HOST REGRESSION TESTS PASSED");
process.exit(0);
