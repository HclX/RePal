/**
 * Browser Level-Based Asset Pack Manager (`RPK1` + `pack-manifest.json`, #237).
 *
 * Features:
 * - Host-agnostic `urls[]` mirror fallback (relative URLs resolved against `manifestUrl`).
 * - OPFS (preferred, zero-copy `File.slice()`) with IndexedDB (`Blob.slice()`) and memory fallback.
 * - Streaming chunked SHA-256 verification via `pal.wasm` (`wasm_sha256_*`) so 25 MiB packs never
 *   allocate two full copies in RAM (falls back to `crypto.subtle.digest` if WASM not passed).
 * - Forward/backward-compatible `RPK1` binary header & TOC parser (`header_len`, `entry_stride`,
 *   `flags` must-understand vs advisory bits, per-entry `ext_len` TLV skip, bomb guards).
 * - Optional per-entry AES-256-GCM decryption & `v1:` `wrapped_key` unwrap via WebCrypto
 *   (byte-for-byte compatible with `crates/pal-runtime/src/crypto.rs` & `tools/build_asset_packs.py`).
 * - Deploy-404 manifest re-fetch + optional `onManifestStale` callback.
 */

const RPK_MAGIC_0 = 0x52; // 'R'
const RPK_MAGIC_1 = 0x50; // 'P'
const RPK_MAGIC_2 = 0x4b; // 'K'
const RPK_MAGIC_3 = 0x31; // '1'
const RPK_FORMAT_MAJOR = 1;
const RPK_MIN_HEADER_LEN = 32;
const RPK_MIN_ENTRY_STRIDE = 32;
const RPK_FLAG_TOC_DEFLATED = 1 << 0;
const RPK_FLAG_ENCRYPTED_ENTRIES = 1 << 1;
const RPK_FLAG_ENCRYPTED_TOC = 1 << 2;
const RPK_FLAG_MUST_UNDERSTAND_MASK = 0x0000ffff;
const RPK_KNOWN_MUST_UNDERSTAND =
  RPK_FLAG_TOC_DEFLATED | RPK_FLAG_ENCRYPTED_ENTRIES | RPK_FLAG_ENCRYPTED_TOC;
const RPK_COMP_RAW = 0;
const RPK_COMP_DEFLATE = 1;
const RPK_TOC_ENTRY_INDEX = 0xffffffff;
const MAX_RPK_ENTRIES = 65536;
const MAX_RPK_TOC_RAW = 32 * 1024 * 1024;
const MAX_RPK_ENTRY_RAW = 64 * 1024 * 1024;

const textEnc = new TextEncoder();
const textDec = new TextDecoder();

let _subtleCache = null;
async function getSubtle() {
  if (_subtleCache) return _subtleCache;
  if (globalThis.crypto?.subtle) {
    _subtleCache = globalThis.crypto.subtle;
    return _subtleCache;
  }
  const { webcrypto } = await import("node:crypto");
  _subtleCache = webcrypto.subtle;
  return _subtleCache;
}

export function hexToBytes(hex) {
  const clean = String(hex || "").trim().toLowerCase();
  if (clean.length % 2 !== 0 || !/^[0-9a-f]+$/.test(clean)) {
    throw new Error("invalid hex string");
  }
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

export function bytesToHex(bytes) {
  let s = "";
  for (let i = 0; i < bytes.length; i++) {
    s += bytes[i].toString(16).padStart(2, "0");
  }
  return s;
}

export function base64ToBytes(b64) {
  if (typeof Buffer !== "undefined") {
    return new Uint8Array(Buffer.from(b64, "base64"));
  }
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** Resolve a pack URL (absolute or relative) against the manifest URL's directory. */
export function resolvePackUrl(manifestUrl, entryUrl) {
  const t = String(entryUrl || "").trim();
  if (!t) return "";
  if (t.startsWith("https://") || t.startsWith("http://") || t.startsWith("file://")) {
    return t;
  }
  const noQ = String(manifestUrl || "").split(/[?#]/)[0];
  const slash = noQ.lastIndexOf("/");
  const base = slash >= 0 ? noQ.slice(0, slash) : "";
  const cleanT = t.startsWith("./") ? t.slice(2) : t;
  return base ? `${base.replace(/\/+$/, "")}/${cleanT}` : cleanT;
}

/** Derive the deterministic 96-bit AES-GCM nonce: `(sha256(packId)[0..8], entryIndex_u32_le)`. */
export async function deriveRpkNonce(packId, entryIndex) {
  const subtle = await getSubtle();
  const hashBuf = await subtle.digest("SHA-256", textEnc.encode(packId));
  const hash = new Uint8Array(hashBuf);
  const nonce = new Uint8Array(12);
  nonce.set(hash.subarray(0, 8), 0);
  new DataView(nonce.buffer).setUint32(8, entryIndex >>> 0, true);
  return nonce;
}

/** Build AAD bytes: `utf8(packId) || 0x00 || utf8(key)`. */
function buildAad(packId, key) {
  const a = textEnc.encode(packId);
  const b = textEnc.encode(key);
  const out = new Uint8Array(a.length + 1 + b.length);
  out.set(a, 0);
  out[a.length] = 0;
  out.set(b, a.length + 1);
  return out;
}

/** Unwrap a manifest `wrapped_key` (`v1:<hex(12B nonce || 32B ct || 16B tag)>`) using 32-byte master key. */
export async function unwrapPackKey(masterKeyBytes, packId, wrappedKey) {
  const rest = String(wrappedKey || "");
  if (!rest.startsWith("v1:")) {
    throw new Error(`unsupported wrapped_key format for '${packId}'`);
  }
  const raw = hexToBytes(rest.slice(3));
  if (raw.length !== 12 + 32 + 16) {
    throw new Error(`invalid wrapped_key length (${raw.length}) for '${packId}'`);
  }
  const nonce = raw.subarray(0, 12);
  const ctAndTag = raw.subarray(12);
  const subtle = await getSubtle();
  const key = await subtle.importKey(
    "raw",
    masterKeyBytes,
    { name: "AES-GCM" },
    false,
    ["decrypt"]
  );
  const aad = textEnc.encode(`repal-pack-key/v1\0${packId}`);
  const pt = await subtle.decrypt(
    { name: "AES-GCM", iv: nonce, additionalData: aad, tagLength: 128 },
    key,
    ctAndTag
  );
  return new Uint8Array(pt);
}

/** Raw DEFLATE inflate using browser `DecompressionStream("deflate-raw")` (with Node `zlib` fallback). */
export async function inflateRaw(bytes) {
  if (typeof DecompressionStream !== "undefined") {
    try {
      const ds = new DecompressionStream("deflate-raw");
      const writer = ds.writable.getWriter();
      writer.write(bytes);
      writer.close();
      const out = await new Response(ds.readable).arrayBuffer();
      return new Uint8Array(out);
    } catch {
      // Node 18 lacks "deflate-raw" in DecompressionStream; fall through to node:zlib.
    }
  }
  const zlib = await import("node:zlib");
  return new Uint8Array(zlib.inflateRawSync(bytes));
}

/** Standard IEEE 802.3 CRC32 (matches `crates/pal-runtime/src/crypto.rs`). */
const CRC32_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let i = 0; i < 256; i++) {
    let c = i;
    for (let k = 0; k < 8; k++) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    t[i] = c >>> 0;
  }
  return t;
})();

export function crc32Ieee(bytes) {
  let c = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) {
    c = CRC32_TABLE[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  }
  return (c ^ 0xffffffff) >>> 0;
}

/** Read a slice `[offset, offset + len)` from a `Blob`/`File` or `Uint8Array`. */
async function readSourceSlice(source, offset, len) {
  if (source instanceof Uint8Array) {
    return source.subarray(offset, offset + len);
  }
  const buf = await source.slice(offset, offset + len).arrayBuffer();
  return new Uint8Array(buf);
}

/** Streaming SHA-256 hex digest of a `Blob`/`File` or `Uint8Array` (uses `pal.wasm` if provided). */
export async function sha256SourceHex(source, wasmExports = null, wasmMemory = null) {
  const total = source instanceof Uint8Array ? source.byteLength : source.size;
  if (
    wasmExports &&
    wasmMemory &&
    typeof wasmExports.wasm_sha256_init === "function" &&
    typeof wasmExports.wasm_sha256_update === "function" &&
    typeof wasmExports.wasm_sha256_final === "function"
  ) {
    wasmExports.wasm_sha256_init();
    const chunkSize = 256 * 1024;
    for (let off = 0; off < total; off += chunkSize) {
      const end = Math.min(total, off + chunkSize);
      const chunk = await readSourceSlice(source, off, end - off);
      const ptr = wasmExports.wasm_scratch_ptr(chunk.length);
      new Uint8Array(wasmMemory.buffer, ptr, chunk.length).set(chunk);
      wasmExports.wasm_sha256_update(chunk.length);
    }
    const len = wasmExports.wasm_sha256_final();
    const ptr = wasmExports.core_buf_ptr();
    return textDec.decode(new Uint8Array(wasmMemory.buffer, ptr, len));
  }
  const full =
    source instanceof Uint8Array ? source : new Uint8Array(await source.arrayBuffer());
  const subtle = await getSubtle();
  const digest = await subtle.digest("SHA-256", full);
  return bytesToHex(new Uint8Array(digest));
}

/**
 * Random-access `RPK1` reader backed by a lazy `Blob`/`File` (zero full-pack RAM copy!)
 * or `Uint8Array` (in-memory tests).
 */
export class RpkPackReader {
  constructor(packId, source, header, entries, cryptoKey) {
    this.packId = packId;
    this.source = source;
    this.header = header;
    this.entries = entries; // Map<string, Entry>
    this.cryptoKey = cryptoKey;
  }

  static async open(packId, source, contentKeyBytes = null) {
    const fileLen = source instanceof Uint8Array ? source.byteLength : source.size;
    if (fileLen < RPK_MIN_HEADER_LEN) {
      throw new Error(`RPK1 '${packId}' truncated (${fileLen} bytes)`);
    }
    const hdrBytes = await readSourceSlice(source, 0, RPK_MIN_HEADER_LEN);
    if (
      hdrBytes[0] !== RPK_MAGIC_0 ||
      hdrBytes[1] !== RPK_MAGIC_1 ||
      hdrBytes[2] !== RPK_MAGIC_2 ||
      hdrBytes[3] !== RPK_MAGIC_3
    ) {
      throw new Error(`RPK1 '${packId}' invalid magic`);
    }
    const dv = new DataView(hdrBytes.buffer, hdrBytes.byteOffset, hdrBytes.byteLength);
    const formatMajor = dv.getUint16(4, true);
    const formatMinor = dv.getUint16(6, true);
    const headerLen = dv.getUint16(8, true);
    const entryStride = dv.getUint16(10, true);
    const flags = dv.getUint32(12, true);
    const tocOffset = Number(dv.getBigUint64(16, true));
    const tocStoredLen = dv.getUint32(24, true);
    const entryCount = dv.getUint32(28, true);

    if (formatMajor !== RPK_FORMAT_MAJOR) {
      throw new Error(`RPK1 '${packId}' unsupported major version ${formatMajor}`);
    }
    if (headerLen < RPK_MIN_HEADER_LEN || headerLen > fileLen) {
      throw new Error(`RPK1 '${packId}' invalid header_len ${headerLen}`);
    }
    if (entryStride < RPK_MIN_ENTRY_STRIDE) {
      throw new Error(`RPK1 '${packId}' invalid entry_stride ${entryStride}`);
    }
    const unknownMust = flags & RPK_FLAG_MUST_UNDERSTAND_MASK & ~RPK_KNOWN_MUST_UNDERSTAND;
    if (unknownMust !== 0) {
      throw new Error(`RPK1 '${packId}' unsupported required flags 0x${unknownMust.toString(16)}`);
    }
    if (entryCount > MAX_RPK_ENTRIES) {
      throw new Error(`RPK1 '${packId}' entry_count ${entryCount} exceeds limit`);
    }
    if (tocStoredLen > MAX_RPK_TOC_RAW || tocOffset + tocStoredLen > fileLen) {
      throw new Error(`RPK1 '${packId}' TOC out of bounds`);
    }

    const tocDeflated = (flags & RPK_FLAG_TOC_DEFLATED) !== 0;
    const encryptedEntries = (flags & RPK_FLAG_ENCRYPTED_ENTRIES) !== 0;
    const encryptedToc = (flags & RPK_FLAG_ENCRYPTED_TOC) !== 0;
    let cryptoKey = null;
    if (encryptedEntries || encryptedToc) {
      if (!contentKeyBytes || contentKeyBytes.length !== 32) {
        throw new Error(`RPK1 '${packId}' is encrypted and requires a 32-byte content key`);
      }
      const subtle = await getSubtle();
      cryptoKey = await subtle.importKey(
        "raw",
        contentKeyBytes,
        { name: "AES-GCM" },
        false,
        ["decrypt"]
      );
    }

    let tocBytes = await readSourceSlice(source, tocOffset, tocStoredLen);
    if (encryptedToc) {
      const nonce = await deriveRpkNonce(packId, RPK_TOC_ENTRY_INDEX);
      const aad = buildAad(packId, "__TOC__");
      const subtle = await getSubtle();
      const pt = await subtle.decrypt(
        { name: "AES-GCM", iv: nonce, additionalData: aad, tagLength: 128 },
        cryptoKey,
        tocBytes
      );
      tocBytes = new Uint8Array(pt);
    }
    if (tocDeflated) {
      tocBytes = await inflateRaw(tocBytes);
    }

    const entries = new Map();
    const tdv = new DataView(tocBytes.buffer, tocBytes.byteOffset, tocBytes.byteLength);
    let pos = 0;
    for (let idx = 0; idx < entryCount; idx++) {
      if (pos + entryStride > tocBytes.length) {
        throw new Error(`RPK1 '${packId}' truncated TOC at entry ${idx}`);
      }
      const dataOffset = Number(tdv.getBigUint64(pos, true));
      const storedLen = tdv.getUint32(pos + 8, true);
      const rawLen = tdv.getUint32(pos + 12, true);
      const crc32 = tdv.getUint32(pos + 16, true);
      const compression = tdv.getUint8(pos + 20);
      const entryFlags = tdv.getUint8(pos + 21);
      const keyLen = tdv.getUint16(pos + 22, true);
      const extLen = tdv.getUint16(pos + 24, true);
      pos += entryStride;

      if (pos + keyLen + extLen > tocBytes.length) {
        throw new Error(`RPK1 '${packId}' truncated key/ext at entry ${idx}`);
      }
      const key = textDec.decode(tocBytes.subarray(pos, pos + keyLen));
      pos += keyLen + extLen;

      if (rawLen > MAX_RPK_ENTRY_RAW || dataOffset + storedLen > fileLen) {
        throw new Error(`RPK1 '${packId}' invalid bounds for '${key}'`);
      }
      entries.set(key, {
        index: idx,
        key,
        dataOffset,
        storedLen,
        rawLen,
        crc32,
        compression,
        entryFlags,
      });
    }

    return new RpkPackReader(
      packId,
      source,
      { formatMajor, formatMinor, headerLen, entryStride, flags, entryCount },
      entries,
      cryptoKey
    );
  }

  has(key) {
    return this.entries.has(key);
  }

  keys() {
    return Array.from(this.entries.keys());
  }

  async readEntry(key) {
    const e = this.entries.get(key);
    if (!e) return null;
    let payload = await readSourceSlice(this.source, e.dataOffset, e.storedLen);
    if ((this.header.flags & RPK_FLAG_ENCRYPTED_ENTRIES) !== 0) {
      const nonce = await deriveRpkNonce(this.packId, e.index);
      const aad = buildAad(this.packId, key);
      const subtle = await getSubtle();
      const pt = await subtle.decrypt(
        { name: "AES-GCM", iv: nonce, additionalData: aad, tagLength: 128 },
        this.cryptoKey,
        payload
      );
      payload = new Uint8Array(pt);
    }
    let raw;
    if (e.compression === RPK_COMP_RAW) {
      raw = payload;
    } else if (e.compression === RPK_COMP_DEFLATE) {
      raw = await inflateRaw(payload);
    } else {
      throw new Error(`RPK1 '${this.packId}' unsupported compression ${e.compression}`);
    }
    if (raw.length !== e.rawLen) {
      throw new Error(`RPK1 '${this.packId}' length mismatch for '${key}'`);
    }
    if (crc32Ieee(raw) !== e.crc32) {
      throw new Error(`RPK1 '${this.packId}' CRC32 mismatch for '${key}'`);
    }
    return raw;
  }
}

/**
 * Storage abstraction: OPFS (`navigator.storage.getDirectory`) preferred,
 * falling back to IndexedDB (`repal-packs`), or in-memory `Map` (for Node/private mode).
 */
class PackStorage {
  constructor() {
    this._mode = "memory";
    this._opfsDir = null;
    this._idb = null;
    this._mem = new Map();
    this._initPromise = null;
  }

  async init() {
    if (this._initPromise) return this._initPromise;
    this._initPromise = (async () => {
      try {
        if (typeof navigator !== "undefined" && navigator.storage?.persist) {
          navigator.storage.persist().catch(() => {});
        }
        if (typeof navigator !== "undefined" && navigator.storage?.getDirectory) {
          const root = await navigator.storage.getDirectory();
          this._opfsDir = await root.getDirectoryHandle("repal-packs", { create: true });
          this._mode = "opfs";
          return;
        }
      } catch {}
      try {
        if (typeof indexedDB !== "undefined") {
          this._idb = await new Promise((resolve, reject) => {
            const req = indexedDB.open("repal-packs-v1", 1);
            req.onupgradeneeded = () => {
              const db = req.result;
              if (!db.objectStoreNames.contains("packs")) db.createObjectStore("packs");
              if (!db.objectStoreNames.contains("meta")) db.createObjectStore("meta");
            };
            req.onsuccess = () => resolve(req.result);
            req.onerror = () => reject(req.error);
          });
          this._mode = "idb";
          return;
        }
      } catch {}
      this._mode = "memory";
    })();
    return this._initPromise;
  }

  async getPackBlob(filename) {
    await this.init();
    if (this._mode === "opfs" && this._opfsDir) {
      try {
        const fh = await this._opfsDir.getFileHandle(filename);
        return await fh.getFile();
      } catch {
        return null;
      }
    }
    if (this._mode === "idb" && this._idb) {
      return await new Promise((resolve) => {
        const tx = this._idb.transaction("packs", "readonly");
        const req = tx.objectStore("packs").get(filename);
        req.onsuccess = () => resolve(req.result || null);
        req.onerror = () => resolve(null);
      });
    }
    return this._mem.get(filename) || null;
  }

  async putPackBytes(filename, bytesOrBlob) {
    await this.init();
    if (this._mode === "opfs" && this._opfsDir) {
      const fh = await this._opfsDir.getFileHandle(filename, { create: true });
      const writable = await fh.createWritable();
      await writable.write(bytesOrBlob);
      await writable.close();
      return await fh.getFile();
    }
    const blob =
      typeof Blob !== "undefined" && !(bytesOrBlob instanceof Blob)
        ? new Blob([bytesOrBlob])
        : bytesOrBlob;
    if (this._mode === "idb" && this._idb) {
      await new Promise((resolve, reject) => {
        const tx = this._idb.transaction("packs", "readwrite");
        tx.objectStore("packs").put(blob, filename);
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error);
      });
      return blob;
    }
    this._mem.set(filename, bytesOrBlob);
    return bytesOrBlob;
  }

  async deletePack(filename) {
    await this.init();
    if (this._mode === "opfs" && this._opfsDir) {
      try {
        await this._opfsDir.removeEntry(filename);
      } catch {}
      return;
    }
    if (this._mode === "idb" && this._idb) {
      await new Promise((resolve) => {
        const tx = this._idb.transaction("packs", "readwrite");
        tx.objectStore("packs").delete(filename);
        tx.oncomplete = () => resolve();
        tx.onerror = () => resolve();
      });
      return;
    }
    this._mem.delete(filename);
  }

  async listFiles() {
    await this.init();
    if (this._mode === "opfs" && this._opfsDir) {
      const names = [];
      for await (const [name] of this._opfsDir.entries()) names.push(name);
      return names;
    }
    if (this._mode === "idb" && this._idb) {
      return await new Promise((resolve) => {
        const tx = this._idb.transaction("packs", "readonly");
        const req = tx.objectStore("packs").getAllKeys();
        req.onsuccess = () => resolve(req.result || []);
        req.onerror = () => resolve([]);
      });
    }
    return Array.from(this._mem.keys());
  }
}

/**
 * Browser PackManager coordinating `pack-manifest.json`, OPFS/IDB storage, SHA-256 verification,
 * lazy `RpkPackReader` mounting, and optional AES-256-GCM decryption.
 */
export class PackManager {
  constructor({
    manifestUrl,
    wasmExports = null,
    wasmMemory = null,
    masterKeyHex = null,
    fetchImpl = null,
    onManifestStale = null,
    onProgress = null,
  } = {}) {
    this.manifestUrl = manifestUrl;
    this.wasmExports = wasmExports;
    this.wasmMemory = wasmMemory;
    this.masterKeyBytes = masterKeyHex ? hexToBytes(masterKeyHex) : null;
    this._fetch = fetchImpl || ((...a) => globalThis.fetch(...a));
    this.onManifestStale = onManifestStale;
    this.onProgress = onProgress;
    this.storage = new PackStorage();
    this.manifest = null;
    this.mounted = new Map(); // packId -> RpkPackReader
    this.mountedKeys = new Map(); // assetKey -> packId
    this.keyIndex = new Map(); // assetKey -> packId (from boot's pack_key_index.json)
    this._inFlight = new Map(); // packId -> Promise<RpkPackReader>
  }

  setMasterKeyHex(hex) {
    this.masterKeyBytes = hex ? hexToBytes(hex) : null;
  }

  /** Fetch `pack-manifest.json`, GC unreferenced cached packs, and mount `boot` packs. */
  async init() {
    const resp = await this._fetch(this.manifestUrl, { cache: "no-cache" });
    if (!resp.ok) throw new Error(`Failed to fetch pack-manifest.json (${resp.status})`);
    const manifest = await resp.json();
    if (manifest?.format !== "repal-packs/1" || !manifest.packs) {
      throw new Error(`Unsupported pack-manifest format: ${manifest?.format}`);
    }
    this.manifest = manifest;
    await this.gcUnreferenced();
    for (const pid of manifest.boot || []) {
      await this.ensurePack(pid);
    }
    if (manifest.key_index_entry) {
      const idxBytes = await this.readBytes(manifest.key_index_entry);
      if (idxBytes) {
        try {
          const obj = JSON.parse(textDec.decode(idxBytes));
          for (const [k, v] of Object.entries(obj)) this.keyIndex.set(k, v);
        } catch {}
      }
    }
    return this;
  }

  /** Remove cached `.rpk` files not referenced by the current manifest. */
  async gcUnreferenced() {
    if (!this.manifest) return 0;
    const referenced = new Set(Object.values(this.manifest.packs).map((p) => p.file));
    const existing = await this.storage.listFiles();
    let removed = 0;
    for (const name of existing) {
      if (name.endsWith(".rpk") && !referenced.has(name)) {
        await this.storage.deletePack(name);
        removed++;
      }
    }
    return removed;
  }

  /** Pack IDs from the manifest not yet cached in storage (size-checked), excluding dev packs. */
  async missingPackIds() {
    const out = [];
    for (const [pid, desc] of Object.entries(this.manifest?.packs || {})) {
      if (pid.startsWith("dev-")) continue;
      if (this.mounted.has(pid)) continue;
      const blob = await this.storage.getPackBlob(desc.file);
      const sz = blob ? (blob instanceof Uint8Array ? blob.byteLength : blob.size) : -1;
      if (!blob || (desc.size && sz !== desc.size)) out.push(pid);
    }
    return out.sort();
  }

  async ensurePack(packId) {
    if (this.mounted.has(packId)) return this.mounted.get(packId);
    if (this._inFlight.has(packId)) return this._inFlight.get(packId);
    const p = this._ensurePackInner(packId).finally(() => this._inFlight.delete(packId));
    this._inFlight.set(packId, p);
    return p;
  }

  async _ensurePackInner(packId) {
    const desc = this.manifest?.packs?.[packId];
    if (!desc) throw new Error(`Unknown pack '${packId}'`);

    let source = await this.storage.getPackBlob(desc.file);
    let fromCache = Boolean(source);
    if (source && desc.size) {
      const sz = source instanceof Uint8Array ? source.byteLength : source.size;
      if (sz !== desc.size) {
        await this.storage.deletePack(desc.file);
        source = null;
        fromCache = false;
      }
    }

    const downloadFresh = async () => {
      const rawUrls = Array.isArray(desc.urls) && desc.urls.length ? desc.urls : [desc.file];
      const urls = rawUrls.map((u) => resolvePackUrl(this.manifestUrl, u)).filter(Boolean);
      let saw404 = false;
      const errs = [];
      for (const url of urls) {
        try {
          const resp = await this._fetch(url);
          if (resp.status === 404) {
            saw404 = true;
            errs.push(`${url}: 404`);
            continue;
          }
          if (!resp.ok) {
            errs.push(`${url}: HTTP ${resp.status}`);
            continue;
          }
          const bytes = new Uint8Array(await resp.arrayBuffer());
          if (desc.sha256) {
            const actual = await sha256SourceHex(bytes, this.wasmExports, this.wasmMemory);
            if (actual.toLowerCase() !== desc.sha256.toLowerCase()) {
              errs.push(`${url}: SHA-256 mismatch`);
              continue;
            }
          }
          try {
            return await this.storage.putPackBytes(desc.file, bytes);
          } catch {
            // QuotaExceededError fallback: keep in memory for this session
            return bytes;
          }
        } catch (e) {
          errs.push(`${url}: ${e?.message || e}`);
        }
      }
      if (saw404 && typeof this.onManifestStale === "function") {
        this.onManifestStale(packId);
      }
      throw new Error(`Failed to fetch pack '${packId}': ${errs.join("; ")}`);
    };

    if (!source) {
      source = await downloadFresh();
    }

    let contentKey = null;
    if (desc.encrypted) {
      if (!this.masterKeyBytes) {
        throw new Error(`Pack '${packId}' is encrypted and no master key is set`);
      }
      contentKey = await unwrapPackKey(this.masterKeyBytes, packId, desc.wrapped_key);
    }

    let reader;
    try {
      reader = await RpkPackReader.open(packId, source, contentKey);
    } catch (openErr) {
      if (fromCache) {
        await this.storage.deletePack(desc.file);
        source = await downloadFresh();
        reader = await RpkPackReader.open(packId, source, contentKey);
      } else {
        throw openErr;
      }
    }
    this.mounted.set(packId, reader);
    for (const k of reader.keys()) {
      this.mountedKeys.set(k, packId);
    }
    return reader;
  }

  async ensureScenePacks(sceneId, mapId = null) {
    if (!this.manifest) return;
    const ids = new Set([
      ...(this.manifest.boot || []),
      ...(this.manifest.shared || []),
      ...(this.manifest.scenes?.[String(sceneId)] || []),
    ]);
    if (mapId != null) {
      const mp = this.manifest.maps?.[String(mapId)];
      if (mp) ids.add(mp);
    }
    await Promise.all(Array.from(ids).map((pid) => this.ensurePack(pid)));
    this.prefetchAdjacent(sceneId);
  }

  /** Low-priority background prefetch of adjacent scenes (`manifest.prefetch[sceneId]`). */
  prefetchAdjacent(sceneId) {
    const adj = this.manifest?.prefetch?.[String(sceneId)];
    if (!Array.isArray(adj) || !adj.length) return;
    this._enqueueBackgroundPrefetch(adj);
  }

  /** Quietly warm `shared` and starting scene packs while the player is on the Title Screen. */
  prefetchBootWarmup() {
    if (!this.manifest) return;
    const ids = [
      ...(this.manifest.shared || []),
      ...(this.manifest.scenes?.["1"] || []),
    ];
    this._enqueueBackgroundPrefetch(ids);
  }

  _enqueueBackgroundPrefetch(packIds) {
    if (!this._prefetchQueue) this._prefetchQueue = [];
    for (const pid of packIds) {
      if (!pid || this.mounted.has(pid) || this._inFlight.has(pid)) continue;
      if (!this._prefetchQueue.includes(pid)) this._prefetchQueue.push(pid);
    }
    if (this._prefetchRunning) return;
    this._prefetchRunning = true;
    (async () => {
      try {
        while (this._prefetchQueue.length > 0) {
          const pid = this._prefetchQueue.shift();
          if (!pid || this.mounted.has(pid)) continue;
          try {
            await this.ensurePack(pid);
          } catch {
            // Non-fatal background prefetch
          }
        }
      } finally {
        this._prefetchRunning = false;
      }
    })();
  }

  async ensureRngPack(rngId) {
    const pid = this.manifest?.rng?.[String(rngId)];
    if (pid) await this.ensurePack(pid);
  }

  hasKey(key) {
    return this.mountedKeys.has(key) || this.keyIndex.has(key);
  }

  async readBytes(key) {
    let pid = this.mountedKeys.get(key);
    if (!pid) {
      pid = this.keyIndex.get(key);
      if (pid) {
        try {
          await this.ensurePack(pid);
        } catch {
          return null;
        }
      }
    }
    if (!pid) return null;
    const reader = this.mounted.get(pid);
    if (!reader) return null;
    return await reader.readEntry(key);
  }

  async readText(key) {
    const bytes = await this.readBytes(key);
    return bytes ? textDec.decode(bytes) : null;
  }
}
