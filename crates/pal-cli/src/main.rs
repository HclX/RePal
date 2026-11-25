//! `pal-cli`: Multi-threaded native binary unpacker & Stage-8 4x HD asset baker for PalRemake.
//! Reads `raw_data/*.MKF` and writes packed RGBA atlases + JSON frame tables + decompressed maps
//! into an intermediate directory for `build_assets.py` to encode into compact WebP & YAML.

use pal_core::graphics::{
    decode_indexed_320x200, decode_rle_sprite, rng_step, Palette, RgbaImage,
};
use pal_core::mkf::{mkf_chunk, smkf_frame, smkf_frame_count};
use pal_core::upscale::{
    is_diamond_1x, upscale_gop_patch_to_tile_4x, upscale_pixel_art_4x, upscale_smooth_4x,
};
use pal_core::yj1::yj1_decompress;
use std::env;
use std::fmt::Display;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

fn record_err(errors: &AtomicUsize, msg: impl Display) {
    eprintln!("[pal-cli] ERROR: {}", msg);
    errors.fetch_add(1, Ordering::Relaxed);
}

fn validate_mkf(name: &str, data: &[u8]) -> Result<usize, String> {
    if data.len() < 4 {
        return Err(format!("{}: MKF too short ({} bytes)", name, data.len()));
    }
    let first_off = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if first_off < 8 || first_off % 4 != 0 || first_off > data.len() {
        return Err(format!(
            "{}: invalid MKF offset table (first_off={}, len={})",
            name,
            first_off,
            data.len()
        ));
    }
    let count = first_off / 4 - 1;
    for idx in 0..count {
        let p = idx * 4;
        let o1 = u32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize;
        let o2 =
            u32::from_le_bytes([data[p + 4], data[p + 5], data[p + 6], data[p + 7]]) as usize;
        if o1 > o2 || o2 > data.len() {
            return Err(format!(
                "{}: chunk {} offset out of bounds ({}..{}, len={})",
                name,
                idx,
                o1,
                o2,
                data.len()
            ));
        }
    }
    Ok(count)
}

fn write_raw_rgba(path: &Path, img: &RgbaImage) -> std::io::Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(&img.width.to_le_bytes())?;
    f.write_all(&img.height.to_le_bytes())?;
    f.write_all(&img.pixels)?;
    Ok(())
}

/// Pack 1x frames and their corresponding 4x frames at exact `4x` coordinates (`ox*4, oy*4, w*4, h*4`)
/// so adjacent frames never cross-bleed during upscaling or WebP encoding!
fn pack_dual_frames_padded(
    frames_1x: &[Option<RgbaImage>],
    frames_4x: &[Option<RgbaImage>],
    pad_1x: u32,
    max_cols: u32,
) -> (RgbaImage, RgbaImage, String) {
    let valid_count = frames_1x.len() as u32;
    if valid_count == 0 {
        return (
            RgbaImage::new(4, 4),
            RgbaImage::new(16, 16),
            "[]".to_string(),
        );
    }
    let cols = valid_count.min(max_cols.max(1));
    let mut max_w = 1u32;
    let mut max_h = 1u32;
    for fr in frames_1x.iter().flatten() {
        max_w = max_w.max(fr.width);
        max_h = max_h.max(fr.height);
    }
    let cell_w = max_w + pad_1x * 2;
    let cell_h = max_h + pad_1x * 2;
    let rows = (valid_count + cols - 1) / cols;
    let atlas_w = cols * cell_w;
    let atlas_h = rows * cell_h;
    let mut atlas_1x = RgbaImage::new(atlas_w, atlas_h);
    let mut atlas_4x = RgbaImage::new(atlas_w * 4, atlas_h * 4);

    let mut rects_json = String::from("[");
    for idx in 0..frames_1x.len() {
        if idx > 0 {
            rects_json.push(',');
        }
        let col = (idx as u32) % cols;
        let row = (idx as u32) / cols;
        let ox = col * cell_w + pad_1x;
        let oy = row * cell_h + pad_1x;
        if let Some(ref fr1) = frames_1x[idx] {
            for y in 0..fr1.height {
                let src_off = (y * fr1.width * 4) as usize;
                let dst_off = (((oy + y) * atlas_w + ox) * 4) as usize;
                let row_bytes = (fr1.width * 4) as usize;
                atlas_1x.pixels[dst_off..dst_off + row_bytes]
                    .copy_from_slice(&fr1.pixels[src_off..src_off + row_bytes]);
            }
            if let Some(ref fr4) = frames_4x[idx] {
                let ox4 = ox * 4;
                let oy4 = oy * 4;
                let aw4 = atlas_w * 4;
                for y in 0..fr4.height {
                    let src_off = (y * fr4.width * 4) as usize;
                    let dst_off = (((oy4 + y) * aw4 + ox4) * 4) as usize;
                    let row_bytes = (fr4.width * 4) as usize;
                    atlas_4x.pixels[dst_off..dst_off + row_bytes]
                        .copy_from_slice(&fr4.pixels[src_off..src_off + row_bytes]);
                }
            }
            rects_json.push_str(&format!("[{},{},{},{}]", ox, oy, fr1.width, fr1.height));
        } else {
            rects_json.push_str("[0,0,0,0]");
        }
    }
    rects_json.push(']');
    (atlas_1x, atlas_4x, rects_json)
}

fn parallel_for<F>(count: usize, threads: usize, f: F)
where
    F: Fn(usize) + Sync,
{
    let next = AtomicUsize::new(0);
    let n_threads = threads.max(1).min(count.max(1));
    thread::scope(|s| {
        for _ in 0..n_threads {
            s.spawn(|| loop {
                let idx = next.fetch_add(1, Ordering::Relaxed);
                if idx >= count {
                    break;
                }
                f(idx);
            });
        }
    });
}

fn load_palettes(pat_bytes: &[u8]) -> Result<Vec<Palette>, String> {
    let n = validate_mkf("PAT.MKF", pat_bytes)?;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let ch = mkf_chunk(pat_bytes, i).ok_or_else(|| format!("PAT.MKF[{}] missing", i))?;
        let p = Palette::from_vga6(ch)
            .ok_or_else(|| format!("PAT.MKF[{}] invalid VGA palette ({} bytes)", i, ch.len()))?;
        out.push(p);
    }
    if out.is_empty() {
        return Err("No palettes found in PAT.MKF".to_string());
    }
    Ok(out)
}

fn process_smkf_archive(
    mkf_bytes: &[u8],
    pal: &Palette,
    out_dir: &Path,
    prefix: &str,
    compressed_yj1: bool,
    max_cols: u32,
    threads: usize,
) -> Result<(), String> {
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("mkdir {}: {}", out_dir.display(), e))?;
    let count = validate_mkf(prefix, mkf_bytes)?;
    let errors = AtomicUsize::new(0);
    parallel_for(count, threads, |idx| {
        let Some(raw_chunk) = mkf_chunk(mkf_bytes, idx) else {
            record_err(&errors, format!("{}[{}]: missing MKF chunk", prefix, idx));
            return;
        };
        if raw_chunk.is_empty() {
            return;
        }
        let owned;
        let smkf_data: &[u8] = if compressed_yj1 {
            match yj1_decompress(raw_chunk) {
                Ok(dec) => {
                    owned = dec;
                    &owned
                }
                Err(e) => {
                    record_err(&errors, format!("{}[{}]: yj1_decompress: {}", prefix, idx, e));
                    return;
                }
            }
        } else {
            raw_chunk
        };

        let fcount = smkf_frame_count(smkf_data);
        if fcount == 0 {
            record_err(
                &errors,
                format!("{}[{}]: invalid/empty SMKF frame table", prefix, idx),
            );
            return;
        }
        let mut frames_1x = Vec::with_capacity(fcount);
        let mut frames_4x = Vec::with_capacity(fcount);
        let mut any_valid = false;
        for fi in 0..fcount {
            let decoded = smkf_frame(smkf_data, fi).and_then(|rle| decode_rle_sprite(rle, pal));
            if let Some(ref f1) = decoded {
                any_valid = true;
                frames_4x.push(Some(upscale_pixel_art_4x(f1)));
            } else {
                frames_4x.push(None);
            }
            frames_1x.push(decoded);
        }
        if !any_valid {
            record_err(
                &errors,
                format!("{}[{}]: SMKF archive has zero valid frames", prefix, idx),
            );
            return;
        }

        let (atlas_1x, atlas_4x, rects_json) =
            pack_dual_frames_padded(&frames_1x, &frames_4x, 2, max_cols);

        let p1 = out_dir.join(format!("{}_{:03}_1x.rgba", prefix, idx));
        let p4 = out_dir.join(format!("{}_{:03}_4x.rgba", prefix, idx));
        let pm = out_dir.join(format!("{}_{:03}.json", prefix, idx));
        if let Err(e) = write_raw_rgba(&p1, &atlas_1x) {
            record_err(&errors, format!("write {}: {}", p1.display(), e));
        }
        if let Err(e) = write_raw_rgba(&p4, &atlas_4x) {
            record_err(&errors, format!("write {}: {}", p4.display(), e));
        }
        let meta = format!(
            "{{\"id\":{},\"w\":{},\"h\":{},\"frames\":{}}}",
            idx, atlas_1x.width, atlas_1x.height, rects_json
        );
        if let Err(e) = fs::write(&pm, meta) {
            record_err(&errors, format!("write {}: {}", pm.display(), e));
        }
    });
    let err_count = errors.load(Ordering::Relaxed);
    if err_count > 0 {
        return Err(format!("{} error(s) in SMKF stage '{}'", err_count, prefix));
    }
    Ok(())
}

fn process_gop_with_maps(
    gop_bytes: &[u8],
    map_bytes: &[u8],
    pal: &Palette,
    out_dir: &Path,
    threads: usize,
) -> Result<(), String> {
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("mkdir {}: {}", out_dir.display(), e))?;
    let count = validate_mkf("GOP.MKF", gop_bytes)?;
    let map_count = validate_mkf("MAP.MKF", map_bytes)?;
    let errors = AtomicUsize::new(0);
    parallel_for(count, threads, |idx| {
        let Some(smkf_data) = mkf_chunk(gop_bytes, idx) else {
            record_err(&errors, format!("GOP.MKF[{}]: missing MKF chunk", idx));
            return;
        };
        if smkf_data.is_empty() {
            return;
        }
        let fcount = smkf_frame_count(smkf_data);
        if fcount == 0 {
            record_err(
                &errors,
                format!("GOP.MKF[{}]: invalid/empty SMKF frame table", idx),
            );
            return;
        }

        let mut raw_tiles: Vec<Option<RgbaImage>> = Vec::with_capacity(fcount);
        let mut any_valid = false;
        for fi in 0..fcount {
            let decoded = smkf_frame(smkf_data, fi).and_then(|rle| decode_rle_sprite(rle, pal));
            if decoded.is_some() {
                any_valid = true;
            }
            raw_tiles.push(decoded);
        }
        if !any_valid {
            record_err(
                &errors,
                format!("GOP.MKF[{}]: SMKF archive has zero valid tiles", idx),
            );
            return;
        }

        // Decompress matching map `idx` (`65,536` bytes = 128 x 64 x 2) when present.
        let map_dec: Option<Vec<u8>> = if idx < map_count {
            let Some(mc) = mkf_chunk(map_bytes, idx) else {
                record_err(&errors, format!("MAP.MKF[{}]: missing MKF chunk", idx));
                return;
            };
            if mc.is_empty() {
                None
            } else {
                match yj1_decompress(mc) {
                    Ok(v) if v.len() == 65536 => Some(v),
                    Ok(v) => {
                        record_err(
                            &errors,
                            format!(
                                "MAP.MKF[{}] (gop context): unexpected size {} (expected 65536)",
                                idx,
                                v.len()
                            ),
                        );
                        return;
                    }
                    Err(e) => {
                        record_err(
                            &errors,
                            format!("MAP.MKF[{}] (gop context): yj1_decompress: {}", idx, e),
                        );
                        return;
                    }
                }
            }
        } else {
            None
        };

        const CW: usize = 2112;
        const CH: usize = 2112;
        const MX: usize = 32;
        const MY: usize = 32;
        const PAD: usize = 4;

        let mut occ_ground: Vec<Vec<(usize, usize, usize)>> = vec![Vec::new(); fcount];
        let mut occ_elev: Vec<Vec<(usize, usize, usize)>> = vec![Vec::new(); fcount];
        let mut btm_grid = vec![-1i32; 128 * 64 * 2];

        let mut ground_1x = Vec::new();
        let mut elev_1x = Vec::new();

        if let Some(ref mbytes) = map_dec {
            ground_1x.resize(CW * CH * 4, 0u8);
            elev_1x.resize(CW * CH * 4, 0u8);

            let blit_1x = |canvas: &mut [u8], tile: &RgbaImage, dx: usize, dy: usize| {
                let tw = tile.width as usize;
                let th = tile.height as usize;
                for y in 0..th {
                    for x in 0..tw {
                        let sp = (y * tw + x) * 4;
                        if tile.pixels[sp + 3] > 0 {
                            let dp = ((dy + y) * CW + (dx + x)) * 4;
                            canvas[dp..dp + 4].copy_from_slice(&tile.pixels[sp..sp + 4]);
                        }
                    }
                }
            };

            for ty in 0..128usize {
                for tx in 0..64usize {
                    for h in 0..2usize {
                        let off = ((ty * 64 + tx) * 2 + h) * 4;
                        let b0 = mbytes[off] as usize;
                        let b1 = mbytes[off + 1] as usize;
                        let b2 = mbytes[off + 2] as usize;
                        let b3 = mbytes[off + 3] as usize;

                        let btm_s = b0 | ((b1 & 0x10) << 4);
                        let btm_l = b1 & 0x0f;
                        let raw_top = b2 | ((b3 & 0x10) << 4);
                        let top_l = b3 & 0x0f;

                        let dx = (tx * 32 + h * 16 + MX) - 16;
                        let dy = (ty * 16 + h * 8 + MY) - 8;

                        if btm_s < fcount {
                            if let Some(ref t) = raw_tiles[btm_s] {
                                if btm_l == 0 {
                                    blit_1x(&mut ground_1x, t, dx, dy);
                                    btm_grid[(ty * 64 + tx) * 2 + h] = btm_s as i32;
                                    occ_ground[btm_s].push((tx, ty, h));
                                } else {
                                    blit_1x(&mut elev_1x, t, dx, dy);
                                    occ_elev[btm_s].push((tx, ty, h));
                                }
                            }
                        }

                        if raw_top > 0 {
                            let top_s = raw_top - 1;
                            if top_s < fcount {
                                if let Some(ref t) = raw_tiles[top_s] {
                                    if top_l == 0 {
                                        blit_1x(&mut ground_1x, t, dx, dy);
                                        occ_ground[top_s].push((tx, ty, h));
                                    } else {
                                        blit_1x(&mut elev_1x, t, dx, dy);
                                        occ_elev[top_s].push((tx, ty, h));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let pw = 32 + PAD * 2;
        let ph = 16 + PAD * 2;
        let mut frames_1x: Vec<Option<RgbaImage>> = Vec::with_capacity(fcount);
        let mut frames_4x: Vec<Option<RgbaImage>> = Vec::with_capacity(fcount);

        for (t_idx, opt_t) in raw_tiles.iter().enumerate() {
            let Some(ref t1) = opt_t else {
                frames_1x.push(None);
                frames_4x.push(None);
                continue;
            };

            if t1.width != 32 || t1.height != 15 {
                frames_1x.push(Some(t1.clone()));
                frames_4x.push(Some(upscale_pixel_art_4x(t1)));
                continue;
            }

            // Pad 1x `32x15` tile to `32x16` (row 15 transparent so 1x retro rendering is 100% pixel-identical!)
            let mut padded_1x = RgbaImage::new(32, 16);
            padded_1x.pixels[0..(32 * 15 * 4)].copy_from_slice(&t1.pixels);

            let mut is_full = true;
            for y in 0..15usize {
                for x in 0..32usize {
                    if (t1.pixels[(y * 32 + x) * 4 + 3] > 0) != is_diamond_1x(x, y) {
                        is_full = false;
                        break;
                    }
                }
                if !is_full {
                    break;
                }
            }

            let mut patch = RgbaImage::new(pw as u32, ph as u32);
            let mut used_ctx = false;

            if map_dec.is_some() {
                let copy_canvas_patch =
                    |canvas: &[u8], tx: usize, ty: usize, h: usize, dst: &mut RgbaImage| {
                        let dx0 = (tx * 32 + h * 16 + MX) - 16 - PAD;
                        let dy0 = (ty * 16 + h * 8 + MY) - 8 - PAD;
                        for py in 0..ph {
                            let src_off = ((dy0 + py) * CW + dx0) * 4;
                            let dst_off = py * pw * 4;
                            dst.pixels[dst_off..dst_off + pw * 4]
                                .copy_from_slice(&canvas[src_off..src_off + pw * 4]);
                        }
                    };

                if is_full && !occ_ground[t_idx].is_empty() {
                    if occ_ground[t_idx].len() == 1 {
                        let (tx, ty, h) = occ_ground[t_idx][0];
                        copy_canvas_patch(&ground_1x, tx, ty, h, &mut patch);
                        used_ctx = true;
                    } else {
                        for &(tx, ty, h) in &occ_ground[t_idx] {
                            if (1..=61).contains(&tx) && (1..=125).contains(&ty) {
                                let nbs: [(usize, usize, usize); 4] = if h == 0 {
                                    [
                                        (tx - 1, ty - 1, 1),
                                        (tx, ty - 1, 1),
                                        (tx - 1, ty, 1),
                                        (tx, ty, 1),
                                    ]
                                } else {
                                    [
                                        (tx, ty, 0),
                                        (tx + 1, ty, 0),
                                        (tx, ty + 1, 0),
                                        (tx + 1, ty + 1, 0),
                                    ]
                                };
                                if nbs
                                    .iter()
                                    .all(|&(nx, ny, nh)| btm_grid[(ny * 64 + nx) * 2 + nh] == t_idx as i32)
                                {
                                    copy_canvas_patch(&ground_1x, tx, ty, h, &mut patch);
                                    used_ctx = true;
                                    break;
                                }
                            }
                        }
                    }
                } else if occ_elev[t_idx].len() == 1 && occ_ground[t_idx].is_empty() {
                    let (tx, ty, h) = occ_elev[t_idx][0];
                    copy_canvas_patch(&elev_1x, tx, ty, h, &mut patch);
                    used_ctx = true;
                }
            }

            // Guarantee central tile's own opaque pixels are 100% exact in `patch`
            for y in 0..15usize {
                for x in 0..32usize {
                    let sp = (y * 32 + x) * 4;
                    if !used_ctx || t1.pixels[sp + 3] > 0 {
                        let dp = ((y + PAD) * pw + (x + PAD)) * 4;
                        patch.pixels[dp..dp + 4].copy_from_slice(&t1.pixels[sp..sp + 4]);
                    }
                }
            }

            let tile_4x = upscale_gop_patch_to_tile_4x(t1, &patch, PAD);
            frames_1x.push(Some(padded_1x));
            frames_4x.push(Some(tile_4x));
        }

        let (atlas_1x, atlas_4x, rects_json) =
            pack_dual_frames_padded(&frames_1x, &frames_4x, 2, 16);
        let p1 = out_dir.join(format!("gop_{:03}_1x.rgba", idx));
        let p4 = out_dir.join(format!("gop_{:03}_4x.rgba", idx));
        let pm = out_dir.join(format!("gop_{:03}.json", idx));
        if let Err(e) = write_raw_rgba(&p1, &atlas_1x) {
            record_err(&errors, format!("write {}: {}", p1.display(), e));
        }
        if let Err(e) = write_raw_rgba(&p4, &atlas_4x) {
            record_err(&errors, format!("write {}: {}", p4.display(), e));
        }
        let meta = format!(
            "{{\"id\":{},\"w\":{},\"h\":{},\"frames\":{}}}",
            idx, atlas_1x.width, atlas_1x.height, rects_json
        );
        if let Err(e) = fs::write(&pm, meta) {
            record_err(&errors, format!("write {}: {}", pm.display(), e));
        }
    });
    let err_count = errors.load(Ordering::Relaxed);
    if err_count > 0 {
        return Err(format!("{} error(s) in GOP stage", err_count));
    }
    Ok(())
}

fn process_single_rle_archive(
    mkf_bytes: &[u8],
    pal: &Palette,
    out_dir: &Path,
    prefix: &str,
    smooth: bool,
    max_cols: u32,
    threads: usize,
) -> Result<(), String> {
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("mkdir {}: {}", out_dir.display(), e))?;
    let count = validate_mkf(prefix, mkf_bytes)?;
    let mut frames_1x = Vec::with_capacity(count);
    for idx in 0..count {
        let ch = mkf_chunk(mkf_bytes, idx)
            .ok_or_else(|| format!("{}[{}]: missing MKF chunk", prefix, idx))?;
        if ch.is_empty() {
            frames_1x.push(None);
        } else {
            let img = decode_rle_sprite(ch, pal)
                .ok_or_else(|| format!("{}[{}]: decode_rle_sprite failed", prefix, idx))?;
            frames_1x.push(Some(img));
        }
    }

    let frames_4x_slots: Vec<std::sync::Mutex<Option<RgbaImage>>> =
        (0..count).map(|_| std::sync::Mutex::new(None)).collect();
    parallel_for(count, threads, |idx| {
        if let Some(ref f1) = frames_1x[idx] {
            let f4 = if smooth {
                upscale_smooth_4x(f1)
            } else {
                upscale_pixel_art_4x(f1)
            };
            *frames_4x_slots[idx].lock().unwrap() = Some(f4);
        }
    });
    let frames_4x: Vec<Option<RgbaImage>> = frames_4x_slots
        .into_iter()
        .map(|m| m.into_inner().unwrap())
        .collect();

    let (atlas_1x, atlas_4x, rects_json) =
        pack_dual_frames_padded(&frames_1x, &frames_4x, 2, max_cols);
    let p1 = out_dir.join(format!("{}_1x.rgba", prefix));
    let p4 = out_dir.join(format!("{}_4x.rgba", prefix));
    let pm = out_dir.join(format!("{}.json", prefix));
    write_raw_rgba(&p1, &atlas_1x).map_err(|e| format!("write {}: {}", p1.display(), e))?;
    write_raw_rgba(&p4, &atlas_4x).map_err(|e| format!("write {}: {}", p4.display(), e))?;
    let meta = format!(
        "{{\"w\":{},\"h\":{},\"frames\":{}}}",
        atlas_1x.width, atlas_1x.height, rects_json
    );
    fs::write(&pm, meta).map_err(|e| format!("write {}: {}", pm.display(), e))?;
    Ok(())
}

/// Authentic 1995 DOS PAL `PAT.MKF` palette index for each `FBP.MKF` backdrop,
/// derived from the game's `palette_set` script opcodes (`scripts_01.yaml:s_1087`).
fn fbp_palette_index(idx: usize) -> usize {
    match idx {
        64 | 69 => 4,           // Ending animation: Water Demon vortex & Ling'er wave (s_1087 palette_set 4)
        40..=49 | 65..=68 => 5, // Ending hydra, snow & staff credits (s_1087 palette_set 5)
        70 | 71 => 8,           // Ending sunset & Anu flute (s_1087 palette_set 8)
        _ => 0,
    }
}

/// Authentic 1995 DOS PAL `PAT.MKF` palette index for each `RNG.MKF` cutscene,
/// derived directly from the `palette_set` opcodes preceding each `rng_play`.
fn rng_palette_index(idx: usize) -> usize {
    match idx {
        3 => 2,  // scripts_06.yaml:s_66bf — Sword Immortal flying over sea of clouds (PAT[2])
        7 => 6,  // scripts_09.yaml:s_966e — Ling'er prayer (PAT[6])
        10 => 8, // scripts_01.yaml:s_1087 — Ending sunset walk (PAT[8])
        _ => 0,  // All other RNG cutscenes (incl. rng_00 in s_40bd) use default PAT[0]
    }
}

fn process_fbp(
    fbp_bytes: &[u8],
    pals: &[Palette],
    out_dir: &Path,
    threads: usize,
) -> Result<(), String> {
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("mkdir {}: {}", out_dir.display(), e))?;
    let count = validate_mkf("FBP.MKF", fbp_bytes)?;
    let errors = AtomicUsize::new(0);
    parallel_for(count, threads, |idx| {
        let Some(ch) = mkf_chunk(fbp_bytes, idx) else {
            record_err(&errors, format!("FBP.MKF[{}]: missing MKF chunk", idx));
            return;
        };
        if ch.is_empty() {
            return;
        }
        let raw = match yj1_decompress(ch) {
            Ok(r) => r,
            Err(e) => {
                record_err(&errors, format!("FBP.MKF[{}]: yj1_decompress: {}", idx, e));
                return;
            }
        };
        let pal_idx = fbp_palette_index(idx);
        let pal = pals.get(pal_idx).unwrap_or(&pals[0]);
        let Some(img_1x) = decode_indexed_320x200(&raw, pal) else {
            record_err(
                &errors,
                format!(
                    "FBP.MKF[{}]: decode_indexed_320x200 failed (len={})",
                    idx,
                    raw.len()
                ),
            );
            return;
        };
        let img_4x = upscale_smooth_4x(&img_1x);
        let p1 = out_dir.join(format!("fbp_{:03}_1x.rgba", idx));
        let p4 = out_dir.join(format!("fbp_{:03}_4x.rgba", idx));
        if let Err(e) = write_raw_rgba(&p1, &img_1x) {
            record_err(&errors, format!("write {}: {}", p1.display(), e));
        }
        if let Err(e) = write_raw_rgba(&p4, &img_4x) {
            record_err(&errors, format!("write {}: {}", p4.display(), e));
        }
    });
    let err_count = errors.load(Ordering::Relaxed);
    if err_count > 0 {
        return Err(format!("{} error(s) in FBP stage", err_count));
    }
    Ok(())
}

fn process_maps(map_bytes: &[u8], out_dir: &Path, threads: usize) -> Result<(), String> {
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("mkdir {}: {}", out_dir.display(), e))?;
    let count = validate_mkf("MAP.MKF", map_bytes)?;
    let errors = AtomicUsize::new(0);
    parallel_for(count, threads, |idx| {
        let Some(ch) = mkf_chunk(map_bytes, idx) else {
            record_err(&errors, format!("MAP.MKF[{}]: missing MKF chunk", idx));
            return;
        };
        if ch.is_empty() {
            return;
        }
        let raw = match yj1_decompress(ch) {
            Ok(v) => v,
            Err(e) => {
                record_err(&errors, format!("MAP.MKF[{}]: yj1_decompress: {}", idx, e));
                return;
            }
        };
        if raw.len() != 65536 {
            record_err(
                &errors,
                format!(
                    "MAP.MKF[{}]: unexpected size {} (expected 65536)",
                    idx,
                    raw.len()
                ),
            );
            return;
        }
        let path = out_dir.join(format!("map_{:03}.bin", idx));
        if let Err(e) = fs::write(&path, &raw) {
            record_err(&errors, format!("write {}: {}", path.display(), e));
        }
    });
    let err_count = errors.load(Ordering::Relaxed);
    if err_count > 0 {
        return Err(format!("{} error(s) in maps stage", err_count));
    }
    Ok(())
}

fn process_rng(
    rng_bytes: &[u8],
    pals: &[Palette],
    out_dir: &Path,
    threads: usize,
) -> Result<(), String> {
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("mkdir {}: {}", out_dir.display(), e))?;
    let count = validate_mkf("RNG.MKF", rng_bytes)?;
    let errors = AtomicUsize::new(0);
    parallel_for(count, threads, |idx| {
        let Some(sub_mkf) = mkf_chunk(rng_bytes, idx) else {
            record_err(&errors, format!("RNG.MKF[{}]: missing MKF chunk", idx));
            return;
        };
        let fcount = match validate_mkf(&format!("RNG.MKF[{}]", idx), sub_mkf) {
            Ok(n) if n > 0 => n,
            Ok(_) => {
                record_err(&errors, format!("RNG.MKF[{}]: sub-MKF has 0 frames", idx));
                return;
            }
            Err(e) => {
                record_err(&errors, e);
                return;
            }
        };
        let pal_idx = rng_palette_index(idx);
        let pal = pals.get(pal_idx).unwrap_or(&pals[0]);

        let rdir = out_dir.join(format!("rng_{:02}", idx));
        if let Err(e) = fs::create_dir_all(&rdir) {
            record_err(&errors, format!("mkdir {}: {}", rdir.display(), e));
            return;
        }
        let mut canvas = [0u8; 64000];
        let step = if fcount <= 48 { 1 } else { 2 };
        let mut exported = 0usize;
        for fi in 0..fcount {
            let Some(fch) = mkf_chunk(sub_mkf, fi) else {
                record_err(
                    &errors,
                    format!("RNG.MKF[{}][{}]: missing sub-MKF chunk", idx, fi),
                );
                return;
            };
            if !fch.is_empty() {
                let diff = match yj1_decompress(fch) {
                    Ok(d) => d,
                    Err(e) => {
                        record_err(
                            &errors,
                            format!("RNG.MKF[{}][{}]: yj1_decompress: {}", idx, fi, e),
                        );
                        return;
                    }
                };
                if let Err(e) = rng_step(&mut canvas, &diff) {
                    record_err(
                        &errors,
                        format!("RNG.MKF[{}][{}]: rng_step: {}", idx, fi, e),
                    );
                    return;
                }
            }
            if fi % step == 0 || fi + 1 == fcount {
                let Some(img_1x) = decode_indexed_320x200(&canvas, pal) else {
                    record_err(
                        &errors,
                        format!("RNG.MKF[{}][{}]: decode_indexed_320x200 failed", idx, fi),
                    );
                    return;
                };
                let p1 = rdir.join(format!("f_{:03}_1x.rgba", exported));
                if let Err(e) = write_raw_rgba(&p1, &img_1x) {
                    record_err(&errors, format!("write {}: {}", p1.display(), e));
                    return;
                }
                exported += 1;
            }
        }
        let meta = format!(
            "{{\"id\":{},\"orig_frames\":{},\"exported_frames\":{},\"frame_step\":{}}}",
            idx, fcount, exported, step
        );
        let pm = rdir.join("meta.json");
        if let Err(e) = fs::write(&pm, meta) {
            record_err(&errors, format!("write {}: {}", pm.display(), e));
        }
    });
    let err_count = errors.load(Ordering::Relaxed);
    if err_count > 0 {
        return Err(format!("{} error(s) in RNG stage", err_count));
    }
    Ok(())
}

fn read_mkf_file(dir: &Path, filename: &str) -> Result<Vec<u8>, String> {
    let p = dir.join(filename);
    fs::read(&p).map_err(|e| format!("read {}: {}", p.display(), e))
}

fn run(raw_dir: &Path, out_dir: &Path, only_stage: &str, threads: usize) -> Result<(), String> {
    if !matches!(
        only_stage,
        "all" | "maps" | "ui" | "fbp" | "gop" | "sprites" | "rng"
    ) {
        return Err(format!(
            "Unknown stage '{}' (expected: all|maps|ui|fbp|gop|sprites|rng)",
            only_stage
        ));
    }

    println!(
        "[pal-cli] Running on {} worker threads | raw={} -> out={} (stage={})",
        threads,
        raw_dir.display(),
        out_dir.display(),
        only_stage
    );

    let pat_bytes = read_mkf_file(raw_dir, "PAT.MKF")?;
    let pals = load_palettes(&pat_bytes)?;

    if only_stage == "all" || only_stage == "maps" {
        println!("[pal-cli] Decompressing 226 MAP.MKF maps...");
        let map_bytes = read_mkf_file(raw_dir, "MAP.MKF")?;
        process_maps(&map_bytes, &out_dir.join("maps"), threads)?;
    }

    if only_stage == "all" || only_stage == "ui" {
        println!("[pal-cli] Upscaling RGM.MKF portraits & BALL.MKF item icons (4x Hybrid Watercolor)...");
        let rgm_bytes = read_mkf_file(raw_dir, "RGM.MKF")?;
        process_single_rle_archive(
            &rgm_bytes,
            &pals[0],
            &out_dir.join("ui"),
            "rgm",
            true,
            10,
            threads,
        )?;

        let ball_bytes = read_mkf_file(raw_dir, "BALL.MKF")?;
        process_single_rle_archive(
            &ball_bytes,
            &pals[0],
            &out_dir.join("ui"),
            "ball",
            false,
            16,
            threads,
        )?;
    }

    if only_stage == "all" || only_stage == "fbp" {
        println!("[pal-cli] Upscaling 72 FBP.MKF backgrounds (4x Hybrid Watercolor)...");
        let fbp_bytes = read_mkf_file(raw_dir, "FBP.MKF")?;
        process_fbp(&fbp_bytes, &pals, &out_dir.join("fbp"), threads)?;
    }

    if only_stage == "all" || only_stage == "gop" {
        println!("[pal-cli] Upscaling 226 GOP.MKF isometric tile atlases (4x Map-Context Seamless Diamond)...");
        let gop_bytes = read_mkf_file(raw_dir, "GOP.MKF")?;
        let map_bytes = read_mkf_file(raw_dir, "MAP.MKF")?;
        process_gop_with_maps(&gop_bytes, &map_bytes, &pals[0], &out_dir.join("gop"), threads)?;
    }

    if only_stage == "all" || only_stage == "sprites" {
        println!("[pal-cli] Upscaling MGO/ABC/F/FIRE sprite sheets (4x Hybrid Watercolor + Translucent Shadows)...");
        let mgo = read_mkf_file(raw_dir, "MGO.MKF")?;
        process_smkf_archive(&mgo, &pals[0], &out_dir.join("mgo"), "mgo", true, 8, threads)?;

        let abc = read_mkf_file(raw_dir, "ABC.MKF")?;
        process_smkf_archive(&abc, &pals[0], &out_dir.join("abc"), "abc", true, 6, threads)?;

        let f_mkf = read_mkf_file(raw_dir, "F.MKF")?;
        process_smkf_archive(&f_mkf, &pals[0], &out_dir.join("f"), "f", true, 6, threads)?;

        let fire = read_mkf_file(raw_dir, "FIRE.MKF")?;
        process_smkf_archive(
            &fire,
            &pals[0],
            &out_dir.join("fire"),
            "fire",
            true,
            6,
            threads,
        )?;
    }

    if only_stage == "all" || only_stage == "rng" {
        println!("[pal-cli] Decoding & upscaling 12 RNG.MKF cutscenes...");
        let rng = read_mkf_file(raw_dir, "RNG.MKF")?;
        process_rng(&rng, &pals, &out_dir.join("rng"), threads)?;
    }

    println!("[pal-cli] Done!");
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let raw_dir = PathBuf::from(args.get(1).map(|s| s.as_str()).unwrap_or("raw_data"));
    let out_dir = PathBuf::from(
        args.get(2)
            .map(|s| s.as_str())
            .unwrap_or("raw_data/_decoded"),
    );
    let only_stage = args.get(3).map(|s| s.as_str()).unwrap_or("all");

    let threads = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);

    if let Err(err) = run(&raw_dir, &out_dir, only_stage, threads) {
        eprintln!("[pal-cli] FAILED: {}", err);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "pal_cli_test_{}_{}_{}",
                label,
                std::process::id(),
                n
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn build_mkf(chunks: &[&[u8]]) -> Vec<u8> {
        let first_off = (chunks.len() + 1) * 4;
        let mut out = Vec::new();
        let mut off = first_off;
        for ch in chunks {
            out.extend_from_slice(&(off as u32).to_le_bytes());
            off += ch.len();
        }
        out.extend_from_slice(&(off as u32).to_le_bytes());
        for ch in chunks {
            out.extend_from_slice(ch);
        }
        out
    }

    fn build_yj1_raw(payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() <= u16::MAX as usize);
        let mut out = Vec::new();
        out.extend_from_slice(b"YJ_1");
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&((24 + payload.len()) as u32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // block_count = 1
        out.push(0);
        out.push(1); // tree_len = 1 (num_nodes = 2)
        out.extend_from_slice(&[0x41, 0x42]);
        out.extend_from_slice(&[0x00, 0x00]);
        out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // c_len = 0 -> raw block
        out.extend_from_slice(payload);
        out
    }

    /// Build a minimal valid 1x1 non-empty DOS RLE sprite (`w=1, h=1`, 1 opaque pixel color 1).
    fn build_tiny_rle_1x1() -> Vec<u8> {
        // w=1, h=1, row0: [count=1, pixel=1]
        vec![1, 0, 1, 0, 1, 1]
    }

    fn build_smkf(frames: &[&[u8]]) -> Vec<u8> {
        let header_words = frames.len() + 1;
        let mut out = Vec::new();
        let mut byte_off = header_words * 2;
        for fr in frames {
            if fr.is_empty() {
                out.extend_from_slice(&0u16.to_le_bytes());
            } else {
                assert_eq!(byte_off % 2, 0);
                out.extend_from_slice(&((byte_off / 2) as u16).to_le_bytes());
                byte_off += fr.len() + (fr.len() % 2);
            }
        }
        out.extend_from_slice(&0u16.to_le_bytes()); // trailing sentinel 0
        for fr in frames {
            if !fr.is_empty() {
                out.extend_from_slice(fr);
                if fr.len() % 2 != 0 {
                    out.push(0);
                }
            }
        }
        out
    }

    fn valid_pat_mkf() -> Vec<u8> {
        let pal = vec![32u8; 768];
        build_mkf(&[&pal])
    }

    #[test]
    fn test_corrupted_map_yj1_returns_error() {
        let tmp = TempDir::new("corrupt_map");
        let raw = tmp.0.join("raw");
        let out = tmp.0.join("out");
        fs::create_dir_all(&raw).unwrap();
        fs::write(raw.join("PAT.MKF"), valid_pat_mkf()).unwrap();
        fs::write(raw.join("MAP.MKF"), build_mkf(&[b"NOT_YJ1_GARBAGE!"])).unwrap();

        let err = run(&raw, &out, "maps", 1).unwrap_err();
        assert!(err.contains("maps"), "unexpected err: {}", err);
    }

    #[test]
    fn test_wrong_size_map_returns_error() {
        let tmp = TempDir::new("wrong_map_size");
        let raw = tmp.0.join("raw");
        let out = tmp.0.join("out");
        fs::create_dir_all(&raw).unwrap();
        fs::write(raw.join("PAT.MKF"), valid_pat_mkf()).unwrap();
        let tiny_map = build_yj1_raw(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        fs::write(raw.join("MAP.MKF"), build_mkf(&[&tiny_map])).unwrap();

        let err = run(&raw, &out, "maps", 1).unwrap_err();
        assert!(err.contains("maps"), "unexpected err: {}", err);
    }

    #[test]
    fn test_unwritable_output_returns_error() {
        let tmp = TempDir::new("unwritable_out");
        let raw = tmp.0.join("raw");
        let out = tmp.0.join("out");
        fs::create_dir_all(&raw).unwrap();
        fs::create_dir_all(out.join("maps/map_000.bin")).unwrap(); // EISDIR collision
        fs::write(raw.join("PAT.MKF"), valid_pat_mkf()).unwrap();
        let valid_map = build_yj1_raw(&vec![0u8; 65535]); // Wait: 65535 < 65536 triggers size; use 2 blocks for 65536
        drop(valid_map);

        // Build exact 65536-byte raw YJ_1 (2 blocks of 32768)
        let mut raw_64k = Vec::new();
        raw_64k.extend_from_slice(b"YJ_1");
        raw_64k.extend_from_slice(&65536u32.to_le_bytes());
        raw_64k.extend_from_slice(&65600u32.to_le_bytes());
        raw_64k.extend_from_slice(&2u16.to_le_bytes());
        raw_64k.push(0);
        raw_64k.push(1);
        raw_64k.extend_from_slice(&[0x41, 0x42, 0x00, 0x00]);
        for _ in 0..2 {
            raw_64k.extend_from_slice(&32768u16.to_le_bytes());
            raw_64k.extend_from_slice(&0u16.to_le_bytes());
            raw_64k.extend(std::iter::repeat(0u8).take(32768));
        }
        fs::write(raw.join("MAP.MKF"), build_mkf(&[&raw_64k])).unwrap();

        let err = run(&raw, &out, "maps", 1).unwrap_err();
        assert!(err.contains("maps"), "unexpected err: {}", err);
    }

    #[test]
    fn test_corrupted_fbp_yj1_and_write_error() {
        let tmp = TempDir::new("corrupt_fbp");
        let pat = valid_pat_mkf();
        let pals = load_palettes(&pat).unwrap();

        let out_bad = tmp.0.join("out_bad");
        let fbp_bad = build_mkf(&[b"BAD_FBP_YJ1"]);
        assert!(process_fbp(&fbp_bad, &pals, &out_bad, 1).is_err());

        // Valid 64000-byte raw YJ_1 FBP with EISDIR target
        let mut raw_fbp = Vec::new();
        raw_fbp.extend_from_slice(b"YJ_1");
        raw_fbp.extend_from_slice(&64000u32.to_le_bytes());
        raw_fbp.extend_from_slice(&64028u32.to_le_bytes());
        raw_fbp.extend_from_slice(&1u16.to_le_bytes());
        raw_fbp.push(0);
        raw_fbp.push(1);
        raw_fbp.extend_from_slice(&[0x41, 0x42, 0x00, 0x00]);
        raw_fbp.extend_from_slice(&64000u16.to_le_bytes());
        raw_fbp.extend_from_slice(&0u16.to_le_bytes());
        raw_fbp.extend(std::iter::repeat(1u8).take(64000));
        let fbp_ok = build_mkf(&[&raw_fbp]);

        let out_coll = tmp.0.join("out_coll");
        fs::create_dir_all(out_coll.join("fbp_000_1x.rgba")).unwrap();
        assert!(process_fbp(&fbp_ok, &pals, &out_coll, 1).is_err());
    }

    #[test]
    fn test_corrupted_smkf_yj1_and_unwritable_output() {
        let tmp = TempDir::new("corrupt_smkf");
        let pat = valid_pat_mkf();
        let pals = load_palettes(&pat).unwrap();

        // Corrupted YJ_1
        let bad_yj1 = build_mkf(&[b"NOT_YJ1"]);
        assert!(
            process_smkf_archive(&bad_yj1, &pals[0], &tmp.0.join("o1"), "mgo", true, 4, 1).is_err()
        );

        // Non-empty SMKF with fcount == 0
        let bad_smkf = build_mkf(&[&[0u8, 0u8, 0u8, 0u8]]);
        assert!(
            process_smkf_archive(&bad_smkf, &pals[0], &tmp.0.join("o2"), "mgo", false, 4, 1)
                .is_err()
        );

        // Valid SMKF with unwritable json path (EISDIR)
        let smkf = build_smkf(&[&build_tiny_rle_1x1()]);
        let ok_mkf = build_mkf(&[&smkf]);
        let o3 = tmp.0.join("o3");
        fs::create_dir_all(o3.join("mgo_000.json")).unwrap();
        assert!(process_smkf_archive(&ok_mkf, &pals[0], &o3, "mgo", false, 4, 1).is_err());
    }

    #[test]
    fn test_corrupted_rng_step_and_mkdir_error() {
        let tmp = TempDir::new("corrupt_rng");
        let pat = valid_pat_mkf();
        let pals = load_palettes(&pat).unwrap();

        // Truncated RNG opcode 0x03 in rng_step -> Err("RNG 0x03 EOF")
        let bad_diff = build_yj1_raw(&[0x03]);
        let sub_bad_op = build_mkf(&[&bad_diff, &[]]);
        let rng_bad_op = build_mkf(&[&sub_bad_op]);
        assert!(process_rng(&rng_bad_op, &pals, &tmp.0.join("r1"), 1).is_err());

        // Corrupted YJ_1 sub-chunk
        let sub_bad_yj1 = build_mkf(&[b"BAD_RNG_YJ1"]);
        let rng_bad_yj1 = build_mkf(&[&sub_bad_yj1]);
        assert!(process_rng(&rng_bad_yj1, &pals, &tmp.0.join("r2"), 1).is_err());

        // Unwritable rng_00 dir (regular file blocking mkdir)
        let valid_diff = build_yj1_raw(&[0x00]); // RNG End opcode 0x00
        let sub_ok = build_mkf(&[&valid_diff, &[]]);
        let rng_ok = build_mkf(&[&sub_ok]);
        let r3 = tmp.0.join("r3");
        fs::create_dir_all(&r3).unwrap();
        fs::write(r3.join("rng_00"), b"block_mkdir").unwrap();
        assert!(process_rng(&rng_ok, &pals, &r3, 1).is_err());
    }

    #[test]
    fn test_corrupted_rle_archive_and_truncated_mkf_header() {
        let tmp = TempDir::new("corrupt_rle");
        let pat = valid_pat_mkf();
        let pals = load_palettes(&pat).unwrap();

        // Corrupt non-empty RLE chunk (1 byte < 4)
        let bad_rle = build_mkf(&[&[0xAAu8]]);
        assert!(
            process_single_rle_archive(&bad_rle, &pals[0], &tmp.0.join("ui"), "rgm", false, 4, 1)
                .is_err()
        );

        // Truncated / OOB MKF headers
        assert!(validate_mkf("TRUNC", &[1, 2]).is_err());
        assert!(validate_mkf("ZERO", &[0, 0, 0, 0, 0, 0, 0, 0]).is_err());
        let oob_mkf = [8, 0, 0, 0, 200, 0, 0, 0];
        assert!(validate_mkf("OOB", &oob_mkf).is_err());
    }

    #[test]
    fn test_legitimate_empty_mkf_slots_and_empty_smkf_frames_succeed() {
        let tmp = TempDir::new("legit_empty");
        let pat = valid_pat_mkf();
        let pals = load_palettes(&pat).unwrap();

        // 1. MAP.MKF with slot 0 legitimately empty (o1==o2)
        let map_mkf = build_mkf(&[&[]]);
        process_maps(&map_mkf, &tmp.0.join("maps"), 1).unwrap();

        // 2. RGM.MKF with slot 0 legitimately empty + slot 1 valid 1x1 RLE
        let rle = build_tiny_rle_1x1();
        let rgm_mkf = build_mkf(&[&[], &rle]);
        process_single_rle_archive(&rgm_mkf, &pals[0], &tmp.0.join("ui"), "rgm", false, 4, 1)
            .unwrap();
        assert!(tmp.0.join("ui/rgm_1x.rgba").exists());

        // 3. MGO/ABC SMKF with frame 0 valid RLE and frame 1 empty sentinel
        let smkf = build_smkf(&[&rle, &[]]);
        let mgo_mkf = build_mkf(&[&[], &smkf]);
        process_smkf_archive(&mgo_mkf, &pals[0], &tmp.0.join("mgo"), "mgo", false, 4, 1)
            .unwrap();
        assert!(tmp.0.join("mgo/mgo_001.json").exists());

        // 4. RNG with valid frame 0 (`0x00` end) and trailing empty sub-chunk
        let diff = build_yj1_raw(&[0x00]);
        let sub = build_mkf(&[&diff, &[]]);
        let rng = build_mkf(&[&sub]);
        process_rng(&rng, &pals, &tmp.0.join("rng"), 1).unwrap();
        assert!(tmp.0.join("rng/rng_00/meta.json").exists());
    }

    #[test]
    fn test_script_authoritative_palette_indices() {
        // RNG cutscenes match their preceding `palette_set` opcodes in scripts_*.yaml
        assert_eq!(rng_palette_index(0), 0); // s_40bd (previously mis-hardcoded to 2)
        assert_eq!(rng_palette_index(3), 2); // s_66bf: palette_set [2]
        assert_eq!(rng_palette_index(7), 6); // s_966e: palette_set [6]
        assert_eq!(rng_palette_index(10), 8); // s_1087: palette_set [8]
        assert_eq!(rng_palette_index(11), 0); // s_1087: palette_set [] -> 0

        // FBP backdrops match `scripts_01.yaml:s_1087`
        assert_eq!(fbp_palette_index(60), 0); // Title screen
        assert_eq!(fbp_palette_index(64), 4); // Ending vortex
        assert_eq!(fbp_palette_index(69), 4); // Ending wave
        assert_eq!(fbp_palette_index(68), 5); // Ending hydra
        assert_eq!(fbp_palette_index(66), 5); // Ending snow
        assert_eq!(fbp_palette_index(49), 5); // Ending "全劇終"
        assert_eq!(fbp_palette_index(71), 8); // Ending Anu flute
    }
}
