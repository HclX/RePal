//! Stage-8 Hybrid Watercolor & Seamless Isometric Tile 4x HD Upscaling Engine:
//! 1. `convert_dos_checker_shadows`: Restores DOS PAL's 1-bit green/brown checkerboard foot shadows
//!    (`(0,36,0)` + `(48,28,16)` / `(28,20,12)`) into smooth translucent shadows (`rgba(8, 8, 12, 110)`).
//! 2. `extrude_rgb_into_transparent`: Voronoi-style nearest-color dilation into `alpha == 0` exterior
//!    so upscalers, WebP YUV420 blocks, and canvas bilinear sampling never bleed black halos.
//! 3. `upscale_rgb_watercolor_4x`: Gentle 256-color stipple de-dithering -> Conservative EPX/Scale2x (2x)
//!    -> Exact half-pel Catmull-Rom bicubic (4x) -> 5x5 Edge-Preserving Bilateral -> 2-pass Clamped
//!    Anime4K Luminance-Gradient Push (zero mosaic worms, zero overshoot/ringing, crisp ink linework).
//! 4. `upscale_gop_tile_4x`: Map-context-aware `32x16 -> 128x64` isometric tile upscaler using the
//!    exact algebraic fundamental-domain diamond partition `(-64 <= u < 64) & (-64 <= v < 64)` (`min_cov=1, max_cov=1`).

use crate::graphics::RgbaImage;

#[inline]
fn get_px(img: &RgbaImage, x: i32, y: i32) -> [u8; 4] {
    let cx = x.clamp(0, img.width as i32 - 1) as usize;
    let cy = y.clamp(0, img.height as i32 - 1) as usize;
    let i = (cy * img.width as usize + cx) * 4;
    [
        img.pixels[i],
        img.pixels[i + 1],
        img.pixels[i + 2],
        img.pixels[i + 3],
    ]
}

#[inline]
pub fn yuv_dist_rgb(a: [u8; 4], b: [u8; 4]) -> i32 {
    let dr = a[0] as i32 - b[0] as i32;
    let dg = a[1] as i32 - b[1] as i32;
    let db = a[2] as i32 - b[2] as i32;
    let y = (dr * 299 + dg * 587 + db * 114) / 1000;
    let u = (-dr * 169 - dg * 331 + db * 500) / 1000;
    let v = (dr * 500 - dg * 419 - db * 81) / 1000;
    y * y * 4 + u * u * 2 + v * v * 3
}

/// Standard 1x DOS PAL full diamond mask (`32x15`, 256 pixels).
#[inline]
pub fn is_diamond_1x(x: usize, y: usize) -> bool {
    if x >= 32 || y >= 15 {
        return false;
    }
    let pad = (y as i32 - 7).unsigned_abs() as usize * 2;
    x >= pad && x < 32 - pad
}

/// True if `(x, y)` is on the 2-pixel diagonal seam rim of `DIAMOND_1X`.
#[inline]
pub fn is_diamond_1x_border(x: usize, y: usize) -> bool {
    if x >= 32 || y >= 15 {
        return false;
    }
    let pad = (y as i32 - 7).unsigned_abs() as usize * 2;
    (x >= pad && x < pad + 2) || (x >= 30 - pad && x < 32 - pad)
}

/// Exact 4x algebraic fundamental-domain diamond (`128x64`, 4,096 pixels, `min_cov = 1, max_cov = 1`
/// on the `(64, 32)` / `(64, -32)` isometric lattice!).
#[inline]
pub fn is_diamond_4x(x: i32, y: i32) -> bool {
    if !(0..128).contains(&x) || !(0..64).contains(&y) {
        return false;
    }
    let u = (x - 64) + 2 * (y - 31);
    let v = (x - 64) - 2 * (y - 31);
    (-64..64).contains(&u) && (-64..64).contains(&v)
}

/// Convert baked DOS PAL 1-bit dark green/brown checkerboard foot shadows into smooth translucent shadow.
pub fn convert_dos_checker_shadows(src: &RgbaImage) -> RgbaImage {
    let w = src.width as i32;
    let h = src.height as i32;
    if w == 0 || h == 0 {
        return src.clone();
    }
    let len = (w * h) as usize;
    let mut is_green = vec![false; len];
    let mut any_green = false;
    for i in 0..len {
        let p = i * 4;
        if src.pixels[p + 3] > 0
            && src.pixels[p] == 0
            && src.pixels[p + 1] == 36
            && src.pixels[p + 2] == 0
        {
            is_green[i] = true;
            any_green = true;
        }
    }
    if !any_green {
        return src.clone();
    }

    let mut out = src.clone();
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) as usize;
            let p = idx * 4;
            if src.pixels[p + 3] == 0 {
                continue;
            }
            let r = src.pixels[p];
            let g = src.pixels[p + 1];
            let b = src.pixels[p + 2];
            let is_brown = (r == 48 && g == 28 && b == 16) || (r == 28 && g == 20 && b == 12);
            let mut mark = is_green[idx];
            if !mark && is_brown {
                'nb: for dy in -1..=1 {
                    for dx in -1..=1 {
                        let nx = x + dx;
                        let ny = y + dy;
                        if nx >= 0 && nx < w && ny >= 0 && ny < h && is_green[(ny * w + nx) as usize]
                        {
                            mark = true;
                            break 'nb;
                        }
                    }
                }
            }
            if mark {
                out.pixels[p] = 8;
                out.pixels[p + 1] = 8;
                out.pixels[p + 2] = 12;
                out.pixels[p + 3] = 110;
            }
        }
    }
    out
}

/// Fill transparent (`alpha == 0`) pixels' RGB with nearest opaque neighbor RGB (retaining `alpha == 0`).
pub fn extrude_rgb_into_transparent(src: &RgbaImage, iters: usize) -> RgbaImage {
    let w = src.width as i32;
    let h = src.height as i32;
    if w == 0 || h == 0 {
        return src.clone();
    }
    let len = (w * h) as usize;
    let mut out = src.clone();
    let mut filled = vec![false; len];
    let mut count_filled = 0usize;
    for i in 0..len {
        if out.pixels[i * 4 + 3] > 0 {
            filled[i] = true;
            count_filled += 1;
        }
    }
    if count_filled == 0 || count_filled == len {
        return out;
    }

    let dirs: [(i32, i32, u32); 8] = [
        (-1, 0, 2),
        (1, 0, 2),
        (0, -1, 2),
        (0, 1, 2),
        (-1, -1, 1),
        (1, -1, 1),
        (-1, 1, 1),
        (1, 1, 1),
    ];

    for _ in 0..iters {
        if count_filled == len {
            break;
        }
        let prev = out.pixels.clone();
        let prev_filled = filled.clone();
        let mut changed = false;

        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) as usize;
                if prev_filled[idx] {
                    continue;
                }
                let mut sr = 0u32;
                let mut sg = 0u32;
                let mut sb = 0u32;
                let mut wt = 0u32;
                for &(dx, dy, wgt) in &dirs {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx >= 0 && nx < w && ny >= 0 && ny < h {
                        let nidx = (ny * w + nx) as usize;
                        if prev_filled[nidx] {
                            let np = nidx * 4;
                            sr += prev[np] as u32 * wgt;
                            sg += prev[np + 1] as u32 * wgt;
                            sb += prev[np + 2] as u32 * wgt;
                            wt += wgt;
                        }
                    }
                }
                if wt > 0 {
                    let p = idx * 4;
                    out.pixels[p] = ((sr + wt / 2) / wt) as u8;
                    out.pixels[p + 1] = ((sg + wt / 2) / wt) as u8;
                    out.pixels[p + 2] = ((sb + wt / 2) / wt) as u8;
                    filled[idx] = true;
                    count_filled += 1;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    out
}

/// Soften harsh VGA 256-color 1px stipple/checkerboard dithers while keeping dark ink lines & eyes intact.
pub fn gentle_vga_dedither_rgb(src: &RgbaImage) -> RgbaImage {
    let w = src.width as i32;
    let h = src.height as i32;
    let mut out = src.clone();
    let stride = src.width as usize;

    for y in 0..h {
        for x in 0..w {
            let c = get_px(src, x, y);
            let l = get_px(src, x - 1, y);
            let r = get_px(src, x + 1, y);
            let u = get_px(src, x, y - 1);
            let d = get_px(src, x, y + 1);

            let dlr = yuv_dist_rgb(l, r);
            let dud = yuv_dist_rgb(u, d);
            let dcl = yuv_dist_rgb(c, l);
            let dcu = yuv_dist_rgb(c, u);

            let h_dith = dlr <= 350 && (150..=8500).contains(&dcl);
            let v_dith = dud <= 350 && (150..=8500).contains(&dcu);
            if !h_dith && !v_dith {
                continue;
            }

            let p = (y as usize * stride + x as usize) * 4;
            for ch in 0..3 {
                let cv = c[ch] as f32;
                let nh = 0.5 * (l[ch] as f32 + r[ch] as f32);
                let nv = 0.5 * (u[ch] as f32 + d[ch] as f32);
                let val = if h_dith && v_dith {
                    cv * 0.55 + 0.225 * (nh + nv)
                } else if h_dith {
                    cv * 0.68 + 0.32 * nh
                } else {
                    cv * 0.68 + 0.32 * nv
                };
                out.pixels[p + ch] = val.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// Conservative perceptual EPX / Scale2x (`1x -> 2x`): rounds true L-corners with zero worms or interior blur.
pub fn ep_scale2x_rgb(src: &RgbaImage, tol: i32) -> RgbaImage {
    let w = src.width as i32;
    let h = src.height as i32;
    let mut dst = RgbaImage::new((w * 2) as u32, (h * 2) as u32);
    let dst_w = (w * 2) as usize;
    let half_tol = tol / 2;

    #[inline]
    fn mix_corner(p: [u8; 4], a: [u8; 4], b: [u8; 4]) -> [u8; 4] {
        let mut res = [0u8; 4];
        for ch in 0..3 {
            let avg = a[ch] as u32 + b[ch] as u32; // 2 * avg
            // 0.75 * avg(a,b) + 0.25 * p = (3 * (a + b) + 2 * p) / 8
            res[ch] = ((3 * avg + 2 * p[ch] as u32 + 4) / 8) as u8;
        }
        res[3] = p[3];
        res
    }

    for y in 0..h {
        for x in 0..w {
            let p = get_px(src, x, y);
            let u = get_px(src, x, y - 1);
            let d = get_px(src, x, y + 1);
            let l = get_px(src, x - 1, y);
            let r = get_px(src, x + 1, y);

            let ud_diff = yuv_dist_rgb(u, d) > tol;
            let lr_diff = yuv_dist_rgb(l, r) > tol;

            let mut e00 = p;
            let mut e10 = p;
            let mut e01 = p;
            let mut e11 = p;

            if ud_diff && lr_diff {
                let d_pu = yuv_dist_rgb(p, u) > half_tol;
                let d_pd = yuv_dist_rgb(p, d) > half_tol;
                if d_pu && yuv_dist_rgb(u, l) <= tol {
                    e00 = mix_corner(p, u, l);
                }
                if d_pu && yuv_dist_rgb(u, r) <= tol {
                    e10 = mix_corner(p, u, r);
                }
                if d_pd && yuv_dist_rgb(d, l) <= tol {
                    e01 = mix_corner(p, d, l);
                }
                if d_pd && yuv_dist_rgb(d, r) <= tol {
                    e11 = mix_corner(p, d, r);
                }
            }

            let dx = (x * 2) as usize;
            let dy = (y * 2) as usize;
            let p00 = (dy * dst_w + dx) * 4;
            let p10 = (dy * dst_w + dx + 1) * 4;
            let p01 = ((dy + 1) * dst_w + dx) * 4;
            let p11 = ((dy + 1) * dst_w + dx + 1) * 4;
            dst.pixels[p00..p00 + 4].copy_from_slice(&e00);
            dst.pixels[p10..p10 + 4].copy_from_slice(&e10);
            dst.pixels[p01..p01 + 4].copy_from_slice(&e01);
            dst.pixels[p11..p11 + 4].copy_from_slice(&e11);
        }
    }
    dst
}

/// Exact separable half-pel Catmull-Rom bicubic `2x` upscaler (`weights = [-9, 111, 29, -3] / 128`).
pub fn catmull_rom_2x_rgb(src: &RgbaImage) -> RgbaImage {
    let w = src.width as i32;
    let h = src.height as i32;
    let w2 = (w * 2) as usize;
    let h2 = (h * 2) as usize;

    // Pass 1: Horizontal `w -> 2w`
    let mut tmp = RgbaImage::new(w2 as u32, h as u32);
    for y in 0..h {
        for x in 0..w {
            let p_m1 = get_px(src, x - 1, y);
            let p_0 = get_px(src, x, y);
            let p_1 = get_px(src, x + 1, y);
            let p_2 = get_px(src, x + 2, y);

            let o0 = (y as usize * w2 + (x * 2) as usize) * 4;
            let o1 = o0 + 4;
            for ch in 0..3 {
                // Left subpixel (-0.25): [-9*p_1 + 111*p_0 + 29*p_m1 - 3*p_m1?]
                // Wait: for left subpixel at x - 0.25 (between x-1 and x, at t=0.75 from x-1, i.e. mirror of t=0.25!):
                // Mirror around p_0: [-3*p_1 + 29*p_m1 + 111*p_0 - 9*p_1]? Check!
                // At pos = x - 0.25: neighbors are (x-2, x-1, x, x+1) with weights (-3, 29, 111, -9)/128!
                let p_m2 = get_px(src, x - 2, y);
                let v_left = (-3 * p_m2[ch] as i32
                    + 29 * p_m1[ch] as i32
                    + 111 * p_0[ch] as i32
                    - 9 * p_1[ch] as i32
                    + 64)
                    >> 7;
                // At pos = x + 0.25: neighbors are (x-1, x, x+1, x+2) with weights (-9, 111, 29, -3)/128!
                let v_right = (-9 * p_m1[ch] as i32
                    + 111 * p_0[ch] as i32
                    + 29 * p_1[ch] as i32
                    - 3 * p_2[ch] as i32
                    + 64)
                    >> 7;
                tmp.pixels[o0 + ch] = v_left.clamp(0, 255) as u8;
                tmp.pixels[o1 + ch] = v_right.clamp(0, 255) as u8;
            }
            tmp.pixels[o0 + 3] = 255;
            tmp.pixels[o1 + 3] = 255;
        }
    }

    // Pass 2: Vertical `h -> 2h`
    let mut dst = RgbaImage::new(w2 as u32, h2 as u32);
    for y in 0..h {
        for x in 0..(w2 as i32) {
            let p_m2 = get_px(&tmp, x, y - 2);
            let p_m1 = get_px(&tmp, x, y - 1);
            let p_0 = get_px(&tmp, x, y);
            let p_1 = get_px(&tmp, x, y + 1);
            let p_2 = get_px(&tmp, x, y + 2);

            let o_top = ((y * 2) as usize * w2 + x as usize) * 4;
            let o_bot = ((y * 2 + 1) as usize * w2 + x as usize) * 4;
            for ch in 0..3 {
                let v_top = (-3 * p_m2[ch] as i32
                    + 29 * p_m1[ch] as i32
                    + 111 * p_0[ch] as i32
                    - 9 * p_1[ch] as i32
                    + 64)
                    >> 7;
                let v_bot = (-9 * p_m1[ch] as i32
                    + 111 * p_0[ch] as i32
                    + 29 * p_1[ch] as i32
                    - 3 * p_2[ch] as i32
                    + 64)
                    >> 7;
                dst.pixels[o_top + ch] = v_top.clamp(0, 255) as u8;
                dst.pixels[o_bot + ch] = v_bot.clamp(0, 255) as u8;
            }
            dst.pixels[o_top + 3] = 255;
            dst.pixels[o_bot + 3] = 255;
        }
    }
    dst
}

/// Edge-preserving 5x5 bilateral filter (`sigmaColor ≈ 18`, `sigmaSpace ≈ 2.2`) for watercolor smoothness.
pub fn bilateral_5x5_rgb(src: &RgbaImage) -> RgbaImage {
    let w = src.width as i32;
    let h = src.height as i32;
    let stride = src.width as usize;
    let mut out = src.clone();

    // Spatial weights (1D [1, 3, 4, 3, 1] outer product -> integer weights)
    const K1D: [u32; 5] = [1, 3, 4, 3, 1];
    // Precompute color range LUT for sq_rgb_diff in 0..2400 (`exp(-d2 / (2 * 18^2 * 3))` scaled by 64)
    let mut range_lut = [0u32; 2401];
    for (d2, slot) in range_lut.iter_mut().enumerate() {
        let w_f = (-(d2 as f32) / 1944.0).exp();
        *slot = (w_f * 64.0).round() as u32;
    }

    for y in 0..h {
        for x in 0..w {
            let c = get_px(src, x, y);
            let mut sr = 0u32;
            let mut sg = 0u32;
            let mut sb = 0u32;
            let mut wt_sum = 0u32;

            for dy in -2..=2 {
                let ky = K1D[(dy + 2) as usize];
                let cy = (y + dy).clamp(0, h - 1) as usize;
                let row_off = cy * stride;
                for dx in -2..=2 {
                    let cx = (x + dx).clamp(0, w - 1) as usize;
                    let np = (row_off + cx) * 4;
                    let nr = src.pixels[np];
                    let ng = src.pixels[np + 1];
                    let nb = src.pixels[np + 2];

                    let dr = c[0] as i32 - nr as i32;
                    let dg = c[1] as i32 - ng as i32;
                    let db = c[2] as i32 - nb as i32;
                    let d2 = (dr * dr + dg * dg + db * db) as usize;
                    if d2 <= 2400 {
                        let rw = range_lut[d2];
                        let wgt = ky * K1D[(dx + 2) as usize] * rw;
                        sr += nr as u32 * wgt;
                        sg += ng as u32 * wgt;
                        sb += nb as u32 * wgt;
                        wt_sum += wgt;
                    }
                }
            }

            if wt_sum > 0 {
                let p = (y as usize * stride + x as usize) * 4;
                out.pixels[p] = ((sr + wt_sum / 2) / wt_sum) as u8;
                out.pixels[p + 1] = ((sg + wt_sum / 2) / wt_sum) as u8;
                out.pixels[p + 2] = ((sb + wt_sum / 2) / wt_sum) as u8;
            }
        }
    }
    out
}

/// Clamped Anime4K-style Luminance-Guided Push step: sharpens interpolation gradients with ZERO ringing.
pub fn anime4k_push_step_rgb(src: &RgbaImage, strength: f32) -> RgbaImage {
    let w = src.width as i32;
    let h = src.height as i32;
    let stride = src.width as usize;
    let len = (w * h) as usize;

    let mut lum = vec![0.0f32; len];
    for i in 0..len {
        let p = i * 4;
        lum[i] = 0.299 * src.pixels[p] as f32
            + 0.587 * src.pixels[p + 1] as f32
            + 0.114 * src.pixels[p + 2] as f32;
    }

    let pairs: [((i32, i32), (i32, i32)); 4] = [
        ((0, -1), (0, 1)),
        ((-1, 0), (1, 0)),
        ((-1, -1), (1, 1)),
        ((1, -1), (-1, 1)),
    ];
    let pair_w = strength * 0.25;
    let mut out = src.clone();

    for y in 0..h {
        for x in 0..w {
            let idx = (y as usize) * stride + (x as usize);
            let cl = lum[idx];
            let cp = idx * 4;
            let cf = [
                src.pixels[cp] as f32,
                src.pixels[cp + 1] as f32,
                src.pixels[cp + 2] as f32,
            ];
            let mut delta = [0.0f32; 3];

            for &((dx1, dy1), (dx2, dy2)) in &pairs {
                let x1 = (x + dx1).clamp(0, w - 1) as usize;
                let y1 = (y + dy1).clamp(0, h - 1) as usize;
                let x2 = (x + dx2).clamp(0, w - 1) as usize;
                let y2 = (y + dy2).clamp(0, h - 1) as usize;
                let i1 = y1 * stride + x1;
                let i2 = y2 * stride + x2;
                let l1 = lum[i1];
                let l2 = lum[i2];
                if (l1 - l2).abs() > 8.0 {
                    let d1 = (l1 - cl).abs();
                    let d2 = (l2 - cl).abs();
                    if d1 < d2 {
                        let p1 = i1 * 4;
                        for ch in 0..3 {
                            delta[ch] += (src.pixels[p1 + ch] as f32 - cf[ch]) * pair_w;
                        }
                    } else if d2 < d1 {
                        let p2 = i2 * 4;
                        for ch in 0..3 {
                            delta[ch] += (src.pixels[p2 + ch] as f32 - cf[ch]) * pair_w;
                        }
                    }
                }
            }

            let mut nb_min = [src.pixels[cp], src.pixels[cp + 1], src.pixels[cp + 2]];
            let mut nb_max = nb_min;
            for dy in -1..=1 {
                let ny = (y + dy).clamp(0, h - 1) as usize * stride;
                for dx in -1..=1 {
                    let np = (ny + (x + dx).clamp(0, w - 1) as usize) * 4;
                    for ch in 0..3 {
                        nb_min[ch] = nb_min[ch].min(src.pixels[np + ch]);
                        nb_max[ch] = nb_max[ch].max(src.pixels[np + ch]);
                    }
                }
            }

            for ch in 0..3 {
                let v = (cf[ch] + delta[ch]).round() as i32;
                out.pixels[cp + ch] = v.clamp(nb_min[ch] as i32, nb_max[ch] as i32) as u8;
            }
        }
    }
    out
}

/// Core 4x RGB Hybrid Watercolor upscaler (used on extruded RGB images).
pub fn upscale_rgb_watercolor_4x(extruded_1x: &RgbaImage) -> RgbaImage {
    let clean = gentle_vga_dedither_rgb(extruded_1x);
    let s2 = ep_scale2x_rgb(&clean, 600);
    let s4 = catmull_rom_2x_rgb(&s2);
    let smooth = bilateral_5x5_rgb(&s4);
    let p1 = anime4k_push_step_rgb(&smooth, 0.65);
    anime4k_push_step_rgb(&p1, 0.45)
}

/// Bilinear 2x upscaler on single-channel alpha array (`w x h -> 2w x 2h`).
fn bilinear_2x_u8(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let w2 = w * 2;
    let h2 = h * 2;
    let mut tmp = vec![0u8; w2 * h];
    for y in 0..h {
        for x in 0..w {
            let xm1 = x.saturating_sub(1);
            let xp1 = (x + 1).min(w - 1);
            let c = src[y * w + x] as u32;
            let l = src[y * w + xm1] as u32;
            let r = src[y * w + xp1] as u32;
            tmp[y * w2 + x * 2] = ((3 * c + l + 2) / 4) as u8;
            tmp[y * w2 + x * 2 + 1] = ((3 * c + r + 2) / 4) as u8;
        }
    }
    let mut dst = vec![0u8; w2 * h2];
    for y in 0..h {
        let ym1 = y.saturating_sub(1);
        let yp1 = (y + 1).min(h - 1);
        for x in 0..w2 {
            let c = tmp[y * w2 + x] as u32;
            let u = tmp[ym1 * w2 + x] as u32;
            let d = tmp[yp1 * w2 + x] as u32;
            dst[(y * 2) * w2 + x] = ((3 * c + u + 2) / 4) as u8;
            dst[(y * 2 + 1) * w2 + x] = ((3 * c + d + 2) / 4) as u8;
        }
    }
    dst
}

/// 5x5 Gaussian blur (`[1, 4, 6, 4, 1] / 16`) on a single-channel `u8` buffer (`w x h`).
fn gaussian_5x5_u8(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    const K: [u32; 5] = [1, 4, 6, 4, 1];
    let mut tmp = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0u32;
            for (i, &kv) in K.iter().enumerate() {
                let cx = (x as i32 + i as i32 - 2).clamp(0, w as i32 - 1) as usize;
                s += src[y * w + cx] as u32 * kv;
            }
            tmp[y * w + x] = ((s + 8) >> 4) as u8;
        }
    }
    let mut dst = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0u32;
            for (i, &kv) in K.iter().enumerate() {
                let cy = (y as i32 + i as i32 - 2).clamp(0, h as i32 - 1) as usize;
                s += tmp[cy * w + x] as u32 * kv;
            }
            dst[y * w + x] = ((s + 8) >> 4) as u8;
        }
    }
    dst
}

/// Upscale a sprite / icon / portrait (`MGO`, `ABC`, `F`, `FIRE`, `RGM`, `BALL`) 4x with:
/// - DOS 1-bit shadow conversion -> soft translucent cast shadow
/// - RGB extrusion into transparent exterior (zero dark halos)
/// - Hybrid Watercolor 4x RGB + EPX-smoothed 1px-AA contour alpha!
pub fn upscale_pixel_art_4x(src: &RgbaImage) -> RgbaImage {
    if src.width == 0 || src.height == 0 {
        return src.clone();
    }
    let w = src.width as usize;
    let h = src.height as usize;
    let fixed = convert_dos_checker_shadows(src);
    let extruded = extrude_rgb_into_transparent(&fixed, 6);
    let mut out = upscale_rgb_watercolor_4x(&extruded);

    // Reconstruct clean 4x alpha (solid contour + soft translucent cast shadow)
    let mut sil_img = RgbaImage::new(src.width, src.height);
    let mut sh_1x = vec![0u8; w * h];
    let mut any_shadow = false;
    let mut all_opaque = true;

    for i in 0..(w * h) {
        let a = fixed.pixels[i * 4 + 3];
        if a < 255 {
            all_opaque = false;
        }
        let s = if a >= 200 { 255u8 } else { 0u8 };
        sil_img.pixels[i * 4] = s;
        sil_img.pixels[i * 4 + 1] = s;
        sil_img.pixels[i * 4 + 2] = s;
        sil_img.pixels[i * 4 + 3] = 255;
        if a > 0 && a < 200 {
            sh_1x[i] = a;
            any_shadow = true;
        }
    }

    if all_opaque {
        for i in 0..((w * 4) * (h * 4)) {
            out.pixels[i * 4 + 3] = 255;
        }
        return out;
    }

    let sil_2x = ep_scale2x_rgb(&sil_img, 400);
    let mut sil_2x_ch = vec![0u8; (w * 2) * (h * 2)];
    for (i, slot) in sil_2x_ch.iter_mut().enumerate() {
        *slot = sil_2x.pixels[i * 4];
    }
    let sil_4x = bilinear_2x_u8(&sil_2x_ch, w * 2, h * 2);

    let sh_4x = if any_shadow {
        let s2 = bilinear_2x_u8(&sh_1x, w, h);
        let s4 = bilinear_2x_u8(&s2, w * 2, h * 2);
        Some(gaussian_5x5_u8(&s4, w * 4, h * 4))
    } else {
        None
    };

    for i in 0..((w * 4) * (h * 4)) {
        let solid_a = ((sil_4x[i] as i32 - 96) * 4).clamp(0, 255) as u8;
        let final_a = if let Some(ref sh) = sh_4x {
            solid_a.max(sh[i])
        } else {
            solid_a
        };
        out.pixels[i * 4 + 3] = final_a;
    }
    out
}

/// Smooth 4x upscaler for `RGM` portraits, `FBP` full-screen art, and `RNG` cutscenes.
pub fn upscale_smooth_4x(src: &RgbaImage) -> RgbaImage {
    upscale_pixel_art_4x(src)
}

/// Upscale a single `32x15` GOP isometric tile inside its `(32 + 2*PAD) x (16 + 2*PAD)` (`40x24` when `PAD=4`)
/// context patch into a seamless `128x64` 4x tile using `DIAMOND_4X`!
pub fn upscale_gop_patch_to_tile_4x(
    tile_1x: &RgbaImage,
    context_patch: &RgbaImage,
    pad: usize,
) -> RgbaImage {
    debug_assert_eq!(tile_1x.width, 32);
    debug_assert_eq!(tile_1x.height, 15);
    let pw = 32 + pad * 2;
    let ph = 16 + pad * 2;
    debug_assert_eq!(context_patch.width as usize, pw);
    debug_assert_eq!(context_patch.height as usize, ph);

    let mut is_full_diamond = true;
    for y in 0..15 {
        for x in 0..32 {
            let op = tile_1x.pixels[(y * 32 + x) * 4 + 3] > 0;
            if op != is_diamond_1x(x, y) {
                is_full_diamond = false;
                break;
            }
        }
        if !is_full_diamond {
            break;
        }
    }

    let extruded = extrude_rgb_into_transparent(context_patch, 8);
    let rgb_up = upscale_rgb_watercolor_4x(&extruded);

    // Align 1x diamond center (`pad*4 + 29.5`) with `DIAMOND_4X` center (`31.0`) via `y0 = pad*4 - 1`
    let y0 = pad * 4 - 1;
    let x0 = pad * 4;
    let up_w = pw * 4;

    let partial_alpha = if !is_full_diamond {
        // Extrude opaque seam pixels outward by 2px so `DIAMOND_4X` cuts internal seams with ZERO 1px gaps!
        let mut sil = RgbaImage::new(pw as u32, ph as u32);
        let mut seam_mask = vec![false; pw * ph];
        for y in 0..15 {
            for x in 0..32 {
                if tile_1x.pixels[(y * 32 + x) * 4 + 3] > 0 {
                    let pi = (y + pad) * pw + (x + pad);
                    sil.pixels[pi * 4] = 255;
                    sil.pixels[pi * 4 + 1] = 255;
                    sil.pixels[pi * 4 + 2] = 255;
                    sil.pixels[pi * 4 + 3] = 255;
                    if is_diamond_1x_border(x, y) {
                        seam_mask[pi] = true;
                    }
                }
            }
        }

        // 5x5 dilation (`radius = 2`) of seam pixels strictly outside `DIAMOND_1X`
        for py in 0..ph {
            for px in 0..pw {
                let tx = px as i32 - pad as i32;
                let ty = py as i32 - pad as i32;
                let inside_d1 =
                    tx >= 0 && tx < 32 && ty >= 0 && ty < 15 && is_diamond_1x(tx as usize, ty as usize);
                if inside_d1 {
                    continue;
                }
                let mut near_seam = false;
                'dil: for dy in -2..=2 {
                    for dx in -2..=2 {
                        let nx = px as i32 + dx;
                        let ny = py as i32 + dy;
                        if nx >= 0
                            && nx < pw as i32
                            && ny >= 0
                            && ny < ph as i32
                            && seam_mask[ny as usize * pw + nx as usize]
                        {
                            near_seam = true;
                            break 'dil;
                        }
                    }
                }
                if near_seam {
                    let pi = (py * pw + px) * 4;
                    sil.pixels[pi] = 255;
                    sil.pixels[pi + 1] = 255;
                    sil.pixels[pi + 2] = 255;
                    sil.pixels[pi + 3] = 255;
                }
            }
        }

        let sil_2x = ep_scale2x_rgb(&sil, 400);
        let mut s2_ch = vec![0u8; (pw * 2) * (ph * 2)];
        for (i, slot) in s2_ch.iter_mut().enumerate() {
            *slot = sil_2x.pixels[i * 4];
        }
        Some(bilinear_2x_u8(&s2_ch, pw * 2, ph * 2))
    } else {
        None
    };

    let mut tile_4x = RgbaImage::new(128, 64);
    for dy in 0..64usize {
        for dx in 0..128usize {
            let src_i = ((y0 + dy) * up_w + (x0 + dx)) * 4;
            let dst_i = (dy * 128 + dx) * 4;
            tile_4x.pixels[dst_i] = rgb_up.pixels[src_i];
            tile_4x.pixels[dst_i + 1] = rgb_up.pixels[src_i + 1];
            tile_4x.pixels[dst_i + 2] = rgb_up.pixels[src_i + 2];

            let in_d4 = is_diamond_4x(dx as i32, dy as i32);
            let a = if !in_d4 {
                0u8
            } else if let Some(ref pa) = partial_alpha {
                if pa[(y0 + dy) * up_w + (x0 + dx)] >= 112 {
                    255u8
                } else {
                    0u8
                }
            } else {
                255u8
            };
            tile_4x.pixels[dst_i + 3] = a;
        }
    }
    tile_4x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diamond_4x_exact_partition() {
        let mut cov = vec![0u32; 500 * 700];
        for ty in 0..6usize {
            for tx in 0..4usize {
                for h in 0..2usize {
                    let dx0 = tx * 128 + h * 64;
                    let dy0 = ty * 64 + h * 32;
                    for y in 0..64usize {
                        for x in 0..128usize {
                            if is_diamond_4x(x as i32, y as i32) {
                                cov[(dy0 + y) * 700 + (dx0 + x)] += 1;
                            }
                        }
                    }
                }
            }
        }
        for y in 96..224 {
            for x in 128..384 {
                assert_eq!(
                    cov[y * 700 + x],
                    1,
                    "Tile coverage mismatch at ({x}, {y}): got {}",
                    cov[y * 700 + x]
                );
            }
        }
    }
}
