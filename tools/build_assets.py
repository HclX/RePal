#!/usr/bin/env python3
"""Master static asset builder for PalRemake (`DoSPal.zip` -> `site/`).

Produces a 100% static, GitHub-Pages-ready `site/` directory containing:
- `site/.nojekyll`
- `site/wasm/pal.wasm`
- `site/assets/data/*.yaml` & `site/assets/data/scripts/*.yaml`
- `site/assets/mods/*.yaml`
- `site/assets/graphics/**` (1x lossless WebP + 4x HD WebP atlases & JSON frame rects)
- `site/assets/maps/map_XXX.bin`
- `site/assets/audio/**` (22.05kHz 16-bit remastered WAV SFX + 88 MIDI & RIX music tracks)
"""

from __future__ import annotations
from concurrent.futures import ProcessPoolExecutor
import json
import os
import shutil
import struct
import subprocess
import sys
import time
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PYLIBS = ROOT / ".pylibs"
if PYLIBS.exists() and str(PYLIBS) not in sys.path:
    sys.path.insert(0, str(PYLIBS))

from PIL import Image  # type: ignore
from pal_pipeline.audio import export_all_audio
from pal_pipeline.decompiler import decompile_all_to_yaml, dump_yaml_with_header
from pal_pipeline.orig_saves import export_orig_saves


def _encode_one_rgba_task(args: tuple[str, str, bool]) -> tuple[str, int]:
    src_str, dst_str, is_1x = args
    src = Path(src_str)
    dst = Path(dst_str)
    dst.parent.mkdir(parents=True, exist_ok=True)

    raw = src.read_bytes()
    w, h = struct.unpack("<II", raw[:8])
    img = Image.frombytes("RGBA", (w, h), raw[8 : 8 + w * h * 4])

    # WebP maximum dimension is 16383 px
    if w > 16380 or h > 16380:
        scale = min(16380.0 / w, 16380.0 / h)
        nw = max(1, int(w * scale))
        nh = max(1, int(h * scale))
        img = img.resize((nw, nh), Image.Resampling.LANCZOS)

    if is_1x:
        img.save(dst, "WEBP", lossless=True, method=3)
    else:
        img.save(dst, "WEBP", quality=88, alpha_quality=100, method=3)
    return str(dst), dst.stat().st_size


TRACK_B_META_KEYS = ("hdTrackB", "shadowStripped")


def copy_graphics_json(src_json: Path, dst_json: Path, *, overwrite_track_b: bool = False) -> None:
    """Copy raw decoded metadata JSON while preserving Track-B flags unless --overwrite-track-b is set."""
    if not overwrite_track_b and dst_json.exists():
        try:
            src_meta = json.loads(src_json.read_text(encoding="utf-8"))
            dst_meta = json.loads(dst_json.read_text(encoding="utf-8"))
            if isinstance(src_meta, dict) and isinstance(dst_meta, dict):
                merged = False
                for key in TRACK_B_META_KEYS:
                    if key in dst_meta:
                        src_meta[key] = dst_meta[key]
                        merged = True
                if merged:
                    dst_json.write_text(json.dumps(src_meta, ensure_ascii=False), encoding="utf-8")
                    return
        except (OSError, json.JSONDecodeError):
            pass
    shutil.copy2(src_json, dst_json)


def is_track_b_protected_4x(sub: str, stem: str, ddir: Path) -> bool:
    """Return True if an existing *_4x.webp asset is a Track-B HD remaster."""
    if not stem.endswith("_4x"):
        return False
    if sub == "mgo":
        meta_path = ddir / f"{stem[:-3]}.json"
        if meta_path.is_file():
            try:
                return bool(json.loads(meta_path.read_text(encoding="utf-8")).get("hdTrackB"))
            except (OSError, json.JSONDecodeError):
                return False
    elif sub == "ui" and stem == "rgm_4x":
        done_path = ddir / "rgm_hd_done.json"
        if done_path.is_file():
            try:
                return bool(json.loads(done_path.read_text(encoding="utf-8")).get("done_portraits"))
            except (OSError, json.JSONDecodeError):
                return False
    elif sub == "fbp" and stem in ("fbp_000_4x", "fbp_001_4x", "fbp_060_4x"):
        return True
    return False


def sync_graphics_assets(
    decoded_dir: Path,
    gfx_dst: Path,
    *,
    force_gfx: bool = False,
    overwrite_track_b: bool = False,
    workers: int | None = None,
) -> tuple[list[tuple[str, str, bool]], int]:
    """Sync decoded JSON/RGBA graphics into site/assets/graphics while protecting Track-B HD assets."""
    tasks: list[tuple[str, str, bool]] = []

    for sub in ["ui", "fbp", "gop", "mgo", "abc", "f", "fire"]:
        sdir = decoded_dir / sub
        if not sdir.exists():
            continue
        ddir = gfx_dst / sub
        ddir.mkdir(parents=True, exist_ok=True)
        for jf in sdir.glob("*.json"):
            copy_graphics_json(jf, ddir / jf.name, overwrite_track_b=overwrite_track_b)
        for rf in sdir.glob("*.rgba"):
            is_1x = rf.stem.endswith("_1x")
            out_webp = ddir / f"{rf.stem}.webp"
            if not overwrite_track_b and out_webp.exists() and is_track_b_protected_4x(sub, rf.stem, ddir):
                continue
            if force_gfx or not out_webp.exists():
                if overwrite_track_b and sub == "ui" and rf.stem == "rgm_4x":
                    done_path = ddir / "rgm_hd_done.json"
                    if done_path.exists():
                        done_path.unlink()
                tasks.append((str(rf), str(out_webp), is_1x))

    # RNG cutscenes: pack frames or encode per-frame WebPs
    rng_root = decoded_dir / "rng"
    if rng_root.exists():
        for rdir in sorted(rng_root.glob("rng_*")):
            ddir = gfx_dst / "rng" / rdir.name
            ddir.mkdir(parents=True, exist_ok=True)
            mf = rdir / "meta.json"
            if mf.exists():
                shutil.copy2(mf, ddir / "meta.json")
            for rf in sorted(rdir.glob("*.rgba")):
                is_1x = rf.stem.endswith("_1x")
                out_webp = ddir / f"{rf.stem}.webp"
                if force_gfx or not out_webp.exists():
                    tasks.append((str(rf), str(out_webp), is_1x))

    pool_workers = workers or os.cpu_count() or 8
    print(f"[build_assets] Encoding {len(tasks)} RGBA atlases/frames to WebP on {pool_workers} workers...")
    total_webp_bytes = 0
    if tasks:
        with ProcessPoolExecutor(max_workers=pool_workers) as pool:
            for _, sz in pool.map(_encode_one_rgba_task, tasks, chunksize=16):
                total_webp_bytes += sz
    return tasks, total_webp_bytes


def main() -> None:
    t0 = time.time()
    force_gfx = "--refresh-graphics" in sys.argv
    gfx_only = "--graphics-only" in sys.argv
    overwrite_track_b = "--overwrite-track-b" in sys.argv
    zip_path = ROOT / "DoSPal.zip"
    raw_dir = ROOT / "raw_data"
    decoded_dir = raw_dir / "_decoded"
    site_dir = ROOT / "site"
    assets_dir = site_dir / "assets"

    if not zip_path.exists():
        raise FileNotFoundError(f"Missing {zip_path}")

    if not (raw_dir / "SSS.MKF").exists():
        print(f"[build_assets] Extracting {zip_path.name} -> {raw_dir}...")
        with zipfile.ZipFile(zip_path) as zf:
            zf.extractall(raw_dir)

    pal_cli = ROOT / "target" / "release" / "pal-cli"
    if force_gfx or not (decoded_dir / "ui" / "rgm_4x.rgba").exists():
        print("[build_assets] Running native 8-core pal-cli unpacker & 4x upscaler...")
        subprocess.run([str(pal_cli), str(raw_dir), str(decoded_dir), "all"], check=True)

    # 1. Ensure .nojekyll and copy WASM core
    site_dir.mkdir(parents=True, exist_ok=True)
    (site_dir / ".nojekyll").write_text("", encoding="utf-8")
    wasm_src = ROOT / "target" / "wasm32-unknown-unknown" / "release" / "pal_wasm.wasm"
    if wasm_src.exists():
        wasm_dst = site_dir / "wasm" / "pal.wasm"
        wasm_dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(wasm_src, wasm_dst)
        print(f"[build_assets] Copied {wasm_dst.relative_to(ROOT)} ({wasm_dst.stat().st_size} bytes)")

    decomp_stats: dict = {}
    audio_stats: dict = {"sfx_exported": 211, "midi_tracks": []}
    map_count = len(list((assets_dir / "maps").glob("map_*.bin")))

    if not gfx_only:
        # 2. Decompile all scripts & game tables to human-readable YAML
        print("[build_assets] Decompiling SSS.MKF / DATA.MKF / M.MSG -> YAML...")
        decomp_stats = decompile_all_to_yaml(raw_dir, assets_dir / "data", assets_dir / "mods")
        print(f"[build_assets] Decompiled stats: {decomp_stats}")

        # 3. Export & remaster audio
        print("[build_assets] Exporting & remastering 276 VOC SFX + 88 MIDI/RIX music tracks...")
        audio_stats = export_all_audio(raw_dir, assets_dir / "audio")
        dump_yaml_with_header(
            assets_dir / "data" / "music.yaml",
            ["PalRemake 音乐与音效索引表 (逆向自 MIDI.MKF / MUS.MKF / VOC.MKF)"],
            audio_stats,
        )

        # 4. Copy decompressed 226 map binaries
        maps_dst = assets_dir / "maps"
        maps_dst.mkdir(parents=True, exist_ok=True)
        map_count = 0
        for mp in sorted((decoded_dir / "maps").glob("map_*.bin")):
            shutil.copy2(mp, maps_dst / mp.name)
            map_count += 1
        print(f"[build_assets] Copied {map_count} binary maps.")

        # 4b. Export original 51 DOS .rpg saves (`0.rpg`..`50.rpg`)
        orig_saves_cnt = export_orig_saves(raw_dir, assets_dir)
        print(f"[build_assets] Exported {orig_saves_cnt} original DOS .rpg save files.")
    gfx_dst = assets_dir / "graphics"
    _, total_webp_bytes = sync_graphics_assets(
        decoded_dir,
        gfx_dst,
        force_gfx=force_gfx,
        overwrite_track_b=overwrite_track_b,
    )

    elapsed = time.time() - t0
    if not gfx_only or not (assets_dir / "manifest.json").exists():
        manifest = {
            "project": "PalRemake",
            "source_archive": "DoSPal.zip",
            "decompiled": decomp_stats,
            "maps_exported": map_count,
            "sfx_exported": audio_stats["sfx_exported"],
            "midi_exported": len(audio_stats["midi_tracks"]),
            "build_seconds": round(elapsed, 2),
        }
        (assets_dir / "manifest.json").write_text(
            json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8"
        )
    print(f"[build_assets] ALL DONE in {elapsed:.1f}s! Newly encoded WebP={total_webp_bytes / (1024*1024):.1f} MiB")


if __name__ == "__main__":
    main()
