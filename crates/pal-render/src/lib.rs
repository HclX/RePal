//! `pal-render` — Pure-Rust Software Framebuffer, Asset Cache, Font Engine & World/Battle Renderer.
//!
//! Shared identically across `pal-wasm` (browser static web host) and `pal-headless` (local native CLI/tests).

pub mod cache;
pub mod font;
pub mod framebuffer;
pub mod world;

use cache::AssetCache;
use font::FontEngine;
use framebuffer::{Color, Framebuffer};
use pal_core::state::GameState;
use world::{render_world, WorldRenderState};

pub const RETRO_W: u32 = 320;
pub const RETRO_H: u32 = 200;
pub const HD_W: u32 = 1280;
pub const HD_H: u32 = 800;

/// Unified compositor owning the final RGBA output framebuffer, cached UI overlay layer,
/// bounded LRU `AssetCache`, and `FontEngine`.
pub struct PalCompositor {
    pub fb: Framebuffer,
    pub ui_layer: Framebuffer,
    pub ui_dirty: bool,
    ui_has_pixels: bool,
    pub hd_mode: bool,
    pub cache: AssetCache,
    pub fonts: FontEngine,
    crossfade_backup: Vec<u8>,
    crossfade_backup_dims: (u32, u32),
    last_crossfade_seq: u32,
}

impl Default for PalCompositor {
    fn default() -> Self {
        Self::new(true)
    }
}

impl PalCompositor {
    pub fn new(hd_mode: bool) -> Self {
        let (w, h) = if hd_mode { (HD_W, HD_H) } else { (RETRO_W, RETRO_H) };
        Self {
            fb: Framebuffer::new(w, h),
            ui_layer: Framebuffer::new(w, h),
            ui_dirty: true,
            ui_has_pixels: false,
            hd_mode,
            cache: AssetCache::default(),
            fonts: FontEngine::new(),
            crossfade_backup: Vec::new(),
            crossfade_backup_dims: (0, 0),
            last_crossfade_seq: 0,
        }
    }

    pub fn new_with_cache_budget(hd_mode: bool, budget_bytes: usize) -> Self {
        let mut this = Self::new(hd_mode);
        this.cache = AssetCache::new(budget_bytes);
        this
    }

    pub fn set_hd_mode(&mut self, hd_mode: bool) {
        if self.hd_mode == hd_mode {
            return;
        }
        self.hd_mode = hd_mode;
        let (w, h) = if hd_mode { (HD_W, HD_H) } else { (RETRO_W, RETRO_H) };
        self.fb.resize(w, h);
        self.ui_layer.resize(w, h);
        self.ui_dirty = true;
    }

    pub fn scale(&self) -> i32 {
        if self.hd_mode { 4 } else { 1 }
    }

    pub fn mark_ui_dirty(&mut self) {
        self.ui_dirty = true;
    }

    /// Render the world into `self.fb`, optionally re-render `self.ui_layer` if dirty,
    /// and composite `self.ui_layer` over `self.fb`.
    pub fn render_frame<F>(&mut self, state: &GameState, world: &WorldRenderState, draw_ui: F)
    where
        F: FnOnce(&mut Framebuffer, &mut AssetCache, &mut FontEngine, bool),
    {
        self.cache.advance_tick();
        if world.crossfade_alpha > 0.001 && world.crossfade_seq != self.last_crossfade_seq {
            // Snapshot the pre-change world framebuffer (authentic SDLPAL VIDEO_BackupScreen).
            self.crossfade_backup = self.fb.pixels.clone();
            self.crossfade_backup_dims = (self.fb.width, self.fb.height);
            self.last_crossfade_seq = world.crossfade_seq;
        }
        render_world(&mut self.fb, &mut self.cache, &mut self.fonts, state, world, self.hd_mode);
        if world.crossfade_alpha > 0.001
            && self.crossfade_backup_dims == (self.fb.width, self.fb.height)
            && self.crossfade_backup.len() == self.fb.pixels.len()
        {
            // Authentic SDLPAL VIDEO_FadeScreen cross-fade: blend backup (old scene) into new scene.
            let a = (world.crossfade_alpha.clamp(0.0, 1.0) * 256.0) as u32;
            let inv = 256 - a;
            for (dst, src) in self
                .fb
                .pixels
                .chunks_exact_mut(4)
                .zip(self.crossfade_backup.chunks_exact(4))
            {
                dst[0] = ((src[0] as u32 * a + dst[0] as u32 * inv) >> 8) as u8;
                dst[1] = ((src[1] as u32 * a + dst[1] as u32 * inv) >> 8) as u8;
                dst[2] = ((src[2] as u32 * a + dst[2] as u32 * inv) >> 8) as u8;
            }
        } else if world.crossfade_alpha <= 0.001 && !self.crossfade_backup.is_empty() {
            self.crossfade_backup.clear();
            self.crossfade_backup.shrink_to_fit();
            self.crossfade_backup_dims = (0, 0);
        }

        if self.ui_dirty {
            self.ui_layer.clear(Color::TRANSPARENT);
            draw_ui(&mut self.ui_layer, &mut self.cache, &mut self.fonts, self.hd_mode);
            self.ui_has_pixels = self
                .ui_layer
                .pixels
                .chunks_exact(4)
                .any(|px| px[3] != 0);
            self.ui_dirty = false;
        }
        if self.ui_has_pixels {
            self.fb.composite_over(&self.ui_layer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compositor_mode_switch_resizes_buffers() {
        let mut comp = PalCompositor::new(true);
        assert_eq!((comp.fb.width, comp.fb.height), (1280, 800));
        comp.set_hd_mode(false);
        assert_eq!((comp.fb.width, comp.fb.height), (320, 200));
        assert_eq!((comp.ui_layer.width, comp.ui_layer.height), (320, 200));
        assert!(comp.ui_dirty);
    }

    #[test]
    fn test_compositor_crossfade_blends_backup_and_frees_when_done() {
        let mut comp = PalCompositor::new(false);
        // Simulate a pre-change rendered frame filled with solid blue.
        comp.fb.clear(Color::rgba(0, 0, 200, 255));
        let state = GameState::default();
        let mut world = WorldRenderState {
            crossfade_alpha: 0.5,
            crossfade_seq: 1,
            ..Default::default()
        };
        // New frame renders black (empty world); 50% blend with blue backup should produce ~100 blue.
        comp.render_frame(&state, &world, |_, _, _, _| {});
        assert!(comp.fb.pixels[2] >= 95 && comp.fb.pixels[2] <= 105, "expected ~100 blue, got {}", comp.fb.pixels[2]);
        assert!(!comp.crossfade_backup.is_empty());

        // Finishing cross-fade frees backup buffer and shows pure new frame.
        world.crossfade_alpha = 0.0;
        comp.render_frame(&state, &world, |_, _, _, _| {});
        assert_eq!(comp.fb.pixels[2], 0);
        assert!(comp.crossfade_backup.is_empty());
    }
}
