#!/usr/bin/env python3
"""End-to-end 1x -> 4x HD remaster pipeline for Chinese Paladin item icons (ball_1x -> ball_4x).

Pipeline:
  1. prepare : Extract all non-empty 48x47 icons from ball_1x.webp into 4x3 grids
               (12 icons/sheet) on solid pure magenta (#FF00FF) with item names from items.yaml.
  2. bake    : Slice 4x3 AI output -> 8px interior seam trim -> per-cell corner median
               chroma sampling -> exterior BFS flood-fill -> strict interior hole check ->
               2px despill & unblend (no green boost) -> zero alpha=0 RGB -> align to 1x bbox
               inside 192x188 slot -> save per-icon RGBA PNG.
  3. assemble: Stitch all baked per-icon PNGs into RePal_Assets/assets/graphics/ui/ball_4x.webp.
  4. qc      : Deterministic Auto-QC across all 229 icons (IoU, RGB correlation, magenta leak,
               a0_bright, edge_bright, empty check) -> .tmp/hd_ball/qc_report.json.
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import sys
from collections import deque
from typing import Dict, List, Tuple

import yaml
from PIL import Image, ImageFilter

import os

REPAL = pathlib.Path(__file__).resolve().parents[1]
ASSETS = pathlib.Path(os.environ.get("REPAL_ASSETS_DIR", str(REPAL.parent / "RePal_Assets" / "assets")))
BALL_JSON = ASSETS / "graphics" / "ui" / "ball.json"
BALL_1X = ASSETS / "graphics" / "ui" / "ball_1x.webp"
BALL_4X = ASSETS / "graphics" / "ui" / "ball_4x.webp"
ITEMS_YAML = ASSETS / "data" / "items.yaml"

WORK_DIR = REPAL / ".tmp" / "hd_ball"
SHEETS_DIR = WORK_DIR / "sheets"
BAKED_DIR = WORK_DIR / "baked"
MANIFEST_PATH = WORK_DIR / "manifest.json"
QC_PATH = WORK_DIR / "qc_report.json"

COLS = 4
ROWS = 3
PER_SHEET = COLS * ROWS
PAD = 24  # 24px magenta margin around 192x188 icon -> 240x236 cell


def load_icon_names() -> Dict[int, str]:
    raw = yaml.safe_load(ITEMS_YAML.read_text())
    items = raw["items"] if isinstance(raw, dict) else raw
    by_icon: Dict[int, List[str]] = {}
    for it in items:
        if not isinstance(it, dict):
            continue
        iid = it.get("icon_id")
        if iid is None:
            iid = it.get("bitmap")
        if iid is not None:
            name = (it.get("name") or "").strip()
            if name and name not in by_icon.setdefault(int(iid), []):
                by_icon[int(iid)].append(name)
    return {k: "/".join(v[:2]) for k, v in by_icon.items()}


def cmd_prepare() -> None:
    meta = json.loads(BALL_JSON.read_text())
    im1 = Image.open(BALL_1X).convert("RGBA")
    names = load_icon_names()

    SHEETS_DIR.mkdir(parents=True, exist_ok=True)
    BAKED_DIR.mkdir(parents=True, exist_ok=True)

    non_empty = []
    for idx, (x, y, w, h) in enumerate(meta["frames"]):
        crop = im1.crop((x, y, x + w, y + h))
        if crop.getbbox() is not None:
            non_empty.append((idx, x, y, w, h))

    max_w = max(r[3] for r in non_empty)
    max_h = max(r[4] for r in non_empty)
    cell_w = max_w * 4 + PAD * 2
    cell_h = max_h * 4 + PAD * 2

    sheets = []
    for s_idx in range(0, len(non_empty), PER_SHEET):
        chunk = non_empty[s_idx : s_idx + PER_SHEET]
        sheet_num = s_idx // PER_SHEET
        canvas = Image.new("RGBA", (COLS * cell_w, ROWS * cell_h), (255, 0, 255, 255))
        cpx = canvas.load()
        cells_meta = []
        for pos, (icon_id, fx, fy, fw, fh) in enumerate(chunk):
            c = pos % COLS
            r = pos // COLS
            crop = im1.crop((fx, fy, fx + fw, fy + fh))
            up = crop.resize((fw * 4, fh * 4), Image.NEAREST)
            ox = c * cell_w + (cell_w - up.width) // 2
            oy = r * cell_h + (cell_h - up.height) // 2
            upx = up.load()
            for yy in range(up.height):
                for xx in range(up.width):
                    pr, pg, pb, pa = upx[xx, yy]
                    if pa >= 128:
                        cpx[ox + xx, oy + yy] = (pr, pg, pb, 255)
            cells_meta.append(
                {
                    "pos": pos,
                    "icon_id": icon_id,
                    "name": names.get(icon_id, f"item_{icon_id}"),
                    "rect_1x": [fx, fy, fw, fh],
                }
            )
        out_p = SHEETS_DIR / f"sheet_{sheet_num:02d}_in.png"
        canvas.convert("RGB").save(out_p, "PNG")
        sheets.append(
            {
                "sheet": sheet_num,
                "input_path": str(out_p),
                "cols": COLS,
                "rows": ROWS,
                "cell_w": cell_w,
                "cell_h": cell_h,
                "count": len(chunk),
                "cells": cells_meta,
            }
        )

    MANIFEST_PATH.write_text(
        json.dumps(
            {"total_non_empty": len(non_empty), "sheets": sheets},
            ensure_ascii=False,
            indent=2,
        )
    )
    print(
        f"Prepared {len(sheets)} sheets ({len(non_empty)} non-empty icons, "
        f"{COLS}x{ROWS} grid, {COLS*cell_w}x{ROWS*cell_h}px) -> {MANIFEST_PATH}"
    )


def _chroma_key_cell(sub: Image.Image, cbg: Tuple[int, int, int]) -> Image.Image:
    cw, ch = sub.size
    spx = sub.load()
    cbg_r, cbg_g, cbg_b = cbg

    is_bg = [[False] * cw for _ in range(ch)]
    bfs: deque = deque()

    def is_ext_bg(r: int, g: int, b: int) -> bool:
        d = abs(r - cbg_r) + abs(g - cbg_g) + abs(b - cbg_b)
        dom = (r + b) // 2 - g
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

    # Strict dual-channel interior hole check (e.g. inside rings/handles/straps)
    for ly in range(ch):
        for lx in range(cw):
            if is_bg[ly][lx]:
                continue
            r, g, b, _ = spx[lx, ly]
            d = abs(r - cbg_r) + abs(g - cbg_g) + abs(b - cbg_b)
            dom = (r + b) // 2 - g
            if r > g + 50 and b > g + 50 and dom > 60 and d < 140:
                is_bg[ly][lx] = True

    clean = Image.new("RGBA", (cw, ch), (0, 0, 0, 0))
    cpx = clean.load()
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
                dom = (r + b) // 2 - g
                if dom > 32 and d < 240:
                    continue
                a_f = max(0.0, min(1.0, (d - 135.0) / 135.0))
                if a_f < 0.10:
                    continue
                orig_g = g
                r = max(0, min(255, round((r - (1.0 - a_f) * cbg_r) / a_f)))
                g = max(0, min(255, round((g - (1.0 - a_f) * cbg_g) / a_f)))
                b = max(0, min(255, round((b - (1.0 - a_f) * cbg_b) / a_f)))
                g = min(g, orig_g)
                cap = min(255, g + 10)
                if r > cap and b > cap:
                    r = min(r, cap)
                    b = min(b, cap)
                a = max(0, min(255, round(a_f * 255.0)))
            cpx[lx, ly] = (r, g, b, a)
    return clean


def cmd_bake(sheet_num: int, hd_path: str) -> None:
    manifest = json.loads(MANIFEST_PATH.read_text())
    smeta = manifest["sheets"][sheet_num]
    im1 = Image.open(BALL_1X).convert("RGBA")
    raw_ai = Image.open(hd_path).convert("RGBA")
    rw, rh = raw_ai.size
    raw_px = raw_ai.load()

    cols, rows = smeta["cols"], smeta["rows"]
    BAKED_DIR.mkdir(parents=True, exist_ok=True)

    # Sample global fallback bg
    pts = [(8, 8), (rw - 9, 8), (8, rh - 9), (rw - 9, rh - 9), (rw // 2, 8), (rw // 2, rh - 9)]
    rs = sorted(raw_px[x, y][0] for x, y in pts)
    gs = sorted(raw_px[x, y][1] for x, y in pts)
    bs = sorted(raw_px[x, y][2] for x, y in pts)
    gbg = (rs[len(rs) // 2], gs[len(gs) // 2], bs[len(bs) // 2])

    for cell in smeta["cells"]:
        pos = cell["pos"]
        icon_id = cell["icon_id"]
        fx, fy, fw, fh = cell["rect_1x"]
        c_idx = pos % cols
        r_idx = pos // cols

        raw_x0 = (c_idx * rw) // cols
        raw_x1 = ((c_idx + 1) * rw) // cols
        raw_y0 = (r_idx * rh) // rows
        raw_y1 = ((r_idx + 1) * rh) // rows

        trim_x = 8 if (raw_x1 - raw_x0) > 40 else 0
        trim_y = 8 if (raw_y1 - raw_y0) > 40 else 0
        x0 = raw_x0 + (trim_x if c_idx > 0 else 3)
        x1 = raw_x1 - (trim_x if c_idx + 1 < cols else 3)
        y0 = raw_y0 + (trim_y if r_idx > 0 else 3)
        y1 = raw_y1 - (trim_y if r_idx + 1 < rows else 3)

        # Per-cell corner median sampling
        cr, cg, cb = [], [], []
        for cx, cy in ((x0 + 2, y0 + 2), (x1 - 3, y0 + 2), (x0 + 2, y1 - 3), (x1 - 3, y1 - 3)):
            p = raw_px[max(0, min(rw - 1, cx)), max(0, min(rh - 1, cy))]
            if (p[0] + p[2]) // 2 > p[1] + 35:
                cr.append(p[0]); cg.append(p[1]); cb.append(p[2])
        if cr:
            cr.sort(); cg.sort(); cb.sort()
            m = len(cr) // 2
            cbg = (cr[m], cg[m], cb[m])
        else:
            cbg = gbg

        sub = raw_ai.crop((x0, y0, x1, y1))
        clean = _chroma_key_cell(sub, cbg)

        # Fit cleanly into 4x target slot (fw*4 x fh*4), aligning to 1x bbox proportions
        tw, th = fw * 4, fh * 4
        slot = Image.new("RGBA", (tw, th), (0, 0, 0, 0))
        bb_ai = clean.getbbox()
        bb_1x = im1.crop((fx, fy, fx + fw, fy + fh)).getbbox()
        if bb_ai and bb_1x:
            ai_crop = clean.crop(bb_ai)
            # Target bbox from 1x (scaled 4x), capped with 6px breathing room if not edge-to-edge
            b1_w = (bb_1x[2] - bb_1x[0]) * 4
            b1_h = (bb_1x[3] - bb_1x[1]) * 4
            # Special case: icon 95 (披风) was hard-cropped in 1x; center nicely with 12px margin
            if icon_id == 95:
                b1_w, b1_h = tw - 24, th - 24
                cx1, cy1 = tw / 2.0, th / 2.0
            else:
                cx1 = (bb_1x[0] + bb_1x[2]) * 2.0
                cy1 = (bb_1x[1] + bb_1x[3]) * 2.0
            scale = min(b1_w / max(1, ai_crop.width), b1_h / max(1, ai_crop.height))
            nw = max(1, min(tw, round(ai_crop.width * scale)))
            nh = max(1, min(th, round(ai_crop.height * scale)))
            resized = ai_crop.resize((nw, nh), Image.LANCZOS)
            px0 = max(0, min(tw - nw, round(cx1 - nw / 2.0)))
            py0 = max(0, min(th - nh, round(cy1 - nh / 2.0)))
            slot.paste(resized, (px0, py0), resized)

        slot.save(BAKED_DIR / f"icon_{icon_id:03d}.png", "PNG")

    print(f"Baked sheet {sheet_num:02d} ({smeta['count']} icons) from {hd_path}")


def cmd_assemble() -> None:
    meta = json.loads(BALL_JSON.read_text())
    w4, h4 = meta["w"] * 4, meta["h"] * 4
    atlas = Image.new("RGBA", (w4, h4), (0, 0, 0, 0))
    placed = 0
    for idx, (fx, fy, fw, fh) in enumerate(meta["frames"]):
        p = BAKED_DIR / f"icon_{idx:03d}.png"
        if p.exists():
            ic = Image.open(p).convert("RGBA")
            atlas.paste(ic, (fx * 4, fy * 4), ic)
            placed += 1
    atlas.save(BALL_4X, "WEBP", lossless=True)
    print(f"Assembled {placed} icons into {BALL_4X} ({w4}x{h4}, lossless RGBA)")


def cmd_qc() -> None:
    meta = json.loads(BALL_JSON.read_text())
    im1 = Image.open(BALL_1X).convert("RGBA")
    im4 = Image.open(BALL_4X).convert("RGBA")
    names = load_icon_names()

    results = []
    counts = {"pass": 0, "warn": 0, "fail": 0}
    tot_a0_b = tot_a0 = tot_edge_b = tot_edge = 0

    for idx, (fx, fy, fw, fh) in enumerate(meta["frames"]):
        c1 = im1.crop((fx, fy, fx + fw, fy + fh))
        if c1.getbbox() is None:
            continue
        c4 = im4.crop((fx * 4, fy * 4, (fx + fw) * 4, (fy + fh) * 4))
        c4_ds = c4.resize((fw, fh), Image.BILINEAR)

        p1 = c1.load()
        p4_ds = c4_ds.load()
        inter = union = 0
        h1 = [0] * 24
        h4 = [0] * 24
        for y in range(fh):
            for x in range(fw):
                a1 = p1[x, y][3] >= 128
                a4 = p4_ds[x, y][3] >= 128
                if a1 and a4:
                    inter += 1
                if a1 or a4:
                    union += 1
                if a1:
                    r, g, b, _ = p1[x, y]
                    h1[r // 32] += 1; h1[8 + g // 32] += 1; h1[16 + b // 32] += 1
                if a4:
                    r, g, b, _ = p4_ds[x, y]
                    h4[r // 32] += 1; h4[8 + g // 32] += 1; h4[16 + b // 32] += 1
        iou = inter / max(1, union)
        dot = sum(a * b for a, b in zip(h1, h4))
        n1 = math.sqrt(sum(a * a for a in h1))
        n4 = math.sqrt(sum(b * b for b in h4))
        col = dot / max(1e-6, n1 * n4)

        # Full-res matting checks (magenta leak, a0_bright, edge_bright)
        w4, h4_sz = c4.size
        a_band = c4.getchannel("A")
        a0_mask = a_band.point(lambda v: 255 if v == 0 else 0)
        adj_a0 = a0_mask.filter(ImageFilter.MaxFilter(3)).load()
        p4 = c4.load()
        leak = fg_cnt = a0_b = a0_t = edge_b = edge_t = 0
        for y in range(h4_sz):
            for x in range(w4):
                r, g, b, a = p4[x, y]
                luma = 0.299 * r + 0.587 * g + 0.114 * b
                if a == 0:
                    a0_t += 1
                    if luma > 180:
                        a0_b += 1
                else:
                    fg_cnt += 1
                    if r > 170 and b > 170 and g < 110 and (r - g) > 75 and (b - g) > 75:
                        leak += 1
                    if a >= 240 and adj_a0[x, y] > 0:
                        edge_t += 1
                        if luma > 190:
                            edge_b += 1
        tot_a0_b += a0_b; tot_a0 += a0_t
        tot_edge_b += edge_b; tot_edge += edge_t
        leak_pct = leak * 100.0 / max(1, fg_cnt)

        reasons = []
        if fg_cnt < 100:
            reasons.append("empty")
        if iou < 0.45 and idx != 95:
            reasons.append(f"low_iou:{iou:.2f}")
        if col < 0.65:
            reasons.append(f"low_col:{col:.2f}")
        if leak_pct > 0.5:
            reasons.append(f"mag_leak:{leak_pct:.2f}%")
        if a0_b > 0:
            reasons.append(f"a0_bright:{a0_b}")

        if "empty" in reasons or (iou < 0.30 and idx != 95) or leak_pct > 2.0:
            status = "fail"
        elif reasons:
            status = "warn"
        else:
            status = "pass"
        counts[status] += 1
        results.append(
            {
                "icon_id": idx,
                "name": names.get(idx, ""),
                "status": status,
                "iou": round(iou, 3),
                "col": round(col, 3),
                "leak_pct": round(leak_pct, 3),
                "a0_bright": a0_b,
                "reasons": reasons,
            }
        )

    QC_PATH.write_text(json.dumps({"counts": counts, "icons": results}, ensure_ascii=False, indent=2))
    print(
        f"Ball Auto-QC ({len(results)} icons): pass={counts['pass']} warn={counts['warn']} fail={counts['fail']} | "
        f"global a0_bright={tot_a0_b*100/max(1,tot_a0):.2f}% edge_bright={tot_edge_b*100/max(1,tot_edge):.2f}%"
    )
    for r in results:
        if r["status"] != "pass":
            print(
                f"  icon_{r['icon_id']:03d} [{r['status'].upper():4s}] {r['name']:12s} "
                f"iou={r['iou']:.2f} col={r['col']:.2f} leak={r['leak_pct']:.2f}% {' '.join(r['reasons'])}"
            )


def main() -> None:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("prepare")
    p_bake = sub.add_parser("bake")
    p_bake.add_argument("sheet", type=int)
    p_bake.add_argument("hd_path", type=str)
    sub.add_parser("assemble")
    sub.add_parser("qc")
    args = ap.parse_args()
    if args.cmd == "prepare":
        cmd_prepare()
    elif args.cmd == "bake":
        cmd_bake(args.sheet, args.hd_path)
    elif args.cmd == "assemble":
        cmd_assemble()
    elif args.cmd == "qc":
        cmd_qc()


if __name__ == "__main__":
    main()
