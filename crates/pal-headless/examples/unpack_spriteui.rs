//! Unpack 1995 DOS PAL `DATA.MKF` chunk 9 (`gpSpriteUI`) into
//! `site/assets/graphics/ui/spriteui_1x.webp`, `spriteui_4x.webp`, and `spriteui.json`.

use image::{Rgba, RgbaImage};
use pal_core::graphics::{decode_rle_sprite, Palette};
use pal_core::mkf::{mkf_chunk, smkf_frame, smkf_frame_count};
use std::fs;
use std::path::Path;

fn main() {
    let data_mkf = fs::read(".tmp/raw_mkf/DATA.MKF").expect("read .tmp/raw_mkf/DATA.MKF");
    let pat_mkf = fs::read(".tmp/raw_mkf/PAT.MKF").expect("read .tmp/raw_mkf/PAT.MKF");
    let pat0 = mkf_chunk(&pat_mkf, 0).expect("PAT chunk 0");
    let pal = Palette::from_vga6(pat0).expect("palette 0");

    let chunk9 = mkf_chunk(&data_mkf, 9).expect("DATA chunk 9");
    let n_frames = smkf_frame_count(chunk9);

    let mut frames = Vec::with_capacity(n_frames);
    for idx in 0..n_frames {
        let decoded = smkf_frame(chunk9, idx).and_then(|rle| decode_rle_sprite(rle, &pal));
        frames.push(decoded);
    }

    // Tight row-pack with 2px logical padding (8px at 4x) -> zero bleed
    let pad = 2u32;
    let max_w = 512u32;
    let mut x = pad;
    let mut y = pad;
    let mut row_h = 0u32;
    let mut used_w = 0u32;
    let mut rects: Vec<[u32; 4]> = Vec::with_capacity(n_frames);
    let mut placements: Vec<(u32, u32, usize)> = Vec::new();

    for (idx, fr) in frames.iter().enumerate() {
        let Some(img) = fr else {
            rects.push([0, 0, 0, 0]);
            continue;
        };
        let (fw, fh) = (img.width, img.height);
        if fw == 0 || fh == 0 {
            rects.push([0, 0, 0, 0]);
            continue;
        }
        if x + fw + pad > max_w {
            x = pad;
            y += row_h + pad;
            row_h = 0;
        }
        rects.push([x, y, fw, fh]);
        placements.push((x, y, idx));
        x += fw + pad;
        used_w = used_w.max(x);
        row_h = row_h.max(fh);
    }

    let atlas_w = used_w.max(4);
    let atlas_h = (y + row_h + pad).max(4);

    let mut img_1x = RgbaImage::from_pixel(atlas_w, atlas_h, Rgba([0, 0, 0, 0]));
    let mut img_4x = RgbaImage::from_pixel(atlas_w * 4, atlas_h * 4, Rgba([0, 0, 0, 0]));

    for &(ox, oy, idx) in &placements {
        let src = frames[idx].as_ref().unwrap();
        for py in 0..src.height {
            for px in 0..src.width {
                let si = ((py * src.width + px) * 4) as usize;
                let rgba = Rgba([
                    src.pixels[si],
                    src.pixels[si + 1],
                    src.pixels[si + 2],
                    src.pixels[si + 3],
                ]);
                img_1x.put_pixel(ox + px, oy + py, rgba);
                for dy in 0..4 {
                    for dx in 0..4 {
                        img_4x.put_pixel((ox + px) * 4 + dx, (oy + py) * 4 + dy, rgba);
                    }
                }
            }
        }
    }

    let out_dir = Path::new("site/assets/graphics/ui");
    fs::create_dir_all(out_dir).unwrap();
    let p1 = out_dir.join("spriteui_1x.webp");
    let p4 = out_dir.join("spriteui_4x.webp");
    let pj = out_dir.join("spriteui.json");

    img_1x.save(&p1).expect("save spriteui_1x.webp");
    img_4x.save(&p4).expect("save spriteui_4x.webp");

    let rects_str: Vec<String> = rects
        .iter()
        .map(|r| format!("[{},{},{},{}]", r[0], r[1], r[2], r[3]))
        .collect();
    let json = format!(
        "{{\"w\":{},\"h\":{},\"frames\":[{}]}}",
        atlas_w,
        atlas_h,
        rects_str.join(",")
    );
    fs::write(&pj, json).expect("write spriteui.json");

    println!(
        "Unpacked {}/{} spriteui frames -> {}x{} (1x: {} B, 4x: {} B)",
        placements.len(),
        n_frames,
        atlas_w,
        atlas_h,
        fs::metadata(&p1).unwrap().len(),
        fs::metadata(&p4).unwrap().len(),
    );
}
