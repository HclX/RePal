#!/usr/bin/env python3
"""Audit cross-frame visual consistency across all `mgo_*_4x.webp` sprites,
and prepare/bake single-sheet unified 4x HD remasters for MGO sprite sheets.

Replaces `crates/pal-headless/examples/mgo_consistency.rs` to stay consistent
with the rest of `tools/*.py`.

Usage:
  python3 tools/mgo_consistency.py audit
  python3 tools/mgo_consistency.py prepare <mgo_id>
  python3 tools/mgo_consistency.py bake <mgo_id> <remastered_img>
  python3 tools/mgo_consistency.py preview <out_dir> <id1> [id2 ...]
"""
from collections import deque
import json
import math
import sys
from pathlib import Path
from typing import List, Optional, Tuple

from PIL import Image

REPAL_ROOT = Path(__file__).resolve().parent.parent
MGO_DIR = REPAL_ROOT / "site" / "assets" / "graphics" / "mgo"
TMP_DIR = REPAL_ROOT / ".tmp"
TMP_HD = TMP_DIR / "hd_mgo"

SHADOW_SEEDS = {(0, 36, 0), (0, 32, 20), (0, 28, 0)}
SHADOW_COLORS = SHADOW_SEEDS | {
    (48, 28, 16),
    (28, 20, 12),
    (48, 20, 8),
    (52, 0, 0),
    (36, 20, 12),
    (20, 16, 8),
}


def load_mgo_meta(path: Path) -> Optional[dict]:
    if not path.exists():
        return None
    try:
        v = json.loads(path.read_text())
        return {
            "id": int(v["id"]),
            "w": int(v["w"]),
            "h": int(v["h"]),
            "hdTrackB": bool(v.get("hdTrackB", False)),
            "frames": [[int(c) for c in f[:4]] for f in v["frames"] if len(f) >= 4],
        }
    except Exception:
        return None


def frame_rgb_hist(img: Image.Image, x0: int, y0: int, w: int, h: int) -> Tuple[List[float], int]:
    hist = [0.0] * 64
    cnt = 0
    iw, ih = img.size
    px = img.load()
    for dy in range(h):
        y = y0 + dy
        if y >= ih:
            continue
        for dx in range(w):
            x = x0 + dx
            if x >= iw:
                continue
            r, g, b, a = px[x, y]
            if a >= 128:
                hist[((r >> 6) << 4) | ((g >> 6) << 2) | (b >> 6)] += 1.0
                cnt += 1
    if cnt > 0:
        inv = 1.0 / cnt
        hist = [v * inv for v in hist]
    return hist, cnt


def frame_mae_4x_vs_1x(img1: Image.Image, img4: Image.Image, rect: List[int]) -> float:
    fx, fy, fw, fh = rect
    if fw == 0 or fh == 0:
        return 0.0
    c1 = img1.crop((fx, fy, fx + fw, fy + fh))
    c4 = img4.crop((fx * 4, fy * 4, (fx + fw) * 4, (fy + fh) * 4)).resize((fw, fh), Image.BOX)
    p1 = c1.load()
    p4 = c4.load()
    err = 0.0
    cnt = 0
    for y in range(fh):
        for x in range(fw):
            r1, g1, b1, a1 = p1[x, y]
            if a1 < 128:
                continue
            r4, g4, b4, a4 = p4[x, y]
            if a4 >= 64:
                err += (abs(r4 - r1) + abs(g4 - g1) + abs(b4 - b1)) / 3.0
                cnt += 1
    return err / cnt if cnt else 0.0


def run_audit() -> None:
    entries = sorted(MGO_DIR.glob("*.json"))
    rows = []
    scanned = 0
    for jp in entries:
        meta = load_mgo_meta(jp)
        if not meta or len(meta["frames"]) < 2:
            continue
        mid = meta["id"]
        p1 = MGO_DIR / f"mgo_{mid:03d}_1x.webp"
        p4 = MGO_DIR / f"mgo_{mid:03d}_4x.webp"
        if not (p1.exists() and p4.exists()):
            continue
        img1 = Image.open(p1).convert("RGBA")
        img4 = Image.open(p4).convert("RGBA")
        if img4.width != img1.width * 4 or img4.height != img1.height * 4:
            continue
        scanned += 1
        n = len(meta["frames"])
        h1s, h4s, maes, cnts = [], [], [], []
        for r in meta["frames"]:
            h1, c1 = frame_rgb_hist(img1, r[0], r[1], r[2], r[3])
            h4, _ = frame_rgb_hist(img4, r[0] * 4, r[1] * 4, r[2] * 4, r[3] * 4)
            h1s.append(h1)
            h4s.append(h4)
            maes.append(frame_mae_4x_vs_1x(img1, img4, r))
            cnts.append(c1)

        max_pair_drift = 0.0
        worst_pair = [0, 0]
        max_adj_jump = 0.0
        worst_adj = [0, 0]
        for i in range(n):
            if cnts[i] < 40:
                continue
            for j in range(i + 1, n):
                if cnts[j] < 40:
                    continue
                d1 = sum(abs(a - b) for a, b in zip(h1s[i], h1s[j]))
                d4 = sum(abs(a - b) for a, b in zip(h4s[i], h4s[j]))
                drift = max(0.0, d4 - d1)
                if drift > max_pair_drift:
                    max_pair_drift = drift
                    worst_pair = [i, j]
                if j == i + 1 and drift > max_adj_jump:
                    max_adj_jump = drift
                    worst_adj = [i, j]

        valid_maes = [m for m, c in zip(maes, cnts) if c >= 40]
        min_mae = min(valid_maes) if valid_maes else 0.0
        max_mae = max(valid_maes) if valid_maes else 0.0
        mae_spread = max(0.0, max_mae - min_mae)
        score = max_pair_drift * 50.0 + max_adj_jump * 50.0 + mae_spread * 1.2
        rows.append(
            {
                "id": mid,
                "frames": n,
                "hdTrackB": meta["hdTrackB"],
                "score": round(score, 2),
                "pair_drift": round(max_pair_drift, 3),
                "worst_pair": worst_pair,
                "adj_jump": round(max_adj_jump, 3),
                "worst_adj": worst_adj,
                "min_mae": round(min_mae, 1),
                "max_mae": round(max_mae, 1),
                "mae_spread": round(mae_spread, 1),
            }
        )

    rows.sort(key=lambda r: r["score"], reverse=True)
    TMP_DIR.mkdir(parents=True, exist_ok=True)
    out_p = TMP_DIR / "mgo_consistency_audit.json"
    out_p.write_text(json.dumps(rows, indent=2))
    print(f"Scanned {scanned} multi-frame MGO sprites (report -> {out_p})")
    print(
        f"{'MGO_ID':>7} {'FRAMES':>6} {'TRACKB':>6} {'SCORE':>7} "
        f"{'PAIR_DRIFT':>10} {'ADJ_JUMP':>10} {'MAE_SPRD':>10} {'JUMP_AT':>10}"
    )
    for r in rows[:35]:
        a0, a1 = r["worst_adj"]
        tb = "yes" if r["hdTrackB"] else "no"
        print(
            f"mgo_{r['id']:03d} {r['frames']:>6} {tb:>6} {r['score']:>7.2f} "
            f"{r['pair_drift']:>10.3f} {r['adj_jump']:>10.3f} {r['mae_spread']:>10.1f}   {a0:>2}->{a1:<2}"
        )


def choose_grid(n: int, cell_w: int, cell_h: int) -> Tuple[int, int]:
    best = (n, 1)
    best_err = math.inf
    for cols in range(1, n + 1):
        rows = (n + cols - 1) // cols
        ar = (cols * cell_w) / max(1, rows * cell_h)
        err = abs(ar - 1.75)
        if err < best_err:
            best_err = err
            best = (cols, rows)
    return best


def strip_dos_shadow_1x(crop: Image.Image) -> Tuple[Image.Image, bool]:
    w, h = crop.size
    if w == 0 or h == 0:
        return crop.copy(), False
    y0 = int(h * 0.30)
    px = crop.load()
    seeds = []
    for y in range(y0, h):
        for x in range(w):
            r, g, b, a = px[x, y]
            if a > 0 and (r, g, b) in SHADOW_SEEDS:
                seeds.append((x, y))
    if len(seeds) < 3:
        return crop.copy(), False
    out = crop.copy()
    opx = out.load()
    rm = [[False] * w for _ in range(h)]
    stack = list(seeds)
    for sx, sy in stack:
        rm[sy][sx] = True
    while stack:
        cx, cy = stack.pop()
        for dy in (-1, 0, 1):
            ny = cy + dy
            if ny < y0 or ny >= h:
                continue
            for dx in (-1, 0, 1):
                nx = cx + dx
                if 0 <= nx < w and not rm[ny][nx]:
                    r, g, b, a = opx[nx, ny]
                    if a > 0 and (r, g, b) in SHADOW_COLORS:
                        rm[ny][nx] = True
                        stack.append((nx, ny))
    for y in range(h):
        for x in range(w):
            if rm[y][x]:
                opx[x, y] = (0, 0, 0, 0)
    return out, True


def pick_chroma_bg(img1: Image.Image, frames: List[List[int]]) -> Tuple[Tuple[int, int, int, int], str]:
    px = img1.load()
    has_magenta = False
    for fx, fy, fw, fh in frames:
        for dy in range(fh):
            for dx in range(fw):
                r, g, b, a = px[fx + dx, fy + dy]
                if a >= 128 and r > 160 and b > 160 and g < 110:
                    has_magenta = True
                    break
            if has_magenta:
                break
        if has_magenta:
            break
    if not has_magenta:
        return (255, 0, 255, 255), "#FF00FF"
    return (0, 224, 80, 255), "#00E050"


def run_prepare(mid: int) -> None:
    meta = load_mgo_meta(MGO_DIR / f"mgo_{mid:03d}.json")
    if not meta:
        raise SystemExit(f"Failed to load metadata for mgo_{mid:03d}")
    img1 = Image.open(MGO_DIR / f"mgo_{mid:03d}_1x.webp").convert("RGBA")
    frames = meta["frames"]
    max_fw = max((r[2] for r in frames), default=32)
    max_fh = max((r[3] for r in frames), default=32)
    pad = 24
    cell_w = max_fw * 4 + pad * 2
    cell_h = max_fh * 4 + pad * 2
    cols, rows = choose_grid(len(frames), cell_w, cell_h)
    canvas_w = cols * cell_w
    canvas_h = rows * cell_h

    bg, bg_hex = pick_chroma_bg(img1, frames)
    sheet = Image.new("RGBA", (canvas_w, canvas_h), bg)
    cells = []
    any_shadow = False

    for idx, (fx, fy, fw, fh) in enumerate(frames):
        c = idx % cols
        r = idx // cols
        ox = c * cell_w + (cell_w - fw * 4) // 2
        oy = r * cell_h + (cell_h - fh * 4) // 2
        if fw > 0 and fh > 0:
            raw_crop = img1.crop((fx, fy, fx + fw, fy + fh))
            crop, stripped = strip_dos_shadow_1x(raw_crop)
            if stripped:
                any_shadow = True
            up = crop.resize((fw * 4, fh * 4), Image.NEAREST)
            # Composite opaque pixels onto solid chroma bg
            up_px = up.load()
            sh_px = sheet.load()
            for y in range(up.height):
                for x in range(up.width):
                    pr, pg, pb, pa = up_px[x, y]
                    if pa >= 128:
                        sh_px[ox + x, oy + y] = (pr, pg, pb, 255)
        cells.append(
            {
                "frame": idx,
                "ox": ox,
                "oy": oy,
                "w": fw * 4,
                "h": fh * 4,
                "src_1x": [fx, fy, fw, fh],
            }
        )

    TMP_HD.mkdir(parents=True, exist_ok=True)
    png_path = TMP_HD / f"mgo_{mid:03d}_in.png"
    sheet.convert("RGB").save(png_path, "PNG")
    layout_path = TMP_HD / f"mgo_{mid:03d}_layout.json"
    layout = {
        "id": mid,
        "cols": cols,
        "rows": rows,
        "cell_w": cell_w,
        "cell_h": cell_h,
        "canvas_w": canvas_w,
        "canvas_h": canvas_h,
        "bg_hex": bg_hex,
        "shadow_stripped": any_shadow,
        "cells": cells,
    }
    layout_path.write_text(json.dumps(layout, indent=2))
    print(
        f"Prepared {png_path} ({canvas_w}x{canvas_h}, {len(frames)} frames "
        f"in {cols}x{rows} grid, bg={bg_hex}, shadowStripped={any_shadow})"
    )


def _sample_global_bg(raw_ai: Image.Image) -> Tuple[int, int, int]:
    rw, rh = raw_ai.size
    ix = max(4, min(24, rw // 40))
    iy = max(4, min(24, rh // 40))
    pts = [
        (ix, iy),
        (rw - 1 - ix, iy),
        (ix, rh - 1 - iy),
        (rw - 1 - ix, rh - 1 - iy),
        (rw // 4, iy),
        (rw // 2, iy),
        (rw * 3 // 4, iy),
        (rw // 4, rh - 1 - iy),
        (rw // 2, rh - 1 - iy),
        (rw * 3 // 4, rh - 1 - iy),
        (ix, rh // 3),
        (ix, rh * 2 // 3),
        (rw - 1 - ix, rh // 3),
        (rw - 1 - ix, rh * 2 // 3),
    ]
    px = raw_ai.load()
    rs = sorted(px[x, y][0] for x, y in pts)
    gs = sorted(px[x, y][1] for x, y in pts)
    bs = sorted(px[x, y][2] for x, y in pts)
    m = len(rs) // 2
    return rs[m], gs[m], bs[m]


def run_bake(mid: int, ai_path: str) -> None:
    meta = load_mgo_meta(MGO_DIR / f"mgo_{mid:03d}.json")
    if not meta:
        raise SystemExit(f"Failed to load metadata for mgo_{mid:03d}")
    img1 = Image.open(MGO_DIR / f"mgo_{mid:03d}_1x.webp").convert("RGBA")
    layout = json.loads((TMP_HD / f"mgo_{mid:03d}_layout.json").read_text())
    cols = int(layout["cols"])
    rows = int(layout["rows"])
    shadow_stripped = bool(layout.get("shadow_stripped", False))

    raw_ai = Image.open(ai_path).convert("RGBA")
    rw, rh = raw_ai.size
    raw_px = raw_ai.load()

    bg_r, bg_g, bg_b = _sample_global_bg(raw_ai)
    is_magenta_bg = bg_r > 150 and bg_b > 150 and ((bg_r + bg_b) // 2) > bg_g + 35
    is_green_bg = bg_g > 150 and bg_g > bg_r + 45 and bg_g > bg_b + 35
    is_chroma_bg = is_magenta_bg or is_green_bg

    out_4x = Image.new("RGBA", (meta["w"] * 4, meta["h"] * 4), (0, 0, 0, 0))

    for cell in layout["cells"]:
        frame_idx = int(cell["frame"])
        w = int(cell["w"])
        h = int(cell["h"])
        fx, fy, fw, fh = [int(v) for v in cell["src_1x"]]
        if w == 0 or h == 0 or fw == 0 or fh == 0:
            continue

        raw_crop_1x = img1.crop((fx, fy, fx + fw, fy + fh))
        crop_1x, _ = strip_dos_shadow_1x(raw_crop_1x) if shadow_stripped else (raw_crop_1x, False)
        mask_4x = crop_1x.resize((w, h), Image.BICUBIC)
        m_px = mask_4x.load()

        if is_chroma_bg:
            c_idx = frame_idx % cols
            r_idx = frame_idx // cols
            raw_x0 = (c_idx * rw) // cols
            raw_x1 = ((c_idx + 1) * rw) // cols
            raw_y0 = (r_idx * rh) // rows
            raw_y1 = ((r_idx + 1) * rh) // rows
            trim_x = 8 if (cols > 1 and (raw_x1 - raw_x0) > 40) else 0
            trim_y = 8 if (rows > 1 and (raw_y1 - raw_y0) > 40) else 0
            cell_x0 = raw_x0 + (trim_x if c_idx > 0 else (3 if cols > 1 else 0))
            cell_x1 = raw_x1 - (trim_x if c_idx + 1 < cols else (3 if cols > 1 else 0))
            cell_y0 = raw_y0 + (trim_y if r_idx > 0 else (3 if rows > 1 else 0))
            cell_y1 = raw_y1 - (trim_y if r_idx + 1 < rows else (3 if rows > 1 else 0))
            cw = max(1, cell_x1 - cell_x0)
            ch = max(1, cell_y1 - cell_y0)

            # Per-cell local corner median chroma sampling
            cr_s, cg_s, cb_s = [], [], []
            for cx, cy in (
                (cell_x0 + 2, cell_y0 + 2),
                (cell_x1 - 3, cell_y0 + 2),
                (cell_x0 + 2, cell_y1 - 3),
                (cell_x1 - 3, cell_y1 - 3),
            ):
                p = raw_px[max(0, min(rw - 1, cx)), max(0, min(rh - 1, cy))]
                is_c = (
                    ((p[0] + p[2]) // 2) > p[1] + 35
                    if is_magenta_bg
                    else p[1] > max(p[0], p[2]) + 35
                )
                if is_c:
                    cr_s.append(p[0])
                    cg_s.append(p[1])
                    cb_s.append(p[2])
            if cr_s:
                cr_s.sort(); cg_s.sort(); cb_s.sort()
                m = len(cr_s) // 2
                cbg_r, cbg_g, cbg_b = cr_s[m], cg_s[m], cb_s[m]
            else:
                cbg_r, cbg_g, cbg_b = bg_r, bg_g, bg_b

            # Extract cell pixel grid once for speed
            sub = raw_ai.crop((cell_x0, cell_y0, cell_x0 + cw, cell_y0 + ch))
            spx = sub.load()
            is_bg = [[False] * cw for _ in range(ch)]
            bfs = deque()

            def is_ext_bg(r: int, g: int, b: int) -> bool:
                d = abs(r - cbg_r) + abs(g - cbg_g) + abs(b - cbg_b)
                dom = ((r + b) // 2 - g) if is_magenta_bg else (g - max(r, b))
                return (d < 150 and dom > 25) or (dom > 45 and d < 240)

            for lx in range(cw):
                for ly in (0, ch - 1):
                    r, g, b, _ = spx[lx, ly]
                    if not is_bg[ly][lx] and is_ext_bg(r, g, b):
                        is_bg[ly][lx] = True
                        bfs.append((lx, ly))
            for ly in range(ch):
                for lx in (0, cw - 1):
                    r, g, b, _ = spx[lx, ly]
                    if not is_bg[ly][lx] and is_ext_bg(r, g, b):
                        is_bg[ly][lx] = True
                        bfs.append((lx, ly))
            while bfs:
                cx, cy = bfs.popleft()
                for dx, dy in ((-1, 0), (1, 0), (0, -1), (0, 1)):
                    nx, ny = cx + dx, cy + dy
                    if 0 <= nx < cw and 0 <= ny < ch and not is_bg[ny][nx]:
                        r, g, b, _ = spx[nx, ny]
                        if is_ext_bg(r, g, b):
                            is_bg[ny][nx] = True
                            bfs.append((nx, ny))

            # Enclosed interior background gaps (between legs/arms/staff)
            for ly in range(ch):
                for lx in range(cw):
                    if is_bg[ly][lx]:
                        continue
                    r, g, b, _ = spx[lx, ly]
                    d = abs(r - cbg_r) + abs(g - cbg_g) + abs(b - cbg_b)
                    dom = ((r + b) // 2 - g) if is_magenta_bg else (g - max(r, b))
                    if is_magenta_bg:
                        if r > g + 50 and b > g + 50 and dom > 60 and d < 140:
                            is_bg[ly][lx] = True
                    else:
                        if d < 110 and dom > 90:
                            is_bg[ly][lx] = True

            clean = Image.new("RGBA", (cw, ch), (0, 0, 0, 0))
            cpx = clean.load()
            min_x, min_y, max_x, max_y = cw, ch, 0, 0
            for ly in range(ch):
                for lx in range(cw):
                    if is_bg[ly][lx]:
                        continue
                    r, g, b, _ = spx[lx, ly]
                    adj_bg = False
                    for dy in range(-2, 3):
                        ny = ly + dy
                        if ny < 0 or ny >= ch:
                            adj_bg = True
                            break
                        row_bg = is_bg[ny]
                        for dx in range(-2, 3):
                            nx = lx + dx
                            if nx < 0 or nx >= cw or row_bg[nx]:
                                adj_bg = True
                                break
                        if adj_bg:
                            break
                    a = 255
                    if adj_bg:
                        d = abs(r - cbg_r) + abs(g - cbg_g) + abs(b - cbg_b)
                        dom = ((r + b) // 2 - g) if is_magenta_bg else (g - max(r, b))
                        if dom > 32 and d < 240:
                            continue
                        a_f = max(0.0, min(1.0, (d - 135.0) / 135.0))
                        if a_f < 0.10:
                            continue
                        orig_r, orig_g, orig_b = r, g, b
                        r = max(0, min(255, round((r - (1.0 - a_f) * cbg_r) / a_f)))
                        g = max(0, min(255, round((g - (1.0 - a_f) * cbg_g) / a_f)))
                        b = max(0, min(255, round((b - (1.0 - a_f) * cbg_b) / a_f)))
                        if is_magenta_bg:
                            g = min(g, orig_g)
                            cap = min(255, g + 10)
                            if r > cap and b > cap:
                                spill = ((r + b) // 2) - cap
                                r = max(0, r - spill)
                                b = max(0, b - spill)
                        else:
                            r = min(r, orig_r)
                            b = min(b, orig_b)
                            cap = min(255, max(r, b) + 10)
                            if g > cap:
                                g = cap
                        a = round(a_f * 255.0)
                    if a >= 16:
                        cpx[lx, ly] = (r, g, b, a)
                        if a >= 64:
                            if lx < min_x: min_x = lx
                            if ly < min_y: min_y = ly
                            if lx > max_x: max_x = lx
                            if ly > max_y: max_y = ly

            if min_x <= max_x and min_y <= max_y:
                # Tight bbox of 1x shadow-stripped target silhouette (mask_4x)
                t_min_x, t_min_y, t_max_x, t_max_y = w, h, 0, 0
                for y in range(h):
                    for x in range(w):
                        if m_px[x, y][3] >= 128:
                            if x < t_min_x: t_min_x = x
                            if y < t_min_y: t_min_y = y
                            if x > t_max_x: t_max_x = x
                            if y > t_max_y: t_max_y = y
                ai_w = max_x - min_x + 1
                ai_h = max_y - min_y + 1
                t_h = (t_max_y - t_min_y + 1) if t_min_y <= t_max_y else h
                t_cx = (t_min_x + t_max_x) * 0.5 if t_min_x <= t_max_x else w * 0.5
                t_bot = t_max_y if t_min_y <= t_max_y else (h - 1)

                scale = max(0.20, min(2.00, t_h / ai_h))
                if ai_w * scale > w:
                    scale = w / ai_w
                if ai_h * scale > h:
                    scale = h / ai_h
                dst_w = max(1, min(w, round(ai_w * scale)))
                dst_h = max(1, min(h, round(ai_h * scale)))

                cropped = clean.crop((min_x, min_y, max_x + 1, max_y + 1))
                scaled = cropped.resize((dst_w, dst_h), Image.LANCZOS) if (dst_w != ai_w or dst_h != ai_h) else cropped
                dst_x = max(0, min(w - dst_w, round(t_cx - dst_w * 0.5)))
                dst_y = max(0, min(h - dst_h, t_bot + 1 - dst_h))
                out_4x.alpha_composite(scaled, (fx * 4 + dst_x, fy * 4 + dst_y))
                print(
                    f"  frame {frame_idx:02d}: ai={ai_w}x{ai_h} -> dst={dst_w}x{dst_h} "
                    f"(scale={scale:.3f}) placed=({dst_x},{dst_y}) bot={t_bot}"
                )
            continue

        # Non-chroma fallback: preserve 1x silhouette mask directly
        canvas_w = int(layout["canvas_w"])
        canvas_h = int(layout["canvas_h"])
        ai_sheet = raw_ai.resize((canvas_w, canvas_h), Image.LANCZOS) if (rw, rh) != (canvas_w, canvas_h) else raw_ai
        ox, oy = int(cell["ox"]), int(cell["oy"])
        apx = ai_sheet.load()
        opx = out_4x.load()
        for y in range(h):
            for x in range(w):
                ma = m_px[x, y][3]
                if ma < 64:
                    continue
                sx = max(0, min(canvas_w - 1, ox + x))
                sy = max(0, min(canvas_h - 1, oy + y))
                r, g, b, _ = apx[sx, sy]
                opx[fx * 4 + x, fy * 4 + y] = (r, g, b, 255 if ma >= 180 else ma)

    out_webp = MGO_DIR / f"mgo_{mid:03d}_4x.webp"
    out_4x.save(out_webp, "WEBP", lossless=True)
    print(f"Baked {out_webp} ({out_4x.width}x{out_4x.height})")


def run_preview(out_dir: str, ids: List[int]) -> None:
    dst = Path(out_dir)
    dst.mkdir(parents=True, exist_ok=True)
    for mid in ids:
        p4 = MGO_DIR / f"mgo_{mid:03d}_4x.webp"
        if not p4.exists():
            continue
        im = Image.open(p4).convert("RGBA")
        bg = Image.new("RGBA", im.size, (43, 48, 58, 255))
        bg.alpha_composite(im)
        bg.convert("RGB").save(dst / f"mgo_{mid:03d}_4x.png", "PNG")
    print(f"Exported {len(ids)} preview PNGs to {out_dir}")


def main() -> None:
    if len(sys.argv) < 2:
        print("Usage: mgo_consistency.py <audit | prepare <id> | bake <id> <img_path> | preview <dir> <ids...>>")
        sys.exit(2)
    cmd = sys.argv[1]
    if cmd == "audit":
        run_audit()
    elif cmd == "prepare":
        run_prepare(int(sys.argv[2]))
    elif cmd == "bake":
        run_bake(int(sys.argv[2]), sys.argv[3])
    elif cmd == "preview":
        run_preview(sys.argv[2], [int(x) for x in sys.argv[3:]])
    else:
        print(f"Unknown command: {cmd}")
        sys.exit(2)


if __name__ == "__main__":
    main()
