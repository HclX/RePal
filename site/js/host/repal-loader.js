/**
 * Zero-dependency `.repal` (`.tar.gz`) & legacy `.json` replay parser + Replay Library
 * (hosted `replays/index.json` + local browser `IndexedDB`) for Web & TV browsers.
 */

const dec = new TextDecoder();
const MAX_REPAL_BYTES = 256 * 1024 * 1024;
const MAX_TAR_ENTRIES = 8192;
const MAX_REPLAY_SEGMENTS = 4096;
const MAX_REPLAY_DURATION_TICKS = 10_000_000;
const MAX_AI_THOUGHTS_PER_SEGMENT = 200;

function normalizeAnnotations(raw) {
  if (!Array.isArray(raw)) return [];
  const items = [];
  for (const a of raw) {
    const t = Number(a?.t ?? a?.tick);
    if (!Number.isFinite(t) || t < 0 || typeof a?.text !== "string") continue;
    items.push({ t: Math.floor(t), text: a.text });
  }
  items.sort((a, b) => a.t - b.t);
  const deduped = [];
  for (const it of items) {
    if (deduped.length > 0 && deduped[deduped.length - 1].t === it.t) {
      deduped[deduped.length - 1].text = it.text;
    } else {
      deduped.push(it);
    }
  }
  if (deduped.length > MAX_AI_THOUGHTS_PER_SEGMENT) {
    return deduped.slice(deduped.length - MAX_AI_THOUGHTS_PER_SEGMENT);
  }
  return deduped;
}

function validateBundleTimeline(bundle) {
  if (bundle.segments.length > MAX_REPLAY_SEGMENTS) {
    throw new Error(`Replay exceeds maximum segment count (${MAX_REPLAY_SEGMENTS})`);
  }
  if ((bundle.total_ticks || 0) > MAX_REPLAY_DURATION_TICKS) {
    throw new Error(`Replay total_ticks exceeds safety limit (${MAX_REPLAY_DURATION_TICKS})`);
  }
  let prevStart = 0;
  for (let i = 0; i < bundle.segments.length; i++) {
    const seg = bundle.segments[i];
    if ((seg.duration_ticks || 0) > MAX_REPLAY_DURATION_TICKS) {
      throw new Error(`Segment ${i} duration_ticks exceeds safety limit`);
    }
    if (i > 0 && (seg.start_tick || 0) < prevStart) {
      throw new Error(`Segment ${i} start_tick is not monotonic`);
    }
    if ((seg.start_tick || 0) + (seg.duration_ticks || 0) > MAX_REPLAY_DURATION_TICKS) {
      throw new Error(`Segment ${i} end tick exceeds safety limit`);
    }
    prevStart = seg.start_tick || 0;
  }
  return bundle;
}

/** Gunzip bytes using browser-native `DecompressionStream('gzip')` with a hard streaming byte cap (#260). */
async function gunzipBytes(u8) {
  if (typeof DecompressionStream === "undefined") {
    throw new Error("Browser does not support DecompressionStream('gzip')");
  }
  const ds = new DecompressionStream("gzip");
  const reader = new Blob([u8]).stream().pipeThrough(ds).getReader();
  const chunks = [];
  let total = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > MAX_REPAL_BYTES) {
        await reader.cancel().catch(() => {});
        throw new Error(`Replay archive exceeds maximum decompressed size (${MAX_REPAL_BYTES} bytes)`);
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const out = new Uint8Array(total);
  let offset = 0;
  for (const c of chunks) {
    out.set(c, offset);
    offset += c.byteLength;
  }
  return out;
}

/** Minimal USTAR tar reader matching `pal-runtime/src/repal_archive.rs`. */
function parseTarEntries(bytes) {
  const files = new Map();
  let off = 0;
  while (off + 512 <= bytes.length) {
    if (files.size >= MAX_TAR_ENTRIES) {
      throw new Error(`Replay archive exceeds maximum tar entry count (${MAX_TAR_ENTRIES})`);
    }
    let allZero = true;
    for (let i = 0; i < 512; i++) {
      if (bytes[off + i] !== 0) {
        allZero = false;
        break;
      }
    }
    if (allZero) break;

    // Validate tar header checksum
    let storedStr = "";
    for (let i = 148; i < 156; i++) {
      const c = bytes[off + i];
      if (c === 0) break;
      storedStr += String.fromCharCode(c);
    }
    const stored = Number.parseInt(storedStr.trim(), 8);
    if (!Number.isFinite(stored)) return null;
    let computed = 0;
    for (let i = 0; i < 512; i++) {
      computed += i >= 148 && i < 156 ? 32 : bytes[off + i];
    }
    if (computed !== stored) return null;

    let nameEnd = 0;
    while (nameEnd < 100 && bytes[off + nameEnd] !== 0) nameEnd++;
    const name = dec.decode(bytes.subarray(off, off + nameEnd)).replace(/^\.\//, "");

    let sizeStr = "";
    for (let i = 124; i < 136; i++) {
      const c = bytes[off + i];
      if (c === 0) break;
      sizeStr += String.fromCharCode(c);
    }
    const size = Number.parseInt(sizeStr.trim(), 8);
    if (!Number.isFinite(size) || size < 0) return null;

    off += 512;
    if (off + size > bytes.length) return null;
    const typeflag = bytes[off - 512 + 156];
    if (typeflag === 48 || typeflag === 0) {
      files.set(name, bytes.slice(off, off + size));
    }
    off += (size + 511) & ~511;
  }
  return files.size > 0 ? files : null;
}

/**
 * Parse a `.repal` (`.tar.gz`), gzipped JSON, or plain `.json` (`v3`/`v2`/`v1`/`CoreDiag_v1`)
 * into a unified segmented bundle:
 * `{ kind, engine_version, total_ticks, note, end_screenshot_png, segments: [...] }`.
 */
export async function parseRepalBytes(rawBytes) {
  const u8 = rawBytes instanceof Uint8Array ? rawBytes : new Uint8Array(rawBytes);
  if (u8.byteLength > MAX_REPAL_BYTES) {
    throw new Error(`Replay file exceeds maximum size (${MAX_REPAL_BYTES} bytes)`);
  }
  const payload = u8.length >= 2 && u8[0] === 0x1f && u8[1] === 0x8b ? await gunzipBytes(u8) : u8;

  const tar = parseTarEntries(payload);
  if (tar && tar.has("manifest.json")) {
    const manifest = JSON.parse(dec.decode(tar.get("manifest.json")));
    const segments = [];
    for (let i = 0; i < (manifest.segments || []).length; i++) {
      const s = manifest.segments[i];
      const anchorBytes = tar.get(s.save_file);
      if (!anchorBytes) continue;
      const anchorVal = JSON.parse(dec.decode(anchorBytes));
      const anchor =
        s.anchor_type === "new_game"
          ? { type: "new_game", seed: anchorVal.seed ?? 1 }
          : { type: "save_json", data: anchorVal };
      const evBytes = tar.get(s.events_file);
      const events = evBytes ? JSON.parse(dec.decode(evBytes)) : [];
      const annBytes = s.annotations_file ? tar.get(s.annotations_file) : null;
      const annotations = normalizeAnnotations(annBytes ? JSON.parse(dec.decode(annBytes)) : []);
      const thumb_png = s.thumb_file ? tar.get(s.thumb_file) || null : null;
      segments.push({
        index: s.index ?? i,
        start_tick: s.start_tick ?? 0,
        duration_ticks: s.duration_ticks ?? (events.length ? (events[events.length - 1].t ?? 0) + 1 : 0),
        scene_id: s.scene_id ?? 0,
        label: s.label || `Chapter ${i + 1}`,
        anchor,
        events,
        annotations,
        thumb_png,
      });
    }
    return validateBundleTimeline({
      kind: manifest.kind || "RePal_CoreReplay_v3",
      engine_version: manifest.engine_version || "unknown",
      total_ticks: manifest.total_ticks ?? segments.reduce((a, s) => a + s.duration_ticks, 0),
      note: manifest.note || null,
      tainted_by_debug: Boolean(manifest.tainted_by_debug),
      debug_mode: Boolean(manifest.debug_mode),
      end_screenshot_png: tar.get("end_screenshot.png") || null,
      segments,
    });
  }

  // Fallback: plain JSON (`v3`, `v2`, `v1`, `CoreDiag_v1`)
  const doc = JSON.parse(dec.decode(payload));
  const rep = doc?.replay || doc;
  if (!String(rep?.kind || "").includes("CoreReplay")) {
    throw new Error("Not a valid RePal replay file");
  }
  if (Array.isArray(rep.segments)) {
    return validateBundleTimeline({
      kind: rep.kind,
      engine_version: rep.engine_version || "unknown",
      total_ticks: rep.total_ticks ?? 0,
      note: rep.note || doc.note || null,
      tainted_by_debug: Boolean(rep.tainted_by_debug || doc.tainted_by_debug),
      debug_mode: Boolean(rep.debug_mode || doc.debug_mode),
      end_screenshot_png: null,
      segments: rep.segments.map((s, i) => ({
        index: s.index ?? i,
        start_tick: s.start_tick ?? 0,
        duration_ticks: s.duration_ticks ?? 0,
        scene_id: s.scene_id ?? 0,
        label: s.label || `Chapter ${i + 1}`,
        anchor: s.anchor,
        events: Array.isArray(s.events) ? s.events : [],
        annotations: normalizeAnnotations(s.annotations),
        thumb_png: null,
      })),
    });
  }
  // Legacy v1/v2 monolithic -> single-segment or checkpoint-split bundle
  const log = Array.isArray(rep.log) ? rep.log : [];
  const total_ticks = rep.total_ticks ?? (log.length ? (log[log.length - 1].t ?? 0) + 1 : 0);
  return validateBundleTimeline({
    kind: rep.kind,
    engine_version: rep.engine_version || "legacy-v1",
    total_ticks,
    note: doc.note || null,
    tainted_by_debug: Boolean(rep.tainted_by_debug || doc.tainted_by_debug),
    debug_mode: Boolean(rep.debug_mode || doc.debug_mode),
    end_screenshot_png: null,
    segments: [
      {
        index: 0,
        start_tick: 0,
        duration_ticks: total_ticks,
        scene_id: 1,
        label: "Start",
        anchor: rep.anchor || { type: "new_game", seed: rep.seed ?? 1 },
        events: log,
        annotations: [],
        thumb_png: null,
      },
    ],
  });
}

/** Decode PNG bytes into `{ width, height, rgba: Uint8Array }` for `pal-ui` chapter thumbnails. */
export async function decodePngToRgba(pngBytes) {
  if (!pngBytes || !pngBytes.length) return null;
  try {
    const blob = new Blob([pngBytes], { type: "image/png" });
    const bmp = await createImageBitmap(blob);
    const canvas = document.createElement("canvas");
    canvas.width = bmp.width;
    canvas.height = bmp.height;
    const ctx = canvas.getContext("2d");
    ctx.drawImage(bmp, 0, 0);
    bmp.close?.();
    const img = ctx.getImageData(0, 0, canvas.width, canvas.height);
    return { width: canvas.width, height: canvas.height, rgba: new Uint8Array(img.data.buffer) };
  } catch {
    return null;
  }
}

// ---- IndexedDB local replay storage (couch/TV personal recordings) ---------------------
const IDB_NAME = "repal_replays_v1";
const IDB_STORE = "replays";

function openReplayDb() {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") return resolve(null);
    const req = indexedDB.open(IDB_NAME, 1);
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains(IDB_STORE)) {
        db.createObjectStore(IDB_STORE, { keyPath: "id" });
      }
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

export async function saveLocalReplay(entry) {
  try {
    const db = await openReplayDb();
    if (!db) return false;
    await new Promise((resolve, reject) => {
      const tx = db.transaction(IDB_STORE, "readwrite");
      tx.objectStore(IDB_STORE).put(entry);
      tx.oncomplete = resolve;
      tx.onerror = () => reject(tx.error);
    });
    return true;
  } catch {
    return false;
  }
}

export async function listLocalReplays() {
  try {
    const db = await openReplayDb();
    if (!db) return [];
    return await new Promise((resolve, reject) => {
      const tx = db.transaction(IDB_STORE, "readonly");
      const req = tx.objectStore(IDB_STORE).getAll();
      req.onsuccess = () => resolve((req.result || []).sort((a, b) => (b.savedAt || 0) - (a.savedAt || 0)));
      req.onerror = () => reject(req.error);
    });
  } catch {
    return [];
  }
}

/**
 * Load combined Replay Library:
 * 1. Built-in / hosted `./replays/index.json` entries
 * 2. Local browser `IndexedDB` recordings
 */
export async function loadReplayLibrary() {
  const hosted = [];
  try {
    const resp = await fetch(`./replays/index.json?v=${Date.now()}`);
    if (resp.ok) {
      const list = await resp.json();
      if (Array.isArray(list)) {
        for (const item of list) {
          hosted.push({
            id: `hosted:${item.file}`,
            source: "hosted",
            title: item.title || item.file,
            subtitle: item.subtitle || "",
            version: item.version || "",
            segments: item.segments || 0,
            ticks: item.ticks || 0,
            url: item.url || `./replays/${item.file}`,
            thumbUrl: item.thumb || null,
          });
        }
      }
    }
  } catch {
    // No hosted index.json — still show local IndexedDB replays
  }

  const locals = (await listLocalReplays()).map((r) => ({
    id: r.id,
    source: "local",
    title: r.title || r.id,
    subtitle: r.subtitle || "Local Recording",
    segments: r.segments || 1,
    ticks: r.ticks || 0,
    bytes: r.bytes,
    thumbUrl: r.thumbDataUrl || null,
  }));

  return [...locals, ...hosted];
}
