#!/usr/bin/env python3
"""Contact-sheet prepare & bake tool for 4x HD RNG cutscenes (`graphics/rng/rng_XX/`).

Packs 9 consecutive 320x200 1x frames into a 3x3 (960x600, exact 16:10) contact sheet so an
image-generation model can remaster 9 frames at once with consistent style, palette, and
character proportions, then slices the AI output back into 1280x800 `f_XXX_4x.webp` frames.

Usage:
  python3 tools/rng_remaster.py status
  python3 tools/rng_remaster.py prepare <rng_id> [--cols 3 --rows 3 --out .tmp/hd_rng]
  python3 tools/rng_remaster.py bake <rng_id> <sheet_idx> <ai_image> [--cols 3 --rows 3]
"""

from __future__ import annotations
import argparse
import glob
import math
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
for pylibs in (ROOT / ".pylibs", ROOT.parent / ".pylibs"):
    if pylibs.is_dir() and str(pylibs) not in sys.path:
        sys.path.insert(0, str(pylibs))

RNG_ROOT = ROOT / "site" / "assets" / "graphics" / "rng"


def list_1x_frames(rng_id: int) -> list[Path]:
    d = RNG_ROOT / f"rng_{rng_id:02d}"
    return sorted(d.glob("f_*_1x.webp"))


def cmd_status(_args: argparse.Namespace) -> None:
    dirs = sorted(RNG_ROOT.glob("rng_*"))
    total_1x, total_4x = 0, 0
    for d in dirs:
        f1 = sorted(d.glob("f_*_1x.webp"))
        f4 = sorted(d.glob("f_*_4x.webp"))
        total_1x += len(f1)
        total_4x += len(f4)
        sheets = math.ceil(len(f1) / 9) if f1 else 0
        print(f"  {d.name}: {len(f4):>3}/{len(f1):<3} 4x done  ({sheets} x 3x3 sheets)")
    print(f"TOTAL: {total_4x}/{total_1x} 4x frames done across {len(dirs)} RNG cutscenes.")


def cmd_prepare(args: argparse.Namespace) -> None:
    from PIL import Image
    frames = list_1x_frames(args.rng_id)
    if not frames:
        sys.exit(f"No 1x frames found for rng_{args.rng_id:02d} in {RNG_ROOT}")
    cols, rows = args.cols, args.rows
    per_sheet = cols * rows
    out_dir = Path(args.out) / f"rng_{args.rng_id:02d}"
    out_dir.mkdir(parents=True, exist_ok=True)

    cell_w, cell_h = 320, 200
    num_sheets = math.ceil(len(frames) / per_sheet)
    for si in range(num_sheets):
        batch = frames[si * per_sheet : (si + 1) * per_sheet]
        sheet = Image.new("RGB", (cols * cell_w, rows * cell_h), (0, 0, 0))
        for idx, fp in enumerate(batch):
            im = Image.open(fp).convert("RGB")
            if im.size != (cell_w, cell_h):
                im = im.resize((cell_w, cell_h), Image.Resampling.NEAREST)
            cx = (idx % cols) * cell_w
            cy = (idx // cols) * cell_h
            sheet.paste(im, (cx, cy))
        out_path = out_dir / f"sheet_{si:02d}_in.png"
        sheet.save(out_path, "PNG")
        first_idx = si * per_sheet
        last_idx = first_idx + len(batch) - 1
        print(f"Prepared {out_path} ({sheet.size[0]}x{sheet.size[1]}, frames f_{first_idx:03d}..f_{last_idx:03d})")


def cmd_bake(args: argparse.Namespace) -> None:
    from PIL import Image
    frames = list_1x_frames(args.rng_id)
    if not frames:
        sys.exit(f"No 1x frames found for rng_{args.rng_id:02d}")
    cols, rows = args.cols, args.rows
    per_sheet = cols * rows
    start = args.sheet_idx * per_sheet
    batch = frames[start : start + per_sheet]
    if not batch:
        sys.exit(f"Sheet index {args.sheet_idx} out of range (total frames={len(frames)})")

    ai = Image.open(args.ai_image).convert("RGBA")
    # Image generators fill the entire output canvas edge-to-edge with the cols x rows grid
    # (even when output aspect ratio is 3:2 or 16:9 vs 16:10 input), so slice the full
    # canvas directly into cols x rows before resizing each cell to 1280x800.
    cw = ai.width / cols
    ch = ai.height / rows
    out_dir = RNG_ROOT / f"rng_{args.rng_id:02d}"
    for idx, fp in enumerate(batch):
        r = idx // cols
        c = idx % cols
        # Trim 3px inner border to avoid seam bleed between adjacent cells
        x0 = int(round(c * cw)) + 3
        y0 = int(round(r * ch)) + 3
        x1 = int(round((c + 1) * cw)) - 3
        y1 = int(round((r + 1) * ch)) - 3
        cell = ai.crop((x0, y0, max(x0 + 1, x1), max(y0 + 1, y1)))
        cell = cell.resize((1280, 800), Image.Resampling.LANCZOS)
        out_name = fp.name.replace("_1x.webp", "_4x.webp")
        out_path = out_dir / out_name
        cell.save(out_path, "WEBP", quality=90, method=6)
    first_idx = start
    last_idx = start + len(batch) - 1
    print(f"Baked {len(batch)} HD frames: rng_{args.rng_id:02d}/f_{first_idx:03d}_4x.webp .. f_{last_idx:03d}_4x.webp")


def main() -> None:
    p = argparse.ArgumentParser(description="4x HD RNG Cutscene Remaster Pipeline")
    sub = p.add_subparsers(dest="cmd", required=True)

    sub.add_parser("status", help="Show 4x HD progress for all 12 RNG cutscenes")

    pp = sub.add_parser("prepare", help="Pack 1x RNG frames into 3x3 contact sheets")
    pp.add_argument("rng_id", type=int, help="RNG cutscene id (0..11)")
    pp.add_argument("--cols", type=int, default=3)
    pp.add_argument("--rows", type=int, default=3)
    pp.add_argument("--out", default=".tmp/hd_rng")

    pb = sub.add_parser("bake", help="Slice an AI-remastered contact sheet into 1280x800 _4x.webp frames")
    pb.add_argument("rng_id", type=int, help="RNG cutscene id (0..11)")
    pb.add_argument("sheet_idx", type=int, help="0-based sheet index")
    pb.add_argument("ai_image", help="Path to AI-remastered sheet (.png or .jpg)")
    pb.add_argument("--cols", type=int, default=3)
    pb.add_argument("--rows", type=int, default=3)

    args = p.parse_args()
    if args.cmd == "status":
        cmd_status(args)
    elif args.cmd == "prepare":
        cmd_prepare(args)
    elif args.cmd == "bake":
        cmd_bake(args)


if __name__ == "__main__":
    main()
