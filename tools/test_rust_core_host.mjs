#!/usr/bin/env node
/**
 * Boots the real site in headless Node and plays the opening through the
 * browser host path (RustCoreHost + pal.wasm core + pal-ui), then checks the canonical state
 * against the legacy-JS golden checkpoint and verifies frame-exact replay determinism.
 */
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { bootPalApp, sleep, waitFor } from "./lib/headless-harness.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const { app } = await bootPalApp({
  siteDir: path.resolve(__dirname, "../site"),
  bootTimeoutMs: 60000,
});
const core = app.core;
assert.ok(core, "RustCoreHost must be active");
assert.equal(app.wasm.exports.core_is_active(), 1);

/** Dismiss dialogues/choices until the core is idle (pressing Enter like a player). */
async function playUntilIdle(maxMs = 30000) {
  const t0 = Date.now();
  let idleFrames = 0;
  while (Date.now() - t0 < maxMs) {
    if (app.hud.isModalOpen() && !app.hud.isTitleVisible()) core.uiKey("Enter");
    idleFrames = app.wasm.exports.core_is_busy() || app.hud.isModalOpen() ? 0 : idleFrames + 1;
    if (idleFrames > 10) return;
    await sleep(5);
  }
  throw new Error("core still busy");
}

// Title -> New Game through the real pal-ui title menu.
assert.ok(app.hud.isTitleVisible(), "title screen visible");
core.uiKey("Enter");
await waitFor(() => app.wasm.exports.core_is_busy() === 1, 5000, "new game started");
await playUntilIdle();

const golden = JSON.parse(await fs.readFile(path.join(__dirname, "parity/golden/ch1_opening.json"), "utf8"));
const want = golden.checkpoints.find((c) => c.checkpoint === "after_intro").state;
const got = core.canonicalState();
assert.deepEqual(got, want, "browser Rust-core intro must match the legacy JS golden state");
console.log("  PASS: intro via pal-ui matches legacy golden (scene", got.sceneId, ")");

// Walk a few steps with held keys, then talk to the bedroom door (Ev#10).
app.keysDown.add("arrowup");
await sleep(400);
app.keysDown.clear();
await playUntilIdle();
const moved = core.canonicalState();
assert.notDeepEqual(moved.party, got.party, "held key must move the party");

// Replay determinism: replay the recorded log from the same seed and compare.
const rep = core.exportReplay();
const before = core.canonicalState();
core.startReplay(rep);
await waitFor(() => core.replay === null, 30000, "replay finished", 10);
await playUntilIdle();
assert.deepEqual(core.canonicalState(), before, "replay must reproduce the exact state");
console.log("  PASS: frame-exact replay reproduced", rep.log.length, "recorded inputs");

// Save / load round trip through localStorage + save_json anchor replay determinism.
core.saveSlot(1);
app.keysDown.add("arrowdown");
await sleep(300);
app.keysDown.clear();
await playUntilIdle();
await app.hud.onLoadSlot(1);
await playUntilIdle();
assert.deepEqual(core.canonicalState().party, before.party, "load restores party position");

// Walk after loading slot 1, export replay (now anchored on save_json!), and verify replay reproduces exact state.
app.keysDown.add("arrowleft");
await sleep(250);
app.keysDown.clear();
await playUntilIdle();
const afterLoadWalk = core.canonicalState();
const saveRep = core.exportReplay();
assert.equal(saveRep.anchor?.type, "save_json", "exportReplay after loadSaveText must include save_json anchor");
core.startReplay(saveRep);
await waitFor(() => core.replay === null, 30000, "save-anchored replay finished", 10);
await playUntilIdle();
assert.deepEqual(core.canonicalState(), afterLoadWalk, "save_json-anchored replay must reproduce exact state");
console.log("  PASS: save/load round trip + save_json-anchored replay");
// Quick battle driven entirely through the pal-ui battle HUD (Enter = attack, Enter = target).
app.hud.hideTitleScreen();
app.wasm.exports.core_set_replay_debug_flags(1, 1);
const dbgBattleEv = JSON.stringify({ t: 0, e: "dbg", act: { op: "battle", gid: 5 } });
app.wasm.exports.core_replay_event(core._scratch(dbgBattleEv));
await waitFor(() => app.wasm.exports.core_is_busy() === 1, 3000, "battle started");
const t0 = Date.now();
while (app.wasm.exports.core_is_busy() && Date.now() - t0 < 60000) {
  if (app.hud.isModalOpen()) core.uiKey("Enter");
  await sleep(10);
}
assert.equal(app.wasm.exports.core_is_busy(), 0, "battle must finish");
console.log("  PASS: battle via pal-ui HUD finished; hero hp", core.canonicalState().roles[0].hp);
console.log("ALL RUST CORE HOST TESTS PASSED");
process.exit(0);
