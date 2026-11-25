//! Dual-mode Font Engine for `pal-render`:
//! - HD mode: `fontdue` TTF rasterizer with bounded glyph cache (supports primary CJK + fallback Latin TTF).
//! - Retro mode / Fallback: 1995 DOS PAL `WOR16.ASC` (8x16 ASCII) + built-in 8x8 ASCII bitmap fallback.

use crate::framebuffer::{Color, Framebuffer};
use std::collections::HashMap;

#[derive(Clone, Debug)]
struct CachedGlyph {
    width: usize,
    height: usize,
    xmin: i32,
    ymin: i32,
    advance_x: f32,
    bitmap: Vec<u8>,
}

pub struct FontEngine {
    primary_ttf: Option<fontdue::Font>,
    fallback_ttf: Option<fontdue::Font>,
    prefer_latin: bool,
    asc_bitmap: Option<Vec<u8>>, // 1995 WOR16.ASC (8x16 per ASCII char)
    glyph_cache: HashMap<(char, u16), CachedGlyph>,
    max_cached_glyphs: usize,
}

impl Default for FontEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl FontEngine {
    pub fn new() -> Self {
        Self {
            primary_ttf: None,
            fallback_ttf: None,
            prefer_latin: false,
            asc_bitmap: None,
            glyph_cache: HashMap::new(),
            max_cached_glyphs: 4096,
        }
    }

    /// When enabled (English mode), prefer the proportional Latin TTF (`fallback_ttf`) before CJK.
    pub fn set_prefer_latin(&mut self, prefer: bool) {
        if self.prefer_latin != prefer {
            self.prefer_latin = prefer;
            self.glyph_cache.clear();
        }
    }

    pub fn has_ttf(&self) -> bool {
        self.primary_ttf.is_some() || self.fallback_ttf.is_some()
    }

    pub fn load_primary_ttf(&mut self, bytes: &[u8]) -> bool {
        let settings = fontdue::FontSettings {
            scale: 40.0,
            ..Default::default()
        };
        if let Ok(font) = fontdue::Font::from_bytes(bytes, settings) {
            self.primary_ttf = Some(font);
            self.glyph_cache.clear();
            true
        } else {
            false
        }
    }

    pub fn load_fallback_ttf(&mut self, bytes: &[u8]) -> bool {
        let settings = fontdue::FontSettings {
            scale: 40.0,
            ..Default::default()
        };
        if let Ok(font) = fontdue::Font::from_bytes(bytes, settings) {
            self.fallback_ttf = Some(font);
            self.glyph_cache.clear();
            true
        } else {
            false
        }
    }

    pub fn load_wor16_asc(&mut self, bytes: Vec<u8>) {
        if bytes.len() >= 128 * 16 {
            self.asc_bitmap = Some(bytes);
        }
    }

    fn normalize_glyph_char(ch: char) -> char {
        match ch {
            // DOS PAL Big5->UTF-8 uses U+223C (TILDE OPERATOR) extensively; map to fullwidth tilde
            // so CJK fonts never render a `.notdef` tofu box (#119).
            '\u{223C}' => '～',
            '\u{2022}' => '·',
            _ => ch,
        }
    }

    fn rasterize_ttf(&mut self, ch: char, px_size: u16) -> Option<&CachedGlyph> {
        let ch = Self::normalize_glyph_char(ch);
        let key = (ch, px_size);
        if !self.glyph_cache.contains_key(&key) {
            let font = match (&self.primary_ttf, &self.fallback_ttf) {
                (Some(p), Some(f)) => {
                    if self.prefer_latin {
                        if f.lookup_glyph_index(ch) != 0 {
                            Some(f)
                        } else if p.lookup_glyph_index(ch) != 0 {
                            Some(p)
                        } else {
                            None
                        }
                    } else if p.lookup_glyph_index(ch) != 0 {
                        Some(p)
                    } else if f.lookup_glyph_index(ch) != 0 {
                        Some(f)
                    } else {
                        None
                    }
                }
                (Some(p), None) => (p.lookup_glyph_index(ch) != 0).then_some(p),
                (None, Some(f)) => (f.lookup_glyph_index(ch) != 0).then_some(f),
                (None, None) => return None,
            };

            let glyph = if let Some(font) = font {
                let (metrics, bitmap) = font.rasterize(ch, px_size as f32);
                CachedGlyph {
                    width: metrics.width,
                    height: metrics.height,
                    xmin: metrics.xmin,
                    ymin: metrics.ymin,
                    advance_x: metrics.advance_width,
                    bitmap,
                }
            } else {
                // Never draw an ugly `.notdef` tofu box `□` for rare unmapped symbols (#119).
                CachedGlyph {
                    width: 0,
                    height: 0,
                    xmin: 0,
                    ymin: 0,
                    advance_x: (px_size as f32) * 0.5,
                    bitmap: Vec::new(),
                }
            };
            if self.glyph_cache.len() >= self.max_cached_glyphs {
                self.glyph_cache.clear();
            }
            self.glyph_cache.insert(key, glyph);
        }
        self.glyph_cache.get(&key)
    }

    fn char_advance(&mut self, ch: char, px_size: u16) -> f32 {
        let px_size = px_size.max(8);
        if self.has_ttf() {
            if let Some(g) = self.rasterize_ttf(ch, px_size) {
                return g.advance_x;
            }
            return (px_size as f32) * 0.6;
        }
        if self.asc_bitmap.is_some() && (ch as u32) < 128 {
            let s = (px_size / 16).max(1) as f32;
            return 8.0 * s;
        }
        let scale = (px_size / 8).max(1) as f32;
        if ch.is_ascii() { 8.0 * scale } else { 12.0 * scale }
    }

    pub fn measure_text(&mut self, text: &str, px_size: u16) -> i32 {
        let px_size = px_size.max(8);
        if self.has_ttf() {
            let mut width = 0.0f32;
            for ch in text.chars() {
                width += self.char_advance(ch, px_size);
            }
            return width.ceil() as i32;
        }
        let scale = (px_size / 8).max(1) as i32;
        let asc_scale = (px_size / 16).max(1) as i32;
        let has_asc = self.asc_bitmap.is_some();
        let mut w = 0i32;
        for ch in text.chars() {
            if has_asc && (ch as u32) < 128 {
                w += 8 * asc_scale;
            } else {
                w += if ch.is_ascii() { 8 * scale } else { 12 * scale };
            }
        }
        w
    }

    pub fn draw_text(&mut self, fb: &mut Framebuffer, x: i32, y: i32, text: &str, px_size: u16, color: Color) {
        let px_size = px_size.max(8);
        if self.has_ttf() {
            let baseline = y + ((px_size as f32) * 0.82) as i32;
            let mut pen_x = x as f32;
            for ch in text.chars() {
                if let Some(g) = self.rasterize_ttf(ch, px_size) {
                    let gx0 = pen_x.round() as i32 + g.xmin;
                    let gy0 = baseline - g.height as i32 - g.ymin;
                    let [cr, cg, cb, ca] = color.0;
                    for row in 0..g.height {
                        let py = gy0 + row as i32;
                        for col in 0..g.width {
                            let cov = g.bitmap[row * g.width + col];
                            if cov == 0 {
                                continue;
                            }
                            let alpha = ((cov as u32 * ca as u32) / 255) as u8;
                            fb.blend_pixel(gx0 + col as i32, py, Color::rgba(cr, cg, cb, alpha));
                        }
                    }
                    pen_x += g.advance_x;
                } else {
                    pen_x += (px_size as f32) * 0.6;
                }
            }
            return;
        }

        // Fallback bitmap rendering (WOR16.ASC or built-in 8x8 ASCII)
        let scale = (px_size / 8).max(1) as i32;
        let mut cur_x = x;
        for ch in text.chars() {
            if let Some(asc) = &self.asc_bitmap {
                if (ch as u32) < 128 {
                    let base = (ch as usize) * 16;
                    let s = (px_size / 16).max(1) as i32;
                    for row in 0..16 {
                        let bits = asc[base + row];
                        for col in 0..8 {
                            if (bits & (0x80 >> col)) != 0 {
                                fb.fill_rect(cur_x + col * s, y + (row as i32) * s, s, s, color);
                            }
                        }
                    }
                    cur_x += 8 * s;
                    continue;
                }
            }
            if ch.is_ascii() {
                let glyph = ascii_8x8(ch as u8);
                for (row, &bits) in glyph.iter().enumerate() {
                    for col in 0..8 {
                        if (bits & (1 << col)) != 0 {
                            fb.fill_rect(cur_x + col * scale, y + (row as i32) * scale, scale, scale, color);
                        }
                    }
                }
                cur_x += 8 * scale;
            } else {
                // Simple placeholder box for non-ASCII when no CJK font is loaded
                fb.stroke_rect(cur_x + 1, y + 1, 10 * scale, 7 * scale, 1, color);
                cur_x += 12 * scale;
            }
        }
    }

    pub fn draw_text_outlined(
        &mut self,
        fb: &mut Framebuffer,
        x: i32,
        y: i32,
        text: &str,
        px_size: u16,
        color: Color,
        outline: Color,
    ) {
        let d = if px_size >= 24 { 2 } else { 1 };
        self.draw_text(fb, x + d, y + d, text, px_size, outline);
        self.draw_text(fb, x - d, y, text, px_size, outline);
        self.draw_text(fb, x + d, y, text, px_size, outline);
        self.draw_text(fb, x, y - d, text, px_size, outline);
        self.draw_text(fb, x, y + d, text, px_size, outline);
        self.draw_text(fb, x, y, text, px_size, color);
    }

    pub fn draw_text_centered(
        &mut self,
        fb: &mut Framebuffer,
        center_x: i32,
        y: i32,
        text: &str,
        px_size: u16,
        color: Color,
        outline: Option<Color>,
    ) {
        let w = self.measure_text(text, px_size);
        let x = center_x - w / 2;
        if let Some(oc) = outline {
            self.draw_text_outlined(fb, x, y, text, px_size, color, oc);
        } else {
            self.draw_text(fb, x, y, text, px_size, color);
        }
    }

    pub fn wrap_text(&mut self, text: &str, px_size: u16, max_width: i32) -> Vec<String> {
        let mut lines = Vec::new();
        let max_w = max_width as f32;
        for raw_line in text.split('\n') {
            if raw_line.is_empty() {
                lines.push(String::new());
                continue;
            }
            let mut cur = String::new();
            let mut cur_w = 0.0f32;
            let chars: Vec<char> = raw_line.chars().collect();
            let mut i = 0usize;
            while i < chars.len() {
                let ch = chars[i];
                // Group contiguous Latin/ASCII word characters (including apostrophes/hyphens)
                // so English words wrap cleanly at word boundaries instead of mid-word.
                let is_latin_word = |c: char| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '\'' | '’' | '-' | '.' | ',' | '!' | '?' | ':' | ';' | '"' | '“' | '”' | '(' | ')')
                };
                if is_latin_word(ch) {
                    let start = i;
                    let mut word_w = 0.0f32;
                    while i < chars.len() && is_latin_word(chars[i]) {
                        word_w += self.char_advance(chars[i], px_size);
                        i += 1;
                    }
                    let word: String = chars[start..i].iter().collect();
                    if !cur.is_empty() && (cur_w + word_w).ceil() > max_w {
                        lines.push(cur.trim_end().to_string());
                        cur.clear();
                        cur_w = 0.0;
                    }
                    if (cur_w + word_w).ceil() <= max_w {
                        cur.push_str(&word);
                        cur_w += word_w;
                    } else {
                        // Single word wider than max_width: fall back to character split.
                        for wc in word.chars() {
                            let adv = self.char_advance(wc, px_size);
                            if !cur.is_empty() && (cur_w + adv).ceil() > max_w {
                                lines.push(cur);
                                cur = wc.to_string();
                                cur_w = adv;
                            } else {
                                cur.push(wc);
                                cur_w += adv;
                            }
                        }
                    }
                } else if ch == ' ' {
                    let adv = self.char_advance(ch, px_size);
                    if !cur.is_empty() {
                        if (cur_w + adv).ceil() > max_w {
                            lines.push(cur.trim_end().to_string());
                            cur.clear();
                            cur_w = 0.0;
                        } else {
                            cur.push(' ');
                            cur_w += adv;
                        }
                    }
                    i += 1;
                } else {
                    let adv = self.char_advance(ch, px_size);
                    if !cur.is_empty() && (cur_w + adv).ceil() > max_w {
                        lines.push(cur.trim_end().to_string());
                        cur = ch.to_string();
                        cur_w = adv;
                    } else {
                        cur.push(ch);
                        cur_w += adv;
                    }
                    i += 1;
                }
            }
            if !cur.is_empty() {
                lines.push(cur.trim_end().to_string());
            }
        }
        lines
    }
}

/// Minimal 8x8 ASCII bitmap fallback for digits/letters/punctuation when no TTF is uploaded yet.
fn ascii_8x8(b: u8) -> [u8; 8] {
    match b {
        b'0' => [0x3C, 0x66, 0x6E, 0x76, 0x66, 0x66, 0x3C, 0x00],
        b'1' => [0x18, 0x1C, 0x18, 0x18, 0x18, 0x18, 0x7E, 0x00],
        b'2' => [0x3C, 0x66, 0x60, 0x38, 0x0C, 0x06, 0x7E, 0x00],
        b'3' => [0x3C, 0x66, 0x60, 0x38, 0x60, 0x66, 0x3C, 0x00],
        b'4' => [0x30, 0x38, 0x3C, 0x36, 0x7E, 0x30, 0x30, 0x00],
        b'5' => [0x7E, 0x06, 0x3E, 0x60, 0x60, 0x66, 0x3C, 0x00],
        b'6' => [0x38, 0x0C, 0x06, 0x3E, 0x66, 0x66, 0x3C, 0x00],
        b'7' => [0x7E, 0x60, 0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x00],
        b'8' => [0x3C, 0x66, 0x66, 0x3C, 0x66, 0x66, 0x3C, 0x00],
        b'9' => [0x3C, 0x66, 0x66, 0x7C, 0x60, 0x30, 0x1C, 0x00],
        b'/' => [0x40, 0x60, 0x30, 0x18, 0x0C, 0x06, 0x02, 0x00],
        b'-' => [0x00, 0x00, 0x00, 0x7E, 0x00, 0x00, 0x00, 0x00],
        b':' => [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x00, 0x00],
        b'.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x00],
        b' ' => [0x00; 8],
        b'A'..=b'Z' | b'a'..=b'z' => [0x3C, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00],
        _ => [0x00, 0x3C, 0x42, 0x42, 0x42, 0x3C, 0x00, 0x00],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_text_draws_pixels() {
        let mut fonts = FontEngine::new();
        let mut fb = Framebuffer::new(120, 40);
        fb.clear(Color::BLACK);
        fonts.draw_text(&mut fb, 4, 4, "123/456", 16, Color::WHITE);
        let non_black = fb.pixels.chunks_exact(4).filter(|p| p[0] > 0).count();
        assert!(non_black > 20);
    }

    #[test]
    fn test_normalize_glyph_char_prevents_tofu_box() {
        assert_eq!(FontEngine::normalize_glyph_char('\u{223C}'), '～');
        assert_eq!(FontEngine::normalize_glyph_char('\u{2022}'), '·');
        assert_eq!(FontEngine::normalize_glyph_char('剑'), '剑');
    }

    #[test]
    fn test_wrap_text_keeps_english_words_intact() {
        let mut fonts = FontEngine::new();
        // Fallback ASCII width at px_size=8 is 8px per char -> max_width=88 fits 11 chars.
        let lines = fonts.wrap_text("Hello world Xiaoyao", 8, 88);
        assert_eq!(lines, vec!["Hello world", "Xiaoyao"]);
    }
}
