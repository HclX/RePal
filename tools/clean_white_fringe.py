#!/usr/bin/env python3
"""Clean white-matte fringe & stale RGB in alpha=0 pixels on 4x HD WebP atlases.

Pure-Pillow implementation (zero external dependencies beyond Pillow).

Targets two distinct matting defects left by early white-background AI keying:
  1. Stale/bright RGB stored inside fully transparent pixels (alpha == 0), which
     bleeds into sprite borders under GPU/Canvas bilinear (GL_LINEAR) filtering.
     NOTE: Zeroing alpha==0 pixels to (0, 0, 0, 0) causes dark/black halos under
     straight-alpha bilinear filtering; instead, the default mode bleeds the
     nearest visible edge RGB into adjacent alpha==0 pixels (alpha-bleeding) and
     only zeroes far-away transparent pixels for lossless compression.
  2. Residual 1-2px cream/white outer fringe & white-matted semi-transparent
     anti-aliased edge pixels around dark-outlined icons (e.g. legacy white-bg
     ui/ball_4x.webp).

Modes:
  (default) : Safe GPU-friendly alpha-bleed that propagates nearest visible RGB
              into adjacent alpha==0 pixels (preventing both white and black
              bilinear halos) without touching any visible (alpha > 0) pixel.
              Safe for all sprites, battle actors (f/), and soft-glow VFX (fire/).
  --full    : Full white-matte fringe cleanup (outer cream/white fringe trim
              against dark outlines + semi-transparent white-matte
              decontamination + alpha-bleed). Preserves soft colored glows by
              default (use --drop-low-alpha N only if needed for hard-edged icons).
              Intended ONLY for dark-outlined opaque icons, NOT soft-glow VFX (fire/).
"""

from __future__ import annotations

import argparse
import pathlib
from typing import Tuple

from PIL import Image, ImageFilter


def stats(im: Image.Image) -> Tuple[float, float, float]:
    """Return (a0_bright%, semi_bright%, edge_bright%) for an RGBA Image."""
    w, h = im.size
    a_band = im.getchannel("A")
    # Transparent mask: 255 where a==0 else 0
    a0_mask = a_band.point(lambda v: 255 if v == 0 else 0)
    adj_a0 = a0_mask.filter(ImageFilter.MaxFilter(3))

    px = im.load()
    adj_px = adj_a0.load()
    a0_b = a0_t = semi_b = semi_t = edge_b = edge_t = 0
    for y in range(h):
        for x in range(w):
            r, g, b, a = px[x, y]
            luma = 0.299 * r + 0.587 * g + 0.114 * b
            if a == 0:
                a0_t += 1
                if luma > 180:
                    a0_b += 1
            elif a < 240:
                semi_t += 1
                if luma > 180:
                    semi_b += 1
            elif adj_px[x, y] > 0:
                edge_t += 1
                if luma > 190:
                    edge_b += 1
    return (
        a0_b * 100.0 / max(1, a0_t),
        semi_b * 100.0 / max(1, semi_t),
        edge_b * 100.0 / max(1, edge_t),
    )


def clean_alpha_bleed(im: Image.Image, radius: int = 2) -> Image.Image:
    """Propagate nearest visible RGB into adjacent alpha==0 pixels (alpha-bleed).

    Prevents both white-matte bleed and black-halo bleed under straight-alpha
    GPU/Canvas bilinear (GL_LINEAR) filtering. Visible pixels (alpha > 0) are
    never modified. Transparent pixels beyond `radius` from any visible pixel
    are zeroed to (0, 0, 0, 0) for clean lossless WebP compression.
    """
    out = im.copy()
    w, h = out.size
    px = out.load()

    # Track which pixels currently hold valid RGB (initially all alpha > 0)
    valid = bytearray(w * h)
    frontier = []
    for y in range(h):
        row = y * w
        for x in range(w):
            if px[x, y][3] > 0:
                valid[row + x] = 1
            else:
                px[x, y] = (0, 0, 0, 0)

    # Seed initial frontier: alpha==0 pixels 8-adjacent to a valid pixel
    for y in range(h):
        for x in range(w):
            if valid[y * w + x]:
                continue
            y0, y1 = max(0, y - 1), min(h - 1, y + 1)
            x0, x1 = max(0, x - 1), min(w - 1, x + 1)
            found = False
            for ny in range(y0, y1 + 1):
                nrow = ny * w
                for nx in range(x0, x1 + 1):
                    if valid[nrow + nx]:
                        found = True
                        break
                if found:
                    break
            if found:
                frontier.append((x, y))

    # Iterative dilation for `radius` rings
    for _ in range(max(0, radius)):
        if not frontier:
            break
        updates = []
        for x, y in frontier:
            y0, y1 = max(0, y - 1), min(h - 1, y + 1)
            x0, x1 = max(0, x - 1), min(w - 1, x + 1)
            sr = sg = sb = cnt = 0
            for ny in range(y0, y1 + 1):
                nrow = ny * w
                for nx in range(x0, x1 + 1):
                    if valid[nrow + nx]:
                        r, g, b, _ = px[nx, ny]
                        sr += r
                        sg += g
                        sb += b
                        cnt += 1
            if cnt > 0:
                updates.append((x, y, sr // cnt, sg // cnt, sb // cnt))
        next_frontier_set = set()
        for x, y, r, g, b in updates:
            px[x, y] = (r, g, b, 0)
            valid[y * w + x] = 1
        for x, y, _, _, _ in updates:
            y0, y1 = max(0, y - 1), min(h - 1, y + 1)
            x0, x1 = max(0, x - 1), min(w - 1, x + 1)
            for ny in range(y0, y1 + 1):
                nrow = ny * w
                for nx in range(x0, x1 + 1):
                    if not valid[nrow + nx]:
                        next_frontier_set.add((nx, ny))
        frontier = list(next_frontier_set)

    return out


def clean_full_white_fringe(
    im: Image.Image,
    bg_rgb: Tuple[float, float, float] = (251.0, 249.0, 236.0),
    erode_passes: int = 2,
    drop_low_alpha: int = 0,
    bleed_radius: int = 2,
) -> Image.Image:
    """Remove outer cream/white matting fringe around dark-outlined icons.

    Preserves soft colored glows by default (`drop_low_alpha=0`). Pass a
    positive `drop_low_alpha` only when discarding faint neutral dust on
    hard-edged icons.
    """
    out = im.copy()
    w, h = out.size
    px = out.load()

    # Step 1 (optional): Drop near-invisible dust strictly below `drop_low_alpha`
    if drop_low_alpha > 0:
        for y in range(h):
            for x in range(w):
                if px[x, y][3] < drop_low_alpha:
                    px[x, y] = (0, 0, 0, 0)

    # Step 2: Erode 1-2px outermost cream/white fringe pixels backed by a dark outline
    for _ in range(erode_passes):
        a_band = out.getchannel("A")
        trans_mask = a_band.point(lambda v: 255 if v < 128 else 0)
        adj_trans = trans_mask.filter(ImageFilter.MaxFilter(3))
        at_px = adj_trans.load()

        # Build dark-outline mask (opaque + luma < 145), dilate 5x5
        dark_mask = Image.new("L", (w, h), 0)
        dpx = dark_mask.load()
        for y in range(h):
            for x in range(w):
                r, g, b, a = px[x, y]
                if a >= 180 and (0.299 * r + 0.587 * g + 0.114 * b) < 145:
                    dpx[x, y] = 255
        near_dark = dark_mask.filter(ImageFilter.MaxFilter(5))
        nd_px = near_dark.load()

        killed = 0
        for y in range(h):
            for x in range(w):
                r, g, b, a = px[x, y]
                if a == 0:
                    continue
                if at_px[x, y] == 0 and a >= 180:
                    continue
                luma = 0.299 * r + 0.587 * g + 0.114 * b
                sat = max(r, g, b) - min(r, g, b)
                if (at_px[x, y] > 0 and luma > 185 and sat < 36 and nd_px[x, y] > 0) or (
                    a < 180 and luma > 205 and sat < 32
                ):
                    px[x, y] = (0, 0, 0, 0)
                    killed += 1
        if killed == 0:
            break

    # Step 3: White-matte color decontamination on remaining semi-transparent edge pixels
    bg_r, bg_g, bg_b = bg_rgb
    for y in range(h):
        for x in range(w):
            r, g, b, a = px[x, y]
            if 0 < a < 250:
                luma = 0.299 * r + 0.587 * g + 0.114 * b
                sat = max(r, g, b) - min(r, g, b)
                # Only decontaminate neutral/bright matte pixels; leave rich colored glows intact
                if luma > 135 and sat < 80:
                    af = max(0.15, a / 255.0)
                    nr = max(0, min(r, round((r - (1.0 - af) * bg_r) / af)))
                    ng = max(0, min(g, round((g - (1.0 - af) * bg_g) / af)))
                    nb = max(0, min(b, round((b - (1.0 - af) * bg_b) / af)))
                    px[x, y] = (nr, ng, nb, a)

    return clean_alpha_bleed(out, radius=bleed_radius)


def process_file(
    path: pathlib.Path,
    full: bool,
    dry_run: bool,
    drop_low_alpha: int = 0,
    bleed_radius: int = 2,
) -> None:
    im = Image.open(path).convert("RGBA")
    before = stats(im)
    cleaned = (
        clean_full_white_fringe(im, drop_low_alpha=drop_low_alpha, bleed_radius=bleed_radius)
        if full
        else clean_alpha_bleed(im, radius=bleed_radius)
    )
    after = stats(cleaned)
    print(
        f"{path.name:22s}: "
        f"a0_b {before[0]:5.1f}%->{after[0]:4.1f}% | "
        f"semi_b {before[1]:5.1f}%->{after[1]:4.1f}% | "
        f"edge_b {before[2]:5.1f}%->{after[2]:4.1f}%"
    )
    if not dry_run:
        cleaned.save(path, "WEBP", lossless=True)


def main() -> None:
    ap = argparse.ArgumentParser(description="Clean white-fringe matting artifacts on 4x WebPs")
    ap.add_argument("paths", nargs="+", type=pathlib.Path)
    ap.add_argument(
        "--full",
        action="store_true",
        help="Full white-fringe removal + decontamination (dark-outlined icons only; not for fire/ VFX)",
    )
    ap.add_argument(
        "--drop-low-alpha",
        type=int,
        default=0,
        help="With --full, drop pixels with alpha < N (default: 0, preserves soft glows)",
    )
    ap.add_argument(
        "--bleed-radius",
        type=int,
        default=2,
        help="Pixel radius for alpha-bleed edge RGB propagation into alpha==0 (default: 2)",
    )
    ap.add_argument("--dry-run", action="store_true", help="Report stats without overwriting")
    args = ap.parse_args()
    for p in args.paths:
        process_file(
            p,
            full=args.full,
            dry_run=args.dry_run,
            drop_low_alpha=args.drop_low_alpha,
            bleed_radius=args.bleed_radius,
        )


if __name__ == "__main__":
    main()
