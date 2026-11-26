#!/usr/bin/env python3
"""Generate the RePal app icon set (ink-brush 仙 + ink-wash mountains + red 剑 seal on xuan paper).

Outputs into site/icons/:
  repal_icon.png (256, window icon / Linux), repal_icon_1024.png (master, macOS),
  repal_icon.ico (16..256, Windows exe), icon-192.png / icon-512.png (PWA "any"),
  icon-maskable-512.png (PWA "maskable", full-bleed with safe zone),
  apple-touch-icon.png (180, opaque), favicon-32.png.

Font: Ma Shan Zheng (SIL Open Font License 1.1, https://github.com/google/fonts/tree/main/ofl/mashanzheng),
downloaded at run time into .tmp/ (not committed). See site/icons/ICON_CREDITS.md.

Usage: python3 tools/make_app_icon.py [--font PATH]
"""
import argparse
import math
import random
import urllib.request
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "site" / "icons"
FONT_URL = "https://raw.githubusercontent.com/google/fonts/main/ofl/mashanzheng/MaShanZheng-Regular.ttf"
S = 1024
INK = (22, 20, 26)
SEAL = (178, 34, 28)


def get_font(path: str) -> str:
    if path:
        return path
    dst = ROOT / ".tmp" / "fonts" / "MaShanZheng-Regular.ttf"
    if not dst.is_file():
        dst.parent.mkdir(parents=True, exist_ok=True)
        urllib.request.urlretrieve(FONT_URL, dst)
    return str(dst)


def xuan_paper(seed: int) -> Image.Image:
    random.seed(seed)
    base = Image.new("RGB", (S, S), (238, 226, 198))
    fib = Image.effect_noise((S, S), 40).filter(ImageFilter.GaussianBlur(0.8))
    fib2 = Image.effect_noise((S // 4, S), 60).resize((S, S)).filter(ImageFilter.GaussianBlur(3))
    tex = ImageChops.multiply(fib.convert("RGB"), fib2.convert("RGB"))
    p = Image.blend(base, ImageChops.multiply(base, tex.point(lambda v: 150 + v // 2)), 0.35)
    v = Image.new("L", (S, S), 0)
    dv = ImageDraw.Draw(v)
    for i in range(40):
        dv.ellipse((-S * 0.25 + i * 8, -S * 0.25 + i * 8, S * 1.25 - i * 8, S * 1.25 - i * 8), fill=int(i * 6))
    v = v.filter(ImageFilter.GaussianBlur(60))
    return Image.composite(p, Image.new("RGB", (S, S), (150, 120, 80)), v).convert("RGBA")


def mountains(img: Image.Image, strength: float) -> Image.Image:
    if strength <= 0:
        return img
    m = Image.new("L", (S, S), 0)
    d = ImageDraw.Draw(m)
    for layer, (base, amp, shade) in enumerate([(0.74, 0.11, 70), (0.84, 0.08, 115)]):
        pts = [(0, S)] + [
            (x, S * base - amp * S * (0.6 * math.sin(x / 140 + layer * 2) + 0.4 * math.sin(x / 53 + layer)))
            for x in range(0, S + 16, 16)
        ] + [(S, S)]
        d.polygon(pts, fill=shade)
    m = m.filter(ImageFilter.GaussianBlur(10))
    img.paste(Image.new("RGBA", (S, S), (60, 64, 70, 255)), (0, 0), m.point(lambda v: int(v * 0.6 * strength)))
    return img


def glyph(font: str, size: int, ch: str, angle: float, dx: int, dy: int) -> Image.Image:
    f = ImageFont.truetype(font, size)
    m = Image.new("L", (S, S), 0)
    d = ImageDraw.Draw(m)
    bb = d.textbbox((0, 0), ch, font=f)
    d.text(((S - (bb[2] - bb[0])) // 2 - bb[0] + dx, (S - (bb[3] - bb[1])) // 2 - bb[1] + dy), ch, font=f, fill=255)
    return m.rotate(angle, resample=Image.BICUBIC, center=(S // 2, S // 2))


def ink(mask: Image.Image, dry: bool) -> Image.Image:
    bleed = mask.filter(ImageFilter.GaussianBlur(3)).point(lambda v: min(255, int(v * 1.25)))
    if not dry:
        return bleed
    streak = Image.effect_noise((S // 8, S), 120).resize((S, S), Image.BILINEAR).rotate(35).point(
        lambda v: 0 if v < 70 else 255
    )
    edge = ImageChops.subtract(bleed, mask.filter(ImageFilter.MinFilter(9)))
    return ImageChops.subtract(bleed, ImageChops.multiply(edge, ImageChops.invert(streak)))


def chop(img: Image.Image, font: str, text: str, x: int, y: int, s: int) -> Image.Image:
    blk = Image.new("L", (S, S), 0)
    ImageDraw.Draw(blk).rounded_rectangle((x, y, x + s, y + s), s // 10, fill=255)
    n = Image.effect_noise((S, S), 80).filter(ImageFilter.GaussianBlur(1.5)).point(lambda v: 255 if v > 100 else 0)
    blk = ImageChops.multiply(blk, ImageChops.lighter(n, blk.filter(ImageFilter.MinFilter(11))))
    f = ImageFont.truetype(font, int(s * 0.74))
    gm = Image.new("L", (S, S), 0)
    g = ImageDraw.Draw(gm)
    bb = g.textbbox((0, 0), text, font=f)
    g.text((x + (s - (bb[2] - bb[0])) // 2 - bb[0], y + (s - (bb[3] - bb[1])) // 2 - bb[1]), text, font=f, fill=255)
    img.paste(Image.new("RGBA", (S, S), SEAL + (235,)), (0, 0), ImageChops.subtract(blk, gm))
    return img


def art(font: str, small: bool, scale: float = 1.0) -> Image.Image:
    """Full-bleed square artwork. `small` = simplified for 16-32 px (faded mountains, bigger
    glyph and seal). `scale` < 1 shrinks the content toward the centre (maskable safe zone)."""
    im = xuan_paper(11)
    im = mountains(im, 0.35 if small else 1.0)
    gsize, gdx, gdy = (0.95, -20, -40) if small else (0.84, -30, -60)
    seal_s, seal_xy = (0.27, (0.66, 0.64)) if small else (0.19, (0.72, 0.68))
    g = glyph(font, int(S * gsize * scale), "仙", -5, int(gdx * scale), int(gdy * scale))
    im.paste(Image.new("RGBA", (S, S), INK + (255,)), (0, 0), ink(g, dry=not small))
    s = int(S * seal_s * scale)
    cx = int(S / 2 + (S * seal_xy[0] - S / 2) * scale)
    cy = int(S / 2 + (S * seal_xy[1] - S / 2) * scale)
    return chop(im, font, "剑", cx, cy, s)


def rounded(img: Image.Image) -> Image.Image:
    m = Image.new("L", (S, S), 0)
    ImageDraw.Draw(m).rounded_rectangle((0, 0, S - 1, S - 1), int(S * 0.2), fill=255)
    out = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    out.paste(img, (0, 0), m)
    ImageDraw.Draw(out).rounded_rectangle((3, 3, S - 4, S - 4), int(S * 0.2), outline=(90, 60, 30, 160), width=6)
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--font", default="", help="Path to MaShanZheng-Regular.ttf (downloaded if omitted)")
    font = get_font(ap.parse_args().font)
    OUT.mkdir(parents=True, exist_ok=True)

    big = rounded(art(font, small=False))
    small = rounded(art(font, small=True))
    big.save(OUT / "repal_icon_1024.png")
    big.resize((256, 256), Image.LANCZOS).save(OUT / "repal_icon.png")
    big.resize((512, 512), Image.LANCZOS).save(OUT / "icon-512.png")
    big.resize((192, 192), Image.LANCZOS).save(OUT / "icon-192.png")
    small.resize((32, 32), Image.LANCZOS).save(OUT / "favicon-32.png")
    # Maskable: full-bleed (no rounded corners), content inside the central 80% safe zone.
    art(font, small=False, scale=0.8).resize((512, 512), Image.LANCZOS).save(OUT / "icon-maskable-512.png")
    # Apple touch icon: opaque square (iOS rounds it).
    art(font, small=False).convert("RGB").resize((180, 180), Image.LANCZOS).save(OUT / "apple-touch-icon.png")
    # Windows .ico: detailed art >= 48 px, simplified art for 16/24/32.
    sizes = [16, 24, 32, 48, 64, 128, 256]
    frames = [(small if s <= 32 else big).resize((s, s), Image.LANCZOS) for s in sizes]
    frames[-1].save(OUT / "repal_icon.ico", sizes=[(s, s) for s in sizes], append_images=frames[:-1])
    print("wrote icon set to", OUT)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
