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
        render_world(&mut self.fb, &mut self.cache, &mut self.fonts, state, world, self.hd_mode);

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
}
