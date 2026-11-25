#!/usr/bin/env python3
"""Zero-cost deterministic Auto-QC gate for 4x HD MGO sprites vs 1x originals.

Checks per-frame:
  1. Silhouette IoU (shadow-stripped 1x vs 4x) -> catches grid shift, double rows, missing chars, wrong poses
  2. Foreground RGB histogram similarity -> catches wrong character/outfit colors (e.g. red vs green, bandages vs cloak)
  3. Chroma fringe leak (neon magenta/green pixels) -> catches unkeyed halos & green-spot bugs
  4. Leaked grid border lines -> catches black/white rectangular frame boxes

Usage:
  python3 tools/mgo_review/auto_qc.py --all-done          # Audit all done_v2 MGOs & write qc_report.json
  python3 tools/mgo_review/auto_qc.py 240 521 632         # Audit specific MGO IDs (exit 1 if any fail)
"""
import argparse
import json
import math
import sys
from pathlib import Path

import yaml
from PIL import Image

REPAL = Path(__file__).resolve().parent.parent.parent
MGO_DIR = REPAL / "site" / "assets" / "graphics" / "mgo"
CATALOG = REPAL / "tools" / "mgo_catalog.yaml"
REPORT_PATH = Path(__file__).resolve().parent / "qc_report.json"


def is_1x_shadow(r: int, g: int, b: int, a: int, y: int, fh: int) -> bool:
    if a < 128:
        return True
    # 1995 PAL dark-green/black isometric ground shadows (only near feet / bottom 35%)
    if y >= int(fh * 0.65) and max(r, g, b) < 28:
        return True
    if y >= int(fh * 0.60) and g >= r + 3 and g >= b + 2 and max(r, g, b) < 52:
        return True
    return False


def audit_mgo(mid: int) -> dict:
    meta_p = MGO_DIR / f"mgo_{mid:03d}.json"
    p1 = MGO_DIR / f"mgo_{mid:03d}_1x.webp"
    p4 = MGO_DIR / f"mgo_{mid:03d}_4x.webp"
    if not (meta_p.exists() and p1.exists() and p4.exists()):
        return {"id": mid, "status": "fail", "score": 0, "reasons": ["missing_files"]}

    meta = json.loads(meta_p.read_text())
    im1 = Image.open(p1).convert("RGBA")
    im4 = Image.open(p4).convert("RGBA")
    frames = meta.get("frames", [])

    ious = []
    color_sims = []
    chroma_leaks = []
    border_hits = 0
    empty_4x_frames = 0
    worst_frames = []

    for fi, rect in enumerate(frames):
        fx, fy, fw, fh = rect
        if fw == 0 or fh == 0:
            continue
        f1 = im1.crop((fx, fy, fx + fw, fy + fh))
        f4 = im4.crop((fx * 4, fy * 4, (fx + fw) * 4, (fy + fh) * 4))
        f4_ds = f4.resize((fw, fh), Image.LANCZOS)

        px1 = f1.load()
        px4_ds = f4_ds.load()

        inter = 0
        union = 0
        fg1 = 0
        fg4 = 0
        hist1 = [0] * 64
        hist4 = [0] * 64

        for y in range(fh):
            for x in range(fw):
                r1, g1, b1, a1 = px1[x, y]
                r4, g4, b4, a4 = px4_ds[x, y]
                m1 = not is_1x_shadow(r1, g1, b1, a1, y, fh)
                m4 = a4 >= 48
                if m1:
                    fg1 += 1
                    hist1[(r1 >> 6) * 16 + (g1 >> 6) * 4 + (b1 >> 6)] += 1
                if m4:
                    fg4 += 1
                    hist4[(r4 >> 6) * 16 + (g4 >> 6) * 4 + (b4 >> 6)] += 1
                if m1 and m4:
                    inter += 1
                if m1 or m4:
                    union += 1

        if fg1 < 8:
            continue
        if fg4 < 4:
            empty_4x_frames += 1
            ious.append(0.0)
            color_sims.append(0.0)
            worst_frames.append((fi, 0.0, 0.0))
            continue

        iou = inter / max(1, union)
        ious.append(iou)

        # Bhattacharyya coefficient on 64-bin RGB histogram (robust & range [0, 1])
        bc = 0.0
        for h1, h4 in zip(hist1, hist4):
            if h1 and h4:
                bc += math.sqrt((h1 / fg1) * (h4 / fg4))
        color_sims.append(bc)
        worst_frames.append((fi, iou, bc))

        # Check full-res 4x for chroma fringe leak & straight border-box artifacts
        px4 = f4.load()
        w4, h4 = f4.size
        opaque4 = 0
        chroma_bad = 0
        top_edge = bot_edge = left_edge = right_edge = 0
        for y in range(h4):
            for x in range(w4):
                r, g, b, a = px4[x, y]
                if a < 48:
                    continue
                opaque4 += 1
                # Strong neon magenta or neon green leak
                if ((r + b) // 2 - g > 65 and r > 165 and b > 165) or (
                    g - max(r, b) > 65 and g > 165
                ):
                    chroma_bad += 1
                if y == 0:
                    top_edge += 1
                if y == h4 - 1:
                    bot_edge += 1
                if x == 0:
                    left_edge += 1
                if x == w4 - 1:
                    right_edge += 1

        leak = chroma_bad / max(1, opaque4)
        chroma_leaks.append(leak)

        # Border-box line check: 4x fills >55% of a border edge when 1x filled <20% of that edge
        e1_top = sum(1 for x in range(fw) if px1[x, 0][3] >= 128) / max(1, fw)
        e1_bot = sum(1 for x in range(fw) if px1[x, fh - 1][3] >= 128) / max(1, fw)
        e1_left = sum(1 for y in range(fh) if px1[0, y][3] >= 128) / max(1, fh)
        e1_right = sum(1 for y in range(fh) if px1[fw - 1, y][3] >= 128) / max(1, fh)
        if (
            (top_edge / max(1, w4) > 0.55 and e1_top < 0.20)
            or (bot_edge / max(1, w4) > 0.55 and e1_bot < 0.20)
            or (left_edge / max(1, h4) > 0.55 and e1_left < 0.20)
            or (right_edge / max(1, h4) > 0.55 and e1_right < 0.20)
        ):
            border_hits += 1

    if not ious:
        return {"id": mid, "status": "pass", "score": 100, "reasons": []}

    mean_iou = sum(ious) / len(ious)
    min_iou = min(ious)
    mean_col = sum(color_sims) / len(color_sims)
    min_col = min(color_sims)
    max_leak = max(chroma_leaks) if chroma_leaks else 0.0

    reasons = []
    if empty_4x_frames > 0:
        reasons.append(f"empty_frames:{empty_4x_frames}")
    if mean_iou < 0.42:
        reasons.append(f"low_iou:{mean_iou:.2f}")
    elif min_iou < 0.25:
        bad_f = min(worst_frames, key=lambda t: t[1])[0]
        reasons.append(f"frame_{bad_f+1}_shift_iou:{min_iou:.2f}")
    if mean_col < 0.68:
        reasons.append(f"color_mismatch:{mean_col:.2f}")
    elif min_col < 0.52:
        bad_f = min(worst_frames, key=lambda t: t[2])[0]
        reasons.append(f"frame_{bad_f+1}_color:{min_col:.2f}")
    if max_leak > 0.012:
        reasons.append(f"chroma_leak:{max_leak*100:.1f}%")
    if border_hits > 0:
        reasons.append(f"border_box:{border_hits}f")

    # Composite 0..100 score
    iou_pts = min(1.0, mean_iou / 0.65) * 45.0
    col_pts = min(1.0, mean_col / 0.85) * 40.0
    clean_pts = max(0.0, 1.0 - max_leak / 0.03) * 15.0
    penalty = border_hits * 8 + empty_4x_frames * 20
    score = max(0, min(100, round(iou_pts + col_pts + clean_pts - penalty)))

    if empty_4x_frames > 0 or mean_iou < 0.35 or mean_col < 0.58 or score < 58:
        status = "fail"
    elif reasons or score < 76:
        status = "warn"
    else:
        status = "pass"

    return {
        "id": mid,
        "status": status,
        "score": score,
        "mean_iou": round(mean_iou, 3),
        "min_iou": round(min_iou, 3),
        "mean_col": round(mean_col, 3),
        "max_leak": round(max_leak, 4),
        "border_hits": border_hits,
        "reasons": reasons,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("ids", nargs="*", type=int, help="Specific MGO IDs to audit")
    ap.add_argument("--all-done", action="store_true", help="Audit all done_v2 MGOs in catalog")
    ap.add_argument("--write", action="store_true", help="Write qc_report.json")
    args = ap.parse_args()

    if args.all_done:
        cat = yaml.safe_load(CATALOG.read_text())
        mids = [s["id"] for s in cat["sprites"] if s.get("status") == "done_v2"]
    elif args.ids:
        mids = args.ids
    else:
        ap.print_help()
        sys.exit(2)

    results = {}
    counts = {"pass": 0, "warn": 0, "fail": 0}
    for mid in mids:
        res = audit_mgo(mid)
        results[str(mid)] = res
        counts[res["status"]] += 1

    if args.write or args.all_done:
        REPORT_PATH.write_text(json.dumps(results, ensure_ascii=False, indent=2))
        print(f"Wrote {REPORT_PATH} ({len(results)} MGOs: pass={counts['pass']} warn={counts['warn']} fail={counts['fail']})")

    for mid in mids:
        r = results[str(mid)]
        if not args.all_done or r["status"] != "pass":
            print(
                f"  mgo_{mid:03d} [{r['status'].upper():4s}] score={r['score']:3d} "
                f"iou={r.get('mean_iou',0):.2f} (min {r.get('min_iou',0):.2f}) "
                f"col={r.get('mean_col',0):.2f} leak={r.get('max_leak',0)*100:.2f}% "
                f"{' '.join(r['reasons'])}"
            )

    if args.ids and counts["fail"] > 0:
        sys.exit(1)


if __name__ == "__main__":
    main()
