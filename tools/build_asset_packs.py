#!/usr/bin/env python3
"""
Level-Based Disjoint Asset Pack Builder (`RPK1` + `pack-manifest.json`) for RePal (#237).

Builds zero-extraction, random-access `.rpk` packs (both 1x Retro and 4x HD in the same pack
to preserve instant `[F2]` switching) with:
  - `boot` pack (data, scripts, fonts, UI, BGM, orig_saves/10.rpg, pack_key_index.json)
  - `shared` pack (shared-by-construction runtime/save-dependent assets: heroes, fire VFX, SFX, multi-region)
  - Region packs (~12 story regions; bounded < 64 KiB duplication across 2 regions)
  - Per-map Track-B HD packs (`map-hd-XXX`, median ~1.4 MiB, independent remaster invalidation)
  - Per-cutscene RNG packs (`rng-XX`)
  - Optional per-entry AES-256-GCM encryption + wrapped content keys (`rotate-key` without re-encrypting!)

Usage:
  python3 tools/build_asset_packs.py build --assets site/assets --out /tmp/packs
  python3 tools/build_asset_packs.py build --assets site/assets --out /tmp/packs_enc --encrypt --master-key-hex <64-hex> --opaque-names
  python3 tools/build_asset_packs.py rotate-key --manifest /tmp/packs_enc/pack-manifest.json --old-key-hex <64-hex> --new-key-hex <64-hex>
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import os
import re
import secrets
import struct
import sys
import zlib
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Set, Tuple

import yaml

# ---- RPK1 Wire Constants (must match `crates/pal-runtime/src/rpk.rs`) ----
RPK_MAGIC = b"RPK1"
RPK_FORMAT_MAJOR = 1
RPK_FORMAT_MINOR = 0
RPK_HEADER_LEN = 32
RPK_ENTRY_STRIDE = 32

RPK_FLAG_TOC_DEFLATED = 1 << 0
RPK_FLAG_ENCRYPTED = 1 << 1
RPK_FLAG_TOC_ENCRYPTED = 1 << 2

RPK_COMP_RAW = 0
RPK_COMP_DEFLATE = 1

PACK_MANIFEST_FORMAT = "repal-packs/1"
PACK_SCHEMA_VERSION = 1

SMALL_DUP_THRESHOLD_BYTES = 64 * 1024  # bounded duplication for < 64 KiB cross-region assets (#237)

# Story region buckets (scene_id inclusive ranges) — keeps in-town transitions 0-fetch!
REGION_RANGES: List[Tuple[str, int, int]] = [
    ("ch01a-yuhang", 1, 13),
    ("ch01b-fairy", 14, 20),
    ("ch02-suzhou", 21, 45),
    ("ch03-baihe", 46, 75),
    ("ch04-yangzhou", 76, 105),
    ("ch05-jingcheng", 106, 145),
    ("ch06-shushan", 146, 185),
    ("ch07-dali", 186, 235),
    ("ch08-endgame", 236, 999),
]


def region_for_scene(scene_id: int) -> str:
    for slug, lo, hi in REGION_RANGES:
        if lo <= scene_id <= hi:
            return slug
    return "ch08-endgame"


def should_compress_key(key: str) -> bool:
    low = key.lower()
    return low.endswith((".yaml", ".yml", ".json", ".bin", ".txt", ".mid", ".asc", ".rpg"))


def raw_deflate(data: bytes) -> bytes:
    comp = zlib.compressobj(level=6, method=zlib.DEFLATED, wbits=-15)
    return comp.compress(data) + comp.flush()


def derive_rpk_nonce(pack_id: str, entry_index: int) -> bytes:
    h = hashlib.sha256(pack_id.encode("utf-8")).digest()
    return h[:8] + struct.pack("<I", entry_index)


def aes256_gcm_encrypt(key: bytes, nonce: bytes, aad: bytes, plaintext: bytes) -> bytes:
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM

    return AESGCM(key).encrypt(nonce, plaintext, aad)


def aes256_gcm_decrypt(key: bytes, nonce: bytes, aad: bytes, ct_tag: bytes) -> bytes:
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM

    return AESGCM(key).decrypt(nonce, ct_tag, aad)


def wrap_pack_key(master_key: bytes, pack_id: str, content_key: bytes) -> str:
    nonce = secrets.token_bytes(12)
    aad = f"repal-pack-key/v1\0{pack_id}".encode("utf-8")
    ct_tag = aes256_gcm_encrypt(master_key, nonce, aad, content_key)
    return "v1:" + (nonce + ct_tag).hex()


def unwrap_pack_key(master_key: bytes, pack_id: str, wrapped: str) -> bytes:
    if not wrapped.startswith("v1:"):
        raise ValueError(f"unsupported wrapped_key version: {wrapped[:16]}")
    blob = bytes.fromhex(wrapped[3:])
    if len(blob) != 12 + 32 + 16:
        raise ValueError(f"invalid wrapped_key length: {len(blob)}")
    nonce = blob[:12]
    aad = f"repal-pack-key/v1\0{pack_id}".encode("utf-8")
    return aes256_gcm_decrypt(master_key, nonce, aad, blob[12:])


def write_rpk_bytes(
    pack_id: str,
    entries: Dict[str, bytes],
    *,
    compress_toc: bool = True,
    content_key: Optional[bytes] = None,
    encrypt_toc: bool = False,
) -> Tuple[bytes, int]:
    """Encode an RPK1 pack matching `crates/pal-runtime/src/rpk.rs` byte-for-byte."""
    sorted_keys = sorted(entries.keys())
    encrypt_entries = content_key is not None
    do_encrypt_toc = encrypt_entries and encrypt_toc

    prepared: List[Tuple[str, bytes, int, int, int]] = []
    for idx, key in enumerate(sorted_keys):
        raw = entries[key]
        crc = zlib.crc32(raw) & 0xFFFF_FFFF
        raw_len = len(raw)
        if should_compress_key(key) and raw_len >= 32:
            deflated = raw_deflate(raw)
            if len(deflated) < raw_len:
                body, comp = deflated, RPK_COMP_DEFLATE
            else:
                body, comp = raw, RPK_COMP_RAW
        else:
            body, comp = raw, RPK_COMP_RAW

        if content_key is not None:
            nonce = derive_rpk_nonce(pack_id, idx)
            aad = f"{pack_id}\0{key}".encode("utf-8")
            body = aes256_gcm_encrypt(content_key, nonce, aad, body)
        prepared.append((key, body, raw_len, crc, comp))

    def encode_toc(base_offset: int) -> bytes:
        chunks = []
        cur = base_offset
        for key, stored, raw_len, crc, comp in prepared:
            rem = cur % 8
            if rem:
                cur += 8 - rem
            kb = key.encode("utf-8")
            chunks.append(
                struct.pack(
                    "<QIIIBBHH6s",
                    cur,
                    len(stored),
                    raw_len,
                    crc,
                    comp,
                    0,
                    len(kb),
                    0,
                    b"\x00" * 6,
                )
            )
            chunks.append(kb)
            cur += len(stored)
        return b"".join(chunks)

    def finalize_toc(raw_toc: bytes) -> Tuple[bytes, int]:
        fl = 0
        if encrypt_entries:
            fl |= RPK_FLAG_ENCRYPTED
        buf = raw_toc
        if compress_toc:
            deflated = raw_deflate(raw_toc)
            if len(deflated) < len(raw_toc):
                buf = deflated
                fl |= RPK_FLAG_TOC_DEFLATED
        if do_encrypt_toc:
            assert content_key is not None
            nonce = derive_rpk_nonce(pack_id, 0xFFFF_FFFF)
            aad = f"{pack_id}\0__TOC__".encode("utf-8")
            buf = aes256_gcm_encrypt(content_key, nonce, aad, buf)
            fl |= RPK_FLAG_TOC_ENCRYPTED
        return buf, fl

    data_offset = (RPK_HEADER_LEN + len(encode_toc(RPK_HEADER_LEN)) + 64 + 7) & ~7
    while True:
        toc_stored, flags = finalize_toc(encode_toc(data_offset))
        needed = (RPK_HEADER_LEN + len(toc_stored) + 7) & ~7
        if needed <= data_offset:
            break
        data_offset = needed + 32

    toc_stored, flags = finalize_toc(encode_toc(data_offset))
    header = struct.pack(
        "<4sHHHHIIQII",
        RPK_MAGIC,
        RPK_FORMAT_MAJOR,
        RPK_FORMAT_MINOR,
        RPK_HEADER_LEN,
        RPK_ENTRY_STRIDE,
        flags & 0xFFFF_FFFF,
        0,  # placeholder — packed below with exact layout
        0,
        0,
        0,
    )
    # Exact 32-byte header layout:
    # 4s H H H H I Q I I = 4+2+2+2+2+4+8+4+4 = 32 bytes
    header = struct.pack(
        "<4sHHHHIQII",
        RPK_MAGIC,
        RPK_FORMAT_MAJOR,
        RPK_FORMAT_MINOR,
        RPK_HEADER_LEN,
        RPK_ENTRY_STRIDE,
        flags,
        RPK_HEADER_LEN,
        len(toc_stored),
        len(prepared),
    )
    assert len(header) == RPK_HEADER_LEN

    out = bytearray(header)
    out.extend(toc_stored)
    if len(out) < data_offset:
        out.extend(b"\x00" * (data_offset - len(out)))
    for _key, stored, _raw_len, _crc, _comp in prepared:
        rem = len(out) % 8
        if rem:
            out.extend(b"\x00" * (8 - rem))
        out.extend(stored)
    return bytes(out), len(prepared)


def scan_all_assets(assets_root: Path) -> Dict[str, Path]:
    files: Dict[str, Path] = {}
    for dp, _, fns in os.walk(assets_root):
        for fn in fns:
            p = Path(dp) / fn
            rel = p.relative_to(assets_root).as_posix()
            files[rel] = p
    return files


def analyze_dependencies(
    assets_root: Path, all_files: Dict[str, Path], include_dev_saves: bool
) -> Tuple[Dict[str, Set[str]], Dict[str, List[str]], Dict[str, str], Dict[str, str], Dict[str, List[str]], Dict[str, int]]:
    """
    Build pack membership + scene/map/rng/prefetch tables covering 100% of public assets.
    Returns: (pack_keys, scene_packs, map_hd_packs, rng_packs, prefetch_map, stats)
    """
    scenes_doc = yaml.safe_load((assets_root / "data/scenes.yaml").read_text("utf-8"))
    events_doc = yaml.safe_load((assets_root / "data/events.yaml").read_text("utf-8"))
    roles_doc = yaml.safe_load((assets_root / "data/roles.yaml").read_text("utf-8"))
    enemies_doc = yaml.safe_load((assets_root / "data/enemies.yaml").read_text("utf-8"))

    scenes_list = scenes_doc.get("scenes", [])
    events_by_id = {int(e["id"]): e for e in events_doc.get("events", []) if "id" in e}

    # Index asset files by structured prefix for fast lookup
    mgo_files: Dict[int, List[str]] = collections.defaultdict(list)
    gop_files: Dict[int, List[str]] = collections.defaultdict(list)
    maps_hd_files: Dict[int, List[str]] = collections.defaultdict(list)
    fbp_files: Dict[int, List[str]] = collections.defaultdict(list)
    abc_files: Dict[int, List[str]] = collections.defaultdict(list)
    rng_files: Dict[int, List[str]] = collections.defaultdict(list)
    bgm_hd_files: Dict[int, List[str]] = collections.defaultdict(list)

    for k in all_files:
        if m := re.match(r"^graphics/mgo/mgo_(\d+)", k):
            mgo_files[int(m.group(1))].append(k)
        elif m := re.match(r"^graphics/gop/gop_(\d+)", k):
            gop_files[int(m.group(1))].append(k)
        elif m := re.match(r"^graphics/maps_hd/map_(\d+)/", k):
            maps_hd_files[int(m.group(1))].append(k)
        elif m := re.match(r"^graphics/fbp/fbp_(\d+)", k):
            fbp_files[int(m.group(1))].append(k)
        elif m := re.match(r"^graphics/abc/abc_(\d+)", k):
            abc_files[int(m.group(1))].append(k)
        elif m := re.match(r"^graphics/rng/rng_(\d+)/", k):
            rng_files[int(m.group(1))].append(k)
        elif m := re.match(r"^audio/bgm_hd/mus_(\d+)\.(mp3|ogg)$", k):
            bgm_hd_files[int(m.group(1))].append(k)

    pack_keys: Dict[str, Set[str]] = collections.defaultdict(set)
    assigned_non_region: Set[str] = set()

    def put_boot(k: str) -> None:
        if k in all_files:
            pack_keys["boot"].add(k)
            assigned_non_region.add(k)

    def put_shared(k: str) -> None:
        if k in all_files and k not in assigned_non_region:
            pack_keys["shared"].add(k)
            assigned_non_region.add(k)

    # 1. Boot pack: core data, scripts, i18n, fonts, UI, BGM, title/status FBP, orig_saves/10.rpg
    for k in all_files:
        if (
            k == "manifest.json"
            or k.startswith("data/")
            or k.startswith("fonts/")
            or k.startswith("graphics/ui/")
            or k.startswith("audio/midi/")
            or k.startswith("audio/rix/")
            or k == "orig_saves/10.rpg"
        ):
            put_boot(k)
    for fid in (0, 1, 2, 3, 4, 68, 69):
        for k in fbp_files.get(fid, []):
            put_boot(k)
    # Title screen HD BGM (track #4) in boot so opening screen plays HD audio immediately
    for k in bgm_hd_files.get(4, []):
        put_boot(k)

    # 2. Shared-by-construction: all runtime/save-dependent assets (#237 review)
    for k in all_files:
        if k.startswith("graphics/f/") or k.startswith("graphics/fire/") or k.startswith("audio/sfx/"):
            put_shared(k)
    for r in roles_doc.get("roles", []):
        mgo_id = int(r.get("walk_sprite_mgo", 0) or 0)
        for k in mgo_files.get(mgo_id, []):
            put_shared(k)
    # Core protagonist/follower/effect MGOs 1..=15
    for mid in range(1, 16):
        for k in mgo_files.get(mid, []):
            put_shared(k)

    # 3. Per-map Track-B HD packs (`map-hd-XXX`) & per-cutscene RNG packs (`rng-XX`)
    map_hd_packs: Dict[str, str] = {}
    for map_id, ks in sorted(maps_hd_files.items()):
        pid = f"map-hd-{map_id:03d}"
        map_hd_packs[str(map_id)] = pid
        for k in ks:
            pack_keys[pid].add(k)
            assigned_non_region.add(k)

    rng_packs: Dict[str, str] = {}
    for rid, ks in sorted(rng_files.items()):
        pid = f"rng-{rid:02d}"
        rng_packs[str(rid)] = pid
        for k in ks:
            pack_keys[pid].add(k)
            assigned_non_region.add(k)

    # 4. Dev-only saves & loose mods (excluded from public packs unless --include-dev-saves)
    skipped_dev: Set[str] = set()
    for k in all_files:
        if k.startswith("orig_saves/") and k != "orig_saves/10.rpg":
            if include_dev_saves:
                pack_keys["dev-orig-saves"].add(k)
            else:
                skipped_dev.add(k)
            assigned_non_region.add(k)
        elif k.startswith("mods/"):
            skipped_dev.add(k)
            assigned_non_region.add(k)

    # 5. Static region dependency graph from scenes + events + scripts
    key_regions: Dict[str, Set[str]] = collections.defaultdict(set)
    scene_packs: Dict[str, List[str]] = {}
    scene_to_region: Dict[int, str] = {}

    for sc in scenes_list:
        sid = int(sc["id"])
        reg = region_for_scene(sid)
        scene_to_region[sid] = reg
        scene_packs[str(sid)] = [reg]

        map_id = int(sc.get("map_id", 0) or 0)
        if map_id > 0:
            mbin = f"maps/map_{map_id:03d}.bin"
            if mbin in all_files and mbin not in assigned_non_region:
                key_regions[mbin].add(reg)
            for k in gop_files.get(map_id, []):
                if k not in assigned_non_region:
                    key_regions[k].add(reg)

        ev_start = int(sc.get("event_start", 0) or 0)
        ev_end = int(sc.get("event_end", 0) or 0)
        for eid in range(ev_start, ev_end + 1):
            ev = events_by_id.get(eid)
            if not ev:
                continue
            sp = int(ev.get("sprite_id", 0) or 0)
            if sp > 0:
                for k in mgo_files.get(sp, []):
                    if k not in assigned_non_region:
                        key_regions[k].add(reg)

    # Scan scripts for scene_change portals (for prefetch adjacency), battle_start (abc/fbp), fbp_show
    enemy_abc: Dict[int, int] = {
        int(e["enemy_id"]): int(e.get("abc_sprite_id", 0) or 0)
        for e in enemies_doc.get("enemies", [])
        if "enemy_id" in e
    }
    team_enemies: Dict[int, List[int]] = {}
    for t in enemies_doc.get("teams", []):
        tid = int(t.get("team_id", 0) or 0)
        team_enemies[tid] = [int(x) for x in t.get("enemies", []) if int(x) > 0]

    # Map script labels to owning scene regions via scene enter/teleport & event triggers
    label_regions: Dict[str, Set[str]] = collections.defaultdict(set)
    for sc in scenes_list:
        reg = region_for_scene(int(sc["id"]))
        for fld in ("script_enter", "script_teleport"):
            lbl = sc.get(fld) or ""
            if lbl:
                label_regions[lbl].add(reg)
        for eid in range(int(sc.get("event_start", 0) or 0), int(sc.get("event_end", 0) or 0) + 1):
            ev = events_by_id.get(eid)
            if not ev:
                continue
            for fld in ("script_trigger", "script_auto"):
                lbl = ev.get(fld) or ""
                if lbl:
                    label_regions[lbl].add(reg)

    prefetch_regions: Dict[str, Set[str]] = collections.defaultdict(set)
    scripts_dir = assets_root / "data/scripts"
    for spath in sorted(scripts_dir.glob("scripts_*.yaml")):
        sdoc = yaml.safe_load(spath.read_text("utf-8")) or {}
        scripts_map = sdoc.get("scripts", sdoc)
        if not isinstance(scripts_map, dict):
            continue
        for lbl, steps in scripts_map.items():
            if not isinstance(steps, list):
                continue
            regs = label_regions.get(lbl)
            if not regs:
                continue
            for st in steps:
                if not isinstance(st, dict):
                    continue
                op = st.get("op", "")
                args = st.get("args") or []
                if op == "scene_change":
                    dst_sid = int(st.get("scene_id", args[0] if args else 0) or 0)
                    dst_reg = scene_to_region.get(dst_sid)
                    if dst_reg:
                        for r in regs:
                            if dst_reg != r:
                                prefetch_regions[r].add(dst_reg)
                elif op == "battle_start":
                    tid = int(st.get("team_id", args[0] if args else 0) or 0)
                    for eid in team_enemies.get(tid, []):
                        aid = enemy_abc.get(eid, 0)
                        for k in abc_files.get(aid, []):
                            if k not in assigned_non_region:
                                key_regions[k].update(regs)
                elif op == "fbp_show":
                    fid = int(st.get("fbp_id", args[0] if args else 0) or 0)
                    for k in fbp_files.get(fid, []):
                        if k not in assigned_non_region:
                            key_regions[k].update(regs)
                elif op in ("music_play", "battle_music_set"):
                    tid = int(st.get("track", args[0] if args else 0) or 0)
                    for k in bgm_hd_files.get(tid, []):
                        if k not in assigned_non_region:
                            key_regions[k].update(regs)

    # 6. Assign region assets:
    #    - 1 region -> exclusive to that region
    #    - 2 regions & size < 64 KiB -> bounded duplication into both regions (#237 review)
    #    - >= 3 regions or >= 64 KiB across >= 2 regions -> `shared`
    dup_bytes = 0
    dup_entries = 0
    for k, regs in key_regions.items():
        if k in assigned_non_region:
            continue
        sz = all_files[k].stat().st_size
        if len(regs) == 1:
            r = next(iter(regs))
            pack_keys[r].add(k)
        elif len(regs) == 2 and sz < SMALL_DUP_THRESHOLD_BYTES:
            for r in regs:
                pack_keys[r].add(k)
            dup_bytes += sz
            dup_entries += 1
        else:
            pack_keys["shared"].add(k)
            assigned_non_region.add(k)

    # 7. Guarantee 100.0% coverage: any remaining unreferenced/dynamic public asset goes to an
    #    on-demand `extra` pack (resolved via `pack_key_index.json` only if ever requested),
    #    keeping `shared` lean (~25 MiB instead of 68 MiB)!
    covered: Set[str] = set(skipped_dev)
    for ks in pack_keys.values():
        covered.update(ks)
    unreferenced = sorted(set(all_files.keys()) - covered)
    for k in unreferenced:
        pack_keys["extra"].add(k)

    # Build per-scene prefetch list from region adjacency
    prefetch_map: Dict[str, List[str]] = {}
    for sid_s, plist in scene_packs.items():
        reg = plist[0]
        adj = sorted(prefetch_regions.get(reg, ()))
        if adj:
            prefetch_map[sid_s] = adj

    stats = {
        "total_assets": len(all_files),
        "skipped_dev_files": len(skipped_dev),
        "unreferenced_to_extra": len(unreferenced),
        "small_dup_entries": dup_entries,
        "small_dup_bytes": dup_bytes,
    }
    return pack_keys, scene_packs, map_hd_packs, rng_packs, prefetch_map, stats


def cmd_build(args: argparse.Namespace) -> int:
    assets_root = Path(args.assets).resolve()
    out_dir = Path(args.out).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)

    master_key: Optional[bytes] = None
    if args.encrypt:
        raw_hex = args.master_key_hex
        if args.master_key_file:
            raw_hex = Path(args.master_key_file).read_text("utf-8").strip()
        if args.master_key_env:
            raw_hex = os.environ.get(args.master_key_env, "").strip()
        if not raw_hex:
            print(
                "ERROR: --encrypt requires --master-key-hex, --master-key-file, or --master-key-env",
                file=sys.stderr,
            )
            return 2
        master_key = bytes.fromhex(raw_hex.strip())
        if len(master_key) != 32:
            print("ERROR: master key must be 32 bytes (64 hex chars)", file=sys.stderr)
            return 2

    all_files = scan_all_assets(assets_root)
    pack_keys, scene_packs, map_hd_packs, rng_packs, prefetch_map, stats = analyze_dependencies(
        assets_root, all_files, args.include_dev_saves
    )

    boot_ids = ["boot"]
    shared_ids = ["shared"]

    # Optional automatic sharding (`--max-pack-mb 18`) so every `.rpk` fits under CDN per-file
    # caps such as jsDelivr's 20 MiB GitHub limit (#237).
    max_pack_bytes = int(args.max_pack_mb * 1024 * 1024) if args.max_pack_mb and args.max_pack_mb > 0 else 0
    if max_pack_bytes > 0:
        sharded_map: Dict[str, List[str]] = {}
        for pid in list(sorted(pack_keys.keys())):
            if pid.startswith("map-hd-") or pid.startswith("rng-"):
                continue
            ks = sorted(pack_keys[pid])
            total_sz = sum(all_files[k].stat().st_size for k in ks)
            if total_sz <= max_pack_bytes or len(ks) <= 1:
                continue
            shards: List[List[str]] = []
            cur: List[str] = []
            cur_sz = 0
            for k in ks:
                sz = all_files[k].stat().st_size
                if cur and cur_sz + sz > max_pack_bytes:
                    shards.append(cur)
                    cur = [k]
                    cur_sz = sz
                else:
                    cur.append(k)
                    cur_sz += sz
            if cur:
                shards.append(cur)
            if len(shards) <= 1:
                continue
            del pack_keys[pid]
            new_ids = []
            for idx, shard_ks in enumerate(shards, start=1):
                spid = f"{pid}-{idx}"
                pack_keys[spid] = set(shard_ks)
                new_ids.append(spid)
            sharded_map[pid] = new_ids

        if sharded_map:
            def expand(id_list: List[str]) -> List[str]:
                out: List[str] = []
                for x in id_list:
                    out.extend(sharded_map.get(x, [x]))
                return out

            boot_ids = expand(boot_ids)
            shared_ids = expand(shared_ids)
            for sc, plist in list(scene_packs.items()):
                scene_packs[sc] = expand(plist)
            for sc, plist in list(prefetch_map.items()):
                prefetch_map[sc] = expand(plist)

    # Build `pack_key_index.json` (key -> primary owning pack_id) and embed it inside the first boot pack!
    boot_primary = boot_ids[0]
    key_index: Dict[str, str] = {}
    for pid in sorted(pack_keys.keys()):
        if pid in boot_ids:
            continue
        for k in sorted(pack_keys[pid]):
            key_index.setdefault(k, pid)
    for pid in boot_ids:
        for k in sorted(pack_keys[pid]):
            key_index[k] = pid
    key_index_bytes = json.dumps(key_index, separators=(",", ":"), sort_keys=True).encode("utf-8")

    # Emit each `.rpk`
    packs_meta: Dict[str, dict] = {}
    total_rpk_bytes = 0
    url_bases: List[str] = []
    if args.base_url:
        url_bases.append(args.base_url.rstrip("/"))
    for m in args.mirror_url or []:
        if m:
            url_bases.append(m.rstrip("/"))

    for pid in sorted(pack_keys.keys()):
        ks = sorted(pack_keys[pid])
        if not ks:
            continue
        entry_map: Dict[str, bytes] = {k: all_files[k].read_bytes() for k in ks}
        if pid == boot_primary:
            entry_map["pack_key_index.json"] = key_index_bytes

        content_key = secrets.token_bytes(32) if master_key is not None else None
        rpk_bytes, entry_count = write_rpk_bytes(
            pid,
            entry_map,
            compress_toc=True,
            content_key=content_key,
            encrypt_toc=bool(master_key is not None and args.encrypt_toc),
        )
        sha = hashlib.sha256(rpk_bytes).hexdigest()
        short = sha[:8]
        filename = f"p-{short}.rpk" if args.opaque_names else f"{pid}.{short}.rpk"
        (out_dir / filename).write_bytes(rpk_bytes)
        total_rpk_bytes += len(rpk_bytes)

        desc: dict = {
            "file": filename,
            "sha256": sha,
            "size": len(rpk_bytes),
            "encrypted": master_key is not None,
            "entry_count": entry_count,
        }
        if url_bases:
            desc["urls"] = [f"{u}/{filename}" for u in url_bases]
        if master_key is not None and content_key is not None:
            desc["wrapped_key"] = wrap_pack_key(master_key, pid, content_key)
        packs_meta[pid] = desc

    manifest = {
        "format": PACK_MANIFEST_FORMAT,
        "schema_version": PACK_SCHEMA_VERSION,
        "version": args.version,
        "boot": boot_ids,
        "shared": shared_ids,
        "packs": packs_meta,
        "scenes": scene_packs,
        "maps": map_hd_packs,
        "rng": rng_packs,
        "prefetch": prefetch_map,
        "key_index_entry": "pack_key_index.json",
    }
    manifest_bytes = (json.dumps(manifest, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
    manifest_path = out_dir / "pack-manifest.json"
    manifest_path.write_bytes(manifest_bytes)
    manifest_sha = hashlib.sha256(manifest_bytes).hexdigest()

    # Summary report
    print(f"=== RePal RPK1 Pack Build Complete ({args.version}) ===")
    print(f"  Output dir:    {out_dir}")
    print(f"  Manifest:      {manifest_path.name} ({len(manifest_bytes)} B, sha256={manifest_sha[:16]}...)")
    print(f"  Total packs:   {len(packs_meta)} packs, {total_rpk_bytes / (1024*1024):.2f} MiB total")
    print(f"  Encrypted:     {bool(master_key is not None)} (encrypt_toc={bool(args.encrypt_toc)})")
    print(
        f"  Coverage:      {stats['total_assets'] - stats['skipped_dev_files']}/{stats['total_assets']} "
        f"public assets packed (skipped_dev={stats['skipped_dev_files']}, "
        f"unreferenced->extra={stats['unreferenced_to_extra']}, "
        f"small_dup={stats['small_dup_entries']} files / {stats['small_dup_bytes']/1024:.1f} KiB)"
    )
    for pid in ("boot", "shared", "extra") + tuple(r[0] for r in REGION_RANGES):
        if pid in packs_meta:
            p = packs_meta[pid]
            print(f"    {pid:18s} {p['entry_count']:4d} entries  {p['size']/(1024*1024):6.2f} MiB  {p['file']}")
    map_hd_total = sum(p["size"] for k, p in packs_meta.items() if k.startswith("map-hd-"))
    rng_total = sum(p["size"] for k, p in packs_meta.items() if k.startswith("rng-"))
    print(f"    {'map-hd-* (x' + str(len(map_hd_packs)) + ')':18s}            {map_hd_total/(1024*1024):6.2f} MiB")
    print(f"    {'rng-* (x' + str(len(rng_packs)) + ')':18s}            {rng_total/(1024*1024):6.2f} MiB")
    return 0


def cmd_rotate_key(args: argparse.Namespace) -> int:
    manifest_path = Path(args.manifest).resolve()
    old_hex = args.old_key_hex or Path(args.old_key_file).read_text("utf-8").strip()
    new_hex = args.new_key_hex or Path(args.new_key_file).read_text("utf-8").strip()
    old_key = bytes.fromhex(old_hex.strip())
    new_key = bytes.fromhex(new_hex.strip())
    if len(old_key) != 32 or len(new_key) != 32:
        print("ERROR: keys must be 32 bytes (64 hex chars)", file=sys.stderr)
        return 2

    manifest = json.loads(manifest_path.read_text("utf-8"))
    rotated = 0
    for pid, desc in manifest.get("packs", {}).items():
        wrapped = desc.get("wrapped_key")
        if wrapped:
            ckey = unwrap_pack_key(old_key, pid, wrapped)
            desc["wrapped_key"] = wrap_pack_key(new_key, pid, ckey)
            rotated += 1
    out_bytes = (json.dumps(manifest, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
    manifest_path.write_bytes(out_bytes)
    print(f"Rotated master key for {rotated} packs in {manifest_path} (0 .rpk files modified; new manifest sha256={hashlib.sha256(out_bytes).hexdigest()})")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description="RePal RPK1 Level-Based Asset Pack Builder (#237)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    pb = sub.add_parser("build", help="Build .rpk packs and pack-manifest.json from loose assets dir")
    pb.add_argument("--assets", default="site/assets", help="Path to loose assets directory")
    pb.add_argument("--out", required=True, help="Output directory for .rpk packs + pack-manifest.json")
    pb.add_argument("--version", default="assets-v3", help="Manifest version tag")
    pb.add_argument("--base-url", default="", help="Optional primary base URL prefix for pack urls[]")
    pb.add_argument("--mirror-url", action="append", default=[], help="Optional fallback mirror base URL (repeatable)")
    pb.add_argument("--max-pack-mb", type=float, default=0.0, help="Auto-shard packs exceeding N MiB (e.g. 18 for jsDelivr 20 MiB cap)")
    pb.add_argument("--include-dev-saves", action="store_true", help="Include non-10.rpg orig_saves in dev-orig-saves pack")
    pb.add_argument("--encrypt", action="store_true", help="Encrypt packs with per-entry AES-256-GCM")
    pb.add_argument("--encrypt-toc", action="store_true", help="Also encrypt each pack's TOC payload")
    pb.add_argument("--opaque-names", action="store_true", help="Use opaque p-<hash>.rpk filenames")
    pb.add_argument("--master-key-hex", default="", help="64-hex-char master key (for dev/CI)")
    pb.add_argument("--master-key-file", default="", help="Path to file containing 64-hex-char master key")
    pb.add_argument("--master-key-env", default="", help="Env var name containing 64-hex-char master key (recommended for CI)")

    pr = sub.add_parser("rotate-key", help="Re-wrap pack content keys in pack-manifest.json under a new master key")
    pr.add_argument("--manifest", required=True, help="Path to pack-manifest.json")
    pr.add_argument("--old-key-hex", default="")
    pr.add_argument("--old-key-file", default="")
    pr.add_argument("--new-key-hex", default="")
    pr.add_argument("--new-key-file", default="")

    args = ap.parse_args()
    if args.cmd == "build":
        return cmd_build(args)
    if args.cmd == "rotate-key":
        return cmd_rotate_key(args)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
