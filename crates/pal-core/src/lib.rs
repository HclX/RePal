//! `pal-core`: Shared core engine algorithms for PalRemake (offline CLI & browser WASM).

pub mod battle;
pub mod game;
pub mod graphics;
pub mod i18n;
pub mod isometric;

pub use i18n::{I18nOverlay, Language};
pub mod mkf;
pub mod npc_ai;
pub mod render;
pub mod rng;
pub mod script;
pub mod script_ops;
pub mod state;
pub mod synth;
pub mod upscale;
pub mod yj1;

#[cfg(test)]
mod tests {
    use crate::graphics::{Palette, RgbaImage};
    use crate::isometric::{
        find_path_world, half_tile_to_world, step_half_tile, world_to_half_tile, CollisionGrid,
    };
    use crate::upscale::{upscale_pixel_art_4x, upscale_smooth_4x};

    #[test]
    fn test_isometric_roundtrip_and_neighbors() {
        for y in [0, 1, 15, 83, 106, 127] {
            for x in [0, 1, 30, 39, 63] {
                for h in [0, 1] {
                    let (px, py) = half_tile_to_world(x, y, h);
                    assert_eq!(world_to_half_tile(px, py), (x, y, h));
                    // Stepping NE then SW returns to exact same half-tile
                    let (nx, ny, nh) = step_half_tile(x, y, h, 2);
                    assert_eq!(step_half_tile(nx, ny, nh, 0), (x, y, h));
                }
            }
        }
    }

    #[test]
    fn test_astar_pathfinding_around_wall() {
        let mut grid = CollisionGrid::empty();
        // Block (10, 10, 1) direct diagonal step
        grid.set_blocked(10, 10, 1, true);
        let (sx, sy) = half_tile_to_world(10, 10, 0);
        let (tx, ty) = half_tile_to_world(12, 12, 0);
        let path = find_path_world(&grid, sx, sy, tx, ty, 2048);
        assert!(!path.is_empty());
        assert_eq!(*path.last().unwrap(), (tx, ty));
        let blocked_world = half_tile_to_world(10, 10, 1);
        assert!(!path.contains(&blocked_world));
    }

    #[test]
    fn test_upscale_4x_dimensions_and_alpha_boundary() {
        let mut img = RgbaImage::new(8, 8);
        // Transparent background with opaque red diamond in center
        for y in 2..6 {
            for x in 2..6 {
                let p = (y * 8 + x) * 4;
                img.pixels[p..p + 4].copy_from_slice(&[220, 40, 30, 255]);
            }
        }
        let hd_px = upscale_pixel_art_4x(&img);
        assert_eq!(hd_px.width, 32);
        assert_eq!(hd_px.height, 32);
        // Corner (0, 0) must stay completely transparent (alpha == 0) with extruded anti-fringe RGB
        assert_eq!(hd_px.pixels[3], 0);
        assert_eq!(&hd_px.pixels[0..3], &[220, 40, 30]);
        // Center must stay opaque red
        let center = (16 * 32 + 16) * 4;
        assert_eq!(&hd_px.pixels[center..center + 4], &[220, 40, 30, 255]);

        let hd_sm = upscale_smooth_4x(&img);
        assert_eq!(hd_sm.width, 32);
        assert_eq!(hd_sm.height, 32);
    }

    #[test]
    fn test_palette_vga6_expansion() {
        let mut raw = vec![0u8; 768];
        raw[3] = 63;
        raw[4] = 32;
        raw[5] = 0;
        let pal = Palette::from_vga6(&raw).unwrap();
        assert_eq!(pal.colors[0], [0, 0, 0, 255]);
        assert_eq!(pal.colors[1][0], 255);
        assert_eq!(pal.colors[1][3], 255);
    }
}
