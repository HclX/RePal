import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  DEFAULT_SITE_DIR,
  DEFAULT_BOOT_HTML,
  sleep,
  waitFor,
  MockClassList,
  make2dContext,
  parseSimpleHtmlInto,
  MockElement,
  MockDocument,
  MockImage,
  MockImageData,
  MockAudioContext,
  createMemoryStorage,
  createRepoFetch,
  installRepoFetch,
  createHeadlessBrowserEnv,
  installHeadlessGlobals,
  bootPalApp,
} from "../lib/headless-harness.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const TOOLS_DIR = path.resolve(__dirname, "..");

// ============================================================================
// 1. Duplication & Drift Guard Across All 6 Target Scripts
// ============================================================================
const TARGET_SCRIPTS = ["test_rust_core_host.mjs"];

const FORBIDDEN_LOCAL_STUBS = [
  [/class\s+MockClassList\b/, "local class MockClassList"],
  [/class\s+MockElement\b/, "local class MockElement"],
  [/class\s+MockDocument\b/, "local class MockDocument"],
  [/function\s+make2dContext\b/, "local function make2dContext"],
  [/\bmakeCtx\s*=/, "local makeCtx"],
  [/function\s+parseSimpleHtmlInto\b/, "local function parseSimpleHtmlInto"],
  [/function\s+parseHtmlInto\b/, "local function parseHtmlInto"],
  [/class\s+MockImage\b/, "local class MockImage"],
  [/globalThis\.fetch\s*=\s*async/, "local globalThis.fetch = async"],
  [/function\s+makeEl\b/, "local function makeEl"],
  [/globalThis\.AudioContext\s*=\s*class/, "local globalThis.AudioContext = class"],
  [/globalThis\.ImageData\s*=\s*class/, "local globalThis.ImageData = class"],
];

for (const scriptName of TARGET_SCRIPTS) {
  const src = await fs.readFile(path.join(TOOLS_DIR, scriptName), "utf-8");
  assert.match(
    src,
    /from\s+["']\.\/lib\/headless-harness\.mjs["']/,
    `${scriptName} must import ./lib/headless-harness.mjs`,
  );
  for (const [pattern, label] of FORBIDDEN_LOCAL_STUBS) {
    assert.doesNotMatch(
      src,
      pattern,
      `${scriptName} must not define ${label}`,
    );
  }
}
console.log("[PASS] headless suite duplication & drift guard passed");

// ============================================================================
// 2. Unit & Contract Tests for headless-harness.mjs
// ============================================================================

// 2a. MockClassList (with and without element)
{
  const clStandalone = new MockClassList();
  clStandalone.add("alpha", "beta", "");
  assert.equal(clStandalone.contains("alpha"), true);
  assert.equal(clStandalone.toggle("alpha"), false);
  assert.equal(clStandalone.contains("alpha"), false);
  assert.equal(clStandalone.toggle("gamma", true), true);
  assert.equal(clStandalone.toggle("gamma", false), false);
  clStandalone.remove("beta");
  assert.equal(clStandalone.contains("beta"), false);

  const el = new MockElement("div");
  el.classList.add("one", "two");
  assert.equal(el.className, "one two");
  el.className = "three four";
  assert.equal(el.classList.contains("three"), true);
  assert.equal(el.classList.contains("one"), false);
}

// 2b. MockElement + parseSimpleHtmlInto + selectors + bubbling
{
  const doc = new MockDocument();
  doc.body.innerHTML = `
    <!-- comment should be ignored -->
    <div id="pal-menu-tabs" class="panel active" data-tab-group="main">
      <button id="btn-ok" class="pal-tab selected" data-item-id="42" width="120" height="40">OK<input type="text" /><br /></button>
      <custom-widget class="pal-tab">TailText</custom-widget>
      LooseTail
    </div>
  `;
  const tabs = doc.getElementById("pal-menu-tabs");
  const btn = doc.getElementById("btn-ok");
  assert.ok(tabs && btn);
  assert.equal(tabs.dataset.tabGroup, "main");
  assert.equal(btn.dataset.itemId, "42");
  assert.equal(btn.width, 120);
  assert.equal(btn.height, 40);
  assert.equal(btn.isConnected, true);
  assert.equal(btn.disabled, false);
  assert.deepEqual(btn.getBoundingClientRect(), {
    left: 0,
    top: 0,
    width: 120,
    height: 40,
  });
  assert.ok(btn.textContent.includes("OK"));
  assert.ok(tabs.textContent.includes("TailText"));
  assert.ok(tabs.textContent.includes("LooseTail"));

  // Selector matching: #id, .a.b, tag[attr="val"], *[attr="val"], [attr], descendant, comma groups
  assert.equal(doc.querySelector("#pal-menu-tabs .pal-tab.selected"), btn);
  assert.equal(doc.querySelectorAll(".pal-tab").length, 2);
  assert.equal(doc.querySelectorAll('#btn-ok, custom-widget[class="pal-tab"]').length, 2);
  assert.equal(doc.querySelector('button[data-item-id="42"]'), btn);
  assert.equal(doc.querySelector('*[data-item-id="42"]'), btn);
  assert.equal(doc.querySelector('[data-item-id]'), btn);
  assert.equal(btn.closest(".other, .panel.active"), tabs);
  assert.equal(btn.closest("#missing"), null);

  // Events: addEventListener({ once }), removeEventListener, bubbling, stopPropagation
  const log = [];
  const onParentClick = () => log.push("parent");
  const onBtnOnce = () => log.push("btn-once");
  const onBtnStay = () => log.push("btn-stay");
  const onBtnRemoved = () => log.push("btn-removed");
  tabs.addEventListener("click", onParentClick);
  btn.addEventListener("click", onBtnOnce, { once: true });
  btn.addEventListener("click", onBtnStay);
  btn.addEventListener("click", onBtnRemoved);
  btn.removeEventListener("click", onBtnRemoved);
  btn.click();
  btn.click();
  assert.deepEqual(log, ["btn-once", "btn-stay", "parent", "btn-stay", "parent"]);

  log.length = 0;
  btn.addEventListener("click", (e) => {
    log.push("stop");
    e.stopPropagation();
  });
  btn.click();
  assert.deepEqual(log, ["btn-stay", "stop"]);

  // Utility methods exist
  btn.focus();
  btn.blur();
  btn.select();
  btn.setSelectionRange(0, 1);
  btn.scrollIntoView();

  // textContent setter clears children
  tabs.textContent = "Reset";
  assert.equal(tabs.children.length, 0);
  assert.equal(tabs.textContent, "Reset");
}

// 2c. MockDocument autoCreateElements
{
  const autoDoc = new MockDocument({ autoCreateElements: true });
  const created = autoDoc.getElementById("game-canvas");
  assert.ok(created instanceof MockElement);
  assert.equal(created.id, "game-canvas");
  assert.equal(autoDoc.getElementById("game-canvas"), created);
  assert.equal(autoDoc.execCommand("copy"), true);
}

// 2d. make2dContext + MockImageData + MockImage + MockAudioContext + createMemoryStorage
{
  const draws = [];
  const ctx = make2dContext({ onDraw: (m) => draws.push(m) });
  ctx.fillRect(0, 0, 10, 10);
  ctx.clearRect(0, 0, 10, 10);
  ctx.drawImage({}, 0, 0);
  const imgData = ctx.createImageData(4, 4);
  assert.equal(imgData.width, 4);
  assert.equal(imgData.height, 4);
  assert.equal(imgData.data.length, 64);
  ctx.putImageData(imgData, 0, 0);
  assert.equal(ctx.getImageData(0, 0, 2, 2).data.length, 16);
  ctx.translate(1, 2);
  ctx.scale(2, 2);
  ctx.beginPath();
  ctx.moveTo(0, 0);
  ctx.lineTo(5, 5);
  ctx.ellipse(0, 0, 1, 1, 0, 0, Math.PI * 2);
  ctx.fill();
  ctx.stroke();
  ctx.fillText("hi", 0, 0);
  assert.equal(typeof ctx.measureText("hi").width, "number");
  const grad = ctx.createRadialGradient(0, 0, 1, 0, 0, 2);
  grad.addColorStop(0, "#000");
  assert.deepEqual(draws, ["fillRect", "clearRect", "drawImage", "putImageData", "fill", "stroke", "fillText"]);

  const md1 = new MockImageData(new Uint8ClampedArray(16), 2, 2);
  assert.equal(md1.width, 2);
  const md2 = new MockImageData(3, 4);
  assert.equal(md2.data.length, 48);

  const img = new MockImage();
  await new Promise((resolve) => {
    img.onload = resolve;
    img.src = "test.webp";
  });
  assert.equal(img.width, 512);

  const ac = new MockAudioContext();
  assert.equal(ac.sampleRate, 44100);
  assert.equal(ac.state, "running");
  await ac.resume();
  ac.createGain().connect(ac.destination);
  const proc = ac.createScriptProcessor(2048, 0, 2);
  proc.connect(ac.destination);
  const decoded = await ac.decodeAudioData(new ArrayBuffer(8));
  const src = ac.createBufferSource();
  src.buffer = decoded;
  src.connect(ac.createBiquadFilter());
  src.start();
  src.stop();

  const ls = createMemoryStorage();
  ls.setItem("k", 123);
  assert.equal(ls.getItem("k"), "123");
  assert.equal(ls.length, 1);
  assert.equal(ls.key(0), "k");
  ls.removeItem("k");
  assert.equal(ls.getItem("k"), null);
  ls.setItem("a", "1");
  ls.clear();
  assert.equal(ls.length, 0);
}

// 2e. createRepoFetch (200 hit, 404 miss, path traversal blocked)
{
  const repoFetch = createRepoFetch({ siteDir: DEFAULT_SITE_DIR });
  const hit = await repoFetch("./assets/data/roles.yaml?v=999");
  assert.equal(hit.ok, true);
  assert.equal(hit.status, 200);
  const txt = await hit.text();
  assert.ok(txt.length > 0);
  const ab = await hit.arrayBuffer();
  assert.ok(ab.byteLength > 0);
  const blob = await hit.blob();
  assert.equal((await blob.arrayBuffer()).byteLength, ab.byteLength);
  assert.equal(typeof hit.headers.get("content-type"), "string");

  const miss = await repoFetch("./assets/does_not_exist_xyz.json");
  assert.equal(miss.ok, false);
  assert.equal(miss.status, 404);
  assert.equal(await miss.text(), "");
  assert.deepEqual(await miss.json(), {});
  assert.equal((await miss.arrayBuffer()).byteLength, 0);
  assert.equal((await (await miss.blob()).arrayBuffer()).byteLength, 0);
  assert.equal(miss.headers.get("content-type"), "");

  const traversal = await repoFetch("../tools/tests/test_headless_harness.mjs");
  assert.equal(traversal.ok, false);
  assert.equal(traversal.status, 404);
}

// 2f. installHeadlessGlobals + uninstall (timeout & noop modes)
{
  const prevDoc = globalThis.document;
  const env = installHeadlessGlobals({ rafMode: "noop" });
  assert.equal(globalThis.document, env.doc);
  assert.equal(globalThis.window, globalThis);
  assert.equal(globalThis.isSecureContext, true);
  assert.equal(globalThis.innerWidth, 1280);
  assert.equal(globalThis.innerHeight, 800);
  assert.equal(globalThis.devicePixelRatio, 1);
  assert.equal(globalThis.location.href, "http://localhost/");
  await globalThis.navigator.clipboard.writeText("copied-text");
  assert.equal(env.getClipboardText(), "copied-text");
  assert.equal(globalThis.requestAnimationFrame(() => {}), 0);
  env.uninstall();
  assert.equal(globalThis.document, prevDoc);

  const envTimeout = installHeadlessGlobals({ rafMode: "timeout" });
  let rafFired = false;
  const rid = globalThis.requestAnimationFrame(() => {
    rafFired = true;
  });
  await waitFor(() => rafFired, 1000, "timeout raf callback");
  assert.ok(rid);
  const rid2 = globalThis.requestAnimationFrame(() => {});
  globalThis.cancelAnimationFrame(rid2);
  envTimeout.uninstall();
}

// 2g. waitFor timeout error
{
  let threw = false;
  try {
    await waitFor(() => false, 40, "expected-timeout", 10);
  } catch (err) {
    threw = /Timeout waiting for: expected-timeout/.test(err.message);
  }
  assert.equal(threw, true);
}

console.log("[PASS] headless-harness.mjs unit & contract tests passed");

// ============================================================================
// 3. bootPalApp Smoke Test (rafMode: "noop" so script exits cleanly)
// ============================================================================
{
  let drawCalls = 0;
  const { app, doc, uninstall } = await bootPalApp({
    rafMode: "noop",
    bootTimeoutMs: 20000,
    onCanvasDraw: (method) => {
      if (method === "fillRect" || method === "drawImage" || method === "putImageData") {
        drawCalls++;
      }
    },
  });
  assert.ok(app?.hud && app?.core && app?.wasm && app?.renderer, "bootPalApp must expose core subsystems");
  const canvas = doc.getElementById("game-canvas");
  assert.ok(canvas, "game-canvas must exist in default boot DOM");
  app.core.newGame(1);
  app.core.frame(1000);
  app.core.frame(1100);
  assert.ok(drawCalls > 0, `Expected drawCalls > 0 after renderWorld, got ${drawCalls}`);
  uninstall();
}

console.log("[PASS] bootPalApp smoke test passed");
