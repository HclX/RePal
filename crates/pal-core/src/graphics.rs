//! VGA 256-color palettes, RLE sprites, FBP backgrounds, and RNG differential cutscenes.

#[derive(Clone)]
pub struct Palette {
    pub colors: [[u8; 4]; 256],
}

impl Palette {
    /// Parse a 768-byte VGA 6-bit RGB slice (`0..63` per channel) into 8-bit RGBA (`0..255`).
    pub fn from_vga6(rgb768: &[u8]) -> Option<Self> {
        if rgb768.len() < 768 {
            return None;
        }
        let mut colors = [[0u8; 4]; 256];
        for i in 0..256 {
            let r6 = rgb768[i * 3].min(63);
            let g6 = rgb768[i * 3 + 1].min(63);
            let b6 = rgb768[i * 3 + 2].min(63);
            colors[i] = [
                (r6 << 2) | (r6 >> 4),
                (g6 << 2) | (g6 >> 4),
                (b6 << 2) | (b6 >> 4),
                255,
            ];
        }
        Some(Self { colors })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // width * height * 4 (RGBA)
}

impl RgbaImage {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0u8; (width as usize) * (height as usize) * 4],
        }
    }
}

/// Decode a DOS PAL RLE sprite into an `RgbaImage` (transparent pixels have `[0, 0, 0, 0]`).
pub fn decode_rle_sprite(rle: &[u8], pal: &Palette) -> Option<RgbaImage> {
    if rle.len() < 4 {
        return None;
    }
    let off = if rle.len() >= 8 && rle[0..4] == [0x02, 0x00, 0x00, 0x00] {
        4
    } else {
        0
    };
    if off + 4 > rle.len() {
        return None;
    }
    let w = u16::from_le_bytes([rle[off], rle[off + 1]]) as u32;
    let h = u16::from_le_bytes([rle[off + 2], rle[off + 3]]) as u32;
    if w == 0 || h == 0 || w > 1024 || h > 1024 {
        return None;
    }

    let total_px = (w as usize) * (h as usize);
    let mut img = RgbaImage::new(w, h);
    let mut src = off + 4;
    let mut dst_px = 0usize;

    while src < rle.len() && dst_px < total_px {
        let t = rle[src];
        src += 1;
        if (t & 0x80) != 0 {
            let skip = (t - 0x80) as usize;
            dst_px = (dst_px + skip).min(total_px);
        } else {
            let count = t as usize;
            for _ in 0..count {
                if src >= rle.len() || dst_px >= total_px {
                    break;
                }
                let idx = rle[src] as usize;
                src += 1;
                let c = pal.colors[idx];
                let p = dst_px * 4;
                img.pixels[p] = c[0];
                img.pixels[p + 1] = c[1];
                img.pixels[p + 2] = c[2];
                img.pixels[p + 3] = 255;
                dst_px += 1;
            }
        }
    }

    Some(img)
}

/// Decode a `320x200` (`64,000` byte) raw indexed image (`FBP` or `RNG` canvas) using `pal`.
pub fn decode_indexed_320x200(raw: &[u8], pal: &Palette) -> Option<RgbaImage> {
    if raw.len() < 64000 {
        return None;
    }
    let mut img = RgbaImage::new(320, 200);
    for i in 0..64000 {
        let c = pal.colors[raw[i] as usize];
        let p = i * 4;
        img.pixels[p] = c[0];
        img.pixels[p + 1] = c[1];
        img.pixels[p + 2] = c[2];
        img.pixels[p + 3] = 255;
    }
    Some(img)
}

/// Apply one `RNG.MKF` differential opcode stream onto a persistent `320x200` (`64,000` byte) canvas.
pub fn rng_step(canvas: &mut [u8; 64000], diff: &[u8]) -> Result<(), &'static str> {
    let mut p = 0usize;
    let mut dst = 0usize;

    while p < diff.len() {
        let b = diff[p];
        p += 1;
        match b {
            0x00 | 0x13 => break,
            0x01 | 0x05 => {}
            0x02 => {
                dst += 2;
            }
            0x03 => {
                let n = *diff.get(p).ok_or("RNG 0x03 EOF")? as usize;
                p += 1;
                dst += (n + 1) * 2;
            }
            0x04 => {
                let b0 = *diff.get(p).ok_or("RNG 0x04 EOF")? as usize;
                let b1 = *diff.get(p + 1).ok_or("RNG 0x04 EOF")? as usize;
                p += 2;
                dst += ((b0 | (b1 << 8)) + 1) * 2;
            }
            0x06..=0x0a => {
                let c = ((b - 5) as usize) * 2;
                let slice = diff.get(p..p + c).ok_or("RNG 0x06..0a EOF")?;
                let end = (dst + c).min(64000);
                if dst < 64000 {
                    canvas[dst..end].copy_from_slice(&slice[0..end - dst]);
                }
                p += c;
                dst += c;
            }
            0x0b => {
                let n = (*diff.get(p).ok_or("RNG 0x0b EOF")? as usize + 1) * 2;
                p += 1;
                let slice = diff.get(p..p + n).ok_or("RNG 0x0b data EOF")?;
                let end = (dst + n).min(64000);
                if dst < 64000 {
                    canvas[dst..end].copy_from_slice(&slice[0..end - dst]);
                }
                p += n;
                dst += n;
            }
            0x0c => {
                let b0 = *diff.get(p).ok_or("RNG 0x0c EOF")? as usize;
                let b1 = *diff.get(p + 1).ok_or("RNG 0x0c EOF")? as usize;
                p += 2;
                let n = ((b0 | (b1 << 8)) + 1) * 2;
                let slice = diff.get(p..p + n).ok_or("RNG 0x0c data EOF")?;
                let end = (dst + n).min(64000);
                if dst < 64000 {
                    canvas[dst..end].copy_from_slice(&slice[0..end - dst]);
                }
                p += n;
                dst += n;
            }
            0x0d..=0x10 => {
                let n = (b - 0x0b) as usize;
                let d0 = *diff.get(p).ok_or("RNG 0x0d..10 EOF")?;
                let d1 = *diff.get(p + 1).ok_or("RNG 0x0d..10 EOF")?;
                p += 2;
                for _ in 0..n {
                    if dst + 1 < 64000 {
                        canvas[dst] = d0;
                        canvas[dst + 1] = d1;
                    }
                    dst += 2;
                }
            }
            0x11 => {
                let n = *diff.get(p).ok_or("RNG 0x11 EOF")? as usize + 1;
                p += 1;
                let d0 = *diff.get(p).ok_or("RNG 0x11 EOF")?;
                let d1 = *diff.get(p + 1).ok_or("RNG 0x11 EOF")?;
                p += 2;
                for _ in 0..n {
                    if dst + 1 < 64000 {
                        canvas[dst] = d0;
                        canvas[dst + 1] = d1;
                    }
                    dst += 2;
                }
            }
            0x12 => {
                let b0 = *diff.get(p).ok_or("RNG 0x12 EOF")? as usize;
                let b1 = *diff.get(p + 1).ok_or("RNG 0x12 EOF")? as usize;
                p += 2;
                let n = (b0 | (b1 << 8)) + 1;
                let d0 = *diff.get(p).ok_or("RNG 0x12 EOF")?;
                let d1 = *diff.get(p + 1).ok_or("RNG 0x12 EOF")?;
                p += 2;
                for _ in 0..n {
                    if dst + 1 < 64000 {
                        canvas[dst] = d0;
                        canvas[dst + 1] = d1;
                    }
                    dst += 2;
                }
            }
            _ => return Err("Unknown RNG opcode"),
        }
    }
    Ok(())
}
