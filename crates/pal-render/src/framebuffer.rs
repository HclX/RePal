//! RGBA Software Framebuffer & 2D Compositing Primitives.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Color(pub [u8; 4]);

impl Color {
    pub const TRANSPARENT: Self = Self([0, 0, 0, 0]);
    pub const BLACK: Self = Self([0, 0, 0, 255]);
    pub const WHITE: Self = Self([255, 255, 255, 255]);
    pub const GOLD_HI: Self = Self([255, 216, 116, 255]);
    pub const PAPER_TEXT: Self = Self([242, 232, 210, 255]);
    pub const MUTED_TEXT: Self = Self([188, 178, 158, 255]);
    pub const CYAN_SPEAKER: Self = Self([110, 232, 232, 255]);
    pub const BEVEL_LIGHT: Self = Self([212, 189, 146, 255]);
    pub const BEVEL_DARK: Self = Self([74, 57, 36, 255]);
    pub const PANEL_TOP: Self = Self([58, 45, 31, 236]);
    pub const PANEL_BOT: Self = Self([34, 26, 18, 242]);
    pub const SEL_BG: Self = Self([124, 90, 44, 235]);
    pub const HP_GREEN: Self = Self([64, 198, 104, 255]);
    pub const MP_BLUE: Self = Self([74, 156, 236, 255]);
    pub const DANGER_RED: Self = Self([232, 82, 82, 255]);

    #[inline]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self([r, g, b, a])
    }
}

#[derive(Clone, Debug)]
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Framebuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0u8; (width as usize) * (height as usize) * 4],
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        self.pixels.resize((width as usize) * (height as usize) * 4, 0);
    }

    pub fn clear(&mut self, color: Color) {
        if color.0 == [0, 0, 0, 0] {
            self.pixels.fill(0);
            return;
        }
        let [r, g, b, a] = color.0;
        let row_bytes = (self.width as usize) * 4;
        if row_bytes == 0 || self.pixels.len() < row_bytes {
            return;
        }
        for chunk in self.pixels[..row_bytes].chunks_exact_mut(4) {
            chunk[0] = r;
            chunk[1] = g;
            chunk[2] = b;
            chunk[3] = a;
        }
        let (first_row, rest) = self.pixels.split_at_mut(row_bytes);
        for row in rest.chunks_exact_mut(row_bytes) {
            row.copy_from_slice(first_row);
        }
    }

    #[inline]
    pub fn blend_pixel(&mut self, x: i32, y: i32, c: Color) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let sa = c.0[3] as u32;
        if sa == 0 {
            return;
        }
        let idx = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        if sa == 255 {
            self.pixels[idx] = c.0[0];
            self.pixels[idx + 1] = c.0[1];
            self.pixels[idx + 2] = c.0[2];
            self.pixels[idx + 3] = 255;
            return;
        }
        let inv = 255 - sa;
        let dr = self.pixels[idx] as u32;
        let dg = self.pixels[idx + 1] as u32;
        let db = self.pixels[idx + 2] as u32;
        let da = self.pixels[idx + 3] as u32;
        self.pixels[idx] = ((c.0[0] as u32 * sa + dr * inv) / 255) as u8;
        self.pixels[idx + 1] = ((c.0[1] as u32 * sa + dg * inv) / 255) as u8;
        self.pixels[idx + 2] = ((c.0[2] as u32 * sa + db * inv) / 255) as u8;
        self.pixels[idx + 3] = (sa + (da * inv) / 255).min(255) as u8;
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: Color) {
        if w <= 0 || h <= 0 || color.0[3] == 0 {
            return;
        }
        let x0 = x.max(0).min(self.width as i32);
        let y0 = y.max(0).min(self.height as i32);
        let x1 = (x + w).max(0).min(self.width as i32);
        let y1 = (y + h).max(0).min(self.height as i32);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let [sr, sg, sb, sa] = color.0;
        let stride = (self.width as usize) * 4;
        if sa == 255 {
            for py in y0..y1 {
                let row_start = (py as usize) * stride + (x0 as usize) * 4;
                let row_end = (py as usize) * stride + (x1 as usize) * 4;
                for px in self.pixels[row_start..row_end].chunks_exact_mut(4) {
                    px[0] = sr;
                    px[1] = sg;
                    px[2] = sb;
                    px[3] = 255;
                }
            }
        } else {
            let sa32 = sa as u32;
            let inv = 255 - sa32;
            for py in y0..y1 {
                let row_start = (py as usize) * stride + (x0 as usize) * 4;
                let row_end = (py as usize) * stride + (x1 as usize) * 4;
                for px in self.pixels[row_start..row_end].chunks_exact_mut(4) {
                    px[0] = ((sr as u32 * sa32 + px[0] as u32 * inv) / 255) as u8;
                    px[1] = ((sg as u32 * sa32 + px[1] as u32 * inv) / 255) as u8;
                    px[2] = ((sb as u32 * sa32 + px[2] as u32 * inv) / 255) as u8;
                    px[3] = (sa32 + (px[3] as u32 * inv) / 255).min(255) as u8;
                }
            }
        }
    }

    pub fn fill_rect_vgrad(&mut self, x: i32, y: i32, w: i32, h: i32, top: Color, bot: Color) {
        if w <= 0 || h <= 0 {
            return;
        }
        let denom = (h - 1).max(1) as f32;
        for dy in 0..h {
            let t = dy as f32 / denom;
            let lerp = |a: u8, b: u8| -> u8 { (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8 };
            let c = Color([
                lerp(top.0[0], bot.0[0]),
                lerp(top.0[1], bot.0[1]),
                lerp(top.0[2], bot.0[2]),
                lerp(top.0[3], bot.0[3]),
            ]);
            self.fill_rect(x, y + dy, w, 1, c);
        }
    }

    pub fn stroke_rect(&mut self, x: i32, y: i32, w: i32, h: i32, thickness: i32, color: Color) {
        if w <= 0 || h <= 0 || thickness <= 0 {
            return;
        }
        self.fill_rect(x, y, w, thickness, color);
        self.fill_rect(x, y + h - thickness, w, thickness, color);
        self.fill_rect(x, y + thickness, thickness, (h - thickness * 2).max(0), color);
        self.fill_rect(x + w - thickness, y + thickness, thickness, (h - thickness * 2).max(0), color);
    }

    /// Authentic 1995 DOS PAL beveled bronze/wood panel box.
    pub fn draw_bevel_box(&mut self, x: i32, y: i32, w: i32, h: i32, scale: i32) {
        let s = scale.max(1);
        let border = (s / 2).max(1);
        // Outer dark outline
        self.stroke_rect(x - border, y - border, w + border * 2, h + border * 2, border, Color::rgba(14, 10, 6, 255));
        // Vertical gradient fill
        self.fill_rect_vgrad(x, y, w, h, Color::PANEL_TOP, Color::PANEL_BOT);
        // Top & left highlight bevel
        self.fill_rect(x, y, w, border, Color::BEVEL_LIGHT);
        self.fill_rect(x, y, border, h, Color::BEVEL_LIGHT);
        // Bottom & right shadow bevel
        self.fill_rect(x, y + h - border, w, border, Color::BEVEL_DARK);
        self.fill_rect(x + w - border, y, border, h, Color::BEVEL_DARK);
        // Subtle inner hairline
        let inset = border + 1;
        if w > inset * 2 + 2 && h > inset * 2 + 2 {
            self.stroke_rect(x + inset, y + inset, w - inset * 2, h - inset * 2, 1, Color::rgba(223, 210, 186, 64));
        }
    }

    /// Blit a sub-rectangle of an RGBA source buffer onto this framebuffer with nearest-neighbor scaling.
    #[allow(clippy::too_many_arguments)]
    pub fn blit_region_scaled(
        &mut self,
        src: &[u8],
        src_w: u32,
        src_h: u32,
        sx: i32,
        sy: i32,
        sw: i32,
        sh: i32,
        dx: i32,
        dy: i32,
        dw: i32,
        dh: i32,
        alpha_mul: u8,
    ) {
        if sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0 || alpha_mul == 0 || src_w == 0 || src_h == 0 {
            return;
        }
        let dst_x0 = dx.max(0).min(self.width as i32);
        let dst_y0 = dy.max(0).min(self.height as i32);
        let dst_x1 = (dx + dw).max(0).min(self.width as i32);
        let dst_y1 = (dy + dh).max(0).min(self.height as i32);
        if dst_x0 >= dst_x1 || dst_y0 >= dst_y1 {
            return;
        }
        let fb_w = self.width as usize;
        let sw_u = src_w as usize;

        // Fast 1:1 blit path (Track-B 4x HD map chunks, occluder strips, 4x sprites & FBPs)
        if sw == dw && sh == dh && alpha_mul == 255 {
            for py in dst_y0..dst_y1 {
                let v = sy + (py - dy);
                if v < 0 || v >= src_h as i32 {
                    continue;
                }
                let u0 = sx + (dst_x0 - dx);
                let u1 = sx + (dst_x1 - dx);
                let clip_l = (-u0).max(0);
                let clip_r = (u1 - src_w as i32).max(0);
                let x_start = dst_x0 + clip_l;
                let x_end = dst_x1 - clip_r;
                if x_start >= x_end {
                    continue;
                }
                let src_row = (v as usize) * sw_u;
                let dst_row = (py as usize) * fb_w;
                let u = (u0 + clip_l) as usize;
                let s0 = (src_row + u) * 4;
                // Same bound as before: stop at the end of `src` (truncated textures).
                let n = ((x_end - x_start) as usize).min(src.len().saturating_sub(s0) / 4);
                if n == 0 {
                    continue;
                }
                let d0 = (dst_row + x_start as usize) * 4;
                blend_row_over(&mut self.pixels[d0..d0 + n * 4], &src[s0..s0 + n * 4]);
            }
            return;
        }

        // Fast 2x -> 4x integer bilinear upscale path (Track-B A5 2x HD map chunks & occluders).
        // Uses exact quarter-step weights (3/4, 1/4) with premultiplied-alpha edge interpolation
        // so transparent occluder edges stay halo-free with zero floating-point math.
        if dw == sw * 2 && dh == sh * 2 && alpha_mul == 255 {
            let max_u = (src_w as i32) - 1;
            let max_v = (src_h as i32) - 1;
            let sx_end = (sx + sw - 1).min(max_u);
            let sy_end = (sy + sh - 1).min(max_v);
            for py in dst_y0..dst_y1 {
                let oy = py - dy;
                let v4 = (oy * 2 - 1).max(0);
                let v0 = (sy + (v4 >> 2)).clamp(0, sy_end);
                let v1 = (v0 + 1).min(sy_end);
                let wy = (v4 & 3) as u32;
                let wy0 = 4 - wy;
                let row0 = (v0 as usize) * sw_u;
                let row1 = (v1 as usize) * sw_u;
                let dst_row = (py as usize) * fb_w;
                for px in dst_x0..dst_x1 {
                    let ox = px - dx;
                    let u4 = (ox * 2 - 1).max(0);
                    let u0 = (sx + (u4 >> 2)).clamp(0, sx_end);
                    let u1 = (u0 + 1).min(sx_end);
                    let wx = (u4 & 3) as u32;
                    let wx0 = 4 - wx;

                    let i00 = (row0 + u0 as usize) * 4;
                    let i10 = (row0 + u1 as usize) * 4;
                    let i01 = (row1 + u0 as usize) * 4;
                    let i11 = (row1 + u1 as usize) * 4;
                    if i11 + 3 >= src.len() {
                        continue;
                    }
                    let a00 = src[i00 + 3] as u32;
                    let a10 = src[i10 + 3] as u32;
                    let a01 = src[i01 + 3] as u32;
                    let a11 = src[i11 + 3] as u32;
                    if (a00 | a10 | a01 | a11) == 0 {
                        continue;
                    }
                    let w00 = wx0 * wy0;
                    let w10 = wx * wy0;
                    let w01 = wx0 * wy;
                    let w11 = wx * wy;
                    let di = (dst_row + px as usize) * 4;

                    if (a00 & a10 & a01 & a11) == 255 {
                        self.pixels[di] = ((src[i00] as u32 * w00
                            + src[i10] as u32 * w10
                            + src[i01] as u32 * w01
                            + src[i11] as u32 * w11
                            + 8)
                            >> 4) as u8;
                        self.pixels[di + 1] = ((src[i00 + 1] as u32 * w00
                            + src[i10 + 1] as u32 * w10
                            + src[i01 + 1] as u32 * w01
                            + src[i11 + 1] as u32 * w11
                            + 8)
                            >> 4) as u8;
                        self.pixels[di + 2] = ((src[i00 + 2] as u32 * w00
                            + src[i10 + 2] as u32 * w10
                            + src[i01 + 2] as u32 * w01
                            + src[i11 + 2] as u32 * w11
                            + 8)
                            >> 4) as u8;
                        self.pixels[di + 3] = 255;
                    } else {
                        let wa00 = w00 * a00;
                        let wa10 = w10 * a10;
                        let wa01 = w01 * a01;
                        let wa11 = w11 * a11;
                        let wa_sum = wa00 + wa10 + wa01 + wa11;
                        let sa = (wa_sum + 8) >> 4;
                        if sa == 0 {
                            continue;
                        }
                        let r = (src[i00] as u32 * wa00
                            + src[i10] as u32 * wa10
                            + src[i01] as u32 * wa01
                            + src[i11] as u32 * wa11
                            + wa_sum / 2)
                            / wa_sum;
                        let g = (src[i00 + 1] as u32 * wa00
                            + src[i10 + 1] as u32 * wa10
                            + src[i01 + 1] as u32 * wa01
                            + src[i11 + 1] as u32 * wa11
                            + wa_sum / 2)
                            / wa_sum;
                        let b = (src[i00 + 2] as u32 * wa00
                            + src[i10 + 2] as u32 * wa10
                            + src[i01 + 2] as u32 * wa01
                            + src[i11 + 2] as u32 * wa11
                            + wa_sum / 2)
                            / wa_sum;
                        if sa >= 255 {
                            self.pixels[di] = r as u8;
                            self.pixels[di + 1] = g as u8;
                            self.pixels[di + 2] = b as u8;
                            self.pixels[di + 3] = 255;
                        } else {
                            let inv = 255 - sa;
                            self.pixels[di] = ((r * sa + self.pixels[di] as u32 * inv) / 255) as u8;
                            self.pixels[di + 1] =
                                ((g * sa + self.pixels[di + 1] as u32 * inv) / 255) as u8;
                            self.pixels[di + 2] =
                                ((b * sa + self.pixels[di + 2] as u32 * inv) / 255) as u8;
                            self.pixels[di + 3] =
                                (sa + (self.pixels[di + 3] as u32 * inv) / 255).min(255) as u8;
                        }
                    }
                }
            }
            return;
        }

        let alpha_m = alpha_mul as u32;

        for py in dst_y0..dst_y1 {
            let v = sy + ((py - dy) as i64 * sh as i64 / dh as i64) as i32;
            if v < 0 || v >= src_h as i32 {
                continue;
            }
            let src_row = (v as usize) * sw_u;
            let dst_row = (py as usize) * fb_w;
            for px in dst_x0..dst_x1 {
                let u = sx + ((px - dx) as i64 * sw as i64 / dw as i64) as i32;
                if u < 0 || u >= src_w as i32 {
                    continue;
                }
                let si = (src_row + u as usize) * 4;
                if si + 3 >= src.len() {
                    continue;
                }
                let sa = (src[si + 3] as u32 * alpha_m) / 255;
                if sa == 0 {
                    continue;
                }
                let di = (dst_row + px as usize) * 4;
                if sa == 255 {
                    self.pixels[di] = src[si];
                    self.pixels[di + 1] = src[si + 1];
                    self.pixels[di + 2] = src[si + 2];
                    self.pixels[di + 3] = 255;
                } else {
                    let inv = 255 - sa;
                    self.pixels[di] = ((src[si] as u32 * sa + self.pixels[di] as u32 * inv) / 255) as u8;
                    self.pixels[di + 1] = ((src[si + 1] as u32 * sa + self.pixels[di + 1] as u32 * inv) / 255) as u8;
                    self.pixels[di + 2] = ((src[si + 2] as u32 * sa + self.pixels[di + 2] as u32 * inv) / 255) as u8;
                    self.pixels[di + 3] = (sa + (self.pixels[di + 3] as u32 * inv) / 255).min(255) as u8;
                }
            }
        }
    }

    /// Blit a region with bright white hit-flash boost (for battle damage impact frames).
    #[allow(clippy::too_many_arguments)]
    pub fn blit_region_flash(
        &mut self,
        src: &[u8],
        src_w: u32,
        src_h: u32,
        sx: i32,
        sy: i32,
        sw: i32,
        sh: i32,
        dx: i32,
        dy: i32,
        dw: i32,
        dh: i32,
    ) {
        if sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0 || src_w == 0 || src_h == 0 {
            return;
        }
        let dst_x0 = dx.max(0).min(self.width as i32);
        let dst_y0 = dy.max(0).min(self.height as i32);
        let dst_x1 = (dx + dw).max(0).min(self.width as i32);
        let dst_y1 = (dy + dh).max(0).min(self.height as i32);
        if dst_x0 >= dst_x1 || dst_y0 >= dst_y1 {
            return;
        }
        let fb_w = self.width as usize;
        let sw_u = src_w as usize;

        for py in dst_y0..dst_y1 {
            let v = sy + ((py - dy) as i64 * sh as i64 / dh as i64) as i32;
            if v < 0 || v >= src_h as i32 {
                continue;
            }
            let src_row = (v as usize) * sw_u;
            let dst_row = (py as usize) * fb_w;
            for px in dst_x0..dst_x1 {
                let u = sx + ((px - dx) as i64 * sw as i64 / dw as i64) as i32;
                if u < 0 || u >= src_w as i32 {
                    continue;
                }
                let si = (src_row + u as usize) * 4;
                if si + 3 >= src.len() {
                    continue;
                }
                let sa = src[si + 3] as u32;
                if sa == 0 {
                    continue;
                }
                let sr = ((src[si] as u32) * 140 / 100 + 115).min(255);
                let sg = ((src[si + 1] as u32) * 140 / 100 + 115).min(255);
                let sb = ((src[si + 2] as u32) * 140 / 100 + 115).min(255);
                let di = (dst_row + px as usize) * 4;
                if sa == 255 {
                    self.pixels[di] = sr as u8;
                    self.pixels[di + 1] = sg as u8;
                    self.pixels[di + 2] = sb as u8;
                    self.pixels[di + 3] = 255;
                } else {
                    let inv = 255 - sa;
                    self.pixels[di] = ((sr * sa + self.pixels[di] as u32 * inv) / 255) as u8;
                    self.pixels[di + 1] = ((sg * sa + self.pixels[di + 1] as u32 * inv) / 255) as u8;
                    self.pixels[di + 2] = ((sb * sa + self.pixels[di + 2] as u32 * inv) / 255) as u8;
                    self.pixels[di + 3] = (sa + (self.pixels[di + 3] as u32 * inv) / 255).min(255) as u8;
                }
            }
        }
    }

    /// Blit opaque source pixels as a translucent black drop shadow (authentic SDLPAL 6px UI shadow).
    #[allow(clippy::too_many_arguments)]
    pub fn blit_region_shadow(
        &mut self,
        src: &[u8],
        src_w: u32,
        src_h: u32,
        sx: i32,
        sy: i32,
        sw: i32,
        sh: i32,
        dx: i32,
        dy: i32,
        dw: i32,
        dh: i32,
        shadow_alpha: u8,
    ) {
        if sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0 || shadow_alpha == 0 || src_w == 0 || src_h == 0 {
            return;
        }
        let dst_x0 = dx.max(0).min(self.width as i32);
        let dst_y0 = dy.max(0).min(self.height as i32);
        let dst_x1 = (dx + dw).max(0).min(self.width as i32);
        let dst_y1 = (dy + dh).max(0).min(self.height as i32);
        if dst_x0 >= dst_x1 || dst_y0 >= dst_y1 {
            return;
        }
        let fb_w = self.width as usize;
        let sw_u = src_w as usize;
        let sa_mul = shadow_alpha as u32;

        for py in dst_y0..dst_y1 {
            let v = sy + ((py - dy) as i64 * sh as i64 / dh as i64) as i32;
            if v < 0 || v >= src_h as i32 {
                continue;
            }
            let src_row = (v as usize) * sw_u;
            let dst_row = (py as usize) * fb_w;
            for px in dst_x0..dst_x1 {
                let u = sx + ((px - dx) as i64 * sw as i64 / dw as i64) as i32;
                if u < 0 || u >= src_w as i32 {
                    continue;
                }
                let si = (src_row + u as usize) * 4;
                if si + 3 >= src.len() || src[si + 3] < 64 {
                    continue;
                }
                let sa = (src[si + 3] as u32 * sa_mul) / 255;
                let inv = 255 - sa;
                let di = (dst_row + px as usize) * 4;
                self.pixels[di] = ((self.pixels[di] as u32 * inv) / 255) as u8;
                self.pixels[di + 1] = ((self.pixels[di + 1] as u32 * inv) / 255) as u8;
                self.pixels[di + 2] = ((self.pixels[di + 2] as u32 * inv) / 255) as u8;
                self.pixels[di + 3] = (sa + (self.pixels[di + 3] as u32 * inv) / 255).min(255) as u8;
            }
        }
    }

    /// Blit a region converted to luminance-tinted monochrome (for dimmed/disabled SDLPAL battle icons & dead faces).
    #[allow(clippy::too_many_arguments)]
    pub fn blit_region_mono(
        &mut self,
        src: &[u8],
        src_w: u32,
        src_h: u32,
        sx: i32,
        sy: i32,
        sw: i32,
        sh: i32,
        dx: i32,
        dy: i32,
        dw: i32,
        dh: i32,
        tint_rgb: [u8; 3],
    ) {
        if sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0 || src_w == 0 || src_h == 0 {
            return;
        }
        let dst_x0 = dx.max(0).min(self.width as i32);
        let dst_y0 = dy.max(0).min(self.height as i32);
        let dst_x1 = (dx + dw).max(0).min(self.width as i32);
        let dst_y1 = (dy + dh).max(0).min(self.height as i32);
        if dst_x0 >= dst_x1 || dst_y0 >= dst_y1 {
            return;
        }
        let fb_w = self.width as usize;
        let sw_u = src_w as usize;

        for py in dst_y0..dst_y1 {
            let v = sy + ((py - dy) as i64 * sh as i64 / dh as i64) as i32;
            if v < 0 || v >= src_h as i32 {
                continue;
            }
            let src_row = (v as usize) * sw_u;
            let dst_row = (py as usize) * fb_w;
            for px in dst_x0..dst_x1 {
                let u = sx + ((px - dx) as i64 * sw as i64 / dw as i64) as i32;
                if u < 0 || u >= src_w as i32 {
                    continue;
                }
                let si = (src_row + u as usize) * 4;
                if si + 3 >= src.len() {
                    continue;
                }
                let sa = src[si + 3] as u32;
                if sa == 0 {
                    continue;
                }
                let luma = (src[si] as u32 * 77 + src[si + 1] as u32 * 150 + src[si + 2] as u32 * 29) >> 8;
                let sr = (luma * tint_rgb[0] as u32) / 255;
                let sg = (luma * tint_rgb[1] as u32) / 255;
                let sb = (luma * tint_rgb[2] as u32) / 255;
                let di = (dst_row + px as usize) * 4;
                if sa == 255 {
                    self.pixels[di] = sr as u8;
                    self.pixels[di + 1] = sg as u8;
                    self.pixels[di + 2] = sb as u8;
                    self.pixels[di + 3] = 255;
                } else {
                    let inv = 255 - sa;
                    self.pixels[di] = ((sr * sa + self.pixels[di] as u32 * inv) / 255) as u8;
                    self.pixels[di + 1] = ((sg * sa + self.pixels[di + 1] as u32 * inv) / 255) as u8;
                    self.pixels[di + 2] = ((sb * sa + self.pixels[di + 2] as u32 * inv) / 255) as u8;
                    self.pixels[di + 3] = (sa + (self.pixels[di + 3] as u32 * inv) / 255).min(255) as u8;
                }
            }
        }
    }

    /// Soft radial elliptical ground shadow for `shadowStripped` Track-B HD characters.
    pub fn draw_soft_ellipse_shadow(&mut self, cx: i32, cy: i32, rx: i32, ry: i32, max_alpha: u8) {
        if rx <= 0 || ry <= 0 || max_alpha == 0 {
            return;
        }
        let x0 = (cx - rx).max(0).min(self.width as i32 - 1);
        let x1 = (cx + rx).max(0).min(self.width as i32 - 1);
        let y0 = (cy - ry).max(0).min(self.height as i32 - 1);
        let y1 = (cy + ry).max(0).min(self.height as i32 - 1);
        if x0 > x1 || y0 > y1 {
            return;
        }
        let inv_rx = 1.0 / (rx as f32);
        let inv_ry = 1.0 / (ry as f32);
        let max_a = max_alpha as f32;
        let fb_w = self.width as usize;

        for py in y0..=y1 {
            let ny = (py - cy) as f32 * inv_ry;
            let ny2 = ny * ny;
            let row = (py as usize) * fb_w;
            for px in x0..=x1 {
                let nx = (px - cx) as f32 * inv_rx;
                let d2 = nx * nx + ny2;
                if d2 >= 1.0 {
                    continue;
                }
                let falloff = 1.0 - d2;
                let sa = (max_a * falloff * falloff).round() as u32;
                if sa == 0 {
                    continue;
                }
                let inv = 255 - sa;
                let di = (row + px as usize) * 4;
                self.pixels[di] = ((8 * sa + self.pixels[di] as u32 * inv) / 255) as u8;
                self.pixels[di + 1] = ((12 * sa + self.pixels[di + 1] as u32 * inv) / 255) as u8;
                self.pixels[di + 2] = ((16 * sa + self.pixels[di + 2] as u32 * inv) / 255) as u8;
                self.pixels[di + 3] = (sa + (self.pixels[di + 3] as u32 * inv) / 255).min(255) as u8;
            }
        }
    }

    /// Elliptical ring stroke (used for modern HD click-to-walk ground ripples).
    pub fn stroke_ellipse(&mut self, cx: i32, cy: i32, rx: i32, ry: i32, thickness: i32, color: Color) {
        if rx <= 1 || ry <= 1 || color.0[3] == 0 {
            return;
        }
        let thick = thickness.max(1);
        let x0 = (cx - rx - thick).max(0).min(self.width as i32 - 1);
        let x1 = (cx + rx + thick).max(0).min(self.width as i32 - 1);
        let y0 = (cy - ry - thick).max(0).min(self.height as i32 - 1);
        let y1 = (cy + ry + thick).max(0).min(self.height as i32 - 1);
        let inv_rx = 1.0 / (rx as f32);
        let inv_ry = 1.0 / (ry as f32);
        let band = (thick as f32) / (rx.min(ry).max(2) as f32);
        for py in y0..=y1 {
            let ny = (py - cy) as f32 * inv_ry;
            let ny2 = ny * ny;
            for px in x0..=x1 {
                let nx = (px - cx) as f32 * inv_rx;
                let r = (nx * nx + ny2).sqrt();
                if (r - 1.0).abs() <= band {
                    self.blend_pixel(px, py, color);
                }
            }
        }
    }

    /// Composite another full-size framebuffer (such as the cached UI layer) over this framebuffer.
    pub fn composite_over(&mut self, overlay: &Framebuffer) {
        if self.width != overlay.width || self.height != overlay.height {
            return;
        }
        for (dst, src) in self.pixels.chunks_exact_mut(4).zip(overlay.pixels.chunks_exact(4)) {
            let sa = src[3] as u32;
            if sa == 0 {
                continue;
            }
            if sa == 255 {
                dst.copy_from_slice(src);
            } else {
                let inv = 255 - sa;
                dst[0] = ((src[0] as u32 * sa + dst[0] as u32 * inv) / 255) as u8;
                dst[1] = ((src[1] as u32 * sa + dst[1] as u32 * inv) / 255) as u8;
                dst[2] = ((src[2] as u32 * sa + dst[2] as u32 * inv) / 255) as u8;
                dst[3] = (sa + (dst[3] as u32 * inv) / 255).min(255) as u8;
            }
        }
    }

    /// Apply full-screen fade tint (`alpha` in `0.0..=1.0`).
    pub fn apply_fade(&mut self, color: Color, alpha: f32) {
        let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        if a == 0 {
            return;
        }
        self.fill_rect(0, 0, self.width as i32, self.height as i32, Color([color.0[0], color.0[1], color.0[2], a]));
    }

    /// Apply horizontal scanline sine-wave distortion (used in dream/illusion scenes).
    pub fn apply_horizontal_wave(&mut self, amplitude_px: f32, phase: f32, freq: f32) {
        if amplitude_px <= 0.05 || self.width == 0 || self.height == 0 {
            return;
        }
        let w = self.width as usize;
        let h = self.height as usize;
        let mut row_tmp = vec![0u8; w * 4];
        for y in 0..h {
            let shift = ((y as f32 * freq + phase).sin() * amplitude_px).round() as i32;
            if shift == 0 {
                continue;
            }
            let row_off = y * w * 4;
            row_tmp.copy_from_slice(&self.pixels[row_off..row_off + w * 4]);
            for x in 0..w {
                let sx = (x as i32 - shift).clamp(0, w as i32 - 1) as usize;
                let di = row_off + x * 4;
                let si = sx * 4;
                self.pixels[di..di + 4].copy_from_slice(&row_tmp[si..si + 4]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_framebuffer_blend_and_bevel() {
        let mut fb = Framebuffer::new(64, 40);
        fb.clear(Color::BLACK);
        fb.draw_bevel_box(8, 8, 48, 24, 1);
        // Center pixel should be non-black panel color
        let idx = (20 * 64 + 32) * 4;
        assert!(fb.pixels[idx] > 20);
        assert_eq!(fb.pixels[idx + 3], 255);
    }

    #[test]
    fn test_blit_region_flash_brightens_opaque_pixels_only() {
        let mut fb = Framebuffer::new(8, 8);
        fb.clear(Color::rgba(10, 20, 30, 255));
        // 2x1 source: opaque dark pixel + transparent pixel
        let src = [40u8, 60, 80, 255, 255, 0, 0, 0];
        fb.blit_region_flash(&src, 2, 1, 0, 0, 2, 1, 2, 2, 2, 1);
        let p0 = (2 * 8 + 2) * 4;
        let p1 = (2 * 8 + 3) * 4;
        assert!(fb.pixels[p0] >= 170 && fb.pixels[p0 + 1] >= 195 && fb.pixels[p0 + 2] >= 220);
        // Transparent pixel leaves background untouched
        assert_eq!(&fb.pixels[p1..p1 + 4], &[10, 20, 30, 255]);
    }

    #[test]
    fn test_blit_region_scaled_2x_bilinear_smooth_and_halo_free() {
        let mut fb = Framebuffer::new(8, 8);
        fb.clear(Color::rgba(10, 20, 30, 255));
        // 2x2 source: left col opaque (200,100,40,255), right col transparent (0,0,0,0)
        let src = [
            200, 100, 40, 255, 0, 0, 0, 0,
            200, 100, 40, 255, 0, 0, 0, 0,
        ];
        fb.blit_region_scaled(&src, 2, 2, 0, 0, 2, 2, 0, 0, 4, 4, 255);
        // Far-left column (ox=0) should be exact opaque color
        assert_eq!(&fb.pixels[0..4], &[200, 100, 40, 255]);
        // Transition column (ox=1) should blend cleanly toward background without black halo
        let p1 = 4;
        assert!(fb.pixels[p1] > 100 && fb.pixels[p1] < 200);
        // Far-right column (ox=3) is fully transparent -> leaves background untouched
        let p3 = 12;
        assert_eq!(&fb.pixels[p3..p3 + 4], &[10, 20, 30, 255]);
    }
}

/// Source-over one row of RGBA8 pixels (`dst.len() == src.len()`, multiple of 4). Runs of fully
/// opaque source pixels are copied with one `copy_from_slice`, fully transparent runs are
/// skipped, and only partial alpha is blended (same integer math as the per-pixel path it
/// replaced, so output is byte-identical).
#[inline]
pub(crate) fn blend_row_over(dst: &mut [u8], src: &[u8]) {
    let n = src.len() / 4;
    let mut i = 0;
    while i < n {
        let sa = src[i * 4 + 3];
        if sa == 255 {
            let mut j = i + 1;
            while j < n && src[j * 4 + 3] == 255 {
                j += 1;
            }
            dst[i * 4..j * 4].copy_from_slice(&src[i * 4..j * 4]);
            i = j;
        } else if sa == 0 {
            i += 1;
            while i < n && src[i * 4 + 3] == 0 {
                i += 1;
            }
        } else {
            let sa = sa as u32;
            let inv = 255 - sa;
            let (s, d) = (&src[i * 4..i * 4 + 4], &mut dst[i * 4..i * 4 + 4]);
            d[0] = ((s[0] as u32 * sa + d[0] as u32 * inv) / 255) as u8;
            d[1] = ((s[1] as u32 * sa + d[1] as u32 * inv) / 255) as u8;
            d[2] = ((s[2] as u32 * sa + d[2] as u32 * inv) / 255) as u8;
            d[3] = (sa + (d[3] as u32 * inv) / 255).min(255) as u8;
            i += 1;
        }
    }
}

/// Pre-upscale a whole RGBA8 texture 2x with exactly the integer bilinear math of the
/// `blit_region_scaled` 2x->4x path (straight colour + coverage alpha per output pixel). Blitting
/// the result 1:1 with `blend_row_over` is byte-identical to the per-frame 2x path for a full-texture
/// blit (`sx = sy = 0`, `sw/sh` = texture size), because each output pixel depends only on its
/// offset inside the texture. Lets static Track-B 2x map chunks be upscaled once instead of every
/// frame.
pub fn upscale_2x_bilinear(src: &[u8], src_w: u32, src_h: u32) -> Vec<u8> {
    let (w, h) = (src_w as usize, src_h as usize);
    let (ow, oh) = (w * 2, h * 2);
    let mut out = vec![0u8; ow * oh * 4];
    if w == 0 || h == 0 || src.len() < w * h * 4 {
        return out;
    }
    let (max_u, max_v) = (src_w as i32 - 1, src_h as i32 - 1);
    for oy in 0..oh as i32 {
        let v4 = (oy * 2 - 1).max(0);
        let v0 = (v4 >> 2).clamp(0, max_v);
        let v1 = (v0 + 1).min(max_v);
        let wy = (v4 & 3) as u32;
        let wy0 = 4 - wy;
        let (row0, row1) = (v0 as usize * w, v1 as usize * w);
        for ox in 0..ow as i32 {
            let u4 = (ox * 2 - 1).max(0);
            let u0 = (u4 >> 2).clamp(0, max_u);
            let u1 = (u0 + 1).min(max_u);
            let wx = (u4 & 3) as u32;
            let wx0 = 4 - wx;
            let i00 = (row0 + u0 as usize) * 4;
            let i10 = (row0 + u1 as usize) * 4;
            let i01 = (row1 + u0 as usize) * 4;
            let i11 = (row1 + u1 as usize) * 4;
            let (a00, a10, a01, a11) = (src[i00 + 3] as u32, src[i10 + 3] as u32, src[i01 + 3] as u32, src[i11 + 3] as u32);
            if (a00 | a10 | a01 | a11) == 0 {
                continue;
            }
            let (w00, w10, w01, w11) = (wx0 * wy0, wx * wy0, wx0 * wy, wx * wy);
            let di = (oy as usize * ow + ox as usize) * 4;
            if (a00 & a10 & a01 & a11) == 255 {
                for c in 0..3 {
                    out[di + c] = ((src[i00 + c] as u32 * w00 + src[i10 + c] as u32 * w10 + src[i01 + c] as u32 * w01 + src[i11 + c] as u32 * w11 + 8) >> 4) as u8;
                }
                out[di + 3] = 255;
            } else {
                let (wa00, wa10, wa01, wa11) = (w00 * a00, w10 * a10, w01 * a01, w11 * a11);
                let wa_sum = wa00 + wa10 + wa01 + wa11;
                let sa = (wa_sum + 8) >> 4;
                if sa == 0 {
                    continue;
                }
                for c in 0..3 {
                    out[di + c] = ((src[i00 + c] as u32 * wa00 + src[i10 + c] as u32 * wa10 + src[i01 + c] as u32 * wa01 + src[i11 + c] as u32 * wa11 + wa_sum / 2) / wa_sum) as u8;
                }
                out[di + 3] = sa.min(255) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod upscale_cache_tests {
    use super::*;

    #[test]
    fn pre_upscaled_1to1_blit_matches_per_frame_2x_path_byte_for_byte() {
        let (w, h) = (37u32, 23u32);
        let mut seed = 0x1234_5678u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut tex = vec![0u8; (w * h * 4) as usize];
        for px in tex.chunks_exact_mut(4) {
            let r = rnd();
            px[0] = r as u8;
            px[1] = (r >> 8) as u8;
            px[2] = (r >> 16) as u8;
            // Mostly opaque with transparent holes and soft edges, like map chunks.
            px[3] = match r >> 28 { 0 => 0, 1 => (r >> 20) as u8, _ => 255 };
        }
        let up = upscale_2x_bilinear(&tex, w, h);
        for (dx, dy) in [(0, 0), (-9, 5), (13, -7), (40, 30)] {
            let mut a = Framebuffer::new(96, 64);
            for (i, b) in a.pixels.iter_mut().enumerate() {
                *b = (i * 7 % 251) as u8;
            }
            let mut b = Framebuffer { width: a.width, height: a.height, pixels: a.pixels.clone() };
            a.blit_region_scaled(&tex, w, h, 0, 0, w as i32, h as i32, dx, dy, (w * 2) as i32, (h * 2) as i32, 255);
            b.blit_region_scaled(&up, w * 2, h * 2, 0, 0, (w * 2) as i32, (h * 2) as i32, dx, dy, (w * 2) as i32, (h * 2) as i32, 255);
            assert!(a.pixels == b.pixels, "mismatch at offset ({dx},{dy})");
        }
    }
}

