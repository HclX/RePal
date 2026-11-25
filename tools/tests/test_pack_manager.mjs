import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import {
  PackManager,
  RpkPackReader,
  crc32Ieee,
  deriveRpkNonce,
  hexToBytes,
  resolvePackUrl,
  sha256SourceHex,
  unwrapPackKey,
} from "../../site/js/host/pack-manager.js";

async function runTests() {
  // 1. Relative & absolute URL resolution against manifest URL
  assert.equal(
    resolvePackUrl("https://cdn.example.com/repal/v2/pack-manifest.json?v=1", "boot.rpk"),
    "https://cdn.example.com/repal/v2/boot.rpk"
  );
  assert.equal(
    resolvePackUrl("https://cdn.example.com/repal/v2/pack-manifest.json", "./packs/ch01.rpk"),
    "https://cdn.example.com/repal/v2/packs/ch01.rpk"
  );
  assert.equal(
    resolvePackUrl("https://cdn.example.com/pack-manifest.json", "https://mirror.example.org/boot.rpk"),
    "https://mirror.example.org/boot.rpk"
  );

  // 2. Cross-language interop with Python-built plain & key-rotated encrypted packs if present
  const plainDir = "/tmp/repal_packs_plain";
  const encDir = "/tmp/repal_packs_enc";
  if (fs.existsSync(path.join(plainDir, "pack-manifest.json"))) {
    const man = JSON.parse(fs.readFileSync(path.join(plainDir, "pack-manifest.json"), "utf8"));
    const bootDesc = man.packs.boot;
    const bootBytes = new Uint8Array(fs.readFileSync(path.join(plainDir, bootDesc.file)));
    assert.equal(await sha256SourceHex(bootBytes), bootDesc.sha256);
    const reader = await RpkPackReader.open("boot", bootBytes);
    assert.ok(reader.has("data/scenes.yaml"), "plain boot pack should have data/scenes.yaml");
    const scenesRaw = await reader.readEntry("data/scenes.yaml");
    assert.ok(scenesRaw.length > 10000, "scenes.yaml decoded size");
  }

  if (fs.existsSync(path.join(encDir, "pack-manifest.json"))) {
    const man = JSON.parse(fs.readFileSync(path.join(encDir, "pack-manifest.json"), "utf8"));
    const bootDesc = man.packs.boot;
    const bootBytes = new Uint8Array(fs.readFileSync(path.join(encDir, bootDesc.file)));
    const newMasterKey = hexToBytes("fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210");
    const oldMasterKey = hexToBytes("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");

    // Old revoked key MUST fail to unwrap
    await assert.rejects(() => unwrapPackKey(oldMasterKey, "boot", bootDesc.wrapped_key));

    // New rotated key MUST unwrap and decrypt both TOC and entries cleanly
    const contentKey = await unwrapPackKey(newMasterKey, "boot", bootDesc.wrapped_key);
    const reader = await RpkPackReader.open("boot", bootBytes, contentKey);
    assert.ok(reader.has("data/scenes.yaml"), "encrypted boot pack should have data/scenes.yaml");
    const scenesRaw = await reader.readEntry("data/scenes.yaml");
    assert.ok(scenesRaw.length > 10000);
  }

  // 3. Forward-compatibility: extended header_len=40, entry_stride=40, advisory flag (1<<16), ext_len=4
  const payload = new TextEncoder().encode("hello forward-compatible rpk1!");
  const keyBytes = new TextEncoder().encode("test/hello.txt");
  const headerLen = 40;
  const entryStride = 40;
  const extLen = 4;
  const dataOffset = headerLen;
  const tocOffset = dataOffset + payload.length;
  const tocLen = entryStride + keyBytes.length + extLen;
  const buf = new Uint8Array(tocOffset + tocLen);
  const dv = new DataView(buf.buffer);
  buf.set([0x52, 0x50, 0x4b, 0x31], 0); // RPK1
  dv.setUint16(4, 1, true); // major=1
  dv.setUint16(6, 5, true); // minor=5 (newer minor!)
  dv.setUint16(8, headerLen, true);
  dv.setUint16(10, entryStride, true);
  dv.setUint32(12, 1 << 16, true); // advisory flag only -> MUST be ignored!
  dv.setBigUint64(16, BigInt(tocOffset), true);
  dv.setUint32(24, tocLen, true);
  dv.setUint32(28, 1, true);
  buf.set(payload, dataOffset);

  const tdv = new DataView(buf.buffer, tocOffset, tocLen);
  tdv.setBigUint64(0, BigInt(dataOffset), true);
  tdv.setUint32(8, payload.length, true);
  tdv.setUint32(12, payload.length, true);
  tdv.setUint32(16, crc32Ieee(payload), true);
  tdv.setUint8(20, 0); // RAW
  tdv.setUint8(21, 0);
  tdv.setUint16(22, keyBytes.length, true);
  tdv.setUint32(24, extLen, true);
  buf.set(keyBytes, tocOffset + entryStride);
  buf.set([0xaa, 0xbb, 0xcc, 0xdd], tocOffset + entryStride + keyBytes.length);

  const fwdReader = await RpkPackReader.open("fwd", buf);
  assert.deepEqual(await fwdReader.readEntry("test/hello.txt"), payload);

  // Unknown must-understand flag (1<<3) MUST fail cleanly
  const badBuf = new Uint8Array(buf);
  new DataView(badBuf.buffer).setUint32(12, 1 << 3, true);
  await assert.rejects(() => RpkPackReader.open("bad", badBuf), /unsupported required flags/);

  // 4. PackManager mirror failover, tamper rejection, and stale-404 callback
  const goodSha = await sha256SourceHex(buf);
  const tampered = new Uint8Array(buf);
  tampered[dataOffset] ^= 0xff;
  const manifest = {
    format: "repal-packs/1",
    schema_version: 1,
    version: "test",
    boot: ["boot"],
    packs: {
      boot: {
        file: "boot.rpk",
        size: buf.length,
        sha256: goodSha,
        urls: ["mirror1/boot.rpk", "mirror2/boot.rpk", "mirror3/boot.rpk"],
      },
      gone: {
        file: "gone.rpk",
        size: 10,
        sha256: goodSha,
        urls: ["gone.rpk"],
      },
    },
  };
  const routes = new Map([
    ["https://host.test/p/pack-manifest.json", { status: 200, body: JSON.stringify(manifest) }],
    ["https://host.test/p/mirror1/boot.rpk", { status: 404, body: new Uint8Array() }],
    ["https://host.test/p/mirror2/boot.rpk", { status: 200, body: tampered }],
    ["https://host.test/p/mirror3/boot.rpk", { status: 200, body: buf }],
    ["https://host.test/p/gone.rpk", { status: 404, body: new Uint8Array() }],
  ]);
  const mockFetch = async (url) => {
    const r = routes.get(url) || { status: 404, body: new Uint8Array() };
    return {
      ok: r.status >= 200 && r.status < 300,
      status: r.status,
      json: async () => JSON.parse(r.body),
      arrayBuffer: async () =>
        r.body instanceof Uint8Array
          ? r.body.buffer.slice(r.body.byteOffset, r.body.byteOffset + r.body.byteLength)
          : new TextEncoder().encode(r.body).buffer,
    };
  };
  let stalePack = null;
  const pm = new PackManager({
    manifestUrl: "https://host.test/p/pack-manifest.json",
    fetchImpl: mockFetch,
    onManifestStale: (pid) => {
      stalePack = pid;
    },
  });
  await pm.init();
  assert.equal(await pm.readText("test/hello.txt"), "hello forward-compatible rpk1!");
  await assert.rejects(() => pm.ensurePack("gone"));
  assert.equal(stalePack, "gone", "404 should invoke onManifestStale");

  // Nonce derivation determinism check
  const n0 = await deriveRpkNonce("boot", 0);
  const n1 = await deriveRpkNonce("boot", 1);
  assert.equal(n0.length, 12);
  assert.equal(n0[8], 0);
  assert.equal(n1[8], 1);

  console.log("test_pack_manager: all assertions passed!");
}

runTests().catch((e) => {
  console.error(e);
  process.exit(1);
});
