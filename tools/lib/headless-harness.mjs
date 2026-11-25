/**
 * Shared Headless Browser Harness for Node.js Verification & Tooling (`tools/lib/headless-harness.mjs`)
 *
 * Consolidates the DOM, Canvas2D, Fetch, AudioContext, Image, ImageData,
 * localStorage, RAF, and PalRemakeApp boot stubs for headless Node execution.
 */

import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export const DEFAULT_SITE_DIR = path.resolve(__dirname, "../../site");
export const DEFAULT_BOOT_HTML = `
  <div id="boot-loading"><div id="boot-bar-fill"></div><div id="boot-status"></div></div>
  <main id="game-stage">
    <canvas id="game-canvas" width="1280" height="800"></canvas>
    <div id="hud-root"></div>
  </main>
`;

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export async function waitFor(pred, timeoutMs = 8000, label = "condition", pollMs = 15) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    if (pred()) return;
    await sleep(pollMs);
  }
  throw new Error(`Timeout waiting for: ${label}`);
}

export class MockClassList {
  constructor(el = null) {
    this.el = el;
    this.set = new Set();
  }
  add(...cls) {
    cls.forEach((c) => c && this.set.add(c));
  }
  remove(...cls) {
    cls.forEach((c) => this.set.delete(c));
  }
  contains(c) {
    return this.set.has(c);
  }
  toggle(c, force) {
    const shouldAdd = force !== undefined ? Boolean(force) : !this.set.has(c);
    if (shouldAdd) this.set.add(c);
    else this.set.delete(c);
    return shouldAdd;
  }
}

export class MockImageData {
  constructor(dataOrW, wOrH, maybeH) {
    if (typeof dataOrW === "number") {
      this.width = dataOrW;
      this.height = wOrH;
      this.data = new Uint8ClampedArray(Math.max(0, this.width * this.height * 4));
    } else {
      this.data = dataOrW;
      this.width = wOrH;
      this.height = maybeH;
    }
  }
}

export function make2dContext(opts = {}) {
  const onDraw = typeof opts.onDraw === "function" ? opts.onDraw : null;
  const record = (method, args) => {
    if (onDraw) onDraw(method, args);
  };
  return {
    imageSmoothingEnabled: opts.imageSmoothingEnabled ?? true,
    fillStyle: "#000",
    strokeStyle: opts.strokeStyle ?? "#fff",
    lineWidth: 1,
    globalAlpha: 1,
    filter: "none",
    font: "12px serif",
    textAlign: "left",
    fillRect(...args) {
      record("fillRect", args);
    },
    clearRect(...args) {
      record("clearRect", args);
    },
    drawImage(...args) {
      record("drawImage", args);
    },
    putImageData(...args) {
      record("putImageData", args);
    },
    getImageData(x, y, w, h) {
      return new MockImageData(w || 1, h || 1);
    },
    createImageData(w, h) {
      return new MockImageData(w || 1, h || 1);
    },
    save() {},
    restore() {},
    translate() {},
    scale() {},
    beginPath() {},
    moveTo() {},
    lineTo() {},
    ellipse() {},
    fill(...args) {
      record("fill", args);
    },
    stroke(...args) {
      record("stroke", args);
    },
    fillText(...args) {
      record("fillText", args);
    },
    measureText(text) {
      return { width: String(text ?? "").length * 6 };
    },
    createRadialGradient() {
      return { addColorStop() {} };
    },
  };
}

export function parseSimpleHtmlInto(parent, html) {
  const cleaned = String(html || "").replace(/<!--[\s\S]*?-->/g, "");
  const tagRe = /<\/?([a-zA-Z0-9_-]+)(\s[^>]*)?>/g;
  const stack = [parent];
  let lastIdx = 0;
  let m;
  const voidTags = new Set(["INPUT", "BR", "IMG", "HR", "META", "LINK"]);

  while ((m = tagRe.exec(cleaned)) !== null) {
    const rawText = cleaned.slice(lastIdx, m.index).trim();
    if (rawText && stack.length > 0) {
      const top = stack[stack.length - 1];
      top._textContent = top._textContent ? `${top._textContent} ${rawText}` : rawText;
    }
    lastIdx = tagRe.lastIndex;

    const full = m[0];
    const tag = m[1];
    const attrs = m[2] || "";
    if (full.startsWith("</")) {
      if (stack.length > 1) stack.pop();
    } else {
      const el = new MockElement(tag, parent.ownerDocument);
      const attrRe = /([a-zA-Z0-9_-]+)(?:="([^"]*)")?/g;
      let am;
      while ((am = attrRe.exec(attrs)) !== null) {
        if (am[1] === "/") continue;
        el.setAttribute(am[1], am[2] ?? "");
      }
      stack[stack.length - 1].appendChild(el);
      if (!full.endsWith("/>") && !voidTags.has(el.tagName)) {
        stack.push(el);
      }
    }
  }
  const tailText = cleaned.slice(lastIdx).trim();
  if (tailText && stack.length > 0) {
    const top = stack[stack.length - 1];
    top._textContent = top._textContent ? `${top._textContent} ${tailText}` : tailText;
  }
}

export class MockElement {
  constructor(tagName = "div", ownerDoc = null) {
    this.tagName = String(tagName || "div").toUpperCase();
    this.ownerDocument = ownerDoc;
    this.id = "";
    this.value = "";
    this._textContent = "";
    this._innerHTML = "";
    this.style = {};
    this.dataset = {};
    this.attributes = new Map();
    this.classList = new MockClassList(this);
    this.children = [];
    this.parentElement = null;
    this.listeners = new Map();
    this.onclick = null;
    this.width = 320;
    this.height = 200;
    this.disabled = false;
    this.isConnected = true;
  }

  set className(v) {
    this.classList.set = new Set(String(v || "").split(/\s+/).filter(Boolean));
  }
  get className() {
    return Array.from(this.classList.set).join(" ");
  }

  set textContent(v) {
    this._textContent = String(v ?? "");
    this.children = [];
  }
  get textContent() {
    const childText = (this.children || []).map((c) => c.textContent).filter(Boolean).join(" ");
    if (this._textContent && childText) return `${this._textContent} ${childText}`;
    return this._textContent || childText;
  }

  setAttribute(k, v) {
    const strVal = String(v);
    this.attributes.set(k, strVal);
    if (k === "id") {
      this.id = strVal;
      this.ownerDocument?.registerById(this);
    } else if (k === "class") {
      this.className = strVal;
    } else if (k === "width" || k === "height") {
      const n = Number(strVal);
      if (Number.isFinite(n)) this[k] = n;
    } else if (k === "disabled") {
      this.disabled = strVal !== "false";
    } else if (k.startsWith("data-")) {
      const camel = k.slice(5).replace(/-([a-z])/g, (_, ch) => ch.toUpperCase());
      this.dataset[camel] = strVal;
    }
  }

  getAttribute(k) {
    return this.attributes.has(k) ? this.attributes.get(k) : null;
  }

  hasAttribute(k) {
    return this.attributes.has(k);
  }

  appendChild(child) {
    child.parentElement = this;
    this.children.push(child);
    this.ownerDocument?.indexSubtree(child);
    return child;
  }

  removeChild(child) {
    const i = this.children.indexOf(child);
    if (i >= 0) this.children.splice(i, 1);
    child.parentElement = null;
  }

  remove() {
    this.parentElement?.removeChild(this);
  }

  focus() {}
  blur() {}
  select() {}
  setSelectionRange() {}
  scrollIntoView() {}

  getContext(_type, ctxOpts = {}) {
    return make2dContext({ ...(this.ownerDocument?.ctxOptions || {}), ...ctxOpts });
  }

  getBoundingClientRect() {
    return { left: 0, top: 0, width: this.width, height: this.height };
  }

  addEventListener(type, fn, opts) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push({ fn, once: Boolean(opts?.once) });
  }

  removeEventListener(type, fn) {
    const list = this.listeners.get(type);
    if (!list) return;
    for (let i = list.length - 1; i >= 0; i--) {
      if (list[i].fn === fn) list.splice(i, 1);
    }
  }

  dispatchEvent(ev) {
    const evt = ev || { type: "click" };
    evt.target = evt.target || this;
    if (typeof evt.stopPropagation !== "function") {
      evt.stopPropagation = () => {
        evt._stopped = true;
      };
    }
    if (typeof evt.preventDefault !== "function") {
      evt.preventDefault = () => {};
    }
    let cur = this;
    while (cur) {
      evt.currentTarget = cur;
      if (evt.type === "click" && typeof cur.onclick === "function") {
        cur.onclick(evt);
      }
      const list = cur.listeners.get(evt.type) || [];
      for (const item of [...list]) {
        if (item.once) {
          const idx = list.indexOf(item);
          if (idx >= 0) list.splice(idx, 1);
        }
        item.fn(evt);
      }
      if (evt._stopped) break;
      cur = cur.parentElement;
    }
    return true;
  }

  click() {
    this.dispatchEvent({
      type: "click",
      target: this,
      stopPropagation() {
        this._stopped = true;
      },
      preventDefault() {},
    });
  }

  matchesSelector(sel) {
    const s = String(sel || "").trim();
    if (!s) return false;
    if (s === "*") return true;
    if (s.startsWith("#")) return this.id === s.slice(1);
    if (s.startsWith(".")) {
      const parts = s.split(".").filter(Boolean);
      return parts.every((c) => this.classList.contains(c));
    }
    const attrM = s.match(/^([a-zA-Z0-9*_-]*)\[([a-zA-Z0-9_-]+)(?:="([^"]*)")?\]$/);
    if (attrM) {
      const [, tag, attrName, attrVal] = attrM;
      if (tag && tag !== "*" && this.tagName !== tag.toUpperCase()) return false;
      if (attrName.startsWith("data-")) {
        const camel = attrName.slice(5).replace(/-([a-z])/g, (_, ch) => ch.toUpperCase());
        const has = camel in this.dataset || this.attributes.has(attrName);
        if (!has) return false;
        const val = camel in this.dataset ? this.dataset[camel] : this.attributes.get(attrName);
        return attrVal === undefined || val === attrVal;
      }
      if (!this.attributes.has(attrName)) return false;
      return attrVal === undefined || this.attributes.get(attrName) === attrVal;
    }
    return this.tagName === s.toUpperCase();
  }

  closest(sel) {
    const groups = String(sel || "")
      .split(",")
      .map((g) => g.trim())
      .filter(Boolean);
    let cur = this;
    while (cur) {
      if (groups.some((g) => cur.matchesSelector?.(g))) return cur;
      cur = cur.parentElement;
    }
    return null;
  }

  querySelectorAll(sel) {
    const groups = String(sel || "")
      .split(",")
      .map((g) => g.trim())
      .filter(Boolean);
    const out = [];
    for (const grp of groups) {
      const tokens = grp.split(/\s+/);
      let roots = [this];
      for (const tok of tokens) {
        const next = [];
        const walk = (node, includeSelf) => {
          if (includeSelf && node.matchesSelector?.(tok)) next.push(node);
          for (const ch of node.children || []) walk(ch, true);
        };
        for (const r of roots) walk(r, false);
        roots = [...new Set(next)];
      }
      for (const el of roots) {
        if (!out.includes(el)) out.push(el);
      }
    }
    return out;
  }

  querySelector(sel) {
    return this.querySelectorAll(sel)[0] || null;
  }

  set innerHTML(html) {
    this._innerHTML = String(html || "");
    this._textContent = "";
    this.children = [];
    parseSimpleHtmlInto(this, this._innerHTML);
    this.ownerDocument?.indexSubtree(this);
  }
  get innerHTML() {
    return this._innerHTML;
  }
}

export class MockDocument {
  constructor(opts = {}) {
    this.autoCreateElements = Boolean(opts.autoCreateElements);
    this.ctxOptions = opts.ctxOptions || {};
    this.byId = new Map();
    this.listeners = new Map();
    this.body = new MockElement("body", this);
    this.head = new MockElement("head", this);
  }
  createElement(tag) {
    return new MockElement(tag, this);
  }
  registerById(el) {
    if (el.id) this.byId.set(el.id, el);
  }
  indexSubtree(root) {
    if (root.id) this.byId.set(root.id, root);
    for (const ch of root.children || []) this.indexSubtree(ch);
  }
  getElementById(id) {
    if (this.byId.has(id)) return this.byId.get(id);
    if (this.autoCreateElements) {
      const el = this.createElement("div");
      el.setAttribute("id", id);
      return el;
    }
    return null;
  }
  querySelectorAll(sel) {
    return this.body.querySelectorAll(sel);
  }
  querySelector(sel) {
    return this.body.querySelector(sel);
  }
  addEventListener(type, fn, opts) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push({ fn, once: Boolean(opts?.once) });
  }
  removeEventListener(type, fn) {
    const list = this.listeners.get(type);
    if (!list) return;
    for (let i = list.length - 1; i >= 0; i--) {
      if (list[i].fn === fn) list.splice(i, 1);
    }
  }
  execCommand() {
    return true;
  }
}

export class MockImage {
  constructor() {
    this.width = 512;
    this.height = 512;
    this.onload = null;
    this.onerror = null;
    this._src = "";
  }
  get src() {
    return this._src;
  }
  set src(url) {
    this._src = String(url ?? "");
    setTimeout(() => this.onload?.(), 1);
  }
}

export class MockAudioContext {
  constructor() {
    this.sampleRate = 44100;
    this.state = "running";
    this.destination = {};
  }
  async resume() {
    this.state = "running";
  }
  createGain() {
    return {
      gain: { value: 1, setValueAtTime() {}, linearRampToValueAtTime() {} },
      connect() {
        return this;
      },
      disconnect() {},
    };
  }
  createScriptProcessor() {
    return {
      onaudioprocess: null,
      connect() {
        return this;
      },
      disconnect() {},
    };
  }
  async decodeAudioData() {
    return { duration: 0.1, length: 4410, numberOfChannels: 2, sampleRate: this.sampleRate };
  }
  createBufferSource() {
    return {
      buffer: null,
      loop: false,
      onended: null,
      connect() {
        return this;
      },
      disconnect() {},
      start() {},
      stop() {},
    };
  }
  createBiquadFilter() {
    return {
      type: "lowpass",
      frequency: { value: 1000 },
      Q: { value: 1 },
      connect() {
        return this;
      },
      disconnect() {},
    };
  }
}

export function createMemoryStorage() {
  const storageMap = new Map();
  return {
    getItem: (k) => (storageMap.has(String(k)) ? storageMap.get(String(k)) : null),
    setItem: (k, v) => {
      storageMap.set(String(k), String(v));
    },
    removeItem: (k) => {
      storageMap.delete(String(k));
    },
    clear: () => {
      storageMap.clear();
    },
    key: (i) => Array.from(storageMap.keys())[i] ?? null,
    get length() {
      return storageMap.size;
    },
    _map: storageMap,
  };
}

function guessContentType(filePath) {
  const ext = path.extname(filePath).toLowerCase();
  if (ext === ".json") return "application/json; charset=utf-8";
  if (ext === ".yaml" || ext === ".yml") return "text/yaml; charset=utf-8";
  if (ext === ".wasm") return "application/wasm";
  if (ext === ".webp") return "image/webp";
  if (ext === ".png") return "image/png";
  if (ext === ".ttf") return "font/ttf";
  if (ext === ".js" || ext === ".mjs") return "application/javascript; charset=utf-8";
  return "application/octet-stream";
}

function make404Response() {
  return {
    ok: false,
    status: 404,
    headers: { get: () => "" },
    arrayBuffer: async () => new ArrayBuffer(0),
    text: async () => "",
    json: async () => ({}),
    blob: async () => ({
      arrayBuffer: async () => new ArrayBuffer(0),
      text: async () => "",
    }),
  };
}

export function createRepoFetch({ siteDir = DEFAULT_SITE_DIR } = {}) {
  const resolvedSiteDir = path.resolve(siteDir);
  return async (relUrl) => {
    const raw = String(relUrl || "").split("?")[0].split("#")[0];
    const clean = raw.replace(/^\.\//, "").replace(/^\/+/, "");
    const fullPath = path.resolve(resolvedSiteDir, clean);
    if (
      fullPath !== resolvedSiteDir &&
      !fullPath.startsWith(resolvedSiteDir + path.sep)
    ) {
      return make404Response();
    }
    try {
      const buf = await fs.readFile(fullPath);
      const ctype = guessContentType(fullPath);
      const copyBuffer = () =>
        buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength);
      return {
        ok: true,
        status: 200,
        headers: { get: (k) => (String(k).toLowerCase() === "content-type" ? ctype : "") },
        arrayBuffer: async () => copyBuffer(),
        text: async () => buf.toString("utf-8"),
        json: async () => JSON.parse(buf.toString("utf-8")),
        blob: async () => ({
          type: ctype,
          size: buf.byteLength,
          arrayBuffer: async () => copyBuffer(),
          text: async () => buf.toString("utf-8"),
        }),
      };
    } catch {
      return make404Response();
    }
  };
}

export function installRepoFetch(opts = {}) {
  const fn = createRepoFetch(opts);
  globalThis.fetch = fn;
  return fn;
}

export function createHeadlessBrowserEnv(options = {}) {
  const siteDir = options.siteDir ?? DEFAULT_SITE_DIR;
  const bootHtml = options.bootHtml ?? DEFAULT_BOOT_HTML;
  const rafMode = options.rafMode ?? "timeout";
  const autoCreateElements = Boolean(options.autoCreateElements);
  const ctxOptions = {
    ...(options.ctxOptions || {}),
    ...(options.onCanvasDraw ? { onDraw: options.onCanvasDraw } : {}),
  };

  const doc = new MockDocument({ autoCreateElements, ctxOptions });
  if (bootHtml) {
    doc.body.innerHTML = bootHtml;
  }

  const winListeners = new Map();
  const localStorage = createMemoryStorage();
  const storageMap = localStorage._map;
  let clipboardText = "";

  const navigator = {
    userAgent: "node",
    clipboard: {
      async writeText(t) {
        clipboardText = String(t ?? "");
      },
    },
  };

  const location = options.location ?? { search: "", href: "http://localhost/" };
  const innerWidth = options.innerWidth ?? 1280;
  const innerHeight = options.innerHeight ?? 800;
  const devicePixelRatio = options.devicePixelRatio ?? 1;

  const activeRafTimers = new Map();
  let nextRafId = 1;
  const requestAnimationFrame =
    rafMode === "noop"
      ? () => 0
      : (cb) => {
          const id = nextRafId++;
          const timer = setTimeout(() => {
            activeRafTimers.delete(id);
            cb(performance.now());
          }, 16);
          timer.unref?.();
          activeRafTimers.set(id, timer);
          return id;
        };

  const cancelAnimationFrame =
    rafMode === "noop"
      ? () => {}
      : (id) => {
          const timer = activeRafTimers.get(id);
          if (timer) {
            clearTimeout(timer);
            activeRafTimers.delete(id);
          }
        };

  const stopRaf = () => {
    for (const timer of activeRafTimers.values()) {
      clearTimeout(timer);
    }
    activeRafTimers.clear();
  };

  const dispatchWindowEvent = async (type, ev = {}) => {
    for (const fn of [...(winListeners.get(type) || [])]) {
      await fn(ev);
    }
  };

  const fetch = createRepoFetch({ siteDir });

  return {
    doc,
    winListeners,
    storageMap,
    localStorage,
    navigator,
    location,
    innerWidth,
    innerHeight,
    devicePixelRatio,
    fetch,
    getClipboardText: () => clipboardText,
    dispatchWindowEvent,
    requestAnimationFrame,
    cancelAnimationFrame,
    stopRaf,
    AudioContext: MockAudioContext,
    Image: MockImage,
    ImageData: MockImageData,
  };
}

export function installHeadlessGlobals(options = {}) {
  const env = createHeadlessBrowserEnv(options);
  const managedKeys = [
    "document",
    "window",
    "location",
    "innerWidth",
    "innerHeight",
    "devicePixelRatio",
    "isSecureContext",
    "navigator",
    "localStorage",
    "addEventListener",
    "removeEventListener",
    "requestAnimationFrame",
    "cancelAnimationFrame",
    "AudioContext",
    "Image",
    "ImageData",
    "fetch",
  ];

  const savedDescriptors = new Map();
  for (const key of managedKeys) {
    const desc = Object.getOwnPropertyDescriptor(globalThis, key);
    savedDescriptors.set(key, desc);
  }

  const defineVal = (key, value) => {
    Object.defineProperty(globalThis, key, {
      configurable: true,
      writable: true,
      enumerable: true,
      value,
    });
  };

  defineVal("document", env.doc);
  defineVal("window", globalThis);
  defineVal("location", env.location);
  defineVal("innerWidth", env.innerWidth);
  defineVal("innerHeight", env.innerHeight);
  defineVal("devicePixelRatio", env.devicePixelRatio);
  defineVal("isSecureContext", true);
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: env.navigator,
  });
  defineVal("localStorage", env.localStorage);
  defineVal("addEventListener", (type, fn) => {
    if (!env.winListeners.has(type)) env.winListeners.set(type, []);
    env.winListeners.get(type).push(fn);
  });
  defineVal("removeEventListener", (type, fn) => {
    const list = env.winListeners.get(type);
    if (!list) return;
    const idx = list.indexOf(fn);
    if (idx >= 0) list.splice(idx, 1);
  });
  defineVal("requestAnimationFrame", env.requestAnimationFrame);
  defineVal("cancelAnimationFrame", env.cancelAnimationFrame);
  defineVal("AudioContext", env.AudioContext);
  defineVal("Image", env.Image);
  defineVal("ImageData", env.ImageData);
  defineVal("fetch", env.fetch);

  const uninstall = () => {
    env.stopRaf();
    for (const key of managedKeys) {
      const desc = savedDescriptors.get(key);
      if (desc === undefined) {
        delete globalThis[key];
      } else {
        Object.defineProperty(globalThis, key, desc);
      }
    }
  };

  return {
    ...env,
    uninstall,
    dispose: uninstall,
  };
}

export async function bootPalApp(options = {}) {
  const env = options.reuseExistingGlobals
    ? null
    : installHeadlessGlobals(options);

  await import("../../site/js/main.js");
  if (env) {
    await env.dispatchWindowEvent("DOMContentLoaded");
  }

  const timeoutMs = options.bootTimeoutMs ?? 20000;
  await waitFor(() => Boolean(globalThis.__pal?.hud), timeoutMs, "PalRemakeApp boot");

  return {
    app: globalThis.__pal,
    ...(env || {}),
    sleep,
    waitFor,
  };
}
