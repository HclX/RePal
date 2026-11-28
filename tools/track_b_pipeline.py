#!/usr/bin/env python3
"""Generalized Offline Track-B HD Asset Pipeline (`tools/track_b_pipeline.py`).

Supports all 294 scenes & 221 maps of PalRemake with ZERO runtime AI dependency:
  - `status`: Report HD completion across scenes, maps, MGO sprites, and RGM portraits.
  - `prepare --scene <N> --work-dir <DIR> [--chain] [--redo-map]`: Compose zero-hole 1376x768 (43:24) reference
    inputs & `manifest_todo.json`. `--chain` emits only each island's first patch; `--redo-map` re-prepares an
    already-baked map.
  - `chain-next --scene <N> --work-dir <DIR>`: After each chained generation round, stitch the finished patches like
    `bake` does and emit the next patch inputs with their already-painted neighbour pixels pasted in.
  - `bake --scene <N> --work-dir <DIR>`: Register, harmonize, seam-cut stitch, de-matte, extract exact PAL `sortY`
    per-depth occluders, hard-gate verify (`verify_map`), write static WebP + JSON into `site/assets/graphics/`,
    and bump `ASSET_VER`.
  - `verify-map --map <ID>`: Hard-gate zero-hole alpha check + differential walkable silhouette occlusion sweep.
  - `rebake-map --map <ID> --work-dir <DIR>`: Recompute zero-hole 1x masks/references & rebake + verify map.
  - `prepare-cutout-patches --map <ID> --work-dir <DIR>`: Emit `*_regen_input_1376x768.png` + `cutout_regen_todo.json`
    ONLY for patches affected by missing cutouts.
  - `calibrate-map --map <ID>`: Apply canonical 1x-anchored Lab calibration, adaptive texture normalization,
    story-variant inheritance (if applicable), and occluder RGB sync in-place, then run `verify-map`.
  - `calibrate-all-maps`: Calibrate & verify every baked map in `site/assets/graphics/maps_hd/` in donor-first order.
"""

from __future__ import annotations
import argparse
from collections import deque
import json
import math
import re
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PYLIBS = ROOT / ".pylibs"
if PYLIBS.exists() and str(PYLIBS) not in sys.path:
    sys.path.insert(0, str(PYLIBS))

import numpy as np
from scipy.ndimage import (
    binary_dilation,
    binary_erosion,
    binary_fill_holes,
    distance_transform_edt,
    find_objects,
    gaussian_filter,
    label,
    maximum_filter,
    minimum_filter,
    uniform_filter,
)
from PIL import Image, ImageDraw, ImageFilter
import yaml

SITE = ROOT / "site"
DATA = SITE / "assets" / "data"
GFX = SITE / "assets" / "graphics"
MAPS_BIN = SITE / "assets" / "maps"
SHARED_TMP_ROOT = Path("/tmp/pal_hd_shared")

BOX_W, BOX_H = 688, 384  # Exact 43:24 (1376x768 / 2) native aspect ratio


def resolve_work_or_asset_path(p: Path | str, work_dir: Path | None = None) -> Path:
    """Resolve work-dir or AI asset paths across worker sandboxes (including `/tmp/pal_hd_shared` mirror)."""
    path = Path(p)
    if path.exists():
        return path
    s = str(path)
    if SHARED_TMP_ROOT.exists():
        m = re.search(r"/brain/[0-9a-f-]+/(.*)$", s)
        if m:
            cand = SHARED_TMP_ROOT / m.group(1)
            if cand.exists() or cand.parent.exists():
                return cand
        cand_flat = SHARED_TMP_ROOT / path.name
        if cand_flat.exists():
            return cand_flat
    if work_dir is not None:
        cand_wd = work_dir / path.name
        if cand_wd.exists():
            return cand_wd
        for anc in work_dir.parents:
            cand_anc = anc / path.name
            if cand_anc.exists():
                return cand_anc
    return path


# ---------------------------------------------------------------------------
# Low-level Image & Geometry Helpers
# ---------------------------------------------------------------------------

def extrude_rgb_voronoi(rgba: np.ndarray, iters: int = 5) -> np.ndarray:
    """Extrude opaque RGB into transparent exterior pixels so bilinear filtering never bleeds dark fringes."""
    out = rgba.copy()
    filled = out[:, :, 3] > 16
    if filled.all() or not filled.any():
        return out
    for _ in range(iters):
        if filled.all():
            break
        prev = out[:, :, :3].astype(np.int32)
        pf = filled.copy()
        acc = np.zeros_like(prev)
        wt = np.zeros(filled.shape, dtype=np.int32)
        for dy, dx, w in [(-1, 0, 2), (1, 0, 2), (0, -1, 2), (0, 1, 2), (-1, -1, 1), (1, -1, 1), (-1, 1, 1), (1, 1, 1)]:
            s_pf = np.roll(np.roll(pf, dy, axis=0), dx, axis=1)
            if dy == 1:
                s_pf[0, :] = False
            elif dy == -1:
                s_pf[-1, :] = False
            if dx == 1:
                s_pf[:, 0] = False
            elif dx == -1:
                s_pf[:, -1] = False
            s_rgb = np.roll(np.roll(prev, dy, axis=0), dx, axis=1)
            m = (~pf) & s_pf
            acc[m] += s_rgb[m] * w
            wt[m] += w
        upd = (~pf) & (wt > 0)
        if not upd.any():
            break
        out[upd, :3] = ((acc[upd] + (wt[upd, None] // 2)) // wt[upd, None]).clip(0, 255).astype(np.uint8)
        filled[upd] = True
    return out


def register_translation_2d(ref_rgb: np.ndarray, cand_rgb: np.ndarray, max_shift: int = 6) -> tuple[int, int]:
    """Find integer (dx, dy) shift of `cand_rgb` maximizing normalized Sobel-edge correlation with `ref_rgb`."""
    # Sparse boundary patches (< 6% non-black) keep exact (0, 0) framing so single silhouette edges never drift
    if float((ref_rgb.sum(axis=2) > 0).mean()) < 0.06:
        return 0, 0

    def edge_mag(rgb: np.ndarray) -> np.ndarray:
        lum = 0.299 * rgb[:, :, 0] + 0.587 * rgb[:, :, 1] + 0.114 * rgb[:, :, 2]
        lum = gaussian_filter(lum.astype(np.float32), sigma=1.2)
        gx = np.roll(lum, -1, axis=1) - np.roll(lum, 1, axis=1)
        gy = np.roll(lum, -1, axis=0) - np.roll(lum, 1, axis=0)
        e = np.sqrt(gx * gx + gy * gy)
        e[:4, :] = e[-4:, :] = e[:, :4] = e[:, -4:] = 0
        return e

    e_ref = edge_mag(ref_rgb)
    e_cand = edge_mag(cand_rgb)
    if float(e_ref.sum()) < 1e-3:
        return 0, 0
    best_score = -1.0
    zero_score = 0.0
    best_dx, best_dy = 0, 0
    h, w = e_ref.shape
    for dy in range(-max_shift, max_shift + 1):
        y0_r, y1_r = max(0, dy), min(h, h + dy)
        y0_c, y1_c = max(0, -dy), min(h, h - dy)
        er_y = e_ref[y0_r:y1_r]
        ec_y = e_cand[y0_c:y1_c]
        for dx in range(-max_shift, max_shift + 1):
            x0_r, x1_r = max(0, dx), min(w, w + dx)
            x0_c, x1_c = max(0, -dx), min(w, w - dx)
            sub_r = er_y[:, x0_r:x1_r]
            sub_c = ec_y[:, x0_c:x1_c]
            denom = float(np.sqrt(np.sum(sub_r * sub_r) * np.sum(sub_c * sub_c))) + 1e-6
            corr = float(np.sum(sub_r * sub_c)) / denom
            if dx == 0 and dy == 0:
                zero_score = corr
            score = corr * (1.0 - 0.004 * math.hypot(dx, dy))
            if score > best_score:
                best_score = score
                best_dx, best_dy = dx, dy
    if best_score - zero_score < 0.025:
        return 0, 0
    return best_dx, best_dy


def shift_image_rgb(arr: np.ndarray, dx: int, dy: int, fill=None) -> np.ndarray:
    """Shift `arr` by `(dx, dy)`, replicating border pixels (`mode='edge'`) so painted borders never get black bars."""
    if dx == 0 and dy == 0:
        return arr
    h, w = arr.shape[:2]
    ys = np.clip(np.arange(h) - dy, 0, h - 1)
    xs = np.clip(np.arange(w) - dx, 0, w - 1)
    return arr[ys[:, None], xs[None, :]].copy()


def strip_dos_shadow_1x(rgba: np.ndarray, is_prop: bool = False) -> tuple[np.ndarray, bool]:
    """Remove DOS 1-bit checkerboard foot shadows from 1x character sprites. Returns (cleaned_rgba, stripped_any)."""
    if is_prop:
        return rgba, False
    out = rgba.copy()
    h, w = out.shape[:2]
    y0 = int(h * 0.30)
    seeds = []
    seed_greens = {(0, 36, 0), (0, 32, 20), (0, 28, 0)}
    shadow_colors = seed_greens | {(48, 28, 16), (28, 20, 12), (48, 20, 8), (52, 0, 0), (36, 20, 12), (20, 16, 8)}
    for y in range(y0, h):
        for x in range(w):
            if out[y, x, 3] > 0 and (int(out[y, x, 0]), int(out[y, x, 1]), int(out[y, x, 2])) in seed_greens:
                seeds.append((y, x))
    if len(seeds) < 3:
        return rgba, False
    rm = np.zeros((h, w), dtype=bool)
    for sy, sx in seeds:
        rm[sy, sx] = True
    stack = list(seeds)
    while stack:
        cy, cx = stack.pop()
        for dy in (-1, 0, 1):
            for dx in (-1, 0, 1):
                ny, nx = cy + dy, cx + dx
                if y0 <= ny < h and 0 <= nx < w and not rm[ny, nx] and out[ny, nx, 3] > 0:
                    c = (int(out[ny, nx, 0]), int(out[ny, nx, 1]), int(out[ny, nx, 2]))
                    if c in shadow_colors:
                        rm[ny, nx] = True
                        stack.append((ny, nx))
    out[rm] = [0, 0, 0, 0]
    return out, True


# ---------------------------------------------------------------------------
# Game Data Loading & Reachability Tracing
# ---------------------------------------------------------------------------

def load_game_tables():
    scenes_raw = yaml.safe_load((DATA / "scenes.yaml").read_text())
    scenes = scenes_raw.get("scenes", scenes_raw)
    events_raw = yaml.safe_load((DATA / "events.yaml").read_text())
    events = events_raw.get("events", events_raw)
    all_scripts: dict[str, list[dict]] = {}
    for p in sorted((DATA / "scripts").glob("*.yaml")):
        d = yaml.safe_load(p.read_text())
        all_scripts.update(d.get("scripts", d))
    return scenes, events, all_scripts


def get_done_rgm_set() -> set[int]:
    p = GFX / "ui" / "rgm_hd_done.json"
    if not p.exists():
        return set()
    return set(json.loads(p.read_text()).get("done_portraits", []))


def save_done_rgm_set(done_ids: set[int]):
    p = GFX / "ui" / "rgm_hd_done.json"
    p.write_text(json.dumps({"done_portraits": sorted(done_ids)}, indent=2) + "\n")


def is_mgo_done(mid: int) -> bool:
    jp = GFX / "mgo" / f"mgo_{mid:03d}.json"
    if not jp.exists():
        return True
    return bool(json.loads(jp.read_text()).get("hdTrackB"))


def is_map_done(map_id: int) -> bool:
    if map_id <= 0:
        return True
    return (GFX / "maps_hd" / f"map_{map_id:03d}" / "meta.json").exists()


def trace_reachable_scripts(seed_labels: list[str], all_scripts: dict[str, list[dict]], max_hops: int = 4) -> set[str]:
    visited: set[str] = set()
    q = deque((lbl, 0) for lbl in seed_labels if lbl and lbl in all_scripts)
    while q:
        lbl, depth = q.popleft()
        if lbl in visited:
            continue
        visited.add(lbl)
        if depth >= max_hops:
            continue
        for st in all_scripts.get(lbl, []):
            op = st.get("op", "")
            # Do NOT follow across `scene_change` to unrelated scenes
            if op == "scene_change":
                break
            for key in ("target", "on_no", "else_jump", "jump_if_less", "on_defeat", "on_flee"):
                v = st.get(key)
                if isinstance(v, str) and v in all_scripts and v not in visited:
                    q.append((v, depth + 1))
            for a in st.get("args") or []:
                if isinstance(a, str) and a in all_scripts and a not in visited:
                    q.append((a, depth + 1))
    return visited


def collect_scene_requirements(scene_id: int, scenes, events, all_scripts):
    sc_by_id = {s["id"]: s for s in scenes}
    ev_by_id = {e["id"]: e for e in events}
    sc = sc_by_id[scene_id]
    map_id = sc["map_id"]

    # Collect MGOs & seed scripts across this scene (and if baking map_id, we bake map_id once for all scenes sharing it!)
    seed_scripts = [sc.get("script_enter", ""), sc.get("script_teleport", "")]
    mgo_char_hint: dict[int, bool] = {}  # mid -> True if character, False if prop candidate

    for eid in range(sc["event_start"], sc["event_end"] + 1):
        ev = ev_by_id.get(eid)
        if not ev:
            continue
        seed_scripts.append(ev.get("script_trigger", ""))
        seed_scripts.append(ev.get("script_auto", ""))
        mid = int(ev.get("sprite_id") or 0)
        if mid > 0:
            is_char = (ev.get("sprite_frames", 0) > 0) or (ev.get("sprite_frames_auto", 0) > 0)
            mgo_char_hint[mid] = mgo_char_hint.get(mid, False) or is_char

    reach = trace_reachable_scripts(seed_scripts, all_scripts, max_hops=4)
    portraits: set[int] = set()
    for lbl in reach:
        for st in all_scripts.get(lbl, []):
            op = st.get("op", "")
            if op in ("dialogue_pos_upper", "dialogue_pos_lower"):
                pid = int(st.get("portrait_id") or 0)
                if pid > 0:
                    portraits.add(pid)
            elif op == "role_set_sprite":
                args = st.get("args") or []
                if len(args) >= 2 and int(args[1] or 0) > 0:
                    mgo_char_hint[int(args[1])] = True

    todo_map = not is_map_done(map_id)
    todo_mgos = sorted(mid for mid in mgo_char_hint if not is_mgo_done(mid))
    done_rgm = get_done_rgm_set()
    rgm_frames = json.loads((GFX / "ui" / "rgm.json").read_text())["frames"]
    todo_rgms = sorted(
        pid for pid in portraits
        if pid not in done_rgm and 0 <= pid < len(rgm_frames) and rgm_frames[pid][2] > 0 and rgm_frames[pid][3] > 0
    )
    return sc, todo_map, todo_mgos, mgo_char_hint, todo_rgms


# ---------------------------------------------------------------------------
# Map Preparation & Baking (Single-Box Indoor Islands + Multi-Patch Stitching)
# ---------------------------------------------------------------------------

def render_map_1x_and_sort_y(map_id: int) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    mbytes = (MAPS_BIN / f"map_{map_id:03d}.bin").read_bytes()
    arr = np.frombuffer(mbytes, dtype=np.uint8).reshape(128, 64, 2, 4)
    btm_s = arr[:, :, :, 0].astype(int) | ((arr[:, :, :, 1].astype(int) & 0x10) << 4)
    btm_l = arr[:, :, :, 1] & 0x0F
    raw_top = arr[:, :, :, 2].astype(int) | ((arr[:, :, :, 3].astype(int) & 0x10) << 4)
    top_l = arr[:, :, :, 3] & 0x0F

    frames = json.loads((GFX / "gop" / f"gop_{map_id:03d}.json").read_text())["frames"]
    gop1 = Image.open(GFX / "gop" / f"gop_{map_id:03d}_1x.webp").convert("RGBA")
    gop1_np = np.array(gop1)

    # Precompute which GOP tile sprites are non-black (not pure `#000000` void tiles)
    non_void_spr = np.zeros(len(frames), dtype=bool)
    for sid, (sx, sy, sw, sh) in enumerate(frames):
        if sw > 0 and sh > 0:
            sub = gop1_np[sy:sy + sh, sx:sx + sw]
            if int(sub[(sub[:, :, 3] > 0), :3].max(initial=0)) > 0:
                non_void_spr[sid] = True

    W, H = 2048, 2048
    full = Image.new("RGBA", (W, H), (0, 0, 0, 255))
    sort_y_buf = np.full((H, W), -1, dtype=np.int32)
    tile_occ = np.zeros((H, W), dtype=bool)

    elev_queue = []
    for ty in range(128):
        for tx in range(64):
            for h in range(2):
                dx = tx * 32 + h * 16 - 16
                dy = ty * 16 + h * 8 - 8
                if dx < 0 or dy < 0 or dx + 32 > W or dy + 16 > H:
                    continue
                bs, bl = int(btm_s[ty, tx, h]), int(btm_l[ty, tx, h])
                rt, tl = int(raw_top[ty, tx, h]), int(top_l[ty, tx, h])
                if bs >= 0 and bs < len(frames) and frames[bs][2] > 0:
                    sx, sy, sw, sh = frames[bs]
                    if non_void_spr[bs]:
                        tile_occ[dy:dy + sh, dx:dx + sw] |= gop1_np[sy:sy + sh, sx:sx + sw, 3] > 0
                    if bl == 0:
                        full.alpha_composite(gop1.crop((sx, sy, sx + sw, sy + sh)), (dx, dy))
                    else:
                        elev_queue.append((ty * 16 + h * 8 + 7 + bl * 8, bs, dx, dy))
                if rt > 0 and (rt - 1) < len(frames) and frames[rt - 1][2] > 0:
                    ts = rt - 1
                    sx, sy, sw, sh = frames[ts]
                    if non_void_spr[ts]:
                        tile_occ[dy:dy + sh, dx:dx + sw] |= gop1_np[sy:sy + sh, sx:sx + sw, 3] > 0
                    if tl == 0:
                        full.alpha_composite(gop1.crop((sx, sy, sx + sw, sy + sh)), (dx, dy))
                    else:
                        elev_queue.append((ty * 16 + h * 8 + 7 + tl * 8, ts, dx, dy))

    elev_queue.sort(key=lambda x: x[0])
    for sy_val, spr, dx, dy in elev_queue:
        sx, sy, sw, sh = frames[spr]
        full.alpha_composite(gop1.crop((sx, sy, sx + sw, sy + sh)), (dx, dy))
        alpha_m = gop1_np[sy:sy + sh, sx:sx + sw, 3] > 0
        sub = sort_y_buf[dy:dy + sh, dx:dx + sw]
        sub[alpha_m] = np.maximum(sub[alpha_m], sy_val)

    return np.array(full), sort_y_buf, tile_occ


def compute_reachable_walkable_and_events(map_id: int, scenes, events, all_scripts):
    """Run Doorway-Bounded Walkable BFS for `map_id`, returning `(vis_half_tiles, active_evs, all_cam_pts)`."""
    mbytes = (MAPS_BIN / f"map_{map_id:03d}.bin").read_bytes()
    arr = np.frombuffer(mbytes, dtype=np.uint8).reshape(128, 64, 2, 4)
    blk = (arr[:, :, :, 1] & 0x20) != 0

    ev_by_id = {e["id"]: e for e in events}
    map_scenes = [s for s in scenes if s["map_id"] == map_id]
    map_scene_ids = {s["id"] for s in map_scenes}

    # Find all `party_set_pos` entries into `map_scene_ids` + `camera_pan` targets
    seeds: list[tuple[int, int]] = []
    cam_pans: list[tuple[int, int]] = []
    for steps in all_scripts.values():
        for i, st in enumerate(steps):
            if st.get("op") == "scene_change":
                sid = st.get("scene_id") or (st.get("args") or [0])[0]
                if sid in map_scene_ids:
                    for j in range(max(0, i - 5), min(len(steps), i + 6)):
                        s2 = steps[j]
                        if s2.get("op") == "party_set_pos":
                            tx = s2.get("x") if "x" in s2 else (s2.get("args") or [0])[0]
                            ty = s2.get("y") if "y" in s2 else (s2.get("args") or [0, 0])[1]
                            th = (s2.get("args") or [0, 0, 0])[2] if len(s2.get("args") or []) > 2 else 0
                            seeds.append((int(tx) * 32 + int(th) * 16, int(ty) * 16 + int(th) * 8))

    def script_has_scene_change(lbl: str) -> bool:
        if not lbl or lbl not in all_scripts:
            return False
        seen = set()
        stk = [lbl]
        while stk:
            cur = stk.pop()
            if cur in seen:
                continue
            seen.add(cur)
            for s in all_scripts.get(cur, []):
                if s.get("op") == "scene_change":
                    return True
                for k in ("target", "on_no", "else_jump"):
                    nxt = s.get(k)
                    if isinstance(nxt, str) and nxt in all_scripts and len(seen) < 12:
                        stk.append(nxt)
        return False

    door_evs = []
    ev_pts: list[tuple[int, int]] = []
    active_evs: list[dict] = []
    for s in map_scenes:
        for lbl in trace_reachable_scripts([s.get("script_enter", "")], all_scripts, max_hops=3):
            for st in all_scripts.get(lbl, []):
                if st.get("op") == "camera_pan":
                    args = st.get("args") or [0, 0]
                    px, py = int(args[0] or 0), int(args[1] or 0)
                    if 0 <= px < 64 and 0 <= py < 128:
                        cam_pans.append((px * 32, py * 16))
                elif st.get("op") == "party_set_pos":
                    tx = st.get("x") if "x" in st else (st.get("args") or [0])[0]
                    ty = st.get("y") if "y" in st else (st.get("args") or [0, 0])[1]
                    th = (st.get("args") or [0, 0, 0])[2] if len(st.get("args") or []) > 2 else 0
                    seeds.append((int(tx) * 32 + int(th) * 16, int(ty) * 16 + int(th) * 8))
        for eid in range(s["event_start"], s["event_end"] + 1):
            e = ev_by_id.get(eid)
            if not e:
                continue
            if (
                int(e.get("state", 0)) > 0
                and int(e.get("vanish_time", 0)) <= 0
                and int(e.get("sprite_id", 0)) > 0
                and (int(e.get("x", 0)) > 0 or int(e.get("y", 0)) > 0)
            ):
                active_evs.append(e)
            trig = e.get("script_trigger") or ""
            if e["trigger_mode"] >= 4 and script_has_scene_change(trig):
                door_evs.append(e)
            elif e["x"] > 0 or e["y"] > 0:
                ev_pts.append((int(e["x"]), int(e["y"])))

    if not seeds:
        seeds = list(ev_pts)

    def in_door(px: int, py: int) -> bool:
        for de in door_evs:
            if abs(int(de["x"]) - px) + abs(int(de["y"]) - py) * 2 < (int(de["trigger_mode"]) - 4) * 32 + 16:
                return True
        return False

    vis: set[tuple[int, int, int]] = set()
    q = deque()
    for sx, sy in seeds:
        h = 1 if (sx % 32) != 0 else 0
        tx, ty = sx // 32, sy // 16
        if 0 <= tx < 64 and 0 <= ty < 128 and not blk[ty, tx, h] and (ty, tx, h) not in vis:
            vis.add((ty, tx, h))
            if not in_door(sx, sy):
                q.append((ty, tx, h))

    while q:
        ty, tx, h = q.popleft()
        px, py = tx * 32 + h * 16, ty * 16 + h * 8
        for dx, dy in [(-16, 8), (-16, -8), (16, -8), (16, 8)]:
            nx, ny = px + dx, py + dy
            nh = 1 if (nx % 32) != 0 else 0
            ntx, nty = nx // 32, ny // 16
            if 0 <= ntx < 64 and 0 <= nty < 128 and not blk[nty, ntx, nh] and (nty, ntx, nh) not in vis:
                vis.add((nty, ntx, nh))
                if not in_door(nx, ny):
                    q.append((nty, ntx, nh))

    all_pts = (
        [(tx * 32 + h * 16, ty * 16 + h * 8) for (ty, tx, h) in vis]
        + seeds
        + ev_pts
        + cam_pans
        + [(int(d["x"]), int(d["y"])) for d in door_evs]
    )
    return vis, active_evs, all_pts


def compute_camera_visible_mask_1x(map_id: int, tile_occ: np.ndarray, scenes, events, all_scripts) -> np.ndarray:
    """Compute zero-hole 2048x2048 1x mask of occupied map pixels visible from reachable camera positions."""
    if not tile_occ.any():
        return tile_occ

    vis, _, all_pts = compute_reachable_walkable_and_events(map_id, scenes, events, all_scripts)
    if len(vis) < 12:
        # Pure cutscene / fallback: include all occupied tile pixels with enclosed cavities filled
        return binary_fill_holes(tile_occ) & tile_occ

    cam_mask = np.zeros((2048, 2048), dtype=bool)
    for px, py in all_pts:
        # Clamp camera just like `pal-render`: `camX = clamp(0, 1728, px - 160)`, `camY = clamp(0, 1848, py - 112)`
        cx = max(0, min(2048 - 320, px - 160))
        cy = max(0, min(2048 - 200, py - 112))
        x0 = max(0, cx - 24)
        x1 = min(2048, cx + 320 + 24)
        y0 = max(0, cy - 24)
        y1 = min(2048, cy + 200 + 24)
        cam_mask[y0:y1, x0:x1] = True

    # Fill enclosed interior camera/forest cavities (e.g. dense tree rings) intersected with `tile_occ`
    cam_filled = binary_fill_holes(cam_mask)
    vis_mask = tile_occ & cam_filled
    return binary_fill_holes(vis_mask) & tile_occ


def build_char_sy_grid_1x(map_id: int, scenes=None, events=None) -> np.ndarray:
    """Build 2048x2048 grid of minimum possible character/event `sortY` at every walkable foot & event spot."""
    if scenes is None or events is None:
        scenes_raw = yaml.safe_load((DATA / "scenes.yaml").read_text())
        scenes = scenes_raw.get("scenes", scenes_raw)
        events_raw = yaml.safe_load((DATA / "events.yaml").read_text())
        events = events_raw.get("events", events_raw)

    mbytes = (MAPS_BIN / f"map_{map_id:03d}.bin").read_bytes()
    arr = np.frombuffer(mbytes, dtype=np.uint8).reshape(128, 64, 2, 4)
    walk = (arr[:, :, :, 1] & 0x20) == 0

    grid = np.full((2048, 2048), 999999, dtype=np.int32)
    tys, txs, hs = np.where(walk)
    pxs = txs * 32 + hs * 16
    pys = tys * 16 + hs * 8
    inb = (pxs >= 0) & (pxs < 2048) & (pys >= 0) & (pys < 2048)
    pxs, pys = pxs[inb], pys[inb]
    np.minimum.at(grid, (pys, pxs), pys + 8)

    # Intermediate 8px steps between adjacent walkable half-tiles (smooth diagonal & cardinal transitions)
    walk_pt = np.zeros((2048, 2048), dtype=bool)
    walk_pt[pys, pxs] = True
    for dx, dy in [(-16, 8), (-16, -8), (16, -8), (16, 8), (0, 16), (32, 0)]:
        nx = pxs + dx
        ny = pys + dy
        ok = (nx >= 0) & (nx < 2048) & (ny >= 0) & (ny < 2048)
        sx_ok, sy_ok, nx_ok, ny_ok = pxs[ok], pys[ok], nx[ok], ny[ok]
        both = walk_pt[ny_ok, nx_ok]
        mx = (sx_ok[both] + nx_ok[both]) // 2
        my = (sy_ok[both] + ny_ok[both]) // 2
        np.minimum.at(grid, (my, mx), my + 8)

    # Include all scene `EventObject` positions on this map
    ev_by_id = {e["id"]: e for e in events}
    for s in scenes:
        if s["map_id"] != map_id:
            continue
        for eid in range(s["event_start"], s["event_end"] + 1):
            e = ev_by_id.get(eid)
            if not e:
                continue
            ex, ey = int(e.get("x", 0)), int(e.get("y", 0))
            if (ex > 0 or ey > 0) and 0 <= ex < 2048 and 0 <= ey < 2048:
                ev_sy = ey + 8
                ly = int(e.get("layer", 0))
                if ly < 0:
                    ev_sy = min(ev_sy, ey + ly * 8 + 8)
                if ev_sy < grid[ey, ex]:
                    grid[ey, ex] = ev_sy
    return grid


# ---------------------------------------------------------------------------
# Cross-Scene Consistency: Canonical 1x-Anchored Lab Calibration,
# Adaptive Texture Normalization, Story-Variant Inheritance & Style Anchors
# ---------------------------------------------------------------------------

CANONICAL_LAB_TARGET_GRAD_4X = 3.2  # Canonical 4x Lab L* detail gradient (~7.8 RGB grad)


def rgb_to_lab_f32(rgb: np.ndarray) -> np.ndarray:
    """Vectorized sRGB (0..255 uint8 or float32) -> CIE Lab (D65) float32."""
    s = np.clip(rgb.astype(np.float32) * (1.0 / 255.0), 0.0, 1.0)
    lin = np.where(s <= 0.04045, s * (1.0 / 12.92), np.power((s + 0.055) * (1.0 / 1.055), 2.4))
    r, g, b = lin[..., 0], lin[..., 1], lin[..., 2]
    x = (r * 0.4124564 + g * 0.3575761 + b * 0.1804375) * (1.0 / 0.95047)
    y = r * 0.2126729 + g * 0.7151522 + b * 0.0721750
    z = (r * 0.0193339 + g * 0.1191920 + b * 0.9503041) * (1.0 / 1.08883)
    eps = 216.0 / 24389.0
    kappa = 24389.0 / 27.0
    fx = np.where(x > eps, np.cbrt(np.maximum(x, 0.0)), (kappa * x + 16.0) * (1.0 / 116.0))
    fy = np.where(y > eps, np.cbrt(np.maximum(y, 0.0)), (kappa * y + 16.0) * (1.0 / 116.0))
    fz = np.where(z > eps, np.cbrt(np.maximum(z, 0.0)), (kappa * z + 16.0) * (1.0 / 116.0))
    L = 116.0 * fy - 16.0
    a = 500.0 * (fx - fy)
    b_ch = 200.0 * (fy - fz)
    return np.stack([L, a, b_ch], axis=-1).astype(np.float32)


def lab_to_rgb_u8(lab: np.ndarray) -> np.ndarray:
    """Vectorized CIE Lab (D65) float32 -> sRGB uint8."""
    L = lab[..., 0]
    a = lab[..., 1]
    b_ch = lab[..., 2]
    fy = (L + 16.0) * (1.0 / 116.0)
    fx = a * (1.0 / 500.0) + fy
    fz = fy - b_ch * (1.0 / 200.0)
    eps = 216.0 / 24389.0
    kappa = 24389.0 / 27.0
    fx3 = fx * fx * fx
    fz3 = fz * fz * fz
    x = np.where(fx3 > eps, fx3, (116.0 * fx - 16.0) * (1.0 / kappa)) * 0.95047
    y = np.where(L > kappa * eps, fy * fy * fy, L * (1.0 / kappa))
    z = np.where(fz3 > eps, fz3, (116.0 * fz - 16.0) * (1.0 / kappa)) * 1.08883
    r_lin = x * 3.2404542 - y * 1.5371385 - z * 0.4985314
    g_lin = -x * 0.9692660 + y * 1.8760108 + z * 0.0415560
    b_lin = x * 0.0556434 - y * 0.2040259 + z * 1.0572252
    lin = np.clip(np.stack([r_lin, g_lin, b_lin], axis=-1), 0.0, 1.0)
    srgb = np.where(lin <= 0.0031308, lin * 12.92, 1.055 * np.power(lin, 1.0 / 2.4) - 0.055)
    return np.clip(np.round(srgb * 255.0), 0, 255).astype(np.uint8)


def canonical_lab_target(ref_lab: np.ndarray) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Universal 1x-anchored canonical HD Lab target `T(ref_1x)`.

    Smoothly rolls off the +2.2 L* lift below L*=10 so pure #000000 doorways/cave portals stay pitch black,
    while dark river water (#1E2428, L*~14), grass, dirt, stone, and wood across EVERY scene share one
    exact brightness, contrast, and hue curve.
    """
    L_r = ref_lab[:, :, 0]
    shadow_ramp = np.clip((L_r - 2.0) * (1.0 / 8.0), 0.0, 1.0)
    L_t = np.clip(L_r * 1.04 + 2.2 * shadow_ramp, 0.0, 96.0)
    a_t = ref_lab[:, :, 1] * 0.98
    b_t = ref_lab[:, :, 2] * 0.98
    return L_t.astype(np.float32), a_t.astype(np.float32), b_t.astype(np.float32)


def _masked_box_mean(img: np.ndarray, w: np.ndarray, size: int) -> np.ndarray:
    num = uniform_filter(img * w, size=size, mode="nearest")
    den = np.maximum(uniform_filter(w, size=size, mode="nearest"), 1e-6)
    return num / den


def guided_filter_masked(
    guide: np.ndarray,
    src: np.ndarray,
    mask: np.ndarray,
    radius: int = 14,
    eps: float = 18.0,
) -> np.ndarray:
    """Edge-preserving masked guided filter (He et al.) operating in float32."""
    w = mask.astype(np.float32)
    sz = radius * 2 + 1
    mean_I = _masked_box_mean(guide, w, sz)
    mean_p = _masked_box_mean(src, w, sz)
    mean_Ip = _masked_box_mean(guide * src, w, sz)
    cov_Ip = mean_Ip - mean_I * mean_p
    mean_II = _masked_box_mean(guide * guide, w, sz)
    var_I = np.maximum(mean_II - mean_I * mean_I, 0.0)
    a = cov_Ip / (var_I + eps)
    b = mean_p - a * mean_I
    mean_a = _masked_box_mean(a, w, sz)
    mean_b = _masked_box_mean(b, w, sz)
    return (mean_a * guide + mean_b).astype(np.float32)


def compute_canonical_calibration_fields_1x(
    hd_1x: np.ndarray,
    ref_1x: np.ndarray,
    valid_1x: np.ndarray,
) -> dict | None:
    """Compute canonical global contrast/mean alignment + edge-preserving guided local Lab delta fields at 1x."""
    v1 = valid_1x & (ref_1x.max(axis=2) > 4)
    if int(v1.sum()) < 200:
        return None

    ref_lab = rgb_to_lab_f32(ref_1x)
    hd_lab = rgb_to_lab_f32(hd_1x)
    L_t, a_t, b_t = canonical_lab_target(ref_lab)

    # Step A1: Global luminance contrast standardization toward 1.08 * ref_L_std
    ref_std = float(ref_lab[:, :, 0][v1].std())
    hd_std = float(hd_lab[:, :, 0][v1].std())
    if hd_std > 2.0 and ref_std > 2.0:
        gain_L = float(np.clip(((ref_std * 1.08) / hd_std) ** 0.75, 0.78, 1.25))
    else:
        gain_L = 1.0
    hd_L_mean = float(hd_lab[:, :, 0][v1].mean())
    L_hd_c = (hd_lab[:, :, 0] - hd_L_mean) * gain_L + hd_L_mean

    # Step A2: Exact 100% global mean Lab alignment before local residual pass
    g_dL = float(np.clip(float(L_t[v1].mean()) - float(L_hd_c[v1].mean()), -24.0, 24.0))
    g_da = float(np.clip(float(a_t[v1].mean()) - float(hd_lab[:, :, 1][v1].mean()), -18.0, 18.0))
    g_db = float(np.clip(float(b_t[v1].mean()) - float(hd_lab[:, :, 2][v1].mean()), -18.0, 18.0))

    L_hd_g = np.clip(L_hd_c + g_dL, 0.0, 99.0)
    a_hd_g = hd_lab[:, :, 1] + g_da
    b_hd_g = hd_lab[:, :, 2] + g_db

    # Step B: Joint-guided edge-preserving local residual Lab calibration (respects shorelines & wall edges!)
    rL = np.clip(L_t - L_hd_g, -20.0, 20.0)
    ra = np.clip(a_t - a_hd_g, -18.0, 18.0)
    rb = np.clip(b_t - b_hd_g, -18.0, 18.0)

    loc_dL = guided_filter_masked(ref_lab[:, :, 0], rL, v1, radius=14, eps=18.0)
    guide_hue = (ref_lab[:, :, 0] * 0.5 + ref_lab[:, :, 1] * 0.8 + ref_lab[:, :, 2] * 0.8).astype(np.float32)
    loc_da = guided_filter_masked(guide_hue, ra, v1, radius=14, eps=24.0)
    loc_db = guided_filter_masked(guide_hue, rb, v1, radius=14, eps=24.0)

    total_dL_1x = (g_dL + 0.85 * loc_dL).astype(np.float32)
    total_da_1x = (g_da + 0.85 * loc_da).astype(np.float32)
    total_db_1x = (g_db + 0.85 * loc_db).astype(np.float32)

    # Protect pure #000000 doorways/cave portals from positive brightness lift
    shadow_lift_gate = np.clip((ref_lab[:, :, 0] - 1.5) * (1.0 / 6.0), 0.0, 1.0).astype(np.float32)
    total_dL_1x = np.where(total_dL_1x > 0.0, total_dL_1x * shadow_lift_gate, total_dL_1x).astype(np.float32)

    # Texture mask & smooth water-safe detail gate for Step C
    gx1 = np.abs(np.diff(ref_lab[:, :, 0], axis=1, prepend=ref_lab[:, :1, 0]))
    gy1 = np.abs(np.diff(ref_lab[:, :, 0], axis=0, prepend=ref_lab[:1, :, 0]))
    textured_1x = v1 & ((gx1 + gy1) > 3.5)
    tex_gate_1x = np.clip(gaussian_filter(textured_1x.astype(np.float32), sigma=2.0) * 1.2, 0.2, 1.0).astype(np.float32)

    return {
        "gain_L": gain_L,
        "hd_L_mean": hd_L_mean,
        "g_dL": g_dL,
        "g_da": g_da,
        "g_db": g_db,
        "dL_1x": total_dL_1x,
        "da_1x": total_da_1x,
        "db_1x": total_db_1x,
        "textured_1x": textured_1x,
        "tex_gate_1x": tex_gate_1x,
        "v1": v1,
    }


def _apply_lab_fields_to_rgb_4x(
    rgb_4x: np.ndarray,
    gain_L: float,
    hd_L_mean: float,
    dL_1x_crop: np.ndarray,
    da_1x_crop: np.ndarray,
    db_1x_crop: np.ndarray,
) -> np.ndarray:
    """Apply Step A+B Lab calibration to a 4x RGB tile, returning calibrated float32 Lab `(h4, w4, 3)`."""
    h4, w4 = rgb_4x.shape[:2]
    lab4 = rgb_to_lab_f32(rgb_4x)
    dL_4x = np.array(Image.fromarray(dL_1x_crop.astype(np.float32)).resize((w4, h4), Image.Resampling.BILINEAR))
    da_4x = np.array(Image.fromarray(da_1x_crop.astype(np.float32)).resize((w4, h4), Image.Resampling.BILINEAR))
    db_4x = np.array(Image.fromarray(db_1x_crop.astype(np.float32)).resize((w4, h4), Image.Resampling.BILINEAR))
    lab4[:, :, 0] = np.clip((lab4[:, :, 0] - hd_L_mean) * gain_L + hd_L_mean + dL_4x, 0.0, 99.0)
    lab4[:, :, 1] = np.clip(lab4[:, :, 1] + da_4x, -110.0, 110.0)
    lab4[:, :, 2] = np.clip(lab4[:, :, 2] + db_4x, -110.0, 110.0)
    return lab4


def _apply_detail_norm_to_lab_4x(
    lab4: np.ndarray,
    k_detail: float,
    tex_gate_1x_crop: np.ndarray,
) -> np.ndarray:
    """Apply Step C adaptive multi-scale texture/brushstroke detail normalization on `L_4x`."""
    if abs(k_detail - 1.0) < 0.015:
        return lab4
    h4, w4 = lab4.shape[:2]
    L4 = lab4[:, :, 0]
    L_smooth = gaussian_filter(L4, sigma=1.6)
    L_detail = L4 - L_smooth
    gate_4x = np.array(Image.fromarray(tex_gate_1x_crop.astype(np.float32)).resize((w4, h4), Image.Resampling.BILINEAR))
    eff_k = 1.0 + (k_detail - 1.0) * gate_4x
    lab4[:, :, 0] = np.clip(L_smooth + L_detail * eff_k, 0.0, 99.0)
    return lab4


def calibrate_hd_island_to_canonical_1x(
    isl_rgb_4x: np.ndarray,
    isl_alpha_4x: np.ndarray,
    ref_1x: np.ndarray,
    valid_1x: np.ndarray,
) -> tuple[np.ndarray, dict]:
    """Strip-tiled (low-RAM) canonical 1x-anchored Lab calibration + adaptive texture detail normalization."""
    h1, w1 = ref_1x.shape[:2]
    hd_1x = isl_rgb_4x.astype(np.float32).reshape(h1, 4, w1, 4, 3).mean(axis=(1, 3))
    a_1x = isl_alpha_4x.reshape(h1, 4, w1, 4).mean(axis=(1, 3)) > 128
    fields = compute_canonical_calibration_fields_1x(hd_1x, ref_1x, valid_1x & a_1x)
    if fields is None:
        return isl_rgb_4x, {"skipped": True}

    gain_L = fields["gain_L"]
    hd_L_mean = fields["hd_L_mean"]
    dL_1x = fields["dL_1x"]
    da_1x = fields["da_1x"]
    db_1x = fields["db_1x"]
    textured_1x = fields["textured_1x"]
    tex_gate_1x = fields["tex_gate_1x"]

    # Pass 1 (strip-tiled): measure post-Step-A/B 4x Lab L* gradient on textured pixels for adaptive `k_detail`
    STRIP_1X = 256  # 1024 px at 4x -> < 80 MB peak RAM even on 8192x8192 worlds
    grad_sum = 0.0
    grad_cnt = 0
    if int(textured_1x.sum()) > 300:
        for y0 in range(0, h1, STRIP_1X):
            y1 = min(h1, y0 + STRIP_1X)
            tex_sub = textured_1x[y0:y1]
            if not tex_sub.any():
                continue
            rgb_s = isl_rgb_4x[y0 * 4:y1 * 4]
            a_s = isl_alpha_4x[y0 * 4:y1 * 4] > 200
            lab_s = _apply_lab_fields_to_rgb_4x(
                rgb_s, gain_L, hd_L_mean, dL_1x[y0:y1], da_1x[y0:y1], db_1x[y0:y1]
            )
            L4 = lab_s[:, :, 0]
            gx4 = np.abs(np.diff(L4, axis=1, prepend=L4[:, :1]))
            gy4 = np.abs(np.diff(L4, axis=0, prepend=L4[:1, :]))
            m4 = np.repeat(np.repeat(tex_sub, 4, axis=0), 4, axis=1) & a_s
            n_m = int(m4.sum())
            if n_m > 0:
                grad_sum += float((gx4 + gy4)[m4].sum())
                grad_cnt += n_m

    cur_grad = (grad_sum / grad_cnt) if grad_cnt > 0 else CANONICAL_LAB_TARGET_GRAD_4X
    k_detail = float(np.clip((CANONICAL_LAB_TARGET_GRAD_4X / max(cur_grad, 0.8)) ** 0.65, 0.72, 1.55))

    # Pass 2 (strip-tiled with 4px 1x / 16px 4x halo for zero-seam gaussian_filter sigma=1.6): write calibrated RGB
    out_rgb_4x = np.empty_like(isl_rgb_4x)
    HALO_1X = 4
    for y0 in range(0, h1, STRIP_1X):
        y1 = min(h1, y0 + STRIP_1X)
        hy0 = max(0, y0 - HALO_1X)
        hy1 = min(h1, y1 + HALO_1X)
        rgb_s = isl_rgb_4x[hy0 * 4:hy1 * 4]
        lab_s = _apply_lab_fields_to_rgb_4x(
            rgb_s, gain_L, hd_L_mean, dL_1x[hy0:hy1], da_1x[hy0:hy1], db_1x[hy0:hy1]
        )
        lab_s = _apply_detail_norm_to_lab_4x(lab_s, k_detail, tex_gate_1x[hy0:hy1])
        pad_top = (y0 - hy0) * 4
        pad_end = pad_top + (y1 - y0) * 4
        out_rgb_4x[y0 * 4:y1 * 4] = lab_to_rgb_u8(lab_s[pad_top:pad_end])

    return out_rgb_4x, {
        "gain_L": round(gain_L, 3),
        "g_dL": round(fields["g_dL"], 2),
        "g_da": round(fields["g_da"], 2),
        "g_db": round(fields["g_db"], 2),
        "cur_grad_L4": round(cur_grad, 2),
        "k_detail": round(k_detail, 3),
    }


def _load_map_hd_alpha_1x(map_id: int) -> np.ndarray | None:
    mdir = GFX / "maps_hd" / f"map_{map_id:03d}"
    mp = mdir / "meta.json"
    if not mp.exists():
        return None
    meta = json.loads(mp.read_text())
    a1 = np.zeros((2048, 2048), dtype=bool)
    for r in meta.get("rooms", []):
        fp = mdir / r["file"]
        if not fp.exists():
            continue
        im = np.array(Image.open(fp).convert("RGBA"))
        rx, ry, rw, rh = int(r["x"]), int(r["y"]), int(r["w"]), int(r["h"])
        sc = max(1, int(meta.get("scale", 4)))  # chunks are stored at `scale` px per 1x px (4 or 2)
        a_sub = im[: rh * sc, : rw * sc, 3].reshape(rh, sc, rw, sc).mean(axis=(1, 3)) > 128
        a1[ry:ry + rh, rx:rx + rw] |= a_sub
    return a1


def _load_map_hd_crop_4x(map_id: int, bbox_1x: tuple[int, int, int, int]) -> tuple[np.ndarray, np.ndarray]:
    """Composite `map_id`'s HD chunks into `(ih*4, iw*4, 3)` RGB and `(ih*4, iw*4)` alpha for `bbox_1x`."""
    ix0, iy0, iw, ih = bbox_1x
    mdir = GFX / "maps_hd" / f"map_{map_id:03d}"
    meta = json.loads((mdir / "meta.json").read_text())
    sc = max(1, int(meta.get("scale", 4)))  # chunks are stored at `scale` px per 1x px (4 or 2)
    canvas = Image.new("RGBA", (iw * 4, ih * 4), (0, 0, 0, 0))
    for r in meta.get("rooms", []):
        rx, ry, rw, rh = int(r["x"]), int(r["y"]), int(r["w"]), int(r["h"])
        ox0, oy0 = max(ix0, rx), max(iy0, ry)
        ox1, oy1 = min(ix0 + iw, rx + rw), min(iy0 + ih, ry + rh)
        if ox1 <= ox0 or oy1 <= oy0:
            continue
        im = Image.open(mdir / r["file"]).convert("RGBA")
        crop = im.crop(((ox0 - rx) * sc, (oy0 - ry) * sc, (ox1 - rx) * sc, (oy1 - ry) * sc))
        if sc != 4:
            crop = crop.resize(((ox1 - ox0) * 4, (oy1 - oy0) * 4), Image.Resampling.LANCZOS)
        canvas.alpha_composite(crop, ((ox0 - ix0) * 4, (oy0 - iy0) * 4))
    arr = np.array(canvas)
    return arr[:, :, :3], arr[:, :, 3]


def find_story_variant_donor(
    map_id: int,
    full_1x: np.ndarray,
    tile_occ_1x: np.ndarray,
    vis_1x: np.ndarray,
    candidate_map_ids: list[int] | None = None,
    min_same: float = 0.68,
    min_cov: float = 0.50,
) -> tuple[int | None, float, float]:
    """Detect an already-baked HD donor map sharing >= `min_same` identical 1x pixels and >= `min_cov` coverage.

    Returns `(donor_map_id, cov_frac, same_frac)` or `(None, 0.0, 0.0)`.
    """
    if candidate_map_ids is None:
        maps_root = GFX / "maps_hd"
        if not maps_root.exists():
            return None, 0.0, 0.0
        candidate_map_ids = sorted(
            int(d.name.split("_")[1])
            for d in maps_root.glob("map_*")
            if d.is_dir() and (d / "meta.json").exists() and int(d.name.split("_")[1]) != map_id
        )

    r_sub = full_1x[::4, ::4, :3]
    o_sub = tile_occ_1x[::4, ::4]
    vis_area = max(1, int(vis_1x.sum()))

    best = None  # tuple(score_key, other_id, cov_frac, same_frac)
    for other_id in candidate_map_ids:
        if other_id == map_id:
            continue
        if not (MAPS_BIN / f"map_{other_id:03d}.bin").exists():
            continue
        o_full, _, o_occ = render_map_1x_and_sort_y(other_id)
        # Fast 4x-strided pre-screen on `tile_occ_1x`
        inter_s = o_sub & o_occ[::4, ::4]
        union_s = o_sub | o_occ[::4, ::4]
        if float(inter_s.sum()) / max(1.0, float(union_s.sum())) < 0.65:
            continue
        same_s = float(((r_sub == o_full[::4, ::4, :3]).all(axis=2) & inter_s).sum()) / max(1.0, float(inter_s.sum()))
        if same_s < 0.60:
            continue

        # Exact 1x check against donor's baked HD alpha coverage
        donor_a1 = _load_map_hd_alpha_1x(other_id)
        if donor_a1 is None:
            continue
        ov = vis_1x & donor_a1
        ov_cnt = int(ov.sum())
        cov_frac = ov_cnt / vis_area
        if cov_frac < min_cov:
            continue
        same_cnt = int(((full_1x[:, :, :3] == o_full[:, :, :3]).all(axis=2) & ov).sum())
        same_frac = same_cnt / max(1, ov_cnt)
        if same_frac < min_same:
            continue

        donor_area = int(donor_a1.sum())
        score = (cov_frac >= 0.98, round(cov_frac, 4), round(same_frac, 4), donor_area, -other_id)
        if best is None or score > best[0]:
            best = (score, other_id, cov_frac, same_frac)

    if best is None:
        return None, 0.0, 0.0
    return best[1], best[2], best[3]


def apply_story_variant_inheritance_4x(
    rgb_4x: np.ndarray,
    bbox_1x: tuple[int, int, int, int],
    ref_1x_crop: np.ndarray,
    donor_id: int,
    donor_1x_full: np.ndarray | None = None,
) -> tuple[np.ndarray, float]:
    """Inherit calibrated 4x RGB from `donor_id` on identical 1x pixels with smooth 20 HD px feather at diffs."""
    ix0, iy0, iw, ih = bbox_1x
    if donor_1x_full is None:
        donor_1x_full, _, _ = render_map_1x_and_sort_y(donor_id)
    donor_ref_crop = donor_1x_full[iy0:iy0 + ih, ix0:ix0 + iw, :3]
    donor_rgb_4x, donor_a_4x = _load_map_hd_crop_4x(donor_id, bbox_1x)
    donor_a_1x = donor_a_4x.reshape(ih, 4, iw, 4).min(axis=(1, 3)) > 220

    unchanged_1x = (ref_1x_crop == donor_ref_crop).all(axis=2) & donor_a_1x
    if not unchanged_1x.any():
        return rgb_4x, 0.0

    if unchanged_1x.all():
        return donor_rgb_4x.copy(), 1.0

    # Erode slightly away from donor boundary & changed tiles, then feather smoothly over ~5 logical (20 HD) px
    dist_1x = distance_transform_edt(unchanged_1x).astype(np.float32)
    w_donor_1x = np.clip((dist_1x - 1.5) * (1.0 / 5.0), 0.0, 1.0).astype(np.float32)
    w_donor_4x = np.array(
        Image.fromarray(w_donor_1x).resize((iw * 4, ih * 4), Image.Resampling.BILINEAR),
        dtype=np.float32,
    )
    blended = (
        donor_rgb_4x.astype(np.float32) * w_donor_4x[:, :, None]
        + rgb_4x.astype(np.float32) * (1.0 - w_donor_4x[:, :, None])
    )
    return np.clip(np.round(blended), 0, 255).astype(np.uint8), float(w_donor_1x.mean())


def export_neighbor_style_anchor(
    map_id: int,
    work_dir: Path,
    scenes,
    events,
    all_scripts,
) -> Path | None:
    """Find an already-baked connected neighbor map (via `scene_change`) and save a 512x320 style anchor swatch."""
    scene_by_id = {s["id"]: s for s in scenes}
    ev_by_id = {e["id"]: e for e in events}
    my_sids = {s["id"] for s in scenes if s["map_id"] == map_id}
    neighbor_maps: list[int] = []

    def collect_targets(lbl: str) -> set[int]:
        out: set[int] = set()
        if not lbl or lbl not in all_scripts:
            return out
        for sl in trace_reachable_scripts([lbl], all_scripts, max_hops=3):
            for st in all_scripts.get(sl, []):
                if st.get("op") == "scene_change":
                    tsid = st.get("scene_id") or (st.get("args") or [0])[0]
                    if tsid in scene_by_id:
                        out.add(int(scene_by_id[tsid]["map_id"]))
        return out

    # Outgoing transitions from this map's scenes
    for s in scenes:
        if s["id"] not in my_sids:
            continue
        for eid in range(s["event_start"], s["event_end"] + 1):
            e = ev_by_id.get(eid)
            if not e:
                continue
            for tm in collect_targets(e.get("script_trigger") or ""):
                if tm != map_id and tm > 0 and is_map_done(tm) and tm not in neighbor_maps:
                    neighbor_maps.append(tm)
    # Incoming transitions into this map's scenes
    for s in scenes:
        sm = int(s["map_id"])
        if sm == map_id or sm <= 0 or not is_map_done(sm) or sm in neighbor_maps:
            continue
        for eid in range(s["event_start"], s["event_end"] + 1):
            e = ev_by_id.get(eid)
            if not e:
                continue
            if map_id in collect_targets(e.get("script_trigger") or ""):
                neighbor_maps.append(sm)
                break

    if not neighbor_maps:
        return None
    anchor_mid = neighbor_maps[0]
    vis_ht, _, _ = compute_reachable_walkable_and_events(anchor_mid, scenes, events, all_scripts)
    if vis_ht:
        pts = [(tx * 32 + h * 16, ty * 16 + h * 8) for ty, tx, h in vis_ht]
        mx, my = int(np.median([q[0] for q in pts])), int(np.median([q[1] for q in pts]))
        cx, cy = min(pts, key=lambda q: (q[0] - mx) ** 2 + (q[1] - my) ** 2)
    else:
        _, _, a_occ = render_map_1x_and_sort_y(anchor_mid)
        ys, xs = np.where(a_occ)
        cx, cy = (int(np.median(xs)), int(np.median(ys))) if len(xs) else (1024, 1024)
    w1, h1 = 320, 200
    x0 = max(0, min(2048 - w1, cx - w1 // 2))
    y0 = max(0, min(2048 - h1, cy - h1 // 2))
    rgb4, a4 = _load_map_hd_crop_4x(anchor_mid, (x0, y0, w1, h1))
    vis4 = np.where(a4[:, :, None] > 128, rgb4, 0)
    out_p = work_dir / f"style_anchor_map{anchor_mid:03d}.png"
    Image.fromarray(vis4).resize((512, 320), Image.Resampling.LANCZOS).save(out_p)
    print(f"  [Style Anchor] Exported connected neighbor Map {anchor_mid:03d} swatch -> {out_p.name}")
    return out_p


PATCH_OVERLAP_X_1X, PATCH_OVERLAP_Y_1X = 128, 112  # min overlap between neighbouring 688x384 patches
TWO_BAND_SEAM_BLEND = True  # frequency-split seam blend in `_stitch_island_patches_4x` (False = legacy linear)


def describe_chained_edges(edges: list[str]) -> str:
    """Prompt clause for a chained patch whose `edges` (subset of top/bottom/left/right) carry final HD pixels."""
    if not edges:
        return ""
    where = ", ".join(edges[:-1]) + (" and " if len(edges) > 1 else "") + edges[-1]
    return (
        f" CHAINED TILE: the {where} edge region(s) of this input are ALREADY-PAINTED final HD artwork from the "
        f"neighbouring tile (smooth painted pixels, NOT blocky pixel art). Keep those already-painted pixels exactly "
        f"as they are, and continue their brushwork, texture scale, grass/foliage direction, water ripple density, "
        f"lighting, and color seamlessly into the rest of the image so the two tiles join with NO visible boundary."
    )


def build_map_patch_prompt(scene_name: str, chain_note: str = "") -> str:
    return (
        f"Faithful 2D HD hand-painted Chinese classic RPG background remaster of this exact isometric scene "
        f"({scene_name} from 1995 Chinese Paladin / Xianjian Qixiazhuan). "
        f"CRITICAL GEOMETRY & PROP LOCK: Keep every single wall, doorway, floor boundary, furniture piece, staircase, "
        f"cliff, path, tree trunk, water shoreline, and black exterior background in the EXACT same position, "
        f"scale, and isometric angle — zero layout changes, zero added characters, zero camera shift. "
        f"TILE-ARTIFACT RULE: jagged diamond-shaped staircase steps along a boundary between two ground materials "
        f"(grass/water, grass/dirt, sand/water) are 1995 tile artifacts, not real geometry: paint that boundary as a "
        f"natural, irregular organic edge that follows the same overall line (never move it more than a few pixels, "
        f"never add new land or water). "
        f"STRICT ANTI-HALLUCINATION RULE: NEVER invent or add extra props (no added steamers, food baskets, buns, "
        f"jars, bowls, teapots, vases, lanterns, candles, signs, or plants) onto bare counters, bare tables, bare "
        f"floors, grass, or dark doorways! NEVER invent shorelines, cliffs, stairs, piers, or rocks over flat open "
        f"water! Bare counters/floors must remain bare, open water must remain open water, and dark doorways must "
        f"remain dark, remastering ONLY the exact objects visible in the input reference. "
        f"STRICT EXPOSURE, COLOR & TEXTURE CONSISTENCY RULE: Match the EXACT brightness, contrast, hue, and color "
        f"temperature of the 1995 input reference — NEVER brighten, wash out, overexpose, warm-tint, or darken the "
        f"scene. Keep dark slate-grey river/pond/lake water (#1E2428) calm, dark, and natural (NEVER glowing cyan, "
        f"bright turquoise, or snowy white ripples). Use a consistent medium-fine hand-painted traditional Chinese "
        f"watercolor/gouache brushstroke texture matching all other maps in this remake (neither blurry wash nor "
        f"over-sharpened grainy noise). "
        f"EVEN-TEXTURE RULE: this image is one tile of a larger seamless map, so paint large uniform areas (lawns, "
        f"open water, plain floors, roofs) with EVEN texture density, scale, and direction across the WHOLE image — "
        f"no vignettes, no lighting gradients toward the corners, no bright spots, and no clusters of ripples or "
        f"flowers in one area only.{chain_note} "
        f"Replace blocky pixelated dithering with crisp, ultra-clean high-definition hand-painted traditional "
        f"Chinese game art: fine natural wood grain, crisp carved lattice woodwork, clean stone tiles, lush foliage, "
        f"and realistic woven textiles. "
        f"Pure black (#000000) background outside the map geometry must stay pure solid black (#000000). Output 1376x768."
    )


def prepare_map_patches(
    map_id: int, scene_name: str, work_dir: Path, scenes, events, all_scripts, chain: bool = False
) -> list[dict]:
    full_1x, sort_y_1x, tile_occ_1x = render_map_1x_and_sort_y(map_id)
    vis_1x = compute_camera_visible_mask_1x(map_id, tile_occ_1x, scenes, events, all_scripts)

    np.save(work_dir / f"map_{map_id:03d}_sort_y_1x.npy", sort_y_1x)
    np.save(work_dir / f"map_{map_id:03d}_vis_1x.npy", vis_1x)
    np.save(work_dir / f"map_{map_id:03d}_occ_1x.npy", tile_occ_1x)
    Image.fromarray(full_1x).save(work_dir / f"map_{map_id:03d}_full_1x.png")

    # Pillar 2: Check if an already-baked story-variant donor covers this map (`>=85%` identical 1x pixels)
    donor_id, donor_cov, donor_same = find_story_variant_donor(map_id, full_1x, tile_occ_1x, vis_1x)
    donor_diff_mask: np.ndarray | None = None
    donor_meta_path = work_dir / f"map_{map_id:03d}_donor.json"
    if donor_meta_path.exists():
        donor_meta_path.unlink()
    if donor_id is not None:
        donor_1x, _, _ = render_map_1x_and_sort_y(donor_id)
        donor_a1 = _load_map_hd_alpha_1x(donor_id)
        unchanged_1x = (full_1x[:, :, :3] == donor_1x[:, :, :3]).all(axis=2) & donor_a1
        diff_1x = vis_1x & (~unchanged_1x)
        donor_diff_mask = binary_dilation(diff_1x, iterations=24) if diff_1x.any() else np.zeros((2048, 2048), dtype=bool)
        donor_meta_path.write_text(json.dumps({
            "donor_map_id": donor_id,
            "cov_frac": round(donor_cov, 4),
            "same_frac": round(donor_same, 4),
            "diff_px_1x": int(diff_1x.sum()),
        }, indent=2))
        print(
            f"  [Story-Variant Donor] Map {map_id:03d} inherits from Map {donor_id:03d} "
            f"(cov={donor_cov:.3f}, same={donor_same:.3f}, diff_px={int(diff_1x.sum())})"
        )

    # Group visible map geometry into connected islands (dilated by 8px to bridge 1-tile gaps)
    lbl, n_isl = label(binary_dilation(vis_1x, iterations=8))
    islands_meta = []
    todo_items = []
    W, H = 2048, 2048

    raw_islands = []
    for cid in range(1, n_isl + 1):
        isl_mask = (lbl == cid) & vis_1x
        area = int(isl_mask.sum())
        if area < 48:
            continue
        ys, xs = np.where(isl_mask)
        ix0 = (max(0, int(xs.min())) // 4) * 4
        iy0 = (max(0, int(ys.min())) // 4) * 4
        ix1 = min(W, ((int(xs.max()) + 4) // 4) * 4)
        iy1 = min(H, ((int(ys.max()) + 4) // 4) * 4)
        raw_islands.append({"cids": [int(cid)], "bbox": [ix0, iy0, ix1, iy1], "area": area})

    # Absorb small satellite islands (e.g. rocks/lilies in water) enclosed inside a larger island's bbox
    raw_islands.sort(key=lambda d: d["area"], reverse=True)
    merged_islands = []
    for cand in raw_islands:
        cx0, cy0, cx1, cy1 = cand["bbox"]
        absorbed = False
        for parent in merged_islands:
            px0, py0, px1, py1 = parent["bbox"]
            if cx0 >= px0 and cy0 >= py0 and cx1 <= px1 and cy1 <= py1:
                parent["cids"].extend(cand["cids"])
                absorbed = True
                break
        if not absorbed and cand["area"] >= 80:
            merged_islands.append(cand)

    prompt = build_map_patch_prompt(scene_name)
    for isl_idx, isl_entry in enumerate(merged_islands, start=1):
        cids = isl_entry["cids"]
        ix0, iy0, ix1, cy1 = isl_entry["bbox"]
        iy1 = cy1
        iw, ih = ix1 - ix0, iy1 - iy0
        isl_mask = np.isin(lbl, cids) & vis_1x

        # Build patch grid of exact 688x384 boxes covering [ix0..ix1, iy0..iy1]
        if iw <= BOX_W:
            cx = (ix0 + ix1) // 2
            xs_starts = [max(0, min(W - BOX_W, ((cx - BOX_W // 2) // 4) * 4))]
        else:
            nx = max(2, math.ceil((iw - PATCH_OVERLAP_X_1X) / (BOX_W - PATCH_OVERLAP_X_1X)))
            xs_starts = sorted({max(0, min(W - BOX_W, (round(ix0 + k * (iw - BOX_W) / (nx - 1)) // 4) * 4)) for k in range(nx)})

        if ih <= BOX_H:
            cy = (iy0 + iy1) // 2
            ys_starts = [max(0, min(H - BOX_H, ((cy - BOX_H // 2) // 4) * 4))]
        else:
            ny = max(2, math.ceil((ih - PATCH_OVERLAP_Y_1X) / (BOX_H - PATCH_OVERLAP_Y_1X)))
            ys_starts = sorted({max(0, min(H - BOX_H, (round(iy0 + k * (ih - BOX_H) / (ny - 1)) // 4) * 4)) for k in range(ny)})

        patches = []
        p_idx = 0
        # Zero-hole island mask + slight 2px context dilation inside `tile_occ_1x`
        isl_ctx = binary_fill_holes(isl_mask) | (binary_dilation(isl_mask, iterations=2) & tile_occ_1x)
        for by0 in ys_starts:
            for bx0 in xs_starts:
                sub_m = isl_ctx[by0:by0 + BOX_H, bx0:bx0 + BOX_W]
                if int(sub_m.sum()) < 80:
                    continue
                # When a story-variant donor covers this box 100% with zero changed pixels, skip redundant AI patch!
                if donor_diff_mask is not None and not donor_diff_mask[by0:by0 + BOX_H, bx0:bx0 + BOX_W].any():
                    continue
                p_idx += 1
                pkey = f"map{map_id:03d}_isl{isl_idx}_p{p_idx:02d}"
                crop = full_1x[by0:by0 + BOX_H, bx0:bx0 + BOX_W].copy()
                crop[~sub_m, :3] = 0
                crop[:, :, 3] = 255
                in_path = work_dir / f"{pkey}_input_1376x768.png"
                Image.fromarray(crop[:, :, :3]).resize((1376, 768), Image.Resampling.NEAREST).save(in_path)

                patches.append({
                    "key": pkey,
                    "box_1x": [bx0, by0, BOX_W, BOX_H],
                    "input_path": str(in_path),
                })
                item = {
                    "key": pkey,
                    "kind": "map_patch",
                    "input_path": str(in_path),
                    "prompt": prompt,
                }
                todo_items.append(item)

        islands_meta.append({
            "island_id": isl_idx,
            "cids": cids,
            "bbox_1x": [ix0, iy0, iw, ih],
            "patches": patches,
            "story_variant_donor": donor_id,
        })
        print(f"  [Map {map_id:03d}] Island {isl_idx}: bbox=({ix0},{iy0},{iw}x{ih}) -> {len(patches)} patch(es)")

    (work_dir / f"map_{map_id:03d}_islands.json").write_text(json.dumps(islands_meta, indent=2))
    chain_p = work_dir / f"map_{map_id:03d}_chain.json"
    if chain:
        # Chained generation: only patches with no earlier overlapping patch are ready now; `chain-next` builds the
        # rest one at a time with their already-painted neighbour pixels pasted into the input image.
        ready = {p["key"] for isl in islands_meta for p in _chain_ready_patches(isl["patches"], set())}
        todo_items = [it for it in todo_items if it["key"] in ready]
        chain_p.write_text(json.dumps({"map_id": map_id, "scene_name": scene_name}, indent=2, ensure_ascii=False))
        print(f"  [Chain] {len(todo_items)} first patch(es) ready; run `chain-next` after each generation round")
    elif chain_p.exists():
        chain_p.unlink()
    return todo_items


def _boxes_overlap(a: list[int], b: list[int]) -> bool:
    ax, ay, aw, ah = a
    bx, by, bw, bh = b
    return ax < bx + bw and bx < ax + aw and ay < by + bh and by < ay + ah


def _chain_ready_patches(patches: list[dict], done: set[str]) -> list[dict]:
    """Patches not yet generated whose every EARLIER (bake-order) overlapping patch already has an output."""
    ready = []
    for k, p in enumerate(patches):
        if p["key"] in done:
            continue
        deps = [q for q in patches[:k] if _boxes_overlap(q["box_1x"], p["box_1x"])]
        if all(q["key"] in done for q in deps):
            ready.append(p)
    return ready


def build_chained_patch_input(
    pinfo: dict,
    island_bbox_1x: tuple[int, int, int, int],
    full_1x: np.ndarray,
    isl_ctx_full: np.ndarray,
    acc_rgb_4x: np.ndarray,
    has_pix_1x: np.ndarray,
) -> tuple[np.ndarray, list[str]]:
    """1376x768 input for `pinfo`: the usual nearest-2x 1x reference, with every pixel already covered by earlier
    stitched patches replaced by those final HD pixels (box-downsampled 4x -> 2x). Returns `(rgb, edges)`."""
    ix0, iy0, iw, ih = island_bbox_1x
    bx0, by0, bw, bh = pinfo["box_1x"]
    crop = full_1x[by0:by0 + bh, bx0:bx0 + bw, :3].copy()
    crop[~isl_ctx_full[by0:by0 + bh, bx0:bx0 + bw]] = 0
    out = np.array(Image.fromarray(crop).resize((bw * 2, bh * 2), Image.Resampling.NEAREST))
    painted_1x = np.zeros((bh, bw), dtype=bool)
    ox0, oy0 = max(ix0, bx0), max(iy0, by0)
    ox1, oy1 = min(ix0 + iw, bx0 + bw), min(iy0 + ih, by0 + bh)
    if ox1 > ox0 and oy1 > oy0:
        painted_1x[oy0 - by0:oy1 - by0, ox0 - bx0:ox1 - bx0] = has_pix_1x[oy0 - iy0:oy1 - iy0, ox0 - ix0:ox1 - ix0]
        hd = acc_rgb_4x[(oy0 - iy0) * 4:(oy1 - iy0) * 4, (ox0 - ix0) * 4:(ox1 - ix0) * 4]
        hd_2x = np.array(
            Image.fromarray(np.clip(np.round(hd), 0, 255).astype(np.uint8)).resize(
                ((ox1 - ox0) * 2, (oy1 - oy0) * 2), Image.Resampling.BOX
            )
        )
        canvas_2x = np.zeros_like(out)
        canvas_2x[(oy0 - by0) * 2:(oy1 - by0) * 2, (ox0 - bx0) * 2:(ox1 - bx0) * 2] = hd_2x
        m2 = np.repeat(np.repeat(painted_1x, 2, axis=0), 2, axis=1)
        out[m2] = canvas_2x[m2]
    if out.shape[:2] != (768, 1376):
        out = np.array(Image.fromarray(out).resize((1376, 768), Image.Resampling.NEAREST))
    # An edge counts as pre-painted only when at least half of a thin band along it is painted, so a left
    # neighbour's overlap (which also touches the top/bottom rows near the corner) is reported as "left" only.
    edges = []
    band_x, band_y = max(4, bw // 12), max(4, bh // 12)
    for name, strip in (
        ("top", painted_1x[:band_y]),
        ("bottom", painted_1x[-band_y:]),
        ("left", painted_1x[:, :band_x]),
        ("right", painted_1x[:, -band_x:]),
    ):
        if strip.size and float(strip.mean()) >= 0.5:
            edges.append(name)
    return out, edges


def _dp_vertical_seam(cost_2d: np.ndarray) -> np.ndarray:
    """Find minimum-cost top-to-bottom path `x_seam[y]` through `cost_2d` `(H, W)` with step `dx in {-2..+2}`."""
    h, w = cost_2d.shape
    if w <= 1:
        return np.zeros(h, dtype=np.int32)
    dp = np.empty((h, w), dtype=np.float32)
    back = np.zeros((h, w), dtype=np.int8)
    dp[0] = cost_2d[0]
    big = np.float32(1e12)
    for y in range(1, h):
        prev = dp[y - 1]
        best = prev.copy()
        bdir = np.zeros(w, dtype=np.int8)

        cand = np.empty_like(prev)
        cand[0] = big
        cand[1:] = prev[:-1] + 2.0
        m = cand < best
        best[m] = cand[m]
        bdir[m] = -1

        cand[-1] = big
        cand[:-1] = prev[1:] + 2.0
        m = cand < best
        best[m] = cand[m]
        bdir[m] = 1

        if w > 2:
            cand[:2] = big
            cand[2:] = prev[:-2] + 5.5
            m = cand < best
            best[m] = cand[m]
            bdir[m] = -2

            cand[-2:] = big
            cand[:-2] = prev[2:] + 5.5
            m = cand < best
            best[m] = cand[m]
            bdir[m] = 2

        dp[y] = cost_2d[y] + best
        back[y] = bdir

    seam = np.empty(h, dtype=np.int32)
    cx = int(np.argmin(dp[-1]))
    seam[-1] = cx
    for y in range(h - 1, 0, -1):
        cx = cx + int(back[y, cx])
        seam[y - 1] = cx
    return seam


def harmonize_patch_overlap_4x(
    prev_4x: np.ndarray,
    sub_patch: np.ndarray,
    prev_has_1x: np.ndarray,
    valid_sub_1x: np.ndarray,
    isl_alpha_sub_4x: np.ndarray,
    ref_1x: np.ndarray | None = None,
) -> np.ndarray:
    """Drift-free overlap harmonization: luminance-locked global match + seam-localized low-freq alignment.

    Using a shared luminance gain + tightly bounded chroma offset (`+-7.5`) + `w_lf / (w_lf + 0.18)` decay into
    `~prev_has_1x` bridges seam lighting cleanly while guaranteeing ZERO multi-row chromatic drift on 28-patch maps.
    On structureless flat water/surfaces (where both patches and `ref_1x` have low high-pass energy), a domain-isolated
    residual pass bridges larger water tone shifts (`> 40`) without bleeding across shoreline banks or inventing drift.
    """
    out = sub_patch.copy()
    h, w = prev_has_1x.shape
    h4, w4 = h * 4, w * 4
    raw_p1_max = prev_4x.reshape(h, 4, w, 4, 3).mean(axis=(1, 3)).max(axis=2)
    raw_c1_max = sub_patch.reshape(h, 4, w, 4, 3).mean(axis=(1, 3)).max(axis=2)
    non_void_4x = (prev_4x.max(axis=2) > 16.0) & (sub_patch.max(axis=2) > 16.0)

    prev_has_4x = np.repeat(np.repeat(prev_has_1x, 4, axis=0), 4, axis=1)
    ov_mask = prev_has_4x & (isl_alpha_sub_4x > 200)
    if int(ov_mask.sum()) >= 400:
        non_void = ov_mask & non_void_4x
        fit_mask = non_void if int(non_void.sum()) >= 400 else ov_mask
        prev_vals = prev_4x[fit_mask]
        cur_vals = out[fit_mask]

        lum_prev = 0.299 * prev_vals[:, 0] + 0.587 * prev_vals[:, 1] + 0.114 * prev_vals[:, 2]
        lum_cur = 0.299 * cur_vals[:, 0] + 0.587 * cur_vals[:, 1] + 0.114 * cur_vals[:, 2]
        s_prev = float(np.percentile(lum_prev, 75) - np.percentile(lum_prev, 25))
        s_cur = float(np.percentile(lum_cur, 75) - np.percentile(lum_cur, 25))
        # Single shared luminance gain (zero R/G/B multiplicative hue twist!) damped 60% toward 1.0
        raw_gain = float(np.clip(s_prev / max(10.0, s_cur), 0.84, 1.18)) if s_cur > 10.0 else 1.0
        gain_lum = 1.0 + 0.60 * (raw_gain - 1.0)

        m_lum_prev = float(np.median(lum_prev))
        m_lum_cur = float(np.median(lum_cur)) * gain_lum
        bias_lum = float(np.clip(m_lum_prev - m_lum_cur, -26.0, 26.0)) * 0.65

        for ch in range(3):
            m_c_prev = float(np.median(prev_vals[:, ch])) - m_lum_prev
            m_c_cur = float(np.median(cur_vals[:, ch])) * gain_lum - m_lum_cur
            # Strictly cap global chromaticity shift to +-7.5 RGB so 6-row chains never drift neon-lime or pink!
            bias_chroma = float(np.clip(m_c_prev - m_c_cur, -7.5, 7.5)) * 0.55
            out[:, :, ch] = np.clip(out[:, :, ch] * gain_lum + bias_lum + bias_chroma, 0.0, 255.0)

    # 1) Seam-localized low-frequency illumination alignment (`sigma=18` 1x) that decays back to natural patch color
    # outside the overlap (`w_lf / (w_lf + 0.18)` -> 0 in `~prev_has_1x`), preventing cumulative multi-hop drift!
    p1 = prev_4x.reshape(h, 4, w, 4, 3).mean(axis=(1, 3))
    c1 = out.reshape(h, 4, w, 4, 3).mean(axis=(1, 3))
    non_void_1x = (raw_p1_max > 16.0) & (raw_c1_max > 16.0)
    agree = prev_has_1x & valid_sub_1x & (np.abs(p1 - c1).max(axis=2) < 40.0) & non_void_1x
    if int(agree.sum()) >= 80:
        w_lf = gaussian_filter(agree.astype(np.float32), sigma=18.0)
        damp = w_lf / (w_lf + 0.18)
        raw_d = np.zeros((h, w, 3), dtype=np.float32)
        for ch in range(3):
            num = gaussian_filter((p1[:, :, ch] - c1[:, :, ch]) * agree, sigma=18.0)
            raw_d[:, :, ch] = num / np.maximum(1e-4, w_lf)
        d_lum = 0.299 * raw_d[:, :, 0] + 0.587 * raw_d[:, :, 1] + 0.114 * raw_d[:, :, 2]
        d_lum_c = np.clip(d_lum, -36.0, 36.0)
        for ch in range(3):
            d_chr = np.clip(raw_d[:, :, ch] - d_lum, -10.0, 10.0)
            d_ch = (d_lum_c + d_chr) * damp
            d_4x = np.array(
                Image.fromarray(d_ch).resize((w4, h4), Image.Resampling.BILINEAR),
                dtype=np.float32,
            )
            out[:, :, ch] = np.clip(out[:, :, ch] + d_4x, 0.0, 255.0)

    # 2) Domain-isolated flat-water / flat-surface residual tone bridge (`|p1 - c2| < 130` where structure is flat)
    c2 = out.reshape(h, 4, w, 4, 3).mean(axis=(1, 3))
    hp_p = gaussian_filter(np.abs(p1 - gaussian_filter(p1, sigma=(3.0, 3.0, 0.0))).max(axis=2), sigma=1.8)
    hp_c = gaussian_filter(np.abs(c2 - gaussian_filter(c2, sigma=(3.0, 3.0, 0.0))).max(axis=2), sigma=1.8)
    if ref_1x is not None:
        rf = ref_1x.astype(np.float32)
        hp_r = gaussian_filter(np.abs(rf - gaussian_filter(rf, sigma=(3.0, 3.0, 0.0))).max(axis=2), sigma=1.8)
        flat_surf = (hp_p < 18.0) & (hp_c < 18.0) & (hp_r < 18.0) & valid_sub_1x
    else:
        flat_surf = (hp_p < 15.0) & (hp_c < 15.0) & valid_sub_1x

    flat_agree = prev_has_1x & flat_surf & non_void_1x & (np.abs(p1 - c2).max(axis=2) < 130.0)
    if int(flat_agree.sum()) >= 48 and float(np.abs(p1[flat_agree] - c2[flat_agree]).max(axis=1).mean()) > 12.0:
        w_fl = gaussian_filter(flat_agree.astype(np.float32), sigma=12.0)
        flat_gate = gaussian_filter(flat_surf.astype(np.float32), sigma=2.2)
        damp_fl = (w_fl / (w_fl + 0.10)) * flat_gate
        raw_fl = np.zeros((h, w, 3), dtype=np.float32)
        for ch in range(3):
            num_fl = gaussian_filter((p1[:, :, ch] - c2[:, :, ch]) * flat_agree, sigma=12.0)
            raw_fl[:, :, ch] = num_fl / np.maximum(1e-4, w_fl)
        fl_lum = 0.299 * raw_fl[:, :, 0] + 0.587 * raw_fl[:, :, 1] + 0.114 * raw_fl[:, :, 2]
        fl_lum_c = np.clip(fl_lum, -72.0, 72.0)
        for ch in range(3):
            fl_chr = np.clip(raw_fl[:, :, ch] - fl_lum, -28.0, 28.0)
            fl_ch = (fl_lum_c + fl_chr) * damp_fl
            fl_4x = np.array(
                Image.fromarray(fl_ch).resize((w4, h4), Image.Resampling.BILINEAR),
                dtype=np.float32,
            )
            out[:, :, ch] = np.clip(out[:, :, ch] + fl_4x, 0.0, 255.0)
    return out


def compute_seam_cut_blend_weight_4x(
    prev_4x: np.ndarray,
    cur_4x: np.ndarray,
    ref_1x: np.ndarray,
    prev_has_1x: np.ndarray,
    valid_sub_1x: np.ndarray,
    must_prev_left: np.ndarray,
    must_prev_top: np.ndarray,
    must_prev_right: np.ndarray,
    must_prev_bot: np.ndarray,
) -> np.ndarray:
    """Compute Reference-Guided Minimum-Error Seam-Cut mask at 1x -> 4x with micro-feather."""
    h, w = prev_has_1x.shape
    h4, w4 = h * 4, w * 4
    if not prev_has_1x.any():
        return np.ones((h4, w4), dtype=np.float32)

    prev_1x = prev_4x.reshape(h, 4, w, 4, 3).mean(axis=(1, 3))
    cur_1x = cur_4x.reshape(h, 4, w, 4, 3).mean(axis=(1, 3))

    # Tone-align 1x reference to overlap midpoint so global HD lighting warmth never biases reference fidelity
    ov_valid = prev_has_1x & valid_sub_1x
    ref_m = ref_1x.astype(np.float32).copy()
    if int(ov_valid.sum()) >= 64:
        mid_1x = 0.5 * (prev_1x + cur_1x)
        for ch in range(3):
            mm = float(np.median(mid_1x[ov_valid, ch]))
            mr = float(np.median(ref_m[ov_valid, ch]))
            sm = float(np.percentile(mid_1x[ov_valid, ch], 75) - np.percentile(mid_1x[ov_valid, ch], 25))
            sr = float(np.percentile(ref_m[ov_valid, ch], 75) - np.percentile(ref_m[ov_valid, ch], 25))
            gain = np.clip(sm / max(8.0, sr), 0.72, 1.40) if sr > 8.0 else 1.0
            bias = np.clip(mm - mr * gain, -44.0, 44.0)
            ref_m[:, :, ch] = np.clip(ref_m[:, :, ch] * gain + bias, 0.0, 255.0)

    diff_pc = np.abs(prev_1x - cur_1x).mean(axis=2)
    err_prev = np.abs(prev_1x - ref_m).mean(axis=2)
    err_cur = np.abs(cur_1x - ref_m).mean(axis=2)

    # Slight spatial edge penalty on 1x reference so seams route around tables/props through flat floor/grass/water
    lum_r = 0.299 * ref_m[:, :, 0] + 0.587 * ref_m[:, :, 1] + 0.114 * ref_m[:, :, 2]
    gx_r = np.abs(np.roll(lum_r, -1, axis=1) - np.roll(lum_r, 1, axis=1))
    gy_r = np.abs(np.roll(lum_r, -1, axis=0) - np.roll(lum_r, 1, axis=0))
    gx_r[:, 0] = gx_r[:, -1] = 0.0
    gy_r[0, :] = gy_r[-1, :] = 0.0
    edge_r = 0.35 * np.hypot(gx_r, gy_r)

    seam_cost = diff_pc + 0.8 * np.minimum(err_prev, err_cur) + edge_r
    neutral_void_cost = float(np.median(seam_cost[ov_valid])) if ov_valid.any() else 12.0
    seam_cost[~valid_sub_1x] = neutral_void_cost

    use_cur_1x = np.ones((h, w), dtype=bool)
    yy = np.arange(h)[:, None]
    xx = np.arange(w)[None, :]
    huge = np.float32(1e7)

    # 1) Left overlap -> vertical seam `x_seam(y)` keeping `prev` for `x < x_seam(y)`
    # Smooth exponential border barrier (never a flat cliff at `x=margin`) + uniform left-band width!
    if must_prev_left.any() and w >= 6:
        pure_l = must_prev_left & (~prev_has_1x[:, -1])
        band_w = (
            float(np.median(prev_has_1x[pure_l].sum(axis=1)))
            if pure_l.any()
            else float(np.median(prev_has_1x[must_prev_left].sum(axis=1)))
        )
        band_w = max(8.0, band_w)
        center_x = np.broadcast_to(
            8.0 * ((xx.astype(np.float32) - 0.5 * band_w) / max(16.0, 0.5 * band_w)) ** 2,
            (h, w),
        )
        border_pen_x = 120.0 * np.exp(-xx.astype(np.float32) / max(4.0, min(14.0, band_w * 0.14)))

        vcost = seam_cost + center_x
        mpl_rows = np.where(must_prev_left)[0]
        vcost[mpl_rows, :] += border_pen_x
        vcost[mpl_rows, :min(3, w - 1)] = huge
        free_rows = np.where(~must_prev_left)[0]
        if len(free_rows) > 0:
            vcost[free_rows, 0] = 0.0
        forbid_x = ~prev_has_1x.copy()
        if len(free_rows) > 0:
            forbid_x[free_rows, 0] = False
        inner_margin_x = (~binary_erosion(prev_has_1x, iterations=3)) & prev_has_1x
        inner_margin_x[:, :5] = False
        vcost[forbid_x] = huge
        vcost[inner_margin_x] += 5e4
        if must_prev_right.any():
            mpr_rows = np.where(must_prev_right)[0]
            vcost[mpr_rows, max(0, w - 3):] = huge
        x_seam = _dp_vertical_seam(vcost)
        use_cur_1x &= xx >= x_seam[:, None]

    # 2) Right overlap -> `x_seam_r(y)` in `[0..w]`, keeping `cur` for `x < x_seam_r(y)`
    if must_prev_right.any() and not must_prev_left.any() and w >= 6:
        rcost = np.empty((h, w + 1), dtype=np.float32)
        target_rx = max(8.0, float(w - 56))
        r_prior = 8.0 * ((np.arange(w, dtype=np.float32)[None, :] - target_rx) / 64.0) ** 2
        border_pen_r = 120.0 * np.exp(-(w - 1 - np.arange(w, dtype=np.float32)) / 10.0)
        rcost[:, :w] = seam_cost + r_prior
        rcost[:, w] = huge
        mpr_rows = np.where(must_prev_right)[0]
        rcost[mpr_rows, :w] += border_pen_r
        rcost[mpr_rows, max(0, w - 3):] = huge
        has_missing_at_or_right = np.cumsum((~prev_has_1x)[:, ::-1], axis=1)[:, ::-1] > 0
        rcost[:, :w][has_missing_at_or_right] = huge
        free_r = np.where(~must_prev_right)[0]
        if len(free_r) > 0:
            rcost[free_r, w] = 0.0
        x_seam_r = _dp_vertical_seam(rcost)
        use_cur_1x &= xx < x_seam_r[:, None]

    # 3) Top overlap -> horizontal seam `y_seam(x)` keeping `prev` for `y < y_seam(x)`
    if must_prev_top.any() and h >= 6:
        pure_t = must_prev_top & (~prev_has_1x[-1, :])
        band_h = (
            float(np.median(prev_has_1x[:, pure_t].sum(axis=0)))
            if pure_t.any()
            else float(np.median(prev_has_1x[:, must_prev_top].sum(axis=0)))
        )
        band_h = max(8.0, band_h)
        center_y = np.broadcast_to(
            8.0 * ((yy.astype(np.float32) - 0.5 * band_h) / max(16.0, 0.5 * band_h)) ** 2,
            (h, w),
        )
        border_pen_y = 120.0 * np.exp(-yy.astype(np.float32) / max(4.0, min(14.0, band_h * 0.14)))

        hcost = seam_cost + center_y
        mpt_cols = np.where(must_prev_top)[0]
        hcost[:, mpt_cols] += border_pen_y
        hcost[:min(3, h - 1), mpt_cols] = huge
        free_cols = np.where(~must_prev_top)[0]
        if len(free_cols) > 0:
            hcost[0, free_cols] = 0.0
        forbid_y = ~prev_has_1x.copy()
        if len(free_cols) > 0:
            forbid_y[0, free_cols] = False
        inner_margin_y = (~binary_erosion(prev_has_1x, iterations=3)) & prev_has_1x
        inner_margin_y[:5, :] = False
        hcost[forbid_y] = huge
        hcost[inner_margin_y] += 5e4
        if must_prev_bot.any():
            mpb_cols = np.where(must_prev_bot)[0]
            hcost[max(0, h - 3):, mpb_cols] = huge
        y_seam = _dp_vertical_seam(hcost.T)
        use_cur_1x &= yy >= y_seam[None, :]

    # 4) Bottom overlap (if `prev` borders bottom without top) -> `y_seam_b(x)` in `[0..h]`
    if must_prev_bot.any() and not must_prev_top.any() and h >= 6:
        bcost = np.empty((h + 1, w), dtype=np.float32)
        target_by = max(8.0, float(h - 48))
        b_prior = 8.0 * ((np.arange(h, dtype=np.float32)[:, None] - target_by) / 56.0) ** 2
        bcost[:h, :] = seam_cost + b_prior
        bcost[h, :] = huge
        mpb_cols = np.where(must_prev_bot)[0]
        bcost[max(0, h - 12):, mpb_cols] = huge
        has_missing_at_or_below = np.cumsum((~prev_has_1x)[::-1, :], axis=0)[::-1, :] > 0
        bcost[:h, :][has_missing_at_or_below] = huge
        free_b = np.where(~must_prev_bot)[0]
        if len(free_b) > 0:
            bcost[h, free_b] = 0.0
        y_seam_b = _dp_vertical_seam(bcost.T)
        use_cur_1x &= yy < y_seam_b[None, :]

    if not (must_prev_left.any() or must_prev_top.any() or must_prev_right.any() or must_prev_bot.any()):
        use_cur_1x = (~prev_has_1x) | (err_cur < err_prev)

    # Hard invariant: wherever `prev` has no pixel yet, MUST use `cur`
    use_cur_1x[~prev_has_1x] = True

    # Bidirectional Reference-Fidelity Hallucination Override on localized high-discrepancy blobs
    # Note: `prev_1x` / `cur_1x` are already 4x4 HD box averages; light `sigma=0.45` preserves small props (jars/bowls)!
    p_blur = gaussian_filter(prev_1x, sigma=(0.45, 0.45, 0.0))
    c_blur = gaussian_filter(cur_1x, sigma=(0.45, 0.45, 0.0))
    r_blur = gaussian_filter(ref_m, sigma=(0.45, 0.45, 0.0))

    ab_pc = np.abs(p_blur - c_blur)
    ab_pr = np.abs(p_blur - r_blur)
    ab_cr = np.abs(c_blur - r_blur)
    diff_s = 0.5 * ab_pc.mean(axis=2) + 0.5 * ab_pc.max(axis=2)
    ep_s = 0.5 * ab_pr.mean(axis=2) + 0.5 * ab_pr.max(axis=2)
    ec_s = 0.5 * ab_cr.mean(axis=2) + 0.5 * ab_cr.max(axis=2)

    can_to_cur = ov_valid.copy()
    if must_prev_top.any():
        can_to_cur[:3, must_prev_top] = False
    if must_prev_left.any():
        can_to_cur[must_prev_left, :3] = False
    if must_prev_bot.any():
        can_to_cur[-3:, must_prev_bot] = False
    if must_prev_right.any():
        can_to_cur[must_prev_right, -3:] = False
    can_to_prev = binary_erosion(prev_has_1x, iterations=3) & valid_sub_1x

    # High-pass structure/prop residual (`sigma=4.5` 1x) is 100% invariant to sunlit wood/stone tone warmth,
    # catching invented props (lanterns, steamers, red jars, bowls) while refusing to swap IN any hallucinated prop!
    p_hp = p_blur - gaussian_filter(p_blur, sigma=(4.5, 4.5, 0.0))
    c_hp = c_blur - gaussian_filter(c_blur, sigma=(4.5, 4.5, 0.0))
    r_hp = r_blur - gaussian_filter(r_blur, sigma=(4.5, 4.5, 0.0))
    hp_ep = 0.5 * np.abs(p_hp - r_hp).mean(axis=2) + 0.5 * np.abs(p_hp - r_hp).max(axis=2)
    hp_ec = 0.5 * np.abs(c_hp - r_hp).mean(axis=2) + 0.5 * np.abs(c_hp - r_hp).max(axis=2)
    hp_ep_reg = gaussian_filter(hp_ep, sigma=2.0)
    hp_ec_reg = gaussian_filter(hp_ec, sigma=2.0)

    # Combine absolute tonal error + local structural prop error
    err_p_comb = 0.45 * ep_s + 0.55 * hp_ep
    err_c_comb = 0.45 * ec_s + 0.55 * hp_ec

    # 1) Override `prev -> cur` where `prev` invented a prop/glow AND `cur` matches 1x local structure (`hp_ec_reg < 24`)
    seed_cur = (
        (~use_cur_1x)
        & can_to_cur
        & (diff_s > 34.0)
        & ((err_p_comb - err_c_comb) > 16.0)
        & (err_p_comb > 28.0)
        & (hp_ep_reg > 13.5)
        & (hp_ec_reg < 24.0)
        & ((hp_ep_reg - hp_ec_reg) > 6.0)
    )
    if seed_cur.any():
        grow_gate = can_to_cur & (diff_s > 10.0) & ((err_p_comb - err_c_comb) > 3.0) & (hp_ec_reg < 25.0) & (hp_ec_reg <= hp_ep_reg + 1.0)
        reg = seed_cur.copy()
        for _ in range(14):
            nxt = binary_dilation(reg, iterations=1) & grow_gate
            if np.array_equal(nxt, reg):
                break
            reg = nxt
        lbl_c, n_c = label(reg, structure=np.ones((3, 3)))
        for cid in range(1, n_c + 1):
            comp = lbl_c == cid
            if int(comp.sum()) < 4:
                continue
            final_c = (comp | (binary_fill_holes(binary_dilation(comp, iterations=4)) & (hp_ec_reg < 25.0))) & can_to_cur
            if float((hp_ep_reg[final_c] - hp_ec_reg[final_c]).mean()) > 4.0 or float((ep_s[comp] - ec_s[comp]).mean()) > 24.0:
                use_cur_1x[final_c] = True

    # 2) Override `cur -> prev` where `cur` invented a prop/glow AND `prev` matches 1x local structure (`hp_ep_reg < 24`)
    seed_prev = (
        use_cur_1x
        & can_to_prev
        & (diff_s > 34.0)
        & ((err_c_comb - err_p_comb) > 16.0)
        & (err_c_comb > 28.0)
        & (hp_ec_reg > 13.5)
        & (hp_ep_reg < 24.0)
        & ((hp_ec_reg - hp_ep_reg) > 6.0)
    )
    if seed_prev.any():
        grow_gate = can_to_prev & (diff_s > 10.0) & ((err_c_comb - err_p_comb) > 3.0) & (hp_ep_reg < 25.0) & (hp_ep_reg <= hp_ec_reg + 1.0)
        reg = seed_prev.copy()
        for _ in range(14):
            nxt = binary_dilation(reg, iterations=1) & grow_gate
            if np.array_equal(nxt, reg):
                break
            reg = nxt
        lbl_p, n_p = label(reg, structure=np.ones((3, 3)))
        for cid in range(1, n_p + 1):
            comp = lbl_p == cid
            if int(comp.sum()) < 4:
                continue
            final_p = (comp | (binary_fill_holes(binary_dilation(comp, iterations=4)) & (hp_ep_reg < 25.0))) & can_to_prev
            if float((hp_ec_reg[final_p] - hp_ep_reg[final_p]).mean()) > 4.0 or float((ec_s[comp] - ep_s[comp]).mean()) > 24.0:
                use_cur_1x[final_p] = False

    use_cur_1x[~prev_has_1x] = True

    # Structure- & discrepancy-gated adaptive feather:
    # - Razor-sharp ~3.5 HD px (`sigma=1.2`) cut ONLY where `|prev-cur|` is high AND real structural edges/props exist!
    # - Smooth ~44 HD px (`sigma=11`) blend across textured grass/cobblestones/walls where patches agree.
    # - Extra-wide ~96 HD px (`sigma=24`) blend across textureless flat water/ponds/rivers (`struct_gate == 0`),
    #   completely eliminating DP staircase teeth over open water!
    w_raw_4x = (
        np.array(
            Image.fromarray((use_cur_1x.astype(np.uint8) * 255)).resize((w4, h4), Image.Resampling.BILINEAR),
            dtype=np.float32,
        )
        / 255.0
    )
    w_sharp = gaussian_filter(w_raw_4x, sigma=1.2)
    w_soft = gaussian_filter(w_raw_4x, sigma=11.0)
    w_flat = gaussian_filter(w_raw_4x, sigma=24.0)

    p_rng = (prev_4x.reshape(h, 4, w, 4, 3).max(axis=(1, 3)) - prev_4x.reshape(h, 4, w, 4, 3).min(axis=(1, 3))).max(axis=2)
    c_rng = (cur_4x.reshape(h, 4, w, 4, 3).max(axis=(1, 3)) - cur_4x.reshape(h, 4, w, 4, 3).min(axis=(1, 3))).max(axis=2)
    local_edge_1x = gaussian_filter(maximum_filter(np.maximum(p_rng, c_rng), size=3), sigma=1.2)
    local_hp_1x = gaussian_filter(
        maximum_filter(
            np.maximum(
                np.maximum(np.abs(p_hp).max(axis=2), np.abs(c_hp).max(axis=2)),
                np.abs(r_hp).max(axis=2),
            ),
            size=3,
        ),
        sigma=1.2,
    )
    struct_gate_1x = np.maximum(
        np.clip((local_edge_1x - 24.0) / 18.0, 0.0, 1.0),
        np.clip((local_hp_1x - 15.0) / 12.0, 0.0, 1.0),
    ).astype(np.float32)
    diff_gate_1x = np.clip((gaussian_filter(diff_s, sigma=2.5) - 16.0) / 16.0, 0.0, 1.0).astype(np.float32)
    prop_gate_1x = (diff_gate_1x * struct_gate_1x).astype(np.float32)
    flat_gate_1x = np.clip(1.0 - struct_gate_1x * 1.5, 0.0, 1.0).astype(np.float32)

    prop_gate_4x = np.array(
        Image.fromarray(prop_gate_1x).resize((w4, h4), Image.Resampling.BILINEAR),
        dtype=np.float32,
    )
    flat_gate_4x = np.array(
        Image.fromarray(flat_gate_1x).resize((w4, h4), Image.Resampling.BILINEAR),
        dtype=np.float32,
    )
    w_terrain = flat_gate_4x * w_flat + (1.0 - flat_gate_4x) * w_soft
    w_4x = prop_gate_4x * w_sharp + (1.0 - prop_gate_4x) * w_terrain

    prev_has_4x = np.repeat(np.repeat(prev_has_1x, 4, axis=0), 4, axis=1)
    taper_n = min(36, h4 // 2, w4 // 2)
    if taper_n >= 4:
        ramp = (0.5 - 0.5 * np.cos(np.linspace(0.0, math.pi, taper_n, dtype=np.float32)))
        ramp[0] = 0.0
        if must_prev_top.any():
            mpt_4x = np.repeat(must_prev_top, 4)
            w_4x[:taper_n, mpt_4x] *= ramp[:, None]
        if must_prev_left.any():
            mpl_4x = np.repeat(must_prev_left, 4)
            w_4x[mpl_4x, :taper_n] *= ramp[None, :]
        if must_prev_bot.any():
            mpb_4x = np.repeat(must_prev_bot, 4)
            w_4x[-taper_n:, mpb_4x] *= ramp[::-1, None]
        if must_prev_right.any():
            mpr_4x = np.repeat(must_prev_right, 4)
            w_4x[mpr_4x, -taper_n:] *= ramp[None, ::-1]
    w_4x[~prev_has_4x] = 1.0
    return np.clip(w_4x, 0.0, 1.0)


def _snap_occluder_alpha_edges_4x(
    a4: np.ndarray,
    rgb_crop_4x: np.ndarray,
    floor_1x_crop: np.ndarray,
    exact_bmask_1x: np.ndarray,
    isl_alpha_crop_4x: np.ndarray,
) -> np.ndarray:
    """Snap `a4` outward by up to 2 HD px onto adjacent walkable floor ONLY on this strip's exact `bval` rim.

    Restricting outward snap strictly to `exact_bmask_1x` (`<= 2 HD px`, `alpha <= 190`) ensures overlapping
    cumulative depth bands (`sub_sy >= bval`) never compound alpha on the same floor boundary!
    """
    occ_core = a4 >= 180
    if not occ_core.any() or not floor_1x_crop.any() or not exact_bmask_1x.any():
        return a4
    floor_4x = np.repeat(np.repeat(floor_1x_crop, 4, axis=0), 4, axis=1)
    exact_4x = np.repeat(np.repeat(exact_bmask_1x, 4, axis=0), 4, axis=1)
    exact_core = occ_core & exact_4x
    if not exact_core.any():
        return a4
    dil2 = binary_dilation(exact_core, iterations=2)
    rim = dil2 & (~occ_core) & floor_4x & (isl_alpha_crop_4x > 200)
    if not rim.any():
        return a4

    inner_band = occ_core & (~binary_erosion(occ_core, iterations=4))
    outer_band = binary_dilation(occ_core, iterations=6) & (~binary_dilation(occ_core, iterations=2)) & floor_4x & (isl_alpha_crop_4x > 200)
    if not inner_band.any() or not outer_band.any():
        return a4

    rgb_f = rgb_crop_4x.astype(np.float32)
    w_in = gaussian_filter(inner_band.astype(np.float32), sigma=3.0)
    w_out = gaussian_filter(outer_band.astype(np.float32), sigma=3.0)
    valid_w = rim & (w_in > 0.015) & (w_out > 0.015)
    if not valid_w.any():
        return a4

    occ_loc = np.zeros_like(rgb_f)
    flr_loc = np.zeros_like(rgb_f)
    for ch in range(3):
        c_in = gaussian_filter(rgb_f[:, :, ch] * inner_band, sigma=3.0)
        c_out = gaussian_filter(rgb_f[:, :, ch] * outer_band, sigma=3.0)
        occ_loc[valid_w, ch] = c_in[valid_w] / w_in[valid_w]
        flr_loc[valid_w, ch] = c_out[valid_w] / w_out[valid_w]

    d_occ = np.linalg.norm(rgb_f - occ_loc, axis=2)
    d_flr = np.linalg.norm(rgb_f - flr_loc, axis=2)
    d_sep = np.linalg.norm(occ_loc - flr_loc, axis=2)

    snap_cand = valid_w & (d_sep > 24.0) & (d_occ < 28.0) & (d_occ * 1.75 + 10.0 < d_flr)
    if not snap_cand.any():
        return a4

    snapped = exact_core.copy()
    for _ in range(2):
        snapped = snapped | (binary_dilation(snapped, iterations=1) & snap_cand)
    newly_snapped = snapped & (~occ_core)
    if not newly_snapped.any():
        return a4

    snap_a = np.zeros_like(a4, dtype=np.float32)
    snap_a[newly_snapped] = np.clip((d_flr[newly_snapped] - d_occ[newly_snapped]) * 7.5, 0.0, 190.0)
    snap_a = gaussian_filter(snap_a, sigma=0.55)
    out_a = np.maximum(a4, np.clip(np.round(snap_a), 0, 190).astype(np.uint8))
    return np.minimum(out_a, isl_alpha_crop_4x)


def _island_masks(
    isl: dict,
    n_islands: int,
    lbl_all: np.ndarray,
    vis_1x: np.ndarray,
    tile_occ_1x: np.ndarray,
) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Zero-hole island masks shared by `bake_map_hd` and chained preparation.

    Returns `(isl_valid_1x (ih, iw), isl_alpha_4x (ih*4, iw*4) uint8, isl_ctx_full (2048, 2048))`.
    """
    ix0, iy0, iw, ih = isl["bbox_1x"]
    iw4, ih4 = iw * 4, ih * 4
    # Match `lbl_all` components enclosed in `[ix0..ix0+iw, iy0..iy0+ih]` (robust to `rebake-map` relabeling!)
    sub_lbl = lbl_all[iy0:iy0 + ih, ix0:ix0 + iw]
    present_cids = [int(c) for c in np.unique(sub_lbl) if c > 0]
    matched_cids = []
    for c in present_cids:
        tot_c = int(((lbl_all == c) & vis_1x).sum())
        in_c = int(((sub_lbl == c) & vis_1x[iy0:iy0 + ih, ix0:ix0 + iw]).sum())
        if tot_c > 0 and (in_c / tot_c >= 0.5 or n_islands == 1):
            matched_cids.append(c)
    if not matched_cids:
        matched_cids = present_cids or (isl.get("cids") or [isl.get("cid", 1)])

    # Zero-Hole Island Masking: intersect `vis_1x` (no 6px dilated black AI collar!) then `binary_fill_holes`!
    isl_valid_1x = (
        np.isin(sub_lbl, matched_cids)
        & vis_1x[iy0:iy0 + ih, ix0:ix0 + iw]
        & tile_occ_1x[iy0:iy0 + ih, ix0:ix0 + iw]
    )
    isl_valid_1x = binary_fill_holes(isl_valid_1x)

    valid_4x_f = np.array(
        Image.fromarray((isl_valid_1x.astype(np.uint8) * 255)).resize((iw4, ih4), Image.Resampling.BILINEAR)
    )
    isl_alpha_4x = np.clip((valid_4x_f.astype(np.int32) - 96) * 3, 0, 255).astype(np.uint8)
    holes_4x = binary_fill_holes(isl_alpha_4x > 200) & (isl_alpha_4x <= 200)
    if holes_4x.any():
        isl_alpha_4x[holes_4x] = 255

    # Full-map context mask for 1x reference registration
    isl_full_mask = np.zeros((2048, 2048), dtype=bool)
    isl_full_mask[iy0:iy0 + ih, ix0:ix0 + iw] = isl_valid_1x
    isl_ctx_full = isl_full_mask | (binary_dilation(isl_full_mask, iterations=2) & tile_occ_1x)

    return isl_valid_1x, isl_alpha_4x, isl_ctx_full


def _two_band_seam_blend_4x(
    prev_4x: np.ndarray,
    cur_4x: np.ndarray,
    w_seam_4x: np.ndarray,
    both_4x: np.ndarray,
    sigma_lo: float = 10.0,
    sigma_w_lo: float = 24.0,
    sigma_w_hi: float = 2.5,
) -> np.ndarray:
    """Frequency-split seam blend for independently generated patches.

    - Low band (lighting/colour, `sigma_lo`): blended WIDE (`sigma_w_lo`) so brightness steps vanish.
    - High band (brushwork/texture): blended NARROW around the seam-cut line (`sigma_w_hi`), so two different
      grass/ripple textures never cross-fade into a blurry double-exposed band.
    - The incoming patch's high band is rescaled to the previous canvas's texture energy measured where both
      sides overlap (clipped to [0.8, 1.25]), so fine-detail density matches across the seam.
    """
    lo = lambda im: np.stack([gaussian_filter(im[:, :, c], sigma_lo, mode="nearest") for c in range(3)], axis=2)
    lo_p, lo_c = lo(prev_4x), lo(cur_4x)
    hi_p, hi_c = prev_4x - lo_p, cur_4x - lo_c
    if int(both_4x.sum()) >= 256:
        e_p = float(np.sqrt((hi_p[both_4x] ** 2).mean()))
        e_c = float(np.sqrt((hi_c[both_4x] ** 2).mean()))
        if e_c > 1e-3:
            hi_c = hi_c * float(np.clip(e_p / e_c, 0.8, 1.25))
    w_lo = np.clip(gaussian_filter(w_seam_4x, sigma_w_lo, mode="nearest"), 0.0, 1.0)[:, :, None]
    w_hi = np.clip(gaussian_filter((w_seam_4x > 0.5).astype(np.float32), sigma_w_hi, mode="nearest"), 0.0, 1.0)[:, :, None]
    return lo_p * (1.0 - w_lo) + lo_c * w_lo + hi_p * (1.0 - w_hi) + hi_c * w_hi


def _stitch_island_patches_4x(
    patches: list[dict],
    ai_map: dict[str, Path],
    work_dir: Path,
    full_1x: np.ndarray,
    isl_ctx_full: np.ndarray,
    isl_valid_1x: np.ndarray,
    isl_alpha_4x: np.ndarray,
    island_bbox_1x: tuple[int, int, int, int],
) -> tuple[np.ndarray, np.ndarray]:
    """Register, harmonize and seam-cut stitch `patches` (in order) into one island canvas.

    Shared by `bake_map_hd` and chained preparation (`chain-next`), so a partial island built from the
    patches generated so far is pixel-identical to what the final bake will produce for that region.
    Returns `(acc_rgb_4x float32 (ih*4, iw*4, 3), has_pix_1x bool (ih, iw))`.
    """
    ix0, iy0, iw, ih = island_bbox_1x
    acc_rgb = np.zeros((ih * 4, iw * 4, 3), dtype=np.float32)
    has_pix_1x = np.zeros((ih, iw), dtype=bool)
    for pinfo in patches:
        pkey = pinfo["key"]
        ai_p = ai_map.get(pkey)
        if ai_p is not None:
            ai_p = resolve_work_or_asset_path(ai_p, work_dir)
        if not ai_p or not ai_p.exists():
            raise FileNotFoundError(f"Missing AI patch output for {pkey}: {ai_p}")
        bx0, by0, bw, bh = pinfo["box_1x"]

        ref_crop_1x = full_1x[by0:by0 + bh, bx0:bx0 + bw, :3].copy()
        ref_crop_1x[~isl_ctx_full[by0:by0 + bh, bx0:bx0 + bw]] = 0
        ref_2x = np.array(Image.fromarray(ref_crop_1x).resize((1376, 768), Image.Resampling.NEAREST))

        ai_im = Image.open(ai_p).convert("RGB")
        if ai_im.size != (1376, 768):
            ai_im = ai_im.resize((1376, 768), Image.Resampling.LANCZOS)
        ai_2x = np.array(ai_im)

        dx2, dy2 = register_translation_2d(ref_2x, ai_2x, max_shift=10)
        ai_2x_reg = shift_image_rgb(ai_2x, dx2, dy2, fill=(0, 0, 0))
        im_4x = Image.fromarray(ai_2x_reg).resize((bw * 4, bh * 4), Image.Resampling.LANCZOS)
        patch_rgb = np.array(im_4x).astype(np.float32)

        # Intersection of this patch `[bx0..bx0+bw, by0..by0+bh]` with island `[ix0..ix0+iw, iy0..iy0+ih]`
        ox0 = max(ix0, bx0)
        oy0 = max(iy0, by0)
        ox1 = min(ix0 + iw, bx0 + bw)
        oy1 = min(iy0 + ih, by0 + bh)
        if ox1 <= ox0 or oy1 <= oy0:
            continue

        y0_i, y1_i = oy0 - iy0, oy1 - iy0
        x0_i, x1_i = ox0 - ix0, ox1 - ix0
        py0_i, py1_i = y0_i * 4, y1_i * 4
        px0_i, px1_i = x0_i * 4, x1_i * 4
        py0_p, py1_p = (oy0 - by0) * 4, (oy1 - by0) * 4
        px0_p, px1_p = (ox0 - bx0) * 4, (ox1 - bx0) * 4

        raw_sub_4x = patch_rgb[py0_p:py1_p, px0_p:px1_p].copy()
        sub_patch = raw_sub_4x.copy()
        ref_sub_1x = full_1x[oy0:oy1, ox0:ox1, :3]
        valid_sub_1x = isl_valid_1x[y0_i:y1_i, x0_i:x1_i]
        # Do not mark `has_pix_1x = True` on un-regenerated `#000000` black void cutouts where `ref_sub_1x >= 12`!
        raw_cur_max_1x = raw_sub_4x.reshape(y1_i - y0_i, 4, x1_i - x0_i, 4, 3).mean(axis=(1, 3)).max(axis=2)
        non_black_cur_1x = (raw_cur_max_1x >= 16.0) | (ref_sub_1x.max(axis=2) < 12)
        prev_has_sub = has_pix_1x[y0_i:y1_i, x0_i:x1_i]

        if not prev_has_sub.any():
            acc_rgb[py0_i:py1_i, px0_i:px1_i] = sub_patch
            has_pix_1x[y0_i:y1_i, x0_i:x1_i] |= valid_sub_1x & non_black_cur_1x
            print(f"  [Patch] {pkey}: shift_2x=({dx2:+d},{dy2:+d}) [anchor]")
            continue

        prev_4x = acc_rgb[py0_i:py1_i, px0_i:px1_i]
        sub_patch = harmonize_patch_overlap_4x(
            prev_4x=prev_4x,
            sub_patch=sub_patch,
            prev_has_1x=prev_has_sub,
            valid_sub_1x=valid_sub_1x,
            isl_alpha_sub_4x=isl_alpha_4x[py0_i:py1_i, px0_i:px1_i],
            ref_1x=ref_sub_1x,
        )

        must_prev_left = has_pix_1x[y0_i:y1_i, x0_i - 1] if x0_i > 0 else np.zeros(y1_i - y0_i, dtype=bool)
        must_prev_top = has_pix_1x[y0_i - 1, x0_i:x1_i] if y0_i > 0 else np.zeros(x1_i - x0_i, dtype=bool)
        must_prev_right = has_pix_1x[y0_i:y1_i, x1_i] if x1_i < iw else np.zeros(y1_i - y0_i, dtype=bool)
        must_prev_bot = has_pix_1x[y1_i, x0_i:x1_i] if y1_i < ih else np.zeros(x1_i - x0_i, dtype=bool)

        w_seam_4x = compute_seam_cut_blend_weight_4x(
            prev_4x=prev_4x,
            cur_4x=sub_patch,
            ref_1x=ref_sub_1x,
            prev_has_1x=prev_has_sub,
            valid_sub_1x=valid_sub_1x,
            must_prev_left=must_prev_left,
            must_prev_top=must_prev_top,
            must_prev_right=must_prev_right,
            must_prev_bot=must_prev_bot,
        )

        # Black-Void Guard: never let `raw_sub_4x` near-black void (`< 16`) overwrite valid `prev_4x` (`> 24`),
        # including dark 1x PAL lake/river water (`ref_max >= 12`), while keeping true `#000000` PAL doorways!
        prev_has_4x = np.repeat(np.repeat(prev_has_sub, 4, axis=0), 4, axis=1)
        ref_max_4x = np.repeat(np.repeat(ref_sub_1x.max(axis=2), 4, axis=0), 4, axis=1)
        cur_blk_void = prev_has_4x & (raw_sub_4x.max(axis=2) < 16.0) & (prev_4x.max(axis=2) > 24.0) & (ref_max_4x >= 12)
        if cur_blk_void.any():
            blk_guard = gaussian_filter(binary_dilation(cur_blk_void, iterations=6).astype(np.float32), sigma=2.2)
            w_seam_4x = w_seam_4x * np.clip(1.0 - blk_guard * 1.5, 0.0, 1.0)
            w_seam_4x[cur_blk_void] = 0.0

        prev_blk_void = prev_has_4x & (prev_4x.max(axis=2) < 16.0) & (raw_sub_4x.max(axis=2) > 24.0) & (ref_max_4x >= 12)
        if prev_blk_void.any():
            pblk_guard = gaussian_filter(binary_dilation(prev_blk_void, iterations=6).astype(np.float32), sigma=2.2)
            w_seam_4x = np.maximum(w_seam_4x, np.clip(pblk_guard * 1.5, 0.0, 1.0))
            w_seam_4x[prev_blk_void] = 1.0

        linear = prev_4x * (1.0 - w_seam_4x[:, :, None]) + sub_patch * w_seam_4x[:, :, None]
        # Two-band seam blend only where BOTH sides hold real (non-void) pixels; void guards stay linear/hard.
        both_4x = prev_has_4x & (raw_sub_4x.max(axis=2) >= 16.0) & (prev_4x.max(axis=2) >= 16.0)
        both_4x &= ~cur_blk_void & ~prev_blk_void
        if TWO_BAND_SEAM_BLEND and both_4x.any():
            mb = _two_band_seam_blend_4x(prev_4x, sub_patch, w_seam_4x, both_4x)
            linear = np.where(both_4x[:, :, None], mb, linear)
        acc_rgb[py0_i:py1_i, px0_i:px1_i] = linear
        has_pix_1x[y0_i:y1_i, x0_i:x1_i] |= valid_sub_1x & non_black_cur_1x
        print(f"  [Patch] {pkey}: shift_2x=({dx2:+d},{dy2:+d}) [seam-cut]")

    return acc_rgb, has_pix_1x


def bake_map_hd(
    map_id: int,
    work_dir: Path,
    ai_map: dict[str, Path],
    verify: bool = True,
    out_dir: Path | None = None,
):
    work_dir = resolve_work_or_asset_path(work_dir)
    if out_dir is None:
        out_dir = GFX / "maps_hd" / f"map_{map_id:03d}"
    out_dir.mkdir(parents=True, exist_ok=True)
    for stale_chunk in out_dir.glob("isl*_c*_4x.webp"):
        stale_chunk.unlink()

    islands_meta = json.loads((work_dir / f"map_{map_id:03d}_islands.json").read_text())
    sort_y_1x = np.load(work_dir / f"map_{map_id:03d}_sort_y_1x.npy")
    vis_1x = np.load(work_dir / f"map_{map_id:03d}_vis_1x.npy")
    occ_path = work_dir / f"map_{map_id:03d}_occ_1x.npy"
    full_1x = np.array(Image.open(work_dir / f"map_{map_id:03d}_full_1x.png").convert("RGBA"))
    tile_occ_1x = np.load(occ_path) if occ_path.exists() else (full_1x[:, :, :3].sum(axis=2) > 0)
    lbl_all, n_lbl_all = label(binary_dilation(vis_1x, iterations=8))

    # Build exact walkable character/event foot `char_sy_grid` (including bridges, stairs, 2F balconies & events!)
    char_sy_grid = build_char_sy_grid_1x(map_id)
    pad_char_sy = np.pad(char_sy_grid, ((8, 64), (16, 16)), mode="constant", constant_values=999999)

    # Resolve optional Story-Variant donor (from prepare metadata or live detection among already-baked maps)
    donor_id: int | None = None
    donor_meta_path = work_dir / f"map_{map_id:03d}_donor.json"
    if donor_meta_path.exists():
        donor_id = json.loads(donor_meta_path.read_text()).get("donor_map_id")
    else:
        for isl_chk in islands_meta:
            if isl_chk.get("story_variant_donor") is not None:
                donor_id = int(isl_chk["story_variant_donor"])
                break
        if donor_id is None:
            cand_donor, _, _ = find_story_variant_donor(map_id, full_1x, tile_occ_1x, vis_1x)
            donor_id = cand_donor
    donor_1x_full = render_map_1x_and_sort_y(donor_id)[0] if donor_id is not None else None

    exported_rooms = []
    occluder_strips_rgba: list[np.ndarray] = []
    occluder_strips_info: list[dict] = []
    covered_valid_1x = np.zeros((2048, 2048), dtype=bool)

    for isl in islands_meta:
        isl_id = isl["island_id"]
        ix0, iy0, iw, ih = isl["bbox_1x"]

        isl_valid_1x, isl_alpha_4x, isl_ctx_full = _island_masks(isl, len(islands_meta), lbl_all, vis_1x, tile_occ_1x)
        covered_valid_1x[iy0:iy0 + ih, ix0:ix0 + iw] |= isl_valid_1x

        # Auto-prune redundant `uniq_vis_1x == 0` border patches that hallucinated into exterior `#000000` void!
        active_patches = list(isl["patches"])
        if len(active_patches) > 1:
            cov_cnt = np.zeros((ih, iw), dtype=np.int32)
            p_masks = []
            p_scores = []
            for idx_p, pinfo in enumerate(active_patches):
                pkey = pinfo["key"]
                ai_p = ai_map.get(pkey)
                if ai_p is not None:
                    ai_p = resolve_work_or_asset_path(ai_p, work_dir)
                if not ai_p or not ai_p.exists():
                    raise FileNotFoundError(f"Missing AI patch output for {pkey}: {ai_p}")
                bx0, by0, bw, bh = pinfo["box_1x"]
                ox0, oy0 = max(ix0, bx0), max(iy0, by0)
                ox1, oy1 = min(ix0 + iw, bx0 + bw), min(iy0 + ih, by0 + bh)
                pm = np.zeros((ih, iw), dtype=bool)
                if ox1 > ox0 and oy1 > oy0:
                    pm[oy0 - iy0:oy1 - iy0, ox0 - ix0:ox1 - ix0] = isl_valid_1x[oy0 - iy0:oy1 - iy0, ox0 - ix0:ox1 - ix0]
                cov_cnt += pm.astype(np.int32)
                p_masks.append(pm)

                ref_c = full_1x[by0:by0 + bh, bx0:bx0 + bw, :3]
                in_m = isl_ctx_full[by0:by0 + bh, bx0:bx0 + bw]
                void_m = binary_erosion(~in_m, iterations=2)
                ai_1x_q = np.array(Image.open(ai_p).convert("RGB").resize((bw, bh), Image.Resampling.BOX))
                vh = float((ai_1x_q[void_m].max(axis=1) > 28).mean()) if int(void_m.sum()) >= 200 else 0.0
                mae = (
                    float(np.abs(ai_1x_q[in_m].astype(np.float32) - ref_c[in_m].astype(np.float32)).mean())
                    if int(in_m.sum()) >= 20
                    else 999.0
                )
                in_frac = float(in_m.mean())
                p_scores.append((vh * 100.0 + mae, idx_p, vh, mae, in_frac))

            drop_indices = set()
            for _, idx_p, vh, mae, in_frac in sorted(p_scores, reverse=True):
                if len(active_patches) - len(drop_indices) <= 1:
                    break
                pm = p_masks[idx_p]
                is_bad = (vh > 0.28 and mae > 33.0) or (in_frac < 0.08 and mae > 30.0)
                if is_bad and (not pm.any() or bool((cov_cnt[pm] >= 2).all())):
                    drop_indices.add(idx_p)
                    cov_cnt -= pm.astype(np.int32)
                    print(
                        f"  [Auto-Skip Redundant Patch] {active_patches[idx_p]['key']}: "
                        f"uniq_vis=0, void_halluc={vh:.2f}, mae={mae:.1f}"
                    )
            if drop_indices:
                active_patches = [p for i, p in enumerate(active_patches) if i not in drop_indices]

        acc_rgb, has_pix_1x = _stitch_island_patches_4x(
            active_patches, ai_map, work_dir, full_1x, isl_ctx_full, isl_valid_1x, isl_alpha_4x, (ix0, iy0, iw, ih)
        )

        isl_rgb_4x = np.clip(np.round(acc_rgb), 0, 255).astype(np.uint8)
        ref_isl_1x = full_1x[iy0:iy0 + ih, ix0:ix0 + iw, :3]

        # Pillar 1: Canonical 1x-anchored Lab calibration + adaptive texture normalization (BEFORE chunks & occluders!)
        if active_patches:
            cal_mask_1x = isl_valid_1x & has_pix_1x if (donor_id is not None and not has_pix_1x.all()) else isl_valid_1x
            isl_rgb_4x, cal_stats = calibrate_hd_island_to_canonical_1x(
                isl_rgb_4x, isl_alpha_4x, ref_isl_1x, cal_mask_1x
            )
            print(f"  [Calibrate] Map {map_id:03d} isl{isl_id}: {cal_stats}")

        # Pillar 2: Story-Variant HD Tile Inheritance (exact donor pixels on unchanged 1x tiles + 20 HD px feather)
        if donor_id is not None:
            if not active_patches:
                isl_rgb_4x, _ = _load_map_hd_crop_4x(donor_id, (ix0, iy0, iw, ih))
                print(f"  [Story-Variant] Map {map_id:03d} isl{isl_id}: 100% inherited from donor Map {donor_id:03d}")
            else:
                if not has_pix_1x.all():
                    d_rgb, _ = _load_map_hd_crop_4x(donor_id, (ix0, iy0, iw, ih))
                    miss_4x = np.repeat(np.repeat(~has_pix_1x, 4, axis=0), 4, axis=1)
                    isl_rgb_4x[miss_4x] = d_rgb[miss_4x]
                isl_rgb_4x, inh_frac = apply_story_variant_inheritance_4x(
                    isl_rgb_4x, (ix0, iy0, iw, ih), ref_isl_1x, donor_id, donor_1x_full=donor_1x_full
                )
                print(
                    f"  [Story-Variant] Map {map_id:03d} isl{isl_id}: inherited {inh_frac * 100:.1f}% from donor Map {donor_id:03d}"
                )

        # Slice island into clean non-overlapping <= 512x512 1x (2048x2048 4x) WebP chunks, naming each
        # cell by its fixed `(row, col)` grid coordinate (`isl{isl_id}_r{row}_c{col}_4x.webp`) so empty
        # corner cells gaining/losing pixels NEVER shift neighboring chunk filenames in git diffs!
        CHUNK_1X = 512
        for cy0_1x in range(0, ih, CHUNK_1X):
            for cx0_1x in range(0, iw, CHUNK_1X):
                grid_r = cy0_1x // CHUNK_1X
                grid_c = cx0_1x // CHUNK_1X
                cy1_1x = min(ih, cy0_1x + CHUNK_1X)
                cx1_1x = min(iw, cx0_1x + CHUNK_1X)
                sub_a = isl_alpha_4x[cy0_1x * 4:cy1_1x * 4, cx0_1x * 4:cx1_1x * 4]
                nz_y, nz_x = np.where(sub_a > 0)
                if len(nz_y) == 0:
                    continue
                tx0 = (int(nz_x.min()) // 4) * 4
                ty0 = (int(nz_y.min()) // 4) * 4
                tx1 = min((cx1_1x - cx0_1x) * 4, ((int(nz_x.max()) + 4) // 4) * 4)
                ty1 = min((cy1_1x - cy0_1x) * 4, ((int(nz_y.max()) + 4) // 4) * 4)

                gy0 = cy0_1x * 4 + ty0
                gy1 = cy0_1x * 4 + ty1
                gx0 = cx0_1x * 4 + tx0
                gx1 = cx0_1x * 4 + tx1

                chunk_rgba = np.zeros((gy1 - gy0, gx1 - gx0, 4), dtype=np.uint8)
                chunk_rgba[:, :, :3] = isl_rgb_4x[gy0:gy1, gx0:gx1]
                chunk_a = isl_alpha_4x[gy0:gy1, gx0:gx1].copy()
                c_holes = binary_fill_holes(chunk_a > 200) & (chunk_a <= 200)
                if c_holes.any():
                    chunk_a[c_holes] = 255
                chunk_rgba[:, :, 3] = chunk_a
                chunk_rgba = extrude_rgb_voronoi(chunk_rgba, iters=4)

                cname = f"isl{isl_id}_r{grid_r}_c{grid_c}"
                cfile = f"{cname}_4x.webp"
                Image.fromarray(chunk_rgba).save(
                    out_dir / cfile, "WEBP", quality=90, alpha_quality=100, exact=True, method=4
                )
                exported_rooms.append({
                    "name": cname,
                    "file": cfile,
                    "x": int(ix0 + gx0 // 4),
                    "y": int(iy0 + gy0 // 4),
                    "w": int((gx1 - gx0) // 4),
                    "h": int((gy1 - gy0) // 4),
                })

        # --- Exact Zero-Error Per-Depth Occluder Extraction + Color-Guided Edge Snapping ---
        sub_sy = sort_y_1x[iy0:iy0 + ih, ix0:ix0 + iw].copy()
        sub_sy[~isl_valid_1x] = -1

        # Exact Y-window: character standing behind `y_pixel` has feet `py in [y - 8 .. y + 64]`, `px in [x - 16 .. x + 16]`
        # `pad_char_sy` is padded by `((8, 64), (16, 16))` -> slice `(ih + 72, iw + 32)` and run centered `(73, 33)` min filter!
        sub_pad = pad_char_sy[iy0:iy0 + ih + 72, ix0:ix0 + iw + 32]
        min_x = minimum_filter(sub_pad, size=(1, 33))[:, 16:16 + iw]
        fy_min_char = minimum_filter(min_x, size=(73, 1))[36:36 + ih, :]

        occluder_1x = (sub_sy >= 0) & (sub_sy > fy_min_char)
        exact_sy = np.where(occluder_1x, sub_sy, -1)

        # Group bounding boxes by exact PAL `sortY` (`bval = exact sub_sy`, already on sparse 8px lattice!)
        oy_all, ox_all = np.where(exact_sy >= 0)
        bval_bounds: dict[int, list[int]] = {}
        for yv, xv, bv in zip(oy_all.tolist(), ox_all.tolist(), exact_sy[oy_all, ox_all].tolist()):
            b = bval_bounds.get(bv)
            if b is None:
                bval_bounds[bv] = [xv, yv, xv, yv]
            else:
                if xv < b[0]: b[0] = xv
                if yv < b[1]: b[1] = yv
                if xv > b[2]: b[2] = xv
                if yv > b[3]: b[3] = yv

        for bval in sorted(bval_bounds):
            bx0_b, by0_b, bx1_b, by1_b = bval_bounds[bval]
            sx0 = max(0, bx0_b - 3)
            sy0 = max(0, by0_b - 3)
            sx1 = min(iw, bx1_b + 4)
            sy1 = min(ih, by1_b + 4)

            sub_q = exact_sy[sy0:sy1, sx0:sx1]
            sub_s = sub_sy[sy0:sy1, sx0:sx1]
            bmask = sub_q == bval
            lbl_b, _ = label(bmask, structure=np.ones((3, 3)))
            slices_b = find_objects(lbl_b)

            for comp_id, slc in enumerate(slices_b, start=1):
                if slc is None:
                    continue
                ry0 = max(0, slc[0].start - 2)
                ry1 = min(sy1 - sy0, slc[0].stop + 2)
                rx0 = max(0, slc[1].start - 2)
                rx1 = min(sx1 - sx0, slc[1].stop + 2)

                cc_loc = lbl_b[ry0:ry1, rx0:rx1] == comp_id
                if not cc_loc.any():
                    continue
                s_loc = sub_s[ry0:ry1, rx0:rx1]
                # Pad 1px into strictly higher-depth neighbors for seamless cumulative overlap
                cc_padded = cc_loc | (binary_dilation(cc_loc, iterations=1) & (s_loc > bval))
                # Also dilate gate into `< bval` neighbors so `a_gate4` only separates disconnected `== bval` components
                # while `a_cum4` (`sub_sy >= bval`) defines the exact un-eroded lower-depth silhouette!
                cc_gate = cc_padded | (binary_dilation(cc_loc, iterations=1) & (s_loc < bval))
                ys1, xs1 = np.where(cc_padded)

                ox0 = max(0, sx0 + rx0 + int(xs1.min()) - 2)
                oy0 = max(0, sy0 + ry0 + int(ys1.min()) - 2)
                ox1 = min(iw, sx0 + rx0 + int(xs1.max()) + 3)
                oy1 = min(ih, sy0 + ry0 + int(ys1.max()) + 3)

                ow4, oh4 = (ox1 - ox0) * 4, (oy1 - oy0) * 4
                cum_crop_b = sub_sy[oy0:oy1, ox0:ox1] >= bval
                cum_crop = cum_crop_b.astype(np.uint8) * 255
                a_cum4 = np.array(Image.fromarray(cum_crop).resize((ow4, oh4), Image.Resampling.BILINEAR))
                a_cum4 = np.clip((a_cum4.astype(np.int32) - 96) * 4, 0, 255).astype(np.uint8)
                # Protect thin 1px lattice bars / corners: inner 2x2 HD core of every true `cum_crop` pixel stays >= 220
                core_4x = np.zeros((oh4, ow4), dtype=bool)
                cy_1, cx_1 = np.where(cum_crop_b)
                for dy4 in (1, 2):
                    for dx4 in (1, 2):
                        core_4x[cy_1 * 4 + dy4, cx_1 * 4 + dx4] = True
                a_cum4[core_4x] = np.maximum(a_cum4[core_4x], 240)

                # Build local gate crop aligned to `[oy0:oy1, ox0:ox1]`
                gate_1x = np.zeros((oy1 - oy0, ox1 - ox0), dtype=np.uint8)
                gy0_l = (sy0 + ry0) - oy0
                gx0_l = (sx0 + rx0) - ox0
                ys_g, xs_g = np.where(cc_gate)
                gy_idx = gy0_l + ys_g
                gx_idx = gx0_l + xs_g
                in_g = (gy_idx >= 0) & (gy_idx < gate_1x.shape[0]) & (gx_idx >= 0) & (gx_idx < gate_1x.shape[1])
                gate_1x[gy_idx[in_g], gx_idx[in_g]] = 255
                a_gate4 = np.array(Image.fromarray(gate_1x).resize((ow4, oh4), Image.Resampling.BILINEAR))
                a_gate4 = np.clip((a_gate4.astype(np.int32) - 64) * 4, 0, 255).astype(np.uint8)

                isl_a_crop = isl_alpha_4x[oy0 * 4:oy1 * 4, ox0 * 4:ox1 * 4]
                a4 = np.minimum(np.minimum(a_cum4, a_gate4), isl_a_crop)
                if int((a4 > 0).sum()) == 0:
                    continue

                rgb_crop = isl_rgb_4x[oy0 * 4:oy1 * 4, ox0 * 4:ox1 * 4]
                floor_1x_crop = (sub_sy[oy0:oy1, ox0:ox1] < 0) & isl_valid_1x[oy0:oy1, ox0:ox1]
                exact_bmask_1x = np.zeros((oy1 - oy0, ox1 - ox0), dtype=bool)
                ys_c, xs_c = np.where(cc_loc)
                cy_idx = gy0_l + ys_c
                cx_idx = gx0_l + xs_c
                in_c = (cy_idx >= 0) & (cy_idx < exact_bmask_1x.shape[0]) & (cx_idx >= 0) & (cx_idx < exact_bmask_1x.shape[1])
                exact_bmask_1x[cy_idx[in_c], cx_idx[in_c]] = True
                a4 = _snap_occluder_alpha_edges_4x(a4, rgb_crop, floor_1x_crop, exact_bmask_1x, isl_a_crop)

                strip = np.zeros((oh4, ow4, 4), dtype=np.uint8)
                strip[:, :, :3] = rgb_crop
                strip[:, :, 3] = a4
                strip = extrude_rgb_voronoi(strip, iters=3)

                occluder_strips_rgba.append(strip)
                occluder_strips_info.append({
                    "sortY": int(bval),
                    "dx": int(ix0 + ox0),
                    "dy": int(iy0 + oy0),
                    "dw": int(ox1 - ox0),
                    "dh": int(oy1 - oy0),
                })

    # Extract elevated pure-black (#000000) tunnel/portal darkness occluders outside `covered_valid_1x`
    # (1995 PAL places elevated pure-black ceiling tiles over dark cave tunnel mouths to mask characters walking in)
    scenes_tb, events_tb, scripts_tb = load_game_tables()
    vis_ht, active_evs_tb, _ = compute_reachable_walkable_and_events(map_id, scenes_tb, events_tb, scripts_tb)
    reach_grid = np.full((2048, 2048), 999999, dtype=np.int32)
    reach_pt = np.zeros((2048, 2048), dtype=bool)
    for r_ty, r_tx, r_h in vis_ht:
        r_px, r_py = r_tx * 32 + r_h * 16, r_ty * 16 + r_h * 8
        if 0 <= r_px < 2048 and 0 <= r_py < 2048:
            reach_grid[r_py, r_px] = min(int(reach_grid[r_py, r_px]), r_py + 8)
            reach_pt[r_py, r_px] = True
    r_pys, r_pxs = np.where(reach_pt)
    for sdx, sdy in ((16, 8), (-16, 8), (16, -8), (-16, -8), (32, 0), (0, 16)):
        nx_r, ny_r = r_pxs + sdx, r_pys + sdy
        ok_r = (nx_r >= 0) & (nx_r < 2048) & (ny_r >= 0) & (ny_r < 2048)
        v_idx = np.where(ok_r)[0]
        if len(v_idx):
            v_idx = v_idx[reach_pt[ny_r[v_idx], nx_r[v_idx]]]
            if len(v_idx):
                mx_r = (r_pxs[v_idx] + nx_r[v_idx]) // 2
                my_r = (r_pys[v_idx] + ny_r[v_idx]) // 2
                np.minimum.at(reach_grid, (my_r, mx_r), my_r + 8)
    for ev_r in active_evs_tb:
        r_px, r_py = int(ev_r["x"]), int(ev_r["y"])
        if 0 <= r_px < 2048 and 0 <= r_py < 2048:
            reach_grid[r_py, r_px] = min(int(reach_grid[r_py, r_px]), r_py + 8)

    pad_reach = np.pad(reach_grid, ((8, 64), (16, 16)), mode="constant", constant_values=999999)
    min_rx = minimum_filter(pad_reach, size=(1, 33))[:, 16:16 + 2048]
    fy_min_reach = minimum_filter(min_rx, size=(73, 1))[36:36 + 2048, :]
    black_occ_1x = (~covered_valid_1x) & (sort_y_1x >= 0) & (sort_y_1x > fy_min_reach)
    if black_occ_1x.any():
        by_all, bx_all = np.where(black_occ_1x)
        blk_bounds: dict[int, list[int]] = {}
        for yv, xv, bv in zip(by_all.tolist(), bx_all.tolist(), sort_y_1x[by_all, bx_all].tolist()):
            b = blk_bounds.get(bv)
            if b is None:
                blk_bounds[bv] = [xv, yv, xv, yv]
            else:
                if xv < b[0]: b[0] = xv
                if yv < b[1]: b[1] = yv
                if xv > b[2]: b[2] = xv
                if yv > b[3]: b[3] = yv

        for bval in sorted(blk_bounds):
            bx0_b, by0_b, bx1_b, by1_b = blk_bounds[bval]
            sx0 = max(0, bx0_b - 3)
            sy0 = max(0, by0_b - 3)
            sx1 = min(2048, bx1_b + 4)
            sy1 = min(2048, by1_b + 4)

            sub_blk = black_occ_1x[sy0:sy1, sx0:sx1] & (sort_y_1x[sy0:sy1, sx0:sx1] == bval)
            sub_s = sort_y_1x[sy0:sy1, sx0:sx1]
            lbl_b, _ = label(sub_blk, structure=np.ones((3, 3)))
            for comp_id, slc in enumerate(find_objects(lbl_b), start=1):
                if slc is None:
                    continue
                ry0 = max(0, slc[0].start - 2)
                ry1 = min(sy1 - sy0, slc[0].stop + 2)
                rx0 = max(0, slc[1].start - 2)
                rx1 = min(sx1 - sx0, slc[1].stop + 2)
                cc_loc = lbl_b[ry0:ry1, rx0:rx1] == comp_id
                if not cc_loc.any():
                    continue
                s_loc = sub_s[ry0:ry1, rx0:rx1]
                cc_padded = cc_loc | (binary_dilation(cc_loc, iterations=1) & (s_loc >= bval))
                cc_gate = cc_padded | (binary_dilation(cc_loc, iterations=1) & (s_loc < bval))
                ys1, xs1 = np.where(cc_padded)

                ox0 = max(0, sx0 + rx0 + int(xs1.min()) - 2)
                oy0 = max(0, sy0 + ry0 + int(ys1.min()) - 2)
                ox1 = min(2048, sx0 + rx0 + int(xs1.max()) + 3)
                oy1 = min(2048, sy0 + ry0 + int(ys1.max()) + 3)
                ow4, oh4 = (ox1 - ox0) * 4, (oy1 - oy0) * 4

                cum_crop_b = sort_y_1x[oy0:oy1, ox0:ox1] >= bval
                cum_crop = cum_crop_b.astype(np.uint8) * 255
                a_cum4 = np.array(Image.fromarray(cum_crop).resize((ow4, oh4), Image.Resampling.BILINEAR))
                a_cum4 = np.clip((a_cum4.astype(np.int32) - 96) * 4, 0, 255).astype(np.uint8)
                core_4x = np.zeros((oh4, ow4), dtype=bool)
                cy_1, cx_1 = np.where(cum_crop_b)
                for dy4 in (1, 2):
                    for dx4 in (1, 2):
                        core_4x[cy_1 * 4 + dy4, cx_1 * 4 + dx4] = True
                a_cum4[core_4x] = np.maximum(a_cum4[core_4x], 240)

                gate_1x = np.zeros((oy1 - oy0, ox1 - ox0), dtype=np.uint8)
                gy0_l = (sy0 + ry0) - oy0
                gx0_l = (sx0 + rx0) - ox0
                ys_g, xs_g = np.where(cc_gate)
                gy_idx = gy0_l + ys_g
                gx_idx = gx0_l + xs_g
                in_g = (gy_idx >= 0) & (gy_idx < gate_1x.shape[0]) & (gx_idx >= 0) & (gx_idx < gx0_l + rx1 - rx0 + 8) & (gx_idx < gate_1x.shape[1])
                gate_1x[gy_idx[in_g], gx_idx[in_g]] = 255
                a_gate4 = np.array(Image.fromarray(gate_1x).resize((ow4, oh4), Image.Resampling.BILINEAR))
                a_gate4 = np.clip((a_gate4.astype(np.int32) - 64) * 4, 0, 255).astype(np.uint8)

                cov_crop_4x = np.array(
                    Image.fromarray((covered_valid_1x[oy0:oy1, ox0:ox1].astype(np.uint8) * 255)).resize(
                        (ow4, oh4), Image.Resampling.BILINEAR
                    )
                )
                void_comp_4x = np.clip((255 - cov_crop_4x.astype(np.int32) + 32) * 2, 0, 255).astype(np.uint8)
                a4 = np.minimum(np.minimum(a_cum4, a_gate4), void_comp_4x)
                if int((a4 > 0).sum()) == 0:
                    continue

                strip = np.zeros((oh4, ow4, 4), dtype=np.uint8)
                strip[:, :, 3] = a4
                occluder_strips_rgba.append(strip)
                occluder_strips_info.append({
                    "sortY": int(bval),
                    "dx": int(ox0),
                    "dy": int(oy0),
                    "dw": int(ox1 - ox0),
                    "dh": int(oy1 - oy0),
                })

    _bake_write_occluders_and_meta(
        map_id, out_dir, exported_rooms, occluder_strips_rgba, occluder_strips_info, verify=verify
    )


def _split_oversized_strip(
    strip: np.ndarray, info: dict, max_dim_4x: int = 2040
) -> list[tuple[np.ndarray, dict]]:
    """Split any occluder strip exceeding `max_dim_4x` on exact 4px boundaries so every page stays <= 2048x2048."""
    sh4, sw4 = strip.shape[:2]
    if sw4 <= max_dim_4x and sh4 <= max_dim_4x:
        return [(strip, info)]
    max_1x = max(1, max_dim_4x // 4)
    dx, dy, dw, dh = int(info["dx"]), int(info["dy"]), int(info["dw"]), int(info["dh"])
    sort_y = int(info["sortY"])
    out: list[tuple[np.ndarray, dict]] = []
    for y0 in range(0, dh, max_1x):
        sub_h = min(max_1x, dh - y0)
        for x0 in range(0, dw, max_1x):
            sub_w = min(max_1x, dw - x0)
            sub_rgba = strip[y0 * 4:(y0 + sub_h) * 4, x0 * 4:(x0 + sub_w) * 4]
            if not (sub_rgba[:, :, 3] > 0).any():
                continue
            out.append((
                sub_rgba,
                {
                    "sortY": sort_y,
                    "dx": dx + x0,
                    "dy": dy + y0,
                    "dw": sub_w,
                    "dh": sub_h,
                },
            ))
    return out


def pack_spatial_occluder_pages(
    occluder_strips_rgba: list[np.ndarray],
    occluder_strips_info: list[dict],
    out_dir: Path,
    max_page_dim: int = 2048,
    pad: int = 4,
) -> tuple[list[str], list[dict], list[tuple[int, int]]]:
    """Shelf-pack foreground occluder strips into spatial 512x512-cell WebP pages strictly <= `max_page_dim`x`max_page_dim`.

    Spatial bucketing guarantees that any 320x200 viewport only streams 1..4 small <=16 MB RGBA pages
    instead of decoding a monolithic 12288x5728 (281 MB RGBA) occluder atlas.
    """
    max_strip_dim = max_page_dim - pad * 2
    buckets: dict[tuple[int, int], list[tuple[np.ndarray, dict]]] = {}
    for strip, info in zip(occluder_strips_rgba, occluder_strips_info):
        for sub_strip, sub_info in _split_oversized_strip(strip, info, max_dim_4x=max_strip_dim):
            cx = int(sub_info["dx"]) + int(sub_info["dw"]) // 2
            cy = int(sub_info["dy"]) + int(sub_info["dh"]) // 2
            cell_r = min(3, max(0, cy // 512))
            cell_c = min(3, max(0, cx // 512))
            buckets.setdefault((cell_r, cell_c), []).append((sub_strip, sub_info))

    page_files: list[str] = []
    page_dims: list[tuple[int, int]] = []
    strips_meta: list[dict] = []

    for (cell_r, cell_c) in sorted(buckets.keys()):
        ordered = sorted(buckets[(cell_r, cell_c)], key=lambda kv: (-kv[0].shape[0], -kv[0].shape[1]))
        sub_pages: list[list[tuple[int, int, int, int, np.ndarray, dict]]] = []
        cur_placements: list[tuple[int, int, int, int, np.ndarray, dict]] = []
        cur_x, cur_y, row_h = pad, pad, 0

        for strip, info in ordered:
            sh4, sw4 = strip.shape[:2]
            if cur_x + sw4 + pad > max_page_dim:
                cur_x = pad
                cur_y += row_h + pad
                row_h = 0
            if cur_y + sh4 + pad > max_page_dim:
                if cur_placements:
                    sub_pages.append(cur_placements)
                cur_placements = []
                cur_x, cur_y, row_h = pad, pad, 0
            cur_placements.append((cur_x, cur_y, sw4, sh4, strip, info))
            cur_x += sw4 + pad
            row_h = max(row_h, sh4)
        if cur_placements:
            sub_pages.append(cur_placements)

        for p_idx, placements in enumerate(sub_pages):
            used_w = max(sx + sw4 + pad for sx, _, sw4, _, _, _ in placements)
            used_h = max(sy + sh4 + pad for _, sy, _, sh4, _, _ in placements)
            page_w = min(max_page_dim, max(16, ((used_w + 3) // 4) * 4))
            page_h = min(max_page_dim, max(16, ((used_h + 3) // 4) * 4))
            atlas = np.zeros((page_h, page_w, 4), dtype=np.uint8)
            page_global_idx = len(page_files)
            fname = f"occ_r{cell_r}_c{cell_c}_p{p_idx}_4x.webp"
            for sx, sy, sw4, sh4, strip, info in placements:
                atlas[sy:sy + sh4, sx:sx + sw4] = strip
                strips_meta.append({
                    "page": page_global_idx,
                    "sortY": int(info["sortY"]),
                    "sx": int(sx),
                    "sy": int(sy),
                    "sw": int(sw4),
                    "sh": int(sh4),
                    "dx": int(info["dx"]),
                    "dy": int(info["dy"]),
                    "dw": int(info["dw"]),
                    "dh": int(info["dh"]),
                })
            Image.fromarray(atlas).save(
                out_dir / fname, "WEBP", quality=92, alpha_quality=100, exact=True, method=4
            )
            page_files.append(fname)
            page_dims.append((page_w, page_h))

    # Remove legacy monolithic occluders_4x.webp and any stale occ_r*_4x.webp pages
    keep_occ = set(page_files)
    legacy_occ = out_dir / "occluders_4x.webp"
    if legacy_occ.exists():
        legacy_occ.unlink()
    for stale_occ in out_dir.glob("occ_*_4x.webp"):
        if stale_occ.name not in keep_occ:
            stale_occ.unlink()

    return page_files, strips_meta, page_dims


def _bake_write_occluders_and_meta(
    map_id: int,
    out_dir: Path,
    exported_rooms: list[dict],
    occluder_strips_rgba: list[np.ndarray],
    occluder_strips_info: list[dict],
    verify: bool,
):
    page_files, strips_meta, page_dims = pack_spatial_occluder_pages(
        occluder_strips_rgba, occluder_strips_info, out_dir
    )
    meta_out = {
        "mapId": map_id,
        "scale": 4,
        "rooms": exported_rooms,
        "occluders": {
            "pages": page_files,
            "strips": strips_meta,
        },
    }
    keep_files = {r["file"] for r in exported_rooms} | set(page_files)
    for stale_wp in out_dir.glob("isl*_4x.webp"):
        if stale_wp.name not in keep_files:
            stale_wp.unlink()
    (out_dir / "meta.json").write_text(json.dumps(meta_out, separators=(",", ":")))
    max_dim = max((max(w, h) for w, h in page_dims), default=0)
    print(
        f"  [Map {map_id:03d}] Exported {len(exported_rooms)} HD chunk(s) + "
        f"{len(strips_meta)} occluders across {len(page_files)} spatial page(s) (max {max_dim}px)"
    )
    if verify:
        verify_map(map_id, out_dir=out_dir)


def verify_map(
    map_id: int,
    scenes=None,
    events=None,
    all_scripts=None,
    out_dir: Path | None = None,
    check_photometric: bool = True,
) -> dict:
    """Hard-gate verification of an HD map pack:
    1) Asserts 0 interior `alpha <= 200` holes across every chunk WebP in `maps_hd/map_<ID>/`.
    2) Runs Differential Walkable Silhouette Sweep across all reachable walkable half-tiles & active EventObjects,
       comparing 1x ground-truth `sort_y_1x > py + 8` vs reconstructed HD `occluders_4x.webp` `strip.sortY > py + 8`
       inside `[px - 10..px + 10, py - 42..py + 4]`, failing if ANY spot has `FN > 16` or `FP > 16`.
    3) Canonical Photometric & Contrast Consistency Gate: asserts stitched HD map stays anchored to universal
       canonical target `T(ref_1x)` (`|dL| <= 3.0, |da| <= 3.0, |db| <= 3.0, dE <= 4.0, Lstd_ratio in [0.72, 1.45]`).
    4) Anti-Hallucination Local Structural Block Gate: asserts no 128x128 valid block has structural RGB MAE > 42.0
       against `T_rgb(ref_1x)` (catching geometry hallucinations such as fake cliffs/stairs over open water).
    """
    if out_dir is None:
        out_dir = GFX / "maps_hd" / f"map_{map_id:03d}"
    meta_path = out_dir / "meta.json"
    if not meta_path.exists():
        raise FileNotFoundError(f"Missing HD map meta.json: {meta_path}")
    meta = json.loads(meta_path.read_text())
    map_scale = max(1, int(meta.get("scale", 4)))

    # 1. Zero-Hole Alpha Check across all chunk/room WebPs + gather 1x downsampled RGB/alpha for Gates #3 & #4
    rooms = meta.get("rooms", [])
    chunk_files = sorted({rm["file"] for rm in rooms} | {p.name for p in out_dir.glob("isl*_c*_4x.webp")})
    hole_failures = []
    hd_1x_sum = np.zeros((2048, 2048, 3), dtype=np.float32) if check_photometric else None
    hd_1x_wt = np.zeros((2048, 2048), dtype=np.float32) if check_photometric else None
    room_by_file: dict[str, list[dict]] = {}
    for rm in rooms:
        room_by_file.setdefault(rm["file"], []).append(rm)

    for cfile in chunk_files:
        cp = out_dir / cfile
        if not cp.exists():
            raise FileNotFoundError(f"Missing chunk WebP referenced in map_{map_id:03d}: {cp}")
        arr = np.array(Image.open(cp).convert("RGBA"))
        alpha = arr[:, :, 3]
        holes = binary_fill_holes(alpha > 200) & (alpha <= 200)
        n_holes = int(holes.sum())
        if n_holes > 0:
            hy, hx = np.where(holes)
            hole_failures.append(
                f"{cfile}: {n_holes} interior hole px (bbox=[{int(hx.min())}..{int(hx.max())}, {int(hy.min())}..{int(hy.max())}])"
            )
        if check_photometric and cfile in room_by_file:
            for rm in room_by_file[cfile]:
                rx, ry, rw, rh = int(rm["x"]), int(rm["y"]), int(rm["w"]), int(rm["h"])
                if arr.shape[0] == rh * map_scale and arr.shape[1] == rw * map_scale:
                    a1 = alpha.astype(np.float32).reshape(rh, map_scale, rw, map_scale).mean(axis=(1, 3)) * (1.0 / 255.0)
                    rgb1 = arr[:, :, :3].astype(np.float32).reshape(rh, map_scale, rw, map_scale, 3).mean(axis=(1, 3))
                    hd_1x_sum[ry:ry + rh, rx:rx + rw] += rgb1 * a1[:, :, None]
                    hd_1x_wt[ry:ry + rh, rx:rx + rw] += a1

    # 2. Differential Walkable Silhouette Sweep
    if scenes is None or events is None or all_scripts is None:
        scenes, events, all_scripts = load_game_tables()
    full_1x, sort_y_1x, tile_occ_1x = render_map_1x_and_sort_y(map_id)
    vis_half_tiles, active_evs, _ = compute_reachable_walkable_and_events(map_id, scenes, events, all_scripts)

    occ_info = meta.get("occluders") or {}
    strips = occ_info.get("strips") or []
    occ_pages_raw = occ_info.get("pages")
    if isinstance(occ_pages_raw, list):
        occ_page_files = list(occ_pages_raw)
        enforce_page_cap = True
    else:
        occ_page_files = [occ_info.get("file", "occluders_4x.webp")]
        enforce_page_cap = False

    occ_alphas_4x: list[np.ndarray] = []
    page_dim_failures: list[str] = []
    for pfile in occ_page_files:
        ppath = out_dir / pfile
        if ppath.exists() and strips:
            parr = np.array(Image.open(ppath).convert("RGBA"))
            if enforce_page_cap and (parr.shape[0] > 2048 or parr.shape[1] > 2048):
                page_dim_failures.append(f"{pfile}: {parr.shape[1]}x{parr.shape[0]} exceeds 2048x2048 cap")
            occ_alphas_4x.append(parr[:, :, 3])
        else:
            occ_alphas_4x.append(np.zeros((1, 1), dtype=np.uint8))

    # Index strips into coarse 64px Y-buckets for O(1) per-position lookup
    y_buckets: dict[int, list[tuple[int, int, int, int, int, int, int, int]]] = {}
    for st in strips:
        dx, dy, dw, dh = int(st["dx"]), int(st["dy"]), int(st["dw"]), int(st["dh"])
        sx, sy = int(st["sx"]), int(st["sy"])
        sy_val = int(st["sortY"])
        pidx = int(st.get("page", 0))
        rec = (sy_val, dx, dy, dx + dw, dy + dh, sx, sy, pidx)
        for b in range(dy // 64, (dy + dh) // 64 + 1):
            y_buckets.setdefault(b, []).append(rec)

    test_spots: list[tuple[int, int, str]] = [
        (tx * 32 + h * 16, ty * 16 + h * 8, "walk") for (ty, tx, h) in sorted(vis_half_tiles)
    ]
    seen_xy = {(px, py) for px, py, _ in test_spots}
    for e in active_evs:
        px, py = int(e["x"]), int(e["y"])
        if (px, py) not in seen_xy and 0 <= px < 2048 and 0 <= py < 2048:
            test_spots.append((px, py, f"ev#{e['id']}"))
            seen_xy.add((px, py))

    max_fn, max_fp = 0, 0
    occ_failures = []
    for px, py, tag in test_spots:
        x0, x1 = max(0, px - 10), min(2048, px + 11)
        y0, y1 = max(0, py - 42), min(2048, py + 5)
        if x1 <= x0 or y1 <= y0:
            continue
        char_sy = py + 8
        gt_1x = sort_y_1x[y0:y1, x0:x1] > char_sy

        # Gather overlapping HD strips with `strip.sortY > py + 8`
        cand_strips = []
        seen_id = set()
        for b in range(y0 // 64, (y1 - 1) // 64 + 1):
            for rec in y_buckets.get(b, ()):
                rid = id(rec)
                if rid in seen_id:
                    continue
                sy_val, s_x0, s_y0, s_x1, s_y1, _, _, _ = rec
                if sy_val > char_sy and s_x1 > x0 and s_x0 < x1 and s_y1 > y0 and s_y0 < y1:
                    seen_id.add(rid)
                    cand_strips.append(rec)

        if not gt_1x.any() and not cand_strips:
            continue

        wh, ww = y1 - y0, x1 - x0
        if not cand_strips:
            hd_1x_occ = np.zeros((wh, ww), dtype=bool)
        else:
            trans_s = np.ones((wh * map_scale, ww * map_scale), dtype=np.float32)
            for _, s_x0, s_y0, s_x1, s_y1, sx, sy, pidx in cand_strips:
                ix0_c, ix1_c = max(x0, s_x0), min(x1, s_x1)
                iy0_c, iy1_c = max(y0, s_y0), min(y1, s_y1)
                dy_d0, dy_d1 = (iy0_c - y0) * map_scale, (iy1_c - y0) * map_scale
                dx_d0, dx_d1 = (ix0_c - x0) * map_scale, (ix1_c - x0) * map_scale
                dy_s0, dy_s1 = sy + (iy0_c - s_y0) * map_scale, sy + (iy1_c - s_y0) * map_scale
                dx_s0, dx_s1 = sx + (ix0_c - s_x0) * map_scale, sx + (ix1_c - s_x0) * map_scale
                a_src = occ_alphas_4x[pidx][dy_s0:dy_s1, dx_s0:dx_s1].astype(np.float32) / 255.0
                trans_s[dy_d0:dy_d1, dx_d0:dx_d1] *= 1.0 - a_src
            hd_cov_1x = (1.0 - trans_s).reshape(wh, map_scale, ww, map_scale).mean(axis=(1, 3))
            hd_1x_occ = hd_cov_1x >= 0.5

        fn = int((gt_1x & (~hd_1x_occ)).sum())
        fp = int(((~gt_1x) & hd_1x_occ).sum())
        if fn > max_fn:
            max_fn = fn
        if fp > max_fp:
            max_fp = fp
        if fn > 16 or fp > 16:
            occ_failures.append((fn + fp, px, py, tag, fn, fp))

    # 3 & 4. Canonical Photometric Consistency Gate + Anti-Hallucination Local Structural Block Gate
    photo_failures = []
    block_failures = []
    photo_metrics = {}
    if check_photometric and hd_1x_wt is not None:
        v1 = (hd_1x_wt > 0.5) & tile_occ_1x & (full_1x[:, :, :3].max(axis=2) > 4)
        if int(v1.sum()) >= 500:
            hd_rgb_1x = np.clip(hd_1x_sum / np.maximum(hd_1x_wt[:, :, None], 1e-4), 0.0, 255.0)
            ref_lab = rgb_to_lab_f32(full_1x[:, :, :3])
            hd_lab = rgb_to_lab_f32(hd_rgb_1x)
            L_t, a_t, b_t = canonical_lab_target(ref_lab)
            dL_c = float((hd_lab[:, :, 0][v1] - L_t[v1]).mean())
            da_c = float((hd_lab[:, :, 1][v1] - a_t[v1]).mean())
            db_c = float((hd_lab[:, :, 2][v1] - b_t[v1]).mean())
            dE_c = math.sqrt(dL_c * dL_c + da_c * da_c + db_c * db_c)
            ref_std = float(ref_lab[:, :, 0][v1].std())
            hd_std = float(hd_lab[:, :, 0][v1].std())
            lstd_ratio = (hd_std / ref_std) if ref_std >= 4.0 else 1.0

            # Anti-hallucination block check on 128x128 blocks
            t_rgb = lab_to_rgb_u8(np.stack([L_t, a_t, b_t], axis=-1)).astype(np.float32)
            abs_err = np.abs(hd_rgb_1x - t_rgb).mean(axis=2)
            max_blk_mae = 0.0
            max_blk_xy = (0, 0)
            for by in range(0, 2048, 128):
                for bx in range(0, 2048, 128):
                    bm = v1[by:by + 128, bx:bx + 128]
                    if int(bm.sum()) < 2500:
                        continue
                    b_mae = float(abs_err[by:by + 128, bx:bx + 128][bm].mean())
                    if b_mae > max_blk_mae:
                        max_blk_mae = b_mae
                        max_blk_xy = (bx, by)
                    if b_mae > 42.0:
                        block_failures.append((b_mae, bx, by))

            photo_metrics = {
                "dL_canon": round(dL_c, 2),
                "da_canon": round(da_c, 2),
                "db_canon": round(db_c, 2),
                "dE_canon": round(dE_c, 2),
                "Lstd_ratio": round(lstd_ratio, 2),
                "max_blk_mae": round(max_blk_mae, 1),
                "max_blk_xy": max_blk_xy,
            }
            if abs(dL_c) > 3.0 or abs(da_c) > 3.0 or abs(db_c) > 3.0 or dE_c > 4.0:
                photo_failures.append(
                    f"Canonical Lab drift out of bounds: dL={dL_c:+.2f}, da={da_c:+.2f}, db={db_c:+.2f}, dE={dE_c:.2f} (limit: |d*|<3.0, dE<4.0)"
                )
            if ref_std >= 4.0 and not (0.72 <= lstd_ratio <= 1.45):
                photo_failures.append(
                    f"Contrast Lstd_ratio={lstd_ratio:.2f} out of bounds [0.72, 1.45] (ref_std={ref_std:.2f}, hd_std={hd_std:.2f})"
                )

    if hole_failures or occ_failures or photo_failures or block_failures or page_dim_failures:
        occ_failures.sort(reverse=True)
        block_failures.sort(reverse=True)
        msg_lines = [f"[VERIFY FAILED] Map {map_id:03d}:"]
        if hole_failures:
            msg_lines.append(f"  Interior alpha<=200 holes ({len(hole_failures)} chunk(s)): " + "; ".join(hole_failures))
        if page_dim_failures:
            msg_lines.append(f"  Occluder page cap (>2048x2048): " + "; ".join(page_dim_failures))
        if occ_failures:
            worst = ", ".join(
                f"({px},{py}) [{tag}] FN={fn} FP={fp}" for _, px, py, tag, fn, fp in occ_failures[:8]
            )
            msg_lines.append(
                f"  Occlusion sweep failures ({len(occ_failures)}/{len(test_spots)} spots, max_FN={max_fn}, max_FP={max_fp}): {worst}"
            )
        if photo_failures:
            msg_lines.append("  Photometric gate: " + "; ".join(photo_failures))
        if block_failures:
            worst_b = ", ".join(f"({bx},{by}) MAE={m:.1f}" for m, bx, by in block_failures[:6])
            msg_lines.append(f"  Anti-hallucination block failures (>42.0): {worst_b}")
        err_msg = "\n".join(msg_lines)
        print(err_msg)
        raise RuntimeError(err_msg)

    pm_str = (
        f" | canon dE={photo_metrics['dE_canon']:.2f} (dL={photo_metrics['dL_canon']:+.2f}, "
        f"LstdR={photo_metrics['Lstd_ratio']:.2f}, maxBlkMAE={photo_metrics['max_blk_mae']:.1f})"
        if photo_metrics
        else ""
    )
    print(
        f"  [Verify PASS] Map {map_id:03d}: {len(chunk_files)} chunk(s) 0 holes | "
        f"{len(test_spots)} reachable spots swept (max_FN={max_fn}<=16, max_FP={max_fp}<=16){pm_str}"
    )
    return {
        "map_id": map_id,
        "chunks_checked": len(chunk_files),
        "spots_swept": len(test_spots),
        "max_fn": max_fn,
        "max_fp": max_fp,
        **photo_metrics,
    }


# ---------------------------------------------------------------------------
# Sprite & Portrait Packing and De-Matting
# ---------------------------------------------------------------------------

def extract_sprite_or_portrait_cell(
    cell_rgb: np.ndarray,
    ref_1x_rgba: np.ndarray,
    target_w4: int,
    target_h4: int,
    expected_bg: tuple[int, int, int],
    is_prop: bool = False,
    is_portrait: bool = False,
) -> np.ndarray:
    border_px = np.concatenate([
        cell_rgb[:4, :, :].reshape(-1, 3),
        cell_rgb[-4:, :, :].reshape(-1, 3),
        cell_rgb[:, :4, :].reshape(-1, 3),
        cell_rgb[:, -4:, :].reshape(-1, 3),
    ], axis=0)
    d_exp = np.linalg.norm(border_px.astype(np.float32) - np.array(expected_bg, dtype=np.float32), axis=1)
    good_b = border_px[d_exp < 35.0]
    bg_col = np.median(good_b, axis=0) if len(good_b) > 20 else np.array(expected_bg, dtype=np.float32)

    sil_1x = (ref_1x_rgba[:, :, 3] > 128).astype(np.uint8) * 255
    sil_4x = np.array(Image.fromarray(sil_1x).resize((target_w4, target_h4), Image.Resampling.BILINEAR)) > 96
    if not sil_4x.any():
        return np.zeros((target_h4, target_w4, 4), dtype=np.uint8)

    dil_r = 8 if is_portrait else (4 if is_prop else 6)
    ero_r = 4 if is_portrait else 2
    outer_gate = binary_dilation(sil_4x, iterations=dil_r)
    inner_core = binary_erosion(sil_4x, iterations=ero_r)

    diff_bg = cell_rgb.astype(np.float32) - bg_col[None, None, :]
    dist = np.sqrt(np.sum(diff_bg * diff_bg, axis=2))

    raw_a = np.clip((dist - 13.0) / 20.0, 0.0, 1.0)
    raw_a[~outer_gate] = 0.0
    raw_a[inner_core] = 1.0

    fg_bin = raw_a > 0.15
    lbl, n_cc = label(fg_bin)
    if n_cc > 1:
        keep_mask = np.zeros_like(fg_bin)
        for cid in range(1, n_cc + 1):
            comp = lbl == cid
            if (comp & sil_4x).any():
                keep_mask |= comp
        raw_a[~keep_mask] = 0.0

    rgb_clean = cell_rgb.astype(np.float32).copy()
    edge = (raw_a > 0.08) & (raw_a < 0.95)
    if edge.any():
        a_safe = np.maximum(0.25, raw_a[edge])[:, None]
        rgb_clean[edge] = np.clip((rgb_clean[edge] - (1.0 - a_safe) * bg_col[None, :]) / a_safe, 0.0, 255.0)

    out = np.zeros((target_h4, target_w4, 4), dtype=np.uint8)
    out[:, :, :3] = np.round(rgb_clean).clip(0, 255).astype(np.uint8)
    out[:, :, 3] = np.round(raw_a * 255.0).clip(0, 255).astype(np.uint8)

    if not is_portrait and not is_prop:
        ys_ref, xs_ref = np.where(sil_4x)
        ys_cur, xs_cur = np.where(out[:, :, 3] > 128)
        if len(ys_ref) > 10 and len(ys_cur) > 10:
            dx = max(-4, min(4, int(round(float(xs_ref.mean() - xs_cur.mean())))))
            dy = max(-4, min(4, int(ys_ref.max() - ys_cur.max())))
            if dx != 0 or dy != 0:
                shifted = np.zeros_like(out)
                y0_d, y1_d = max(0, dy), min(target_h4, target_h4 + dy)
                y0_s, y1_s = max(0, -dy), min(target_h4, target_h4 - dy)
                x0_d, x1_d = max(0, dx), min(target_w4, target_w4 + dx)
                x0_s, x1_s = max(0, -dx), min(target_w4, target_w4 - dx)
                shifted[y0_d:y1_d, x0_d:x1_d] = out[y0_s:y1_s, x0_s:x1_s]
                out = shifted

    return extrude_rgb_voronoi(out, iters=5)


def prepare_sprite_sheets(scene_id: int, todo_mgos: list[int], mgo_char_hint: dict[int, bool], work_dir: Path) -> list[dict]:
    if not todo_mgos:
        return []
    CW, CH = 1376, 768
    BG = (46, 62, 72, 255)

    # Collect frame lists per MGO; keep 12-frame walking MGOs contiguous on 9x4 grids (cols divisible by 3!)
    walk_mgos = []
    other_frames = []
    for mid in todo_mgos:
        jp = GFX / "mgo" / f"mgo_{mid:03d}.json"
        if not jp.exists():
            continue
        meta = json.loads(jp.read_text())
        im1 = np.array(Image.open(GFX / "mgo" / f"mgo_{mid:03d}_1x.webp").convert("RGBA"))
        valid_frames = [(fi, r) for fi, r in enumerate(meta["frames"]) if r[2] > 0 and r[3] > 0]
        if not valid_frames:
            continue
        is_char = mgo_char_hint.get(mid, False) or len(valid_frames) in (12, 16)
        max_w = max(r[2] for _, r in valid_frames)
        max_h = max(r[3] for _, r in valid_frames)
        if len(valid_frames) == 12 and max_w <= 48 and max_h <= 64:
            walk_mgos.append((mid, not is_char, valid_frames))
        else:
            for fi, r in valid_frames:
                other_frames.append((mid, fi, r, not is_char))

    sheets_meta = []
    todo_items = []
    sheet_idx = 0

    def emit_sheet(frames_batch: list[tuple[int, int, list[int], bool]], cols: int, rows: int):
        nonlocal sheet_idx
        sheet_idx += 1
        sname = f"sc{scene_id:03d}_sprites_{sheet_idx:02d}"
        cell_w, cell_h = CW // cols, CH // rows
        canvas = Image.new("RGBA", (CW, CH), BG)
        draw = ImageDraw.Draw(canvas)
        for c in range(1, cols):
            draw.line([(c * cell_w, 0), (c * cell_w, CH)], fill=(34, 46, 54, 255), width=2)
        for r in range(1, rows):
            draw.line([(0, r * cell_h), (CW, r * cell_h)], fill=(34, 46, 54, 255), width=2)

        placements = []
        im1_cache: dict[int, np.ndarray] = {}
        for slot, (mid, fi, rect, is_prop) in enumerate(frames_batch):
            if mid not in im1_cache:
                im1_cache[mid] = np.array(Image.open(GFX / "mgo" / f"mgo_{mid:03d}_1x.webp").convert("RGBA"))
            sx, sy, sw, sh = rect
            raw_crop = im1_cache[mid][sy:sy + sh, sx:sx + sw]
            clean_crop, stripped = strip_dos_shadow_1x(raw_crop, is_prop=is_prop)
            eff_is_prop = is_prop and not stripped
            scale = 4
            while (sw * scale > cell_w - 16 or sh * scale > cell_h - 16) and scale > 1:
                scale -= 1
            tw = min(cell_w - 12, sw * scale)
            th = min(cell_h - 12, sh * scale)
            up = Image.fromarray(clean_crop).resize((tw, th), Image.Resampling.NEAREST)
            col, row = slot % cols, slot // cols
            ox = col * cell_w + (cell_w - tw) // 2
            oy = row * cell_h + (cell_h - th) // 2
            canvas.alpha_composite(up, (ox, oy))
            placements.append({
                "mgo_id": mid,
                "frame_idx": fi,
                "orig_rect": rect,
                "is_prop": eff_is_prop,
                "shadow_stripped": stripped or (not eff_is_prop),
                "paste_box": [ox, oy, tw, th],
            })

        in_path = work_dir / f"{sname}_input_1376x768.png"
        canvas.convert("RGB").save(in_path)
        (work_dir / f"{sname}_meta.json").write_text(json.dumps(placements, indent=2))
        sheets_meta.append(sname)
        prompt = (
            f"Faithful 2D HD hand-painted Chinese classic RPG sprite sheet remaster of this exact {cols}x{rows} grid "
            f"(from 1995 Chinese Paladin / Xianjian Qixiazhuan). "
            f"CRITICAL CONSTRAINTS: Keep the exact {cols}x{rows} grid layout, keep every sprite inside its exact cell "
            f"at the exact same pixel position, pose, facing direction, silhouette size, and foot baseline! "
            f"Back-facing character frames (showing only hair / back of head) MUST remain back-facing with NO face or "
            f"front headband drawn on the back of the head! Keep the flat solid dark slate-teal background (#2E3E48) "
            f"completely uniform and clean behind every sprite (no drop shadows, no floor, no glow). "
            f"Upgrade pixelated 1995 sprites into crisp, clean high-resolution hand-painted Chinese RPG art with clean "
            f"ink linework, smooth fabric/material shading, and consistent outfits across frames. Output 1376x768."
        )
        todo_items.append({
            "key": sname,
            "kind": "sprite_sheet",
            "input_path": str(in_path),
            "prompt": prompt,
        })
        print(f"  [Sprites] {sname}: {len(frames_batch)}/{cols*rows} frames ({cols}x{rows})")

    # Pack 12-frame walking characters 3-at-a-time on 9x4 (36 slots, cols=9 is divisible by 3!)
    for i in range(0, len(walk_mgos), 3):
        chunk = walk_mgos[i:i + 3]
        batch = []
        for mid, is_prop, vframes in chunk:
            for fi, r in vframes:
                batch.append((mid, fi, r, is_prop))
        rows = max(2, math.ceil(len(batch) / 9))
        emit_sheet(batch, cols=9, rows=min(4, rows))

    # Pack remaining props / special poses adaptively based on frame size
    idx = 0
    while idx < len(other_frames):
        # Peek max dimensions in next window
        rem = other_frames[idx:idx + 32]
        mw = max(x[2][2] for x in rem[:8])
        mh = max(x[2][3] for x in rem[:8])
        if mw > 96 or mh > 96:
            cols, rows = 4, 2
        elif mw > 56 or mh > 68:
            cols, rows = 6, 3
        else:
            cols, rows = 8, 4
        cap = cols * rows
        # Check all items in batch fit at >=2x if small enough
        batch = []
        while idx < len(other_frames) and len(batch) < cap:
            cand = other_frames[idx]
            if len(batch) > 0 and (cand[2][2] * 2 > (CW // cols) - 12 or cand[2][3] * 2 > (CH // rows) - 12) and cols > 4:
                break
            batch.append(cand)
            idx += 1
        emit_sheet(batch, cols=cols, rows=rows)

    (work_dir / "sprite_sheets_list.json").write_text(json.dumps(sheets_meta, indent=2))
    return todo_items


def harmonize_back_triplets(frames_dict: dict[int, np.ndarray]):
    """Auto-detect and fix 1-in-3 back-of-head bright headband bleed on 12-frame directional walk cycles."""
    if len(frames_dict) != 12:
        return
    for base in (3, 6):  # NW back-left [3,4,5] and NE back-right [6,7,8]
        trip = [frames_dict.get(base + k) for k in range(3)]
        if any(f is None for f in trip):
            continue
        shapes = {f.shape for f in trip}
        if len(shapes) != 1:
            continue
        h4, w4 = trip[0].shape[:2]
        head_h = min(int(h4 * 0.34), h4)
        bright_counts = []
        for f in trip:
            hd = f[:head_h]
            cnt = int(((hd[:, :, 3] > 128) & (hd[:, :, 0] > 170) & (hd[:, :, 1] > 170) & (hd[:, :, 2] > 170)).sum())
            bright_counts.append(cnt)
        med = sorted(bright_counts)[1]
        for k in range(3):
            if bright_counts[k] > max(120, int(med * 2.1)):
                # Outlier frame `base + k` — harmonize upper head from sibling with closest count to median
                donor_k = min((j for j in range(3) if j != k), key=lambda j: abs(bright_counts[j] - med))
                donor = trip[donor_k]
                target = trip[k]
                a_d = (donor[:head_h, :, 3] > 128).astype(np.float32)
                a_t = (target[:head_h, :, 3] > 128).astype(np.float32)
                best_iou, bdx, bdy = -1.0, 0, 0
                for dy in range(-6, 7):
                    for dx in range(-8, 9):
                        s = np.zeros_like(a_d)
                        y0d, y1d = max(0, dy), min(head_h, head_h + dy)
                        y0s, y1s = max(0, -dy), min(head_h, head_h - dy)
                        x0d, x1d = max(0, dx), min(w4, w4 + dx)
                        x0s, x1s = max(0, -dx), min(w4, w4 - dx)
                        s[y0d:y1d, x0d:x1d] = a_d[y0s:y1s, x0s:x1s]
                        iou = float((s * a_t).sum() / max(1.0, ((s + a_t) > 0).sum()))
                        if iou > best_iou:
                            best_iou, bdx, bdy = iou, dx, dy
                if best_iou >= 0.88:
                    d_shift = np.zeros_like(donor)
                    y0d, y1d = max(0, bdy), min(h4, h4 + bdy)
                    y0s, y1s = max(0, -bdy), min(h4, h4 - bdy)
                    x0d, x1d = max(0, bdx), min(w4, w4 + bdx)
                    x0s, x1s = max(0, -bdx), min(w4, w4 - bdx)
                    d_shift[y0d:y1d, x0d:x1d] = donor[y0s:y1s, x0s:x1s]
                    fixed = target.copy()
                    sy0 = max(8, head_h - 8)
                    fixed[:sy0] = d_shift[:sy0]
                    for ry in range(sy0, head_h):
                        wo = (ry - sy0) / max(1.0, float(head_h - sy0))
                        fixed[ry] = np.clip(
                            np.round((1.0 - wo) * d_shift[ry].astype(np.float32) + wo * target[ry].astype(np.float32)),
                            0, 255
                        ).astype(np.uint8)
                    frames_dict[base + k] = extrude_rgb_voronoi(fixed, iters=4)


def bake_sprites_hd(work_dir: Path, ai_map: dict[str, Path]):
    list_p = work_dir / "sprite_sheets_list.json"
    if not list_p.exists():
        return
    sheet_names = json.loads(list_p.read_text())
    extracted: dict[int, dict[int, tuple[np.ndarray, bool]]] = {}

    for sname in sheet_names:
        ai_p = ai_map.get(sname)
        if not ai_p or not ai_p.exists():
            raise FileNotFoundError(f"Missing AI sprite sheet output for {sname}: {ai_p}")
        placements = json.loads((work_dir / f"{sname}_meta.json").read_text())
        im_ai = Image.open(ai_p).convert("RGB")
        if im_ai.size != (1376, 768):
            im_ai = im_ai.resize((1376, 768), Image.Resampling.LANCZOS)
        arr_ai = np.array(im_ai)

        im1_cache: dict[int, np.ndarray] = {}
        for pl in placements:
            mid = pl["mgo_id"]
            fi = pl["frame_idx"]
            sx, sy, sw, sh = pl["orig_rect"]
            is_prop = pl["is_prop"]
            sh_strip = pl["shadow_stripped"]
            px, py, pw, ph = pl["paste_box"]

            if mid not in im1_cache:
                im1_cache[mid] = np.array(Image.open(GFX / "mgo" / f"mgo_{mid:03d}_1x.webp").convert("RGBA"))
            ref_1x, _ = strip_dos_shadow_1x(im1_cache[mid][sy:sy + sh, sx:sx + sw], is_prop=is_prop)

            sub_ai = Image.fromarray(arr_ai[py:py + ph, px:px + pw])
            if sub_ai.size != (sw * 4, sh * 4):
                sub_ai = sub_ai.resize((sw * 4, sh * 4), Image.Resampling.LANCZOS)
            sub_ai = sub_ai.filter(ImageFilter.UnsharpMask(radius=0.9, percent=50, threshold=2))

            rgba_4x = extract_sprite_or_portrait_cell(
                np.array(sub_ai), ref_1x, sw * 4, sh * 4,
                expected_bg=(46, 62, 72), is_prop=is_prop, is_portrait=False
            )
            extracted.setdefault(mid, {})[fi] = (rgba_4x, sh_strip)

    for mid in sorted(extracted):
        jp = GFX / "mgo" / f"mgo_{mid:03d}.json"
        meta = json.loads(jp.read_text())
        w4, h4 = meta["w"] * 4, meta["h"] * 4
        p4 = GFX / "mgo" / f"mgo_{mid:03d}_4x.webp"

        fdict = {fi: fr for fi, (fr, _) in extracted[mid].items()}
        any_stripped = any(st for _, st in extracted[mid].values())
        if any_stripped:
            harmonize_back_triplets(fdict)

        atlas_4x = np.zeros((h4, w4, 4), dtype=np.uint8)
        for fi, rect in enumerate(meta["frames"]):
            sx, sy, sw, sh = rect
            if sw <= 0 or sh <= 0:
                continue
            if fi in fdict:
                atlas_4x[sy * 4:(sy + sh) * 4, sx * 4:(sx + sw) * 4] = fdict[fi]
        Image.fromarray(atlas_4x).save(p4, "WEBP", lossless=True, method=4)
        meta["shadowStripped"] = bool(any_stripped)
        meta["hdTrackB"] = True
        jp.write_text(json.dumps(meta))
        print(f"  [MGO] Updated mgo_{mid:03d}_4x.webp ({w4}x{h4}, shadowStripped={any_stripped})")


def prepare_portrait_sheets(scene_id: int, todo_rgms: list[int], work_dir: Path) -> list[dict]:
    if not todo_rgms:
        return []
    CW, CH = 1376, 768
    BG = (22, 30, 36, 255)
    rgm_meta = json.loads((GFX / "ui" / "rgm.json").read_text())["frames"]
    rgm1 = Image.open(GFX / "ui" / "rgm_1x.webp").convert("RGBA")

    sheets_meta = []
    todo_items = []
    cols, rows = 5, 2
    cell_w, cell_h = CW // cols, CH // rows

    for s_idx, i in enumerate(range(0, len(todo_rgms), 10), start=1):
        pids = todo_rgms[i:i + 10]
        sname = f"sc{scene_id:03d}_portraits_{s_idx:02d}"
        canvas = Image.new("RGBA", (CW, CH), BG)
        draw = ImageDraw.Draw(canvas)
        for c in range(1, cols):
            draw.line([(c * cell_w, 0), (c * cell_w, CH)], fill=(14, 20, 24, 255), width=2)
        for r in range(1, rows):
            draw.line([(0, r * cell_h), (CW, r * cell_h)], fill=(14, 20, 24, 255), width=2)

        placements = []
        for slot, pid in enumerate(pids):
            sx, sy, sw, sh = rgm_meta[pid]
            crop = rgm1.crop((sx, sy, sx + sw, sy + sh))
            scale = 3
            while (sw * scale > cell_w - 20 or sh * scale > cell_h - 20) and scale > 1:
                scale -= 1
            tw, th = sw * scale, sh * scale
            up = crop.resize((tw, th), Image.Resampling.NEAREST)
            col, row = slot % cols, slot // cols
            ox = col * cell_w + (cell_w - tw) // 2
            oy = row * cell_h + (cell_h - th) // 2
            canvas.alpha_composite(up, (ox, oy))
            placements.append({
                "portrait_id": pid,
                "orig_rect": [sx, sy, sw, sh],
                "paste_box": [ox, oy, tw, th],
            })

        in_path = work_dir / f"{sname}_input_1376x768.png"
        canvas.convert("RGB").save(in_path)
        (work_dir / f"{sname}_meta.json").write_text(json.dumps(placements, indent=2))
        sheets_meta.append(sname)
        prompt = (
            f"Faithful 2D HD hand-painted Chinese classic RPG character portrait sheet remaster of this exact {cols}x{rows} grid "
            f"from 1995 Chinese Paladin (Xianjian Qixiazhuan). Keep the exact {cols}x{rows} grid layout, exact head/shoulder "
            f"position, silhouette, hairstyle, clothing colors, and exact facial expression of each cell! "
            f"Upgrade low-res dithered pixel art into crisp, expressive high-definition classical Chinese RPG painted bust "
            f"portraits (clean fine brushwork, natural skin shading, detailed eyes, silk & hair strands) while staying 100% "
            f"faithful to each original character design. Keep the uniform dark slate background (#161E24) solid and clean "
            f"behind every portrait. Output 1376x768."
        )
        todo_items.append({
            "key": sname,
            "kind": "portrait_sheet",
            "input_path": str(in_path),
            "prompt": prompt,
        })
        print(f"  [Portraits] {sname}: {len(pids)} portrait(s) {pids}")

    (work_dir / "portrait_sheets_list.json").write_text(json.dumps(sheets_meta, indent=2))
    return todo_items


def bake_portraits_hd(work_dir: Path, ai_map: dict[str, Path]):
    list_p = work_dir / "portrait_sheets_list.json"
    if not list_p.exists():
        return
    sheet_names = json.loads(list_p.read_text())
    rgm1_full = np.array(Image.open(GFX / "ui" / "rgm_1x.webp").convert("RGBA"))
    p4 = GFX / "ui" / "rgm_4x.webp"
    rgm4_full = np.array(Image.open(p4).convert("RGBA"))
    done_set = get_done_rgm_set()
    updated = []

    for sname in sheet_names:
        ai_p = ai_map.get(sname)
        if not ai_p or not ai_p.exists():
            raise FileNotFoundError(f"Missing AI portrait sheet output for {sname}: {ai_p}")
        placements = json.loads((work_dir / f"{sname}_meta.json").read_text())
        im_ai = Image.open(ai_p).convert("RGB")
        if im_ai.size != (1376, 768):
            im_ai = im_ai.resize((1376, 768), Image.Resampling.LANCZOS)
        arr_ai = np.array(im_ai)

        for pl in placements:
            pid = pl["portrait_id"]
            sx, sy, sw, sh = pl["orig_rect"]
            px, py, pw, ph = pl["paste_box"]
            ref_1x = rgm1_full[sy:sy + sh, sx:sx + sw]

            sub_ai = Image.fromarray(arr_ai[py:py + ph, px:px + pw]).resize(
                (sw * 4, sh * 4), Image.Resampling.LANCZOS
            )
            sub_ai = sub_ai.filter(ImageFilter.UnsharpMask(radius=0.9, percent=55, threshold=2))
            rgba_4x = extract_sprite_or_portrait_cell(
                np.array(sub_ai), ref_1x, sw * 4, sh * 4,
                expected_bg=(22, 30, 36), is_prop=False, is_portrait=True
            )
            rgm4_full[sy * 4:(sy + sh) * 4, sx * 4:(sx + sw) * 4] = rgba_4x
            done_set.add(pid)
            updated.append(pid)

    if updated:
        Image.fromarray(rgm4_full).save(p4, "WEBP", quality=94, alpha_quality=100, exact=True, method=4)
        save_done_rgm_set(done_set)
        print(f"  [RGM] Spliced {len(updated)} HD portraits into rgm_4x.webp: {sorted(updated)}")


def bump_asset_version(tag: str):
    r_path = SITE / "js/host/asset-provider.js"
    if r_path.exists():
        txt = r_path.read_text()
        txt_new = re.sub(r'(export\s+)?const ASSET_VER = "[^"]+";', lambda m: f'{m.group(1) or ""}const ASSET_VER = "{tag}";', txt)
        if txt_new != txt:
            r_path.write_text(txt_new)
    idx_path = SITE / "index.html"
    if idx_path.exists():
        itxt = idx_path.read_text()
        itxt_new = re.sub(r'\?v=[a-zA-Z0-9_.-]+', f'?v={tag}', itxt)
        if itxt_new != itxt:
            idx_path.write_text(itxt_new)


# ---------------------------------------------------------------------------
# CLI Entrypoints
# ---------------------------------------------------------------------------

def cmd_status():
    scenes, events, all_scripts = load_game_tables()
    done_maps = sum(1 for m in {s["map_id"] for s in scenes if s["map_id"] > 0} if is_map_done(m))
    total_maps = len({s["map_id"] for s in scenes if s["map_id"] > 0})
    done_mgos = sum(1 for p in (GFX / "mgo").glob("mgo_*.json") if json.loads(p.read_text()).get("hdTrackB"))
    total_mgos = len(list((GFX / "mgo").glob("mgo_*.json")))
    done_rgms = len(get_done_rgm_set())

    full_scenes = 0
    next_todo = []
    for s in scenes:
        _, tm, tmg, _, trgm = collect_scene_requirements(s["id"], scenes, events, all_scripts)
        if not tm and not tmg and not trgm:
            full_scenes += 1
        elif len(next_todo) < 12:
            next_todo.append((s["id"], s["map_id"], s["name"], tm, len(tmg), len(trgm)))

    print(f"=== Track-B HD Remaster Progress ===")
    print(f"  Scenes fully complete : {full_scenes} / {len(scenes)}")
    print(f"  Unique Maps HD        : {done_maps} / {total_maps}")
    print(f"  MGO Sprites HD        : {done_mgos} / {total_mgos}")
    print(f"  RGM Portraits HD      : {done_rgms}")
    print(f"\nNext incomplete scenes:")
    for sid, mid, name, tm, n_mgo, n_rgm in next_todo:
        print(f"  Scene {sid:3d} (Map {mid:3d}) {name:24s} -> map_todo={tm}, mgo_todo={n_mgo}, rgm_todo={n_rgm}")


def cmd_prepare(scene_id: int, work_dir: Path, chain: bool = False, redo_map: bool = False):
    work_dir.mkdir(parents=True, exist_ok=True)
    for stale in ("sprite_sheets_list.json", "portrait_sheets_list.json"):
        sp = work_dir / stale
        if sp.exists():
            sp.unlink()
    scenes, events, all_scripts = load_game_tables()
    sc, todo_map, todo_mgos, mgo_char_hint, todo_rgms = collect_scene_requirements(scene_id, scenes, events, all_scripts)
    if redo_map:
        todo_map = True  # re-generate an already-baked map (bake overwrites its HD pack)
    print(f"=== Preparing Scene {scene_id}: {sc['name']} (Map {sc['map_id']}) ===")
    print(f"  todo_map={todo_map}, todo_mgos={todo_mgos}, todo_rgms={todo_rgms}")

    todo_all = []
    if todo_map:
        todo_all.extend(
            prepare_map_patches(sc["map_id"], sc["name"], work_dir, scenes, events, all_scripts, chain=chain)
        )
    if todo_mgos:
        todo_all.extend(prepare_sprite_sheets(scene_id, todo_mgos, mgo_char_hint, work_dir))
    if todo_rgms:
        todo_all.extend(prepare_portrait_sheets(scene_id, todo_rgms, work_dir))

    manifest = {
        "scene_id": scene_id,
        "scene_name": sc["name"],
        "map_id": sc["map_id"],
        "todo_map": todo_map,
        "todo_count": len(todo_all),
        "items": todo_all,
    }
    (work_dir / "manifest_todo.json").write_text(json.dumps(manifest, indent=2, ensure_ascii=False))
    print(f"=== Prepared {len(todo_all)} AI generation task(s) -> {work_dir / 'manifest_todo.json'} ===")


def cmd_chain_next(scene_id: int, work_dir: Path):
    """Chained map-patch generation step: after the current round of AI outputs is recorded in `ai_outputs.json`,
    stitch the finished patches exactly like `bake` would, then write the next ready patch inputs (with their
    already-painted neighbour pixels pasted in) and a matching prompt into `manifest_todo.json`."""
    work_dir = resolve_work_or_asset_path(work_dir)
    manifest = json.loads((work_dir / "manifest_todo.json").read_text())
    map_id = manifest["map_id"]
    chain_p = work_dir / f"map_{map_id:03d}_chain.json"
    if not chain_p.exists():
        raise SystemExit(f"{work_dir} was not prepared with `prepare --chain`")
    scene_name = json.loads(chain_p.read_text())["scene_name"]
    ai_map = load_ai_outputs_map(work_dir)
    done = {k for k, v in ai_map.items() if v.exists()}

    islands_meta = json.loads((work_dir / f"map_{map_id:03d}_islands.json").read_text())
    vis_1x = np.load(work_dir / f"map_{map_id:03d}_vis_1x.npy")
    full_1x = np.array(Image.open(work_dir / f"map_{map_id:03d}_full_1x.png").convert("RGBA"))
    occ_path = work_dir / f"map_{map_id:03d}_occ_1x.npy"
    tile_occ_1x = np.load(occ_path) if occ_path.exists() else (full_1x[:, :, :3].sum(axis=2) > 0)
    lbl_all, _ = label(binary_dilation(vis_1x, iterations=8))

    map_items = []
    remaining = 0
    for isl in islands_meta:
        patches = isl["patches"]
        remaining += sum(1 for p in patches if p["key"] not in done)
        ready = _chain_ready_patches(patches, done)
        if not ready:
            continue
        bbox = tuple(isl["bbox_1x"])
        isl_valid_1x, isl_alpha_4x, isl_ctx_full = _island_masks(isl, len(islands_meta), lbl_all, vis_1x, tile_occ_1x)
        for p in ready:
            # Only finished patches within one patch-width of this box can influence the pixels pasted into it,
            # so stitch just those (bake still stitches the whole island); keeps every round O(neighbours).
            bx0, by0, bw, bh = p["box_1x"]
            near = [bx0 - BOX_W, by0 - BOX_H, bw + 2 * BOX_W, bh + 2 * BOX_H]
            finished = [q for q in patches if q["key"] in done and _boxes_overlap(q["box_1x"], near)]
            if finished:
                acc_rgb, has_pix_1x = _stitch_island_patches_4x(
                    finished, ai_map, work_dir, full_1x, isl_ctx_full, isl_valid_1x, isl_alpha_4x, bbox
                )
            else:
                acc_rgb = np.zeros((bbox[3] * 4, bbox[2] * 4, 3), dtype=np.float32)
                has_pix_1x = np.zeros((bbox[3], bbox[2]), dtype=bool)
            rgb, edges = build_chained_patch_input(p, bbox, full_1x, isl_ctx_full, acc_rgb, has_pix_1x)
            Image.fromarray(rgb).save(p["input_path"])
            map_items.append({
                "key": p["key"],
                "kind": "map_patch",
                "input_path": p["input_path"],
                "prompt": build_map_patch_prompt(scene_name, chain_note=describe_chained_edges(edges)),
                "chained_edges": edges,
            })
            print(f"  [Chain] {p['key']}: painted edges={edges or ['none']}")

    other = [it for it in manifest["items"] if it.get("kind") != "map_patch" and it["key"] not in done]
    manifest["items"] = map_items + other
    manifest["todo_count"] = len(manifest["items"])
    (work_dir / "manifest_todo.json").write_text(json.dumps(manifest, indent=2, ensure_ascii=False))
    if remaining == 0:
        print(f"=== All map patches for Map {map_id:03d} have outputs — run `bake --scene {scene_id}` ===")
    else:
        print(f"=== {len(map_items)} map patch(es) ready, {remaining} map patch(es) still to generate ===")


def load_ai_outputs_map(work_dir: Path) -> dict[str, Path]:
    ai_p = work_dir / "ai_outputs.json"
    if not ai_p.exists():
        return {}
    ai_outputs = json.loads(ai_p.read_text())
    return {k: resolve_work_or_asset_path(v, work_dir) for k, v in ai_outputs.items()}


def cmd_bake(scene_id: int, work_dir: Path):
    work_dir = resolve_work_or_asset_path(work_dir)
    manifest = json.loads((work_dir / "manifest_todo.json").read_text())
    if manifest["todo_count"] == 0 and not manifest.get("todo_map"):
        print(f"Scene {scene_id} already 100% complete — nothing to bake.")
        return
    ai_map = load_ai_outputs_map(work_dir)

    if manifest["todo_map"]:
        bake_map_hd(manifest["map_id"], work_dir, ai_map, verify=True)
    bake_sprites_hd(work_dir, ai_map)
    bake_portraits_hd(work_dir, ai_map)
    bump_asset_version(f"sc{scene_id:03d}")
    print(f"=== Scene {scene_id} ({manifest['scene_name']}) Bake Complete! ===")


def calibrate_existing_map_in_place(
    map_id: int,
    donor_id: int | None = None,
    verify: bool = True,
    scenes=None,
    events=None,
    all_scripts=None,
    full_1x_cache: tuple[np.ndarray, np.ndarray] | None = None,
) -> dict:
    """Calibrate an existing baked HD map in-place without changing any chunk filenames or occluder alpha masks.

    1) Computes canonical 1x-anchored Lab alignment & edge-preserving guided residual fields across all chunks.
    2) Measures & applies adaptive 4x texture/brushstroke detail normalization with seamless 8 HD px neighbor halos.
    3) Applies Story-Variant HD Tile Inheritance from `donor_id` (if set) on identical 1x pixels.
    4) Resamples `occluders_4x.webp` RGB from the newly calibrated chunks (keeping exact verified alpha + pure-black
       cave tunnel portals untouched!).
    5) Runs full hard-gated `verify_map(map_id)`.
    """
    out_dir = GFX / "maps_hd" / f"map_{map_id:03d}"
    meta_path = out_dir / "meta.json"
    if not meta_path.exists():
        raise FileNotFoundError(f"Missing meta.json for map_{map_id:03d}: {meta_path}")
    meta = json.loads(meta_path.read_text())
    if int(meta.get("scale", 4)) != 4:
        raise ValueError(f"calibrate-map supports only scale-4 HD packs; map_{map_id:03d} is stored at scale {meta.get('scale')}")
    rooms = meta.get("rooms", [])
    if not rooms:
        return {"map_id": map_id, "skipped": True}

    if full_1x_cache is not None:
        full_1x, tile_occ_1x = full_1x_cache
    else:
        full_1x, _, tile_occ_1x = render_map_1x_and_sort_y(map_id)

    # Pass 1: Build 1x downsampled map RGB & alpha across all chunks
    hd_1x_sum = np.zeros((2048, 2048, 3), dtype=np.float32)
    hd_1x_wt = np.zeros((2048, 2048), dtype=np.float32)
    room_boxes = []
    for rm in rooms:
        rx, ry, rw, rh = int(rm["x"]), int(rm["y"]), int(rm["w"]), int(rm["h"])
        arr = np.array(Image.open(out_dir / rm["file"]).convert("RGBA"))
        a1 = arr[:, :, 3].astype(np.float32).reshape(rh, 4, rw, 4).mean(axis=(1, 3)) * (1.0 / 255.0)
        rgb1 = arr[:, :, :3].astype(np.float32).reshape(rh, 4, rw, 4, 3).mean(axis=(1, 3))
        hd_1x_sum[ry:ry + rh, rx:rx + rw] += rgb1 * a1[:, :, None]
        hd_1x_wt[ry:ry + rh, rx:rx + rw] += a1
        room_boxes.append((rm, rx, ry, rw, rh))

    hd_1x = np.clip(hd_1x_sum / np.maximum(hd_1x_wt[:, :, None], 1e-4), 0.0, 255.0)
    valid_1x = (hd_1x_wt > 0.5) & tile_occ_1x
    fields = compute_canonical_calibration_fields_1x(hd_1x, full_1x[:, :, :3], valid_1x)
    if fields is None:
        return {"map_id": map_id, "skipped": True}

    gain_L = fields["gain_L"]
    hd_L_mean = fields["hd_L_mean"]
    dL_1x = fields["dL_1x"]
    da_1x = fields["da_1x"]
    db_1x = fields["db_1x"]
    textured_1x = fields["textured_1x"]
    tex_gate_1x = fields["tex_gate_1x"]

    # Pass 2: Measure post-Step-A/B 4x Lab L* gradient on textured pixels across all chunks
    grad_sum = 0.0
    grad_cnt = 0
    if int(textured_1x.sum()) > 300:
        for rm, rx, ry, rw, rh in room_boxes:
            tex_sub = textured_1x[ry:ry + rh, rx:rx + rw]
            if not tex_sub.any():
                continue
            arr = np.array(Image.open(out_dir / rm["file"]).convert("RGBA"))
            lab4 = _apply_lab_fields_to_rgb_4x(
                arr[:, :, :3],
                gain_L,
                hd_L_mean,
                dL_1x[ry:ry + rh, rx:rx + rw],
                da_1x[ry:ry + rh, rx:rx + rw],
                db_1x[ry:ry + rh, rx:rx + rw],
            )
            L4 = lab4[:, :, 0]
            gx4 = np.abs(np.diff(L4, axis=1, prepend=L4[:, :1]))
            gy4 = np.abs(np.diff(L4, axis=0, prepend=L4[:1, :]))
            m4 = np.repeat(np.repeat(tex_sub, 4, axis=0), 4, axis=1) & (arr[:, :, 3] > 200)
            n_m = int(m4.sum())
            if n_m > 0:
                grad_sum += float((gx4 + gy4)[m4].sum())
                grad_cnt += n_m

    cur_grad = (grad_sum / grad_cnt) if grad_cnt > 0 else CANONICAL_LAB_TARGET_GRAD_4X
    k_detail = float(np.clip((CANONICAL_LAB_TARGET_GRAD_4X / max(cur_grad, 0.8)) ** 0.65, 0.72, 1.55))

    # Pass 3: Calibrate every chunk with seamless 2px-1x (8px-4x) neighbor halos + optional story-variant inheritance
    donor_1x_full = render_map_1x_and_sort_y(donor_id)[0] if donor_id is not None else None
    HALO_1X = 2
    tmp_writes: list[tuple[Path, Path]] = []
    inh_fracs: list[float] = []

    for idx_r, (rm, rx, ry, rw, rh) in enumerate(room_boxes):
        arr = np.array(Image.open(out_dir / rm["file"]).convert("RGBA"))
        hx0 = max(0, rx - HALO_1X)
        hy0 = max(0, ry - HALO_1X)
        hx1 = min(2048, rx + rw + HALO_1X)
        hy1 = min(2048, ry + rh + HALO_1X)
        pt = (ry - hy0) * 4
        pb = (hy1 - (ry + rh)) * 4
        pl = (rx - hx0) * 4
        pr = (hx1 - (rx + rw)) * 4
        pad_rgb = np.pad(arr[:, :, :3], ((pt, pb), (pl, pr), (0, 0)), mode="edge")

        # Fill exact halo strips from any adjacent chunk in `room_boxes`
        for idx_o, (rm2, ox, oy, ow, oh) in enumerate(room_boxes):
            if idx_o == idx_r:
                continue
            ix0, iy0 = max(hx0, ox), max(hy0, oy)
            ix1, iy1 = min(hx1, ox + ow), min(hy1, oy + oh)
            if ix1 <= ix0 or iy1 <= iy0:
                continue
            # Only copy pixels strictly outside the current chunk's interior `[rx..rx+rw, ry..ry+rh]`
            im_o = Image.open(out_dir / rm2["file"]).convert("RGBA")
            sub_o = np.array(im_o.crop(((ix0 - ox) * 4, (iy0 - oy) * 4, (ix1 - ox) * 4, (iy1 - oy) * 4)))
            dy0, dy1 = (iy0 - hy0) * 4, (iy1 - hy0) * 4
            dx0, dx1 = (ix0 - hx0) * 4, (ix1 - hx0) * 4
            # Mask out current chunk interior so we never overwrite `arr`
            in_cur = np.zeros((dy1 - dy0, dx1 - dx0), dtype=bool)
            cy0 = max(0, pt - dy0)
            cy1 = min(dy1 - dy0, pt + rh * 4 - dy0)
            cx0 = max(0, pl - dx0)
            cx1 = min(dx1 - dx0, pl + rw * 4 - dx0)
            if cy1 > cy0 and cx1 > cx0:
                in_cur[cy0:cy1, cx0:cx1] = True
            copy_m = (~in_cur) & (sub_o[:, :, 3] > 16)
            if copy_m.any():
                pad_rgb[dy0:dy1, dx0:dx1][copy_m] = sub_o[:, :, :3][copy_m]

        lab_pad = _apply_lab_fields_to_rgb_4x(
            pad_rgb,
            gain_L,
            hd_L_mean,
            dL_1x[hy0:hy1, hx0:hx1],
            da_1x[hy0:hy1, hx0:hx1],
            db_1x[hy0:hy1, hx0:hx1],
        )
        lab_pad = _apply_detail_norm_to_lab_4x(lab_pad, k_detail, tex_gate_1x[hy0:hy1, hx0:hx1])
        cal_rgb_4x = lab_to_rgb_u8(lab_pad[pt:pt + rh * 4, pl:pl + rw * 4])

        if donor_id is not None:
            cal_rgb_4x, frac = apply_story_variant_inheritance_4x(
                cal_rgb_4x, (rx, ry, rw, rh), full_1x[ry:ry + rh, rx:rx + rw, :3], donor_id, donor_1x_full=donor_1x_full
            )
            inh_fracs.append(frac)

        out_rgba = np.empty_like(arr)
        out_rgba[:, :, :3] = cal_rgb_4x
        out_rgba[:, :, 3] = arr[:, :, 3]
        out_rgba = extrude_rgb_voronoi(out_rgba, iters=4)

        dst_p = out_dir / rm["file"]
        tmp_p = out_dir / f".caltmp_{rm['file']}"
        Image.fromarray(out_rgba).save(tmp_p, "WEBP", quality=90, alpha_quality=100, exact=True, method=4)
        tmp_writes.append((tmp_p, dst_p))

    for tmp_p, dst_p in tmp_writes:
        tmp_p.replace(dst_p)

    # Pass 4: Resample occluder RGB from newly calibrated chunks (preserving exact verified alpha across all pages!)
    occ_info = meta.get("occluders") or {}
    strips = occ_info.get("strips") or []
    occ_pages_raw = occ_info.get("pages")
    occ_page_files = list(occ_pages_raw) if isinstance(occ_pages_raw, list) else [occ_info.get("file", "occluders_4x.webp")]
    synced_strips = 0
    if strips:
        occ_atlases: list[np.ndarray | None] = []
        occ_paths: list[Path] = []
        occ_dirty: list[bool] = []
        for pfile in occ_page_files:
            pp = out_dir / pfile
            occ_paths.append(pp)
            if pp.exists():
                occ_atlases.append(np.array(Image.open(pp).convert("RGBA")))
            else:
                occ_atlases.append(None)
            occ_dirty.append(False)

        cal_room_cache: dict[str, np.ndarray] = {}

        def get_cal_room(fname: str) -> np.ndarray:
            arr_c = cal_room_cache.get(fname)
            if arr_c is None:
                arr_c = np.array(Image.open(out_dir / fname).convert("RGBA"))
                cal_room_cache[fname] = arr_c
            return arr_c

        for st in strips:
            pidx = int(st.get("page", 0))
            if pidx < 0 or pidx >= len(occ_atlases) or occ_atlases[pidx] is None:
                continue
            occ_atlas = occ_atlases[pidx]
            sx, sy, sw, sh = int(st["sx"]), int(st["sy"]), int(st["sw"]), int(st["sh"])
            dx, dy, dw, dh = int(st["dx"]), int(st["dy"]), int(st["dw"]), int(st["dh"])
            old_strip = occ_atlas[sy:sy + sh, sx:sx + sw]
            op = old_strip[:, :, 3] > 128
            # Preserve pure-black cave tunnel-portal darkness occluders verbatim!
            if not op.any() or int(old_strip[:, :, :3][op].max()) <= 2:
                continue
            if sw != dw * 4 or sh != dh * 4:
                continue

            new_rgb = np.zeros((sh, sw, 3), dtype=np.uint8)
            best_a = np.full((sh, sw), -1, dtype=np.int16)
            for rm, rx, ry, rw, rh in room_boxes:
                ox0, oy0 = max(dx, rx), max(dy, ry)
                ox1, oy1 = min(dx + dw, rx + rw), min(dy + dh, ry + rh)
                if ox1 <= ox0 or oy1 <= oy0:
                    continue
                rc = get_cal_room(rm["file"])
                src_sub = rc[(oy0 - ry) * 4:(oy1 - ry) * 4, (ox0 - rx) * 4:(ox1 - rx) * 4]
                dst_sl_y = slice((oy0 - dy) * 4, (oy1 - dy) * 4)
                dst_sl_x = slice((ox0 - dx) * 4, (ox1 - dx) * 4)
                sa = src_sub[:, :, 3].astype(np.int16)
                better = sa > best_a[dst_sl_y, dst_sl_x]
                if better.any():
                    new_rgb[dst_sl_y, dst_sl_x][better] = src_sub[:, :, :3][better]
                    best_a[dst_sl_y, dst_sl_x][better] = sa[better]

            if (best_a > 0).any():
                new_strip = np.zeros((sh, sw, 4), dtype=np.uint8)
                new_strip[:, :, :3] = new_rgb
                new_strip[:, :, 3] = old_strip[:, :, 3]
                new_strip = extrude_rgb_voronoi(new_strip, iters=3)
                occ_atlas[sy:sy + sh, sx:sx + sw] = new_strip
                occ_dirty[pidx] = True
                synced_strips += 1

        for pidx, dirty in enumerate(occ_dirty):
            if dirty and occ_atlases[pidx] is not None:
                Image.fromarray(occ_atlases[pidx]).save(
                    occ_paths[pidx], "WEBP", quality=92, alpha_quality=100, exact=True, method=4
                )

    donor_str = f" + donor=M{donor_id:03d} ({np.mean(inh_fracs) * 100:.1f}%)" if donor_id is not None and inh_fracs else ""
    print(
        f"  [Calibrated] Map {map_id:03d}: gainL={gain_L:.2f}, gLab=({fields['g_dL']:+.1f},{fields['g_da']:+.1f},{fields['g_db']:+.1f}), "
        f"grad4={cur_grad:.2f}->k={k_detail:.2f}, synced={synced_strips}/{len(strips)} occ strips{donor_str}"
    )

    ver_res = {}
    if verify:
        ver_res = verify_map(map_id, scenes=scenes, events=events, all_scripts=all_scripts, out_dir=out_dir)
    return {
        "map_id": map_id,
        "donor_id": donor_id,
        "gain_L": round(gain_L, 3),
        "g_dL": round(fields["g_dL"], 2),
        "k_detail": round(k_detail, 3),
        "synced_strips": synced_strips,
        **ver_res,
    }


def cmd_calibrate_map(map_id: int, donor_id: int | None = None):
    print(f"=== Calibrating HD Map {map_id:03d} In-Place ===")
    scenes, events, all_scripts = load_game_tables()
    if donor_id is None:
        full_1x, _, tile_occ_1x = render_map_1x_and_sort_y(map_id)
        hd_a1 = _load_map_hd_alpha_1x(map_id)
        if hd_a1 is not None:
            cand, _, _ = find_story_variant_donor(map_id, full_1x, tile_occ_1x, hd_a1)
            if cand is not None:
                cand_a1 = _load_map_hd_alpha_1x(cand)
                if cand_a1 is not None and (int(cand_a1.sum()) > int(hd_a1.sum()) or (int(cand_a1.sum()) == int(hd_a1.sum()) and cand < map_id)):
                    donor_id = cand
    calibrate_existing_map_in_place(map_id, donor_id=donor_id, verify=True, scenes=scenes, events=events, all_scripts=all_scripts)


def cmd_calibrate_all_maps(no_bump: bool = False):
    maps_root = GFX / "maps_hd"
    mids = sorted(
        int(d.name.split("_")[1])
        for d in maps_root.glob("map_*")
        if d.is_dir() and (d / "meta.json").exists()
    )
    print(f"=== Calibrating & Verifying ALL {len(mids)} Baked HD Maps ===")
    scenes, events, all_scripts = load_game_tables()

    # Precompute 1x references & HD coverage areas to resolve story-variant donors in optimal donor-first order
    ref_cache: dict[int, tuple[np.ndarray, np.ndarray]] = {}
    hd_alpha_cache: dict[int, np.ndarray] = {}
    hd_area: dict[int, int] = {}
    for m in mids:
        full_1x, _, tile_occ_1x = render_map_1x_and_sort_y(m)
        ref_cache[m] = (full_1x, tile_occ_1x)
        a1 = _load_map_hd_alpha_1x(m)
        hd_alpha_cache[m] = a1 if a1 is not None else np.zeros((2048, 2048), dtype=bool)
        hd_area[m] = int(hd_alpha_cache[m].sum())

    donor_map: dict[int, int] = {}
    for m in mids:
        full_m, occ_m = ref_cache[m]
        a_m = hd_alpha_cache[m]
        area_m = max(1, hd_area[m])
        best_cand = None
        for o in mids:
            if o == m:
                continue
            # Strictly higher coverage donor (or lower map_id on exact tie) guarantees acyclic donor graph!
            if (hd_area[o], -o) <= (hd_area[m], -m):
                continue
            full_o, occ_o = ref_cache[o]
            inter_s = occ_m[::4, ::4] & occ_o[::4, ::4]
            union_s = occ_m[::4, ::4] | occ_o[::4, ::4]
            if float(inter_s.sum()) / max(1.0, float(union_s.sum())) < 0.65:
                continue
            same_s = float(((full_m[::4, ::4, :3] == full_o[::4, ::4, :3]).all(axis=2) & inter_s).sum()) / max(1.0, float(inter_s.sum()))
            if same_s < 0.72:
                continue
            ov = a_m & hd_alpha_cache[o]
            ov_cnt = int(ov.sum())
            cov_frac = ov_cnt / area_m
            if cov_frac < 0.50:
                continue
            same_frac = int(((full_m[:, :, :3] == full_o[:, :, :3]).all(axis=2) & ov).sum()) / max(1, ov_cnt)
            if same_frac < 0.85:
                continue
            score = (cov_frac >= 0.98, round(cov_frac, 4), round(same_frac, 4), hd_area[o], -o)
            if best_cand is None or score > best_cand[0]:
                best_cand = (score, o, cov_frac, same_frac)
        if best_cand is not None:
            donor_map[m] = best_cand[1]
            print(
                f"  [Detected Story-Variant Pair] Map {m:03d} <- Donor Map {best_cand[1]:03d} "
                f"(cov={best_cand[2]:.3f}, same={best_cand[3]:.3f})"
            )

    # Calibrate primary maps first (including all donors), then story-variant recipients
    primary_mids = [m for m in mids if m not in donor_map]
    variant_mids = [m for m in mids if m in donor_map]
    results = []
    for m in primary_mids + variant_mids:
        res = calibrate_existing_map_in_place(
            m,
            donor_id=donor_map.get(m),
            verify=True,
            scenes=scenes,
            events=events,
            all_scripts=all_scripts,
            full_1x_cache=ref_cache[m],
        )
        results.append(res)

    if not no_bump:
        bump_asset_version("sc053_issue8")
    max_de = max((r.get("dE_canon", 0.0) for r in results), default=0.0)
    max_blk = max((r.get("max_blk_mae", 0.0) for r in results), default=0.0)
    print(
        f"=== ALL {len(results)} HD Maps Calibrated & Verified PASS! "
        f"(max canon dE={max_de:.2f}, max 128x128 blk MAE={max_blk:.1f}) ==="
    )


def cmd_verify_map(map_id: int):
    print(f"=== Verifying HD Map {map_id:03d} ===")
    verify_map(map_id)


def cmd_rebake_map(map_id: int, work_dir: Path):
    work_dir = resolve_work_or_asset_path(work_dir)
    print(f"=== Rebaking HD Map {map_id:03d} (work_dir={work_dir}) ===")
    scenes, events, all_scripts = load_game_tables()
    full_1x, sort_y_1x, tile_occ_1x = render_map_1x_and_sort_y(map_id)
    vis_1x = compute_camera_visible_mask_1x(map_id, tile_occ_1x, scenes, events, all_scripts)

    np.save(work_dir / f"map_{map_id:03d}_sort_y_1x.npy", sort_y_1x)
    np.save(work_dir / f"map_{map_id:03d}_vis_1x.npy", vis_1x)
    np.save(work_dir / f"map_{map_id:03d}_occ_1x.npy", tile_occ_1x)
    Image.fromarray(full_1x).save(work_dir / f"map_{map_id:03d}_full_1x.png")

    ai_map = load_ai_outputs_map(work_dir)
    bake_map_hd(map_id, work_dir, ai_map, verify=True)
    print(f"=== Map {map_id:03d} Rebake + Verify Complete! ===")


def cmd_prepare_cutout_patches(map_id: int, work_dir: Path):
    work_dir = resolve_work_or_asset_path(work_dir)
    print(f"=== Preparing Cutout Regen Patches for Map {map_id:03d} (work_dir={work_dir}) ===")
    scenes, events, all_scripts = load_game_tables()

    scene_name = f"Map {map_id}"
    manifest_p = work_dir / "manifest_todo.json"
    if manifest_p.exists():
        scene_name = json.loads(manifest_p.read_text()).get("scene_name") or scene_name
    else:
        for s in scenes:
            if s["map_id"] == map_id:
                scene_name = s["name"]
                break

    orig_vis_path = work_dir / f"map_{map_id:03d}_orig_vis_1x.npy"
    cur_vis_path = work_dir / f"map_{map_id:03d}_vis_1x.npy"
    if not orig_vis_path.exists() and cur_vis_path.exists():
        shutil.copy2(cur_vis_path, orig_vis_path)
    old_vis_1x = np.load(orig_vis_path) if orig_vis_path.exists() else np.zeros((2048, 2048), dtype=bool)

    full_1x, sort_y_1x, tile_occ_1x = render_map_1x_and_sort_y(map_id)
    true_vis_1x = compute_camera_visible_mask_1x(map_id, tile_occ_1x, scenes, events, all_scripts)

    np.save(work_dir / f"map_{map_id:03d}_sort_y_1x.npy", sort_y_1x)
    np.save(work_dir / f"map_{map_id:03d}_vis_1x.npy", true_vis_1x)
    np.save(work_dir / f"map_{map_id:03d}_occ_1x.npy", tile_occ_1x)
    Image.fromarray(full_1x).save(work_dir / f"map_{map_id:03d}_full_1x.png")

    newly_vis_1x = true_vis_1x & (~old_vis_1x)
    eroded_new_1x = binary_erosion(newly_vis_1x, iterations=1)

    isl_json_p = work_dir / f"map_{map_id:03d}_islands.json"
    islands_meta = json.loads(isl_json_p.read_text())
    lbl_all, _ = label(binary_dilation(true_vis_1x, iterations=8))

    prompt = build_map_patch_prompt(scene_name)
    regen_todo = []
    cov_1x = np.zeros((2048, 2048), dtype=bool)

    for isl in islands_meta:
        isl_id = isl["island_id"]
        ix0, iy0, iw, ih = isl["bbox_1x"]
        sub_lbl = lbl_all[iy0:iy0 + ih, ix0:ix0 + iw]
        present_cids = [int(c) for c in np.unique(sub_lbl) if c > 0]
        matched_cids = []
        for c in present_cids:
            tot_c = int(((lbl_all == c) & true_vis_1x).sum())
            in_c = int(((sub_lbl == c) & true_vis_1x[iy0:iy0 + ih, ix0:ix0 + iw]).sum())
            if tot_c > 0 and (in_c / tot_c >= 0.5 or len(islands_meta) == 1):
                matched_cids.append(c)
        if not matched_cids:
            matched_cids = present_cids or (isl.get("cids") or [isl.get("cid", 1)])
        isl["cids"] = matched_cids

        isl_valid_sub = (
            np.isin(sub_lbl, matched_cids)
            & true_vis_1x[iy0:iy0 + ih, ix0:ix0 + iw]
            & tile_occ_1x[iy0:iy0 + ih, ix0:ix0 + iw]
        )
        isl_valid_sub = binary_fill_holes(isl_valid_sub)
        isl_full = np.zeros((2048, 2048), dtype=bool)
        isl_full[iy0:iy0 + ih, ix0:ix0 + iw] = isl_valid_sub
        isl_ctx = isl_full | (binary_dilation(isl_full, iterations=2) & tile_occ_1x)

        for pinfo in isl["patches"]:
            pkey = pinfo["key"]
            bx0, by0, bw, bh = pinfo["box_1x"]
            cov_1x[by0:by0 + bh, bx0:bx0 + bw] = True

            eroded_cnt = int(eroded_new_1x[by0:by0 + bh, bx0:bx0 + bw].sum())
            png_missing_cnt = 0
            orig_in_p = work_dir / f"{pkey}_input_1376x768.png"
            if orig_in_p.exists():
                old_in_1x = np.array(Image.open(orig_in_p).convert("RGB"))[::2, ::2]
                old_blk = old_in_1x.sum(axis=2) == 0
                true_nonblk = isl_ctx[by0:by0 + bh, bx0:bx0 + bw] & (
                    full_1x[by0:by0 + bh, bx0:bx0 + bw, :3].sum(axis=2) > 0
                )
                png_missing_cnt = int(binary_erosion(old_blk & true_nonblk, iterations=1).sum())

            if max(eroded_cnt, png_missing_cnt) > 20:
                crop = full_1x[by0:by0 + bh, bx0:bx0 + bw, :3].copy()
                crop[~isl_ctx[by0:by0 + bh, bx0:bx0 + bw]] = 0
                regen_p = work_dir / f"{pkey}_regen_input_1376x768.png"
                Image.fromarray(crop).resize((1376, 768), Image.Resampling.NEAREST).save(regen_p)
                pinfo["input_path"] = str(regen_p)
                regen_todo.append({
                    "key": pkey,
                    "input_path": str(regen_p),
                    "prompt": prompt,
                })
                print(f"  [Cutout Patch] {pkey}: eroded_new={eroded_cnt}, png_missing={png_missing_cnt} -> {regen_p.name}")

    # Check if any newly visible island region (> 20 eroded px) was completely outside existing patch boxes
    uncov_eroded = binary_erosion(true_vis_1x & (~cov_1x), iterations=1)
    if int(uncov_eroded.sum()) > 20 and islands_meta:
        uy, ux = np.where(true_vis_1x & (~cov_1x))
        isl = islands_meta[0]
        isl_id = isl["island_id"]
        ix0, iy0, iw, ih = isl["bbox_1x"]
        nx0 = (min(ix0, int(ux.min())) // 4) * 4
        ny0 = (min(iy0, int(uy.min())) // 4) * 4
        nx1 = min(2048, ((max(ix0 + iw, int(ux.max()) + 1) + 3) // 4) * 4)
        ny1 = min(2048, ((max(iy0 + ih, int(uy.max()) + 1) + 3) // 4) * 4)
        isl["bbox_1x"] = [nx0, ny0, nx1 - nx0, ny1 - ny0]
        bx0 = max(0, min(2048 - BOX_W, (((int(ux.min()) + int(ux.max())) // 2 - BOX_W // 2) // 4) * 4))
        by0 = max(0, min(2048 - BOX_H, (((int(uy.min()) + int(uy.max())) // 2 - BOX_H // 2) // 4) * 4))
        p_idx = len(isl["patches"]) + 1
        pkey = f"map{map_id:03d}_isl{isl_id}_p{p_idx:02d}"
        crop = full_1x[by0:by0 + BOX_H, bx0:bx0 + BOX_W, :3].copy()
        crop[~true_vis_1x[by0:by0 + BOX_H, bx0:bx0 + BOX_W]] = 0
        regen_p = work_dir / f"{pkey}_regen_input_1376x768.png"
        Image.fromarray(crop).resize((1376, 768), Image.Resampling.NEAREST).save(regen_p)
        isl["patches"].append({"key": pkey, "box_1x": [bx0, by0, BOX_W, BOX_H], "input_path": str(regen_p)})
        regen_todo.append({"key": pkey, "input_path": str(regen_p), "prompt": prompt})
        print(f"  [Uncovered Patch] {pkey}: box=({bx0},{by0}) -> {regen_p.name}")

    isl_json_p.write_text(json.dumps(islands_meta, indent=2))
    out_todo_p = work_dir / "cutout_regen_todo.json"
    out_todo_p.write_text(json.dumps(regen_todo, indent=2, ensure_ascii=False))
    print(f"=== Map {map_id:03d}: {len(regen_todo)} cutout patch(es) written -> {out_todo_p} ===")


def repack_map_occluders(
    map_id: int,
    verify: bool = True,
    scenes=None,
    events=None,
    all_scripts=None,
) -> dict:
    """Losslessly repack an existing HD map's occluders into spatial <=2048x2048 pages."""
    out_dir = GFX / "maps_hd" / f"map_{map_id:03d}"
    meta_path = out_dir / "meta.json"
    if not meta_path.exists():
        raise FileNotFoundError(f"Missing HD map meta.json: {meta_path}")
    meta = json.loads(meta_path.read_text())
    occ_info = meta.get("occluders") or {}
    strips = occ_info.get("strips") or []
    occ_pages_raw = occ_info.get("pages")
    src_page_files = (
        list(occ_pages_raw)
        if isinstance(occ_pages_raw, list)
        else [occ_info.get("file", "occluders_4x.webp")]
    )
    src_atlases: list[np.ndarray | None] = [
        np.array(Image.open(out_dir / pf).convert("RGBA")) if (out_dir / pf).exists() else None
        for pf in src_page_files
    ]
    strips_rgba: list[np.ndarray] = []
    strips_info: list[dict] = []
    for st in strips:
        pidx = int(st.get("page", 0))
        if pidx < 0 or pidx >= len(src_atlases) or src_atlases[pidx] is None:
            continue
        sx, sy, sw, sh = int(st["sx"]), int(st["sy"]), int(st["sw"]), int(st["sh"])
        sub = src_atlases[pidx][sy:sy + sh, sx:sx + sw].copy()
        strips_rgba.append(sub)
        strips_info.append({
            "sortY": int(st["sortY"]),
            "dx": int(st["dx"]),
            "dy": int(st["dy"]),
            "dw": int(st["dw"]),
            "dh": int(st["dh"]),
        })

    page_files, strips_meta, page_dims = pack_spatial_occluder_pages(strips_rgba, strips_info, out_dir)
    meta_out = {
        "mapId": int(meta.get("mapId", map_id)),
        "scale": int(meta.get("scale", 4)),
        "rooms": meta.get("rooms", []),
        "occluders": {
            "pages": page_files,
            "strips": strips_meta,
        },
    }
    meta_path.write_text(json.dumps(meta_out, separators=(",", ":")))
    max_dim = max((max(w, h) for w, h in page_dims), default=0)
    print(
        f"  [Repack Map {map_id:03d}] {len(strips_meta)} strips -> {len(page_files)} spatial page(s) (max {max_dim}px)"
    )
    if verify:
        return verify_map(map_id, scenes=scenes, events=events, all_scripts=all_scripts, out_dir=out_dir)
    return {"map_id": map_id, "pages": len(page_files), "max_dim": max_dim}


def cmd_repack_occluders(map_id: int):
    repack_map_occluders(map_id, verify=True)


def cmd_repack_all_occluders(no_bump: bool = False):
    scenes, events, all_scripts = load_game_tables()
    mids = sorted(
        int(p.name.split("_")[1])
        for p in (GFX / "maps_hd").glob("map_*")
        if (p / "meta.json").exists()
    )
    print(f"=== Repacking & Verifying Spatial <=2048x2048 Occluders for {len(mids)} HD Maps ===")
    for mid in mids:
        repack_map_occluders(mid, verify=True, scenes=scenes, events=events, all_scripts=all_scripts)
    if not no_bump:
        bump_asset_version("sc060_tiled")
    print(f"=== ALL {len(mids)} HD Maps Repacked & Verified PASS! ===")


def convert_map_to_2x(
    map_id: int,
    verify: bool = True,
    scenes=None,
    events=None,
    all_scripts=None,
    quality: int = 90,
) -> tuple[int, int]:
    """Convert a 4x Track-B HD map pack to 2x storage (A5) for runtime 2x->4x bilinear upscaling.
    Preserves coordinate-stable filenames, updates `meta.json` (`scale: 2` and halved strip coords),
    eliminates any Lanczos-induced alpha holes, and enforces all 5 `verify_map` hard gates.
    Returns `(before_bytes, after_bytes)`.
    """
    out_dir = GFX / "maps_hd" / f"map_{map_id:03d}"
    meta_path = out_dir / "meta.json"
    meta = json.loads(meta_path.read_text(encoding="utf-8"))
    if int(meta.get("scale", 4)) == 2:
        total = sum(p.stat().st_size for p in out_dir.glob("*.webp"))
        if verify:
            verify_map(map_id, scenes=scenes, events=events, all_scripts=all_scripts, out_dir=out_dir)
        return total, total

    before_bytes = 0
    after_bytes = 0
    room_files = {rm["file"] for rm in meta.get("rooms", [])}
    occ_info = meta.get("occluders") or {}
    occ_files = set(occ_info.get("pages") or [])
    if occ_info.get("file"):
        occ_files.add(occ_info["file"])
    all_files = sorted(room_files | occ_files)

    for fname in all_files:
        p = out_dir / fname
        if not p.is_file():
            continue
        before_bytes += p.stat().st_size
        im = Image.open(p).convert("RGBA")
        w, h = im.size
        w2, h2 = max(1, w // 2), max(1, h // 2)
        im2 = im.resize((w2, h2), Image.Resampling.LANCZOS)
        if fname in room_files:
            arr = np.array(im2)
            a = arr[:, :, 3]
            holes = binary_fill_holes(a > 200) & (a <= 200)
            if holes.any():
                a[holes] = 255
                arr[:, :, 3] = a
                im2 = Image.fromarray(arr, mode="RGBA")
        im2.save(p, format="WEBP", quality=quality, method=6)
        after_bytes += p.stat().st_size

    meta["scale"] = 2
    for st in occ_info.get("strips") or []:
        st["sx"] = int(st["sx"]) // 2
        st["sy"] = int(st["sy"]) // 2
        st["sw"] = max(1, int(st["sw"]) // 2)
        st["sh"] = max(1, int(st["sh"]) // 2)
    meta_path.write_text(json.dumps(meta, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")

    if verify:
        verify_map(map_id, scenes=scenes, events=events, all_scripts=all_scripts, out_dir=out_dir)
    print(
        f"  [2x] map_{map_id:03d}: {before_bytes/1048576:.2f} -> {after_bytes/1048576:.2f} MiB "
        f"({after_bytes/max(before_bytes,1)*100:.1f}%)"
    )
    return before_bytes, after_bytes


def cmd_convert_all_2x(no_bump: bool = False):
    scenes, events, all_scripts = load_game_tables()
    mids = sorted(
        int(p.parent.name.split("_")[1])
        for p in (GFX / "maps_hd").glob("map_*/meta.json")
    )
    print(f"=== Converting & Verifying {len(mids)} HD Maps to 2x Storage (A5) ===")
    tot_before = 0
    tot_after = 0
    for mid in mids:
        b, a = convert_map_to_2x(mid, verify=True, scenes=scenes, events=events, all_scripts=all_scripts)
        tot_before += b
        tot_after += a
    if not no_bump:
        bump_asset_version("a5_2x_v1")
    print(
        f"=== ALL {len(mids)} HD Maps Converted & Verified PASS! "
        f"Total: {tot_before/1048576:.1f} -> {tot_after/1048576:.1f} MiB "
        f"({tot_after/max(tot_before,1)*100:.1f}%, saved {(tot_before-tot_after)/1048576:.1f} MiB) ==="
    )


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("status")

    p_prep = sub.add_parser("prepare")
    p_prep.add_argument("--scene", type=int, required=True)
    p_prep.add_argument("--work-dir", type=Path, required=True)
    p_prep.add_argument("--chain", action="store_true", help="chained neighbour-aware map patch generation")
    p_prep.add_argument("--redo-map", action="store_true", help="re-prepare the scene map even if already baked")

    p_chain = sub.add_parser("chain-next")
    p_chain.add_argument("--scene", type=int, required=True)
    p_chain.add_argument("--work-dir", type=Path, required=True)

    p_bake = sub.add_parser("bake")
    p_bake.add_argument("--scene", type=int, required=True)
    p_bake.add_argument("--work-dir", type=Path, required=True)

    p_ver = sub.add_parser("verify-map")
    p_ver.add_argument("--map", "--map-id", dest="map", type=int, required=True)

    p_rebake = sub.add_parser("rebake-map")
    p_rebake.add_argument("--map", "--map-id", dest="map", type=int, required=True)
    p_rebake.add_argument("--work-dir", type=Path, required=True)
    p_rebake.add_argument("--no-bump", action="store_true")

    p_cutout = sub.add_parser("prepare-cutout-patches")
    p_cutout.add_argument("--map", "--map-id", dest="map", type=int, required=True)
    p_cutout.add_argument("--work-dir", type=Path, required=True)

    p_cal = sub.add_parser("calibrate-map")
    p_cal.add_argument("--map", "--map-id", dest="map", type=int, required=True)
    p_cal.add_argument("--donor", type=int, default=None)

    p_cal_all = sub.add_parser("calibrate-all-maps")
    p_cal_all.add_argument("--no-bump", action="store_true")

    p_repack = sub.add_parser("repack-occluders")
    p_repack.add_argument("--map", "--map-id", dest="map", type=int, required=True)

    p_repack_all = sub.add_parser("repack-all-occluders")
    p_repack_all.add_argument("--no-bump", action="store_true")

    p_c2x_all = sub.add_parser("convert-all-2x")
    p_c2x_all.add_argument("--no-bump", action="store_true")

    args = ap.parse_args()
    if args.cmd == "status":
        cmd_status()
    elif args.cmd == "prepare":
        cmd_prepare(args.scene, resolve_work_or_asset_path(args.work_dir), chain=args.chain, redo_map=args.redo_map)
    elif args.cmd == "chain-next":
        cmd_chain_next(args.scene, args.work_dir)
    elif args.cmd == "bake":
        cmd_bake(args.scene, args.work_dir)
    elif args.cmd == "verify-map":
        cmd_verify_map(args.map)
    elif args.cmd == "rebake-map":
        cmd_rebake_map(args.map, args.work_dir)
    elif args.cmd == "prepare-cutout-patches":
        cmd_prepare_cutout_patches(args.map, args.work_dir)
    elif args.cmd == "calibrate-map":
        cmd_calibrate_map(args.map, donor_id=args.donor)
    elif args.cmd == "calibrate-all-maps":
        cmd_calibrate_all_maps(no_bump=args.no_bump)
    elif args.cmd == "repack-occluders":
        cmd_repack_occluders(args.map)
    elif args.cmd == "repack-all-occluders":
        cmd_repack_all_occluders(no_bump=args.no_bump)
    elif args.cmd == "convert-all-2x":
        cmd_convert_all_2x(no_bump=args.no_bump)


if __name__ == "__main__":
    main()
