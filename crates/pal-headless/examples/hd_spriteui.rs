//! Export & bake HD remaster sheets for `spriteui_4x.webp` (4 Bagua battle icons + 5 mini player faces).
//! Keeps `spriteui_1x.webp` untouched for `[Tab]` retro parity and preserves exact alpha silhouettes.

use image::{imageops::FilterType, GenericImageView, Rgba, RgbaImage};
use std::env;
use std::fs;
use std::path::Path;

const ICON_IDS: [usize; 4] = [40, 41, 42, 43];
const FACE_IDS: [usize; 5] = [48, 49, 50, 51, 52];

fn parse_rects(json_str: &str) -> Vec<[u32; 4]> {
    let start = json_str.find("\"frames\":[").expect("frames key") + 10;
    let body = &json_str[start..json_str.len() - 2];
    let mut out = Vec::new();
    let mut cur = [0u32; 4];
    let mut ci = 0usize;
    let mut num = 0u32;
    let mut in_num = false;
    for b in body.bytes() {
        match b {
            b'0'..=b'9' => {
                num = num * 10 + (b - b'0') as u32;
                in_num = true;
            }
            b',' | b']' => {
                if in_num {
                    cur[ci] = num;
                    ci += 1;
                    num = 0;
                    in_num = false;
                }
                if b == b']' {
                    out.push(cur);
                    cur = [0; 4];
                    ci = 0;
                }
            }
            _ => {}
        }
    }
    out
}

fn export_sheets(rects: &[[u32; 4]], img_1x: &RgbaImage) {
    let dir = Path::new(".tmp/hd_spriteui");
    fs::create_dir_all(dir).unwrap();

    // Sheet 1: 4 Bagua battle icons (2x2 grid, each cell 160x160, icon 120x120 centered) -> 320x320 -> saved at 640x640
    let mut icons = RgbaImage::from_pixel(320, 320, Rgba([44, 42, 48, 255]));
    for (i, &fid) in ICON_IDS.iter().enumerate() {
        let [fx, fy, fw, fh] = rects[fid];
        let crop = img_1x.view(fx, fy, fw, fh).to_image();
        let up = image::imageops::resize(&crop, fw * 4, fh * 4, FilterType::Nearest);
        let cx = ((i % 2) as u32) * 160 + (160 - fw * 4) / 2;
        let cy = ((i / 2) as u32) * 160 + (160 - fh * 4) / 2;
        for y in 0..up.height() {
            for x in 0..up.width() {
                let p = *up.get_pixel(x, y);
                if p[3] > 0 {
                    icons.put_pixel(cx + x, cy + y, p);
                }
            }
        }
    }
    icons.save(dir.join("sheet_icons_in.png")).unwrap();

    // Sheet 2: 5 mini player faces (3x2 grid, each cell 180x180, face 4x centered) -> 540x360
    let mut faces = RgbaImage::from_pixel(540, 360, Rgba([38, 30, 24, 255]));
    for (i, &fid) in FACE_IDS.iter().enumerate() {
        let [fx, fy, fw, fh] = rects[fid];
        let crop = img_1x.view(fx, fy, fw, fh).to_image();
        let up = image::imageops::resize(&crop, fw * 4, fh * 4, FilterType::Nearest);
        let cx = ((i % 3) as u32) * 180 + (180 - fw * 4) / 2;
        let cy = ((i / 3) as u32) * 180 + (180 - fh * 4) / 2;
        for y in 0..up.height() {
            for x in 0..up.width() {
                let p = *up.get_pixel(x, y);
                if p[3] > 0 {
                    faces.put_pixel(cx + x, cy + y, p);
                }
            }
        }
    }
    faces.save(dir.join("sheet_faces_in.png")).unwrap();
    println!("Exported .tmp/hd_spriteui/sheet_icons_in.png (320x320) & sheet_faces_in.png (540x360)");
}

fn splice_cell(
    dst_4x: &mut RgbaImage,
    src_sheet: &RgbaImage,
    mask_1x: &RgbaImage,
    rect: [u32; 4],
    cell_x: u32,
    cell_y: u32,
    cell_w: u32,
    cell_h: u32,
) {
    let [fx, fy, fw, fh] = rect;
    let dw = fw * 4;
    let dh = fh * 4;
    let sx0 = cell_x + (cell_w - dw) / 2;
    let sy0 = cell_y + (cell_h - dh) / 2;

    // Smooth 4x alpha mask from 1x silhouette (crisp anti-aliased edge, zero background halo)
    let mut alpha_1x = RgbaImage::from_pixel(fw, fh, Rgba([0, 0, 0, 0]));
    for y in 0..fh {
        for x in 0..fw {
            let a = mask_1x.get_pixel(fx + x, fy + y)[3];
            alpha_1x.put_pixel(x, y, Rgba([a, a, a, a]));
        }
    }
    let alpha_4x = image::imageops::resize(&alpha_1x, dw, dh, FilterType::Triangle);

    for y in 0..dh {
        for x in 0..dw {
            let a_raw = alpha_4x.get_pixel(x, y)[3];
            // Sharpen alpha edge slightly so there's zero soft fringe
            let a = if a_raw < 96 {
                0
            } else if a_raw > 192 {
                255
            } else {
                ((a_raw as u32 - 96) * 255 / 96) as u8
            };
            if a == 0 {
                dst_4x.put_pixel(fx * 4 + x, fy * 4 + y, Rgba([0, 0, 0, 0]));
            } else {
                let p = *src_sheet.get_pixel(sx0 + x, sy0 + y);
                dst_4x.put_pixel(fx * 4 + x, fy * 4 + y, Rgba([p[0], p[1], p[2], a]));
            }
        }
    }
}

fn bake_sheets(rects: &[[u32; 4]], img_1x: &RgbaImage, icons_path: &str, faces_path: &str) {
    let mut img_4x = image::open("site/assets/graphics/ui/spriteui_4x.webp")
        .expect("open spriteui_4x.webp")
        .to_rgba8();

    let icons_raw = image::open(icons_path).expect("open icons hd").to_rgba8();
    let icons_320 = image::imageops::resize(&icons_raw, 320, 320, FilterType::Lanczos3);
    for (i, &fid) in ICON_IDS.iter().enumerate() {
        let cx = ((i % 2) as u32) * 160;
        let cy = ((i / 2) as u32) * 160;
        splice_cell(&mut img_4x, &icons_320, img_1x, rects[fid], cx, cy, 160, 160);
    }

    let faces_raw = image::open(faces_path).expect("open faces hd").to_rgba8();
    let faces_540 = image::imageops::resize(&faces_raw, 540, 360, FilterType::Lanczos3);
    for (i, &fid) in FACE_IDS.iter().enumerate() {
        let cx = ((i % 3) as u32) * 180;
        let cy = ((i / 3) as u32) * 180;
        splice_cell(&mut img_4x, &faces_540, img_1x, rects[fid], cx, cy, 180, 180);
    }

    img_4x
        .save("site/assets/graphics/ui/spriteui_4x.webp")
        .expect("save spriteui_4x.webp");
    println!("Baked HD battle icons & mini portraits into site/assets/graphics/ui/spriteui_4x.webp!");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let json_str = fs::read_to_string("site/assets/graphics/ui/spriteui.json").expect("read spriteui.json");
    let rects = parse_rects(&json_str);
    let img_1x = image::open("site/assets/graphics/ui/spriteui_1x.webp")
        .expect("open spriteui_1x.webp")
        .to_rgba8();

    match args.get(1).map(|s| s.as_str()) {
        Some("bake") => {
            bake_sheets(&rects, &img_1x, &args[2], &args[3]);
        }
        _ => export_sheets(&rects, &img_1x),
    }
}
