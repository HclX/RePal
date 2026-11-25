//! Pure-Rust Isometric World & Battle Framebuffer Renderer.
//!
//! Renders 100% into `Framebuffer` using `AssetCache` (pull-based missing-asset requests)
//! and `pal_core` state (`GameState`, `DrawSprite`, `compute_camera`).

use crate::cache::{AssetCache, HdOccluderStrip};
use crate::font::FontEngine;
use crate::framebuffer::{Color, Framebuffer};
use pal_core::render::{build_field_draw_list, compute_camera, DrawSprite};
use pal_core::state::GameState;

#[derive(Clone, Debug, Default)]
pub struct BattleEnemyVisual {
    pub slot: usize,
    pub sprite_id: u16,
    pub frame: usize,
    pub x: i32,
    pub y: i32,
    pub hp: u16,
    pub max_hp: u16,
    pub alive: bool,
    pub flash: bool,
    pub name: String,
    /// When `!alive`, non-zero `fade_out` (`1..=255`) renders the dying enemy fading out smoothly.
    pub fade_out: u8,
    /// Active status IDs (`0..=8` SDLPAL status, `99` = poisoned).
    pub statuses: Vec<u16>,
}

#[derive(Clone, Debug, Default)]
pub struct BattleHeroVisual {
    pub role_id: u16,
    pub sprite_id: u16,
    pub frame: usize,
    pub x: i32,
    pub y: i32,
    pub hp: u16,
    pub max_hp: u16,
    pub mp: u16,
    pub max_mp: u16,
    pub flash: bool,
    pub name: String,
    pub avatar_rgm: u16,
    /// Live ATB action readiness (`0..=100`).
    pub atb_pct: u8,
    pub defending: bool,
    /// Active status IDs (`0..=8` SDLPAL status, `99` = poisoned).
    pub statuses: Vec<u16>,
}

#[derive(Clone, Debug, Default)]
pub struct BattleEffectVisual {
    pub fire_id: u16,
    pub frame: usize,
    pub x: i32,
    pub y: i32,
    pub alpha: u8,
    pub target_all: bool,
    pub target_enemy: bool,
    pub is_summon: bool,
}

#[derive(Clone, Debug)]
pub struct BattleFloatText {
    pub x: i32,
    pub y: i32,
    pub text: String,
    pub color: Color,
}

#[derive(Clone, Debug, Default)]
pub struct BattleVisuals {
    pub active: bool,
    pub battlefield_fbp: u16,
    pub enemies: Vec<BattleEnemyVisual>,
    pub heroes: Vec<BattleHeroVisual>,
    pub selected_enemy: Option<usize>,
    pub selected_ally: Option<usize>,
    pub active_hero: usize,
    pub prompting: bool,
    pub auto_battle: bool,
    pub dim_bg: bool,
    pub spell_banner: Option<String>,
    pub shake_offset: (i32, i32),
    pub active_effect: Option<BattleEffectVisual>,
    pub float_texts: Vec<BattleFloatText>,
}

#[derive(Clone, Debug)]
pub struct WorldRenderState {
    /// If `Some((rng_id, frame_idx))`, renders fullscreen RNG cutscene frame (live HD/1x switchable).
    pub active_rng: Option<(u16, u16)>,
    /// If `Some(fbp_id)`, renders full-screen FBP (title screen, cutscene, etc.) instead of isometric field.
    pub fbp_override: Option<u16>,
    /// Explicit active map_id synced from host/VM (overrides `state.scenes[scene_id].map_id`).
    pub map_id_override: Option<u16>,
    /// Optional scripted camera override `(cam_x, cam_y)` in 1x logical world pixels (`camera_pan`).
    pub cam_override: Option<(i32, i32)>,
    /// Transient screen-shake pixel offset `(dx, dy)` in 1x logical world units (`screen_shake`).
    pub shake_offset: (i32, i32),
    /// 0.0 = fully visible, 1.0 = fully faded to `fade_color`.
    pub fade_alpha: f32,
    /// Full-screen fade tint color (defaults to `Color::BLACK`; supports red/white flashes).
    pub fade_color: Color,
    /// Active `0x0073` (`VIDEO_FadeScreen`) cross-fade weight: `1.0` = backed-up pre-change frame, `0.0` = new frame.
    pub crossfade_alpha: f32,
    /// Monotonic counter bumped each time `0x0073` starts a cross-fade so `PalCompositor` snapshots `fb` once.
    pub crossfade_seq: u32,
    /// Subtle night palette atmospheric tint (applied in both HD and Retro modes).
    pub night_palette: bool,
    /// Optional HD click-to-walk ground ripple `(world_x, world_y, age_0_to_1)`.
    pub click_ripple: Option<(i32, i32, f32)>,
    /// Horizontal wave amplitude (DOS PAL screen wave effect).
    pub wave_amplitude: f32,
    /// Frame counter for wave animation & idle sprite bounce.
    pub frame_count: u64,
    /// Active scene event range `[ev_start, ev_end]` for `build_field_draw_list`.
    pub ev_start: u16,
    pub ev_end: u16,
    /// Battle scene visuals (when `battle.active == true`).
    pub battle: BattleVisuals,
}

impl Default for WorldRenderState {
    fn default() -> Self {
        Self {
            active_rng: None,
            fbp_override: None,
            map_id_override: None,
            cam_override: None,
            shake_offset: (0, 0),
            fade_alpha: 0.0,
            fade_color: Color::BLACK,
            crossfade_alpha: 0.0,
            crossfade_seq: 0,
            night_palette: false,
            click_ripple: None,
            wave_amplitude: 0.0,
            frame_count: 0,
            ev_start: 0,
            ev_end: 0,
            battle: BattleVisuals::default(),
        }
    }
}

/// Compute the clamped logical `320x200` world camera origin `(cam_x, cam_y)` identically for
/// the software renderer, pointer hit-testing (`logical_to_world`), fair-vision filtering, and
/// Text Mode `view.camera` responses (#222, #224).
pub fn resolve_world_camera(state: &GameState, world: &WorldRenderState) -> (i32, i32) {
    if let Some((cx, cy)) = world.cam_override {
        (cx.clamp(0, 2048 - 320), cy.clamp(0, 2048 - 200))
    } else {
        let (lead_x, lead_y) = state
            .party
            .first()
            .map(|p| (p.x, p.y))
            .unwrap_or((160, 100));
        let cam = compute_camera(lead_x, lead_y, 320, 200);
        (cam.cam_x, cam.cam_y)
    }
}

pub fn fbp_key(fbp_id: u16, hd: bool) -> String {
    let suffix = if hd { "4x" } else { "1x" };
    format!("graphics/fbp/fbp_{:03}_{}.webp", fbp_id, suffix)
}

pub fn rng_frame_key(rng_id: u16, frame_idx: u16, hd: bool) -> String {
    let suffix = if hd { "4x" } else { "1x" };
    format!("graphics/rng/rng_{:02}/f_{:03}_{}.webp", rng_id, frame_idx, suffix)
}

pub fn hd_map_meta_key(map_id: u16) -> String {
    format!("graphics/maps_hd/map_{:03}/meta.json", map_id)
}

pub fn hd_map_asset_key(map_id: u16, filename: &str) -> String {
    format!("graphics/maps_hd/map_{:03}/{}", map_id, filename)
}

pub fn atlas_tex_key(category: &str, base: &str, hd: bool) -> String {
    let suffix = if hd { "4x" } else { "1x" };
    format!("graphics/{}/{}_{}.webp", category, base, suffix)
}

pub fn atlas_meta_key(category: &str, base: &str) -> String {
    format!("graphics/{}/{}.json", category, base)
}

pub fn map_bin_key(map_id: u16) -> String {
    format!("maps/map_{:03}.bin", map_id)
}

/// Blit a single frame from an atlas texture (1x or 4x) to destination coordinates in framebuffer pixels.
pub fn blit_atlas_frame(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    category: &str,
    base: &str,
    frame_idx: usize,
    dst_x: i32,
    dst_y: i32,
    scale: i32,
    hd: bool,
) -> Option<(i32, i32)> {
    blit_atlas_frame_ex(fb, cache, category, base, frame_idx, dst_x, dst_y, scale, hd, false)
}

#[allow(clippy::too_many_arguments)]
pub fn blit_atlas_frame_ex(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    category: &str,
    base: &str,
    frame_idx: usize,
    dst_x: i32,
    dst_y: i32,
    scale: i32,
    hd: bool,
    flash: bool,
) -> Option<(i32, i32)> {
    blit_atlas_frame_alpha(fb, cache, category, base, frame_idx, dst_x, dst_y, scale, hd, flash, 255)
}

#[allow(clippy::too_many_arguments)]
pub fn blit_atlas_frame_alpha(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    category: &str,
    base: &str,
    frame_idx: usize,
    dst_x: i32,
    dst_y: i32,
    scale: i32,
    hd: bool,
    flash: bool,
    alpha: u8,
) -> Option<(i32, i32)> {
    let meta_key = atlas_meta_key(category, base);
    let tex_key = atlas_tex_key(category, base, hd);

    let frame = {
        let meta = cache.get_or_request_atlas(&meta_key)?;
        if meta.frames.is_empty() {
            return None;
        }
        let idx = frame_idx.min(meta.frames.len() - 1);
        let mut rect = meta.frames[idx];
        // Some DOS MGO archives (e.g. mgo_035 farmer Ev#83, #121) have trailing `[0,0,0,0]`
        // sentinel slots that scripts still index into; fall back to a valid non-empty frame
        // instead of making the NPC vanish.
        if rect[2] <= 0 || rect[3] <= 0 {
            let valid: Vec<[i32; 4]> = meta
                .frames
                .iter()
                .copied()
                .filter(|r| r[2] > 0 && r[3] > 0)
                .collect();
            if valid.is_empty() {
                return None;
            }
            rect = valid[frame_idx % valid.len()];
        }
        (meta.w, rect)
    };
    let (meta_w, [fx, fy, fw, fh]) = frame;
    if fw <= 0 || fh <= 0 {
        return None;
    }

    let tex = cache.get_or_request_texture(&tex_key)?;
    let tex_scale = if meta_w > 0 && tex.width >= meta_w * 2 {
        (tex.width / meta_w).max(1) as i32
    } else {
        1
    };

    let sx = fx * tex_scale;
    let sy = fy * tex_scale;
    let sw = fw * tex_scale;
    let sh = fh * tex_scale;
    let dw = fw * scale;
    let dh = fh * scale;

    if flash {
        fb.blit_region_flash(
            &tex.pixels,
            tex.width,
            tex.height,
            sx,
            sy,
            sw,
            sh,
            dst_x,
            dst_y,
            dw,
            dh,
        );
    } else {
        fb.blit_region_scaled(
            &tex.pixels,
            tex.width,
            tex.height,
            sx,
            sy,
            sw,
            sh,
            dst_x,
            dst_y,
            dw,
            dh,
            alpha,
        );
    }
    Some((dw, dh))
}

/// Query a frame's 1x logical `(w, h)` from an atlas JSON (triggers pull-request if not loaded).
pub fn atlas_frame_size(
    cache: &mut AssetCache,
    category: &str,
    base: &str,
    frame_idx: usize,
) -> Option<(i32, i32)> {
    let meta_key = atlas_meta_key(category, base);
    let meta = cache.get_or_request_atlas(&meta_key)?;
    let rect = *meta.frames.get(frame_idx)?;
    if rect[2] > 0 && rect[3] > 0 {
        Some((rect[2], rect[3]))
    } else {
        None
    }
}

/// Blit an atlas frame as a translucent black drop shadow.
#[allow(clippy::too_many_arguments)]
pub fn blit_atlas_frame_shadow(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    category: &str,
    base: &str,
    frame_idx: usize,
    dst_x: i32,
    dst_y: i32,
    scale: i32,
    hd: bool,
    shadow_alpha: u8,
) -> Option<(i32, i32)> {
    let meta_key = atlas_meta_key(category, base);
    let tex_key = atlas_tex_key(category, base, hd);
    let (meta_w, [fx, fy, fw, fh]) = {
        let meta = cache.get_or_request_atlas(&meta_key)?;
        let rect = *meta.frames.get(frame_idx)?;
        (meta.w, rect)
    };
    if fw <= 0 || fh <= 0 {
        return None;
    }
    let tex = cache.get_or_request_texture(&tex_key)?;
    let tex_scale = if meta_w > 0 && tex.width >= meta_w * 2 {
        (tex.width / meta_w).max(1) as i32
    } else {
        1
    };
    fb.blit_region_shadow(
        &tex.pixels,
        tex.width,
        tex.height,
        fx * tex_scale,
        fy * tex_scale,
        fw * tex_scale,
        fh * tex_scale,
        dst_x,
        dst_y,
        fw * scale,
        fh * scale,
        shadow_alpha,
    );
    Some((fw * scale, fh * scale))
}

/// Blit an atlas frame with luminance-tinted monochrome shading (for dimmed/inactive battle icons & dead faces).
#[allow(clippy::too_many_arguments)]
pub fn blit_atlas_frame_mono(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    category: &str,
    base: &str,
    frame_idx: usize,
    dst_x: i32,
    dst_y: i32,
    scale: i32,
    hd: bool,
    tint_rgb: [u8; 3],
) -> Option<(i32, i32)> {
    let meta_key = atlas_meta_key(category, base);
    let tex_key = atlas_tex_key(category, base, hd);
    let (meta_w, [fx, fy, fw, fh]) = {
        let meta = cache.get_or_request_atlas(&meta_key)?;
        let rect = *meta.frames.get(frame_idx)?;
        (meta.w, rect)
    };
    if fw <= 0 || fh <= 0 {
        return None;
    }
    let tex = cache.get_or_request_texture(&tex_key)?;
    let tex_scale = if meta_w > 0 && tex.width >= meta_w * 2 {
        (tex.width / meta_w).max(1) as i32
    } else {
        1
    };
    fb.blit_region_mono(
        &tex.pixels,
        tex.width,
        tex.height,
        fx * tex_scale,
        fy * tex_scale,
        fw * tex_scale,
        fh * tex_scale,
        dst_x,
        dst_y,
        fw * scale,
        fh * scale,
        tint_rgb,
    );
    Some((fw * scale, fh * scale))
}

/// Blit an atlas frame scaled to fit inside `(max_w, max_h)` while preserving aspect ratio (centered).
#[allow(clippy::too_many_arguments)]
pub fn blit_atlas_frame_fit(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    category: &str,
    base: &str,
    frame_idx: usize,
    dst_x: i32,
    dst_y: i32,
    max_w: i32,
    max_h: i32,
    hd: bool,
) -> Option<(i32, i32)> {
    let meta_key = atlas_meta_key(category, base);
    let tex_key = atlas_tex_key(category, base, hd);

    let frame = {
        let meta = cache.get_or_request_atlas(&meta_key)?;
        if meta.frames.is_empty() {
            return None;
        }
        let idx = frame_idx.min(meta.frames.len() - 1);
        let mut rect = meta.frames[idx];
        // Some DOS MGO archives (e.g. mgo_035 farmer Ev#83, #121) have trailing `[0,0,0,0]`
        // sentinel slots that scripts still index into; fall back to a valid non-empty frame
        // instead of making the NPC vanish.
        if rect[2] <= 0 || rect[3] <= 0 {
            let valid: Vec<[i32; 4]> = meta
                .frames
                .iter()
                .copied()
                .filter(|r| r[2] > 0 && r[3] > 0)
                .collect();
            if valid.is_empty() {
                return None;
            }
            rect = valid[frame_idx % valid.len()];
        }
        (meta.w, rect)
    };
    let (meta_w, [fx, fy, fw, fh]) = frame;
    if fw <= 0 || fh <= 0 || max_w <= 0 || max_h <= 0 {
        return None;
    }

    let tex = cache.get_or_request_texture(&tex_key)?;
    let tex_scale = if meta_w > 0 && tex.width >= meta_w * 2 {
        (tex.width / meta_w).max(1) as i32
    } else {
        1
    };

    let sx = fx * tex_scale;
    let sy = fy * tex_scale;
    let sw = fw * tex_scale;
    let sh = fh * tex_scale;

    let ratio_w = max_w as f32 / fw as f32;
    let ratio_h = max_h as f32 / fh as f32;
    let fit = ratio_w.min(ratio_h);
    let dw = ((fw as f32 * fit).round() as i32).max(1);
    let dh = ((fh as f32 * fit).round() as i32).max(1);
    let ox = dst_x + (max_w - dw) / 2;
    let oy = dst_y + (max_h - dh) / 2;

    fb.blit_region_scaled(
        &tex.pixels,
        tex.width,
        tex.height,
        sx,
        sy,
        sw,
        sh,
        ox,
        oy,
        dw,
        dh,
        255,
    );
    Some((dw, dh))
}

/// Blit an FBP full-screen image scaled to fill the framebuffer.
pub fn blit_fbp_fullscreen(fb: &mut Framebuffer, cache: &mut AssetCache, fbp_id: u16, hd: bool) -> bool {
    let key = fbp_key(fbp_id, hd);
    let Some(tex) = cache.get_or_request_texture(&key) else {
        return false;
    };
    let w = fb.width as i32;
    let h = fb.height as i32;
    fb.blit_region_scaled(
        &tex.pixels,
        tex.width,
        tex.height,
        0,
        0,
        tex.width as i32,
        tex.height as i32,
        0,
        0,
        w,
        h,
        255,
    );
    true
}

/// Blit an RNG cutscene frame scaled to fill the framebuffer.
/// Uses `f_XXX_1x.webp` directly (original 320x200 DOS cutscenes) unless an AI-remastered `4x`
/// texture is already cached, avoiding redundant 66 MiB procedural 4x WebP files.
pub fn blit_rng_fullscreen(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    rng_id: u16,
    frame_idx: u16,
    hd: bool,
) -> bool {
    let hd_key = rng_frame_key(rng_id, frame_idx, true);
    let base_key = rng_frame_key(rng_id, frame_idx, false);
    let tex = if hd && cache.has_texture(&hd_key) {
        cache.get_texture(&hd_key)
    } else {
        let has_base = cache.get_or_request_texture(&base_key).is_some();
        if has_base {
            cache.get_texture(&base_key)
        } else {
            None
        }
    };
    let Some(tex) = tex else {
        return false;
    };
    let w = fb.width as i32;
    let h = fb.height as i32;
    fb.blit_region_scaled(
        &tex.pixels,
        tex.width,
        tex.height,
        0,
        0,
        tex.width as i32,
        tex.height as i32,
        0,
        0,
        w,
        h,
        255,
    );
    true
}

#[derive(Clone, Debug)]
enum DepthItem {
    Tile {
        sort_y: i32,
        sprite: usize,
        dx: i32,
        dy: i32,
    },
    HdOccluder {
        sort_y: i32,
        page_key: String,
        strip: HdOccluderStrip,
    },
    Sprite {
        sort_y: i32,
        sprite_id: u32,
        frame: usize,
        screen_x: i32,
        screen_y: i32,
        cast_shadow: bool,
        layer: i32,
    },
}

/// Resolve the atlas frame index for an NPC/event sprite matching DOS PAL `PAL_SceneDrawSprites` (#77).
///
/// - Directional sprites (`sprite_frames > 0`: e.g. `1` for 4-direction standing market vendors,
///   `3` for 12-frame walking villagers, `4` for 16-frame animals) map `(dir, frame)` to
///   `(dir & 3) * sprite_frames + frame` whenever `frame < sprite_frames` and `total_frames >= sprite_frames * 4`.
/// - Absolute pose frames (`frame >= sprite_frames`, e.g. Fisherman `Ev#113` frames `4..5`) and
///   non-directional ambient props (`sprite_frames == 0`) index directly by `frame % total_frames`.
pub fn resolve_npc_frame_idx(dir: u32, frame: u32, sprite_frames: usize, total_frames: usize) -> usize {
    if sprite_frames == 3 && (frame as usize) < 4 && total_frames >= 12 {
        // Authentic SDLPAL PAL_SceneDrawSprites 4-phase walk cycle for 3-frame sprites (#5):
        // phase 0 -> 0 (stand), 1 -> 1 (left step), 2 -> 0 (stand), 3 -> 2 (right step).
        let mapped = match frame {
            2 => 0,
            3 => 2,
            f => f as usize,
        };
        ((dir & 3) as usize) * 3 + mapped
    } else if sprite_frames > 0 && (frame as usize) < sprite_frames && total_frames >= sprite_frames * 4 {
        ((dir & 3) as usize) * sprite_frames + (frame as usize)
    } else if total_frames > 0 {
        (frame as usize) % total_frames
    } else {
        frame as usize
    }
}

/// Render RNG cutscene, FBP override, Battle scene, or Isometric Field World (Track-B HD or 1995 GOP) into `fb`.
pub fn render_world(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    state: &GameState,
    world: &WorldRenderState,
    hd: bool,
) {
    fb.clear(Color::BLACK);

    if let Some((rng_id, frame_idx)) = world.active_rng {
        blit_rng_fullscreen(fb, cache, rng_id, frame_idx, hd);
        apply_post_fx(fb, world, hd);
        return;
    }

    if let Some(fbp_id) = world.fbp_override {
        blit_fbp_fullscreen(fb, cache, fbp_id, hd);
        apply_post_fx(fb, world, hd);
        return;
    }

    if world.battle.active {
        render_battle(fb, cache, fonts, &world.battle, hd);
        apply_post_fx(fb, world, hd);
        return;
    }

    let scale = if hd { 4 } else { 1 };
    let map_id = world
        .map_id_override
        .filter(|&id| id > 0)
        .or_else(|| {
            state
                .scenes
                .iter()
                .find(|s| s.id == state.scene_id)
                .map(|s| s.map_id)
                .filter(|&id| id > 0)
        })
        .unwrap_or(state.scene_id)
        .max(1);

    let (base_cam_x, base_cam_y) = resolve_world_camera(state, world);
    let view_x = base_cam_x + world.shake_offset.0;
    let view_y = base_cam_y + world.shake_offset.1;

    let mut depth_queue: Vec<DepthItem> = Vec::with_capacity(512);
    let gop_base = format!("gop_{:03}", map_id);

    // 1. Track-B HD Whole-Map Streaming (when HD mode is enabled and `maps_hd/map_XXX/meta.json` exists)
    let mut hd_fully_covered = false;
    let mut hd_used = false;
    let mut hd_meta_pending = false;
    if hd {
        let meta_key = hd_map_meta_key(map_id);
        let hd_snapshot = cache.get_or_request_hd_map(&meta_key).map(|meta| {
            let visible_rooms: Vec<(String, i32, i32, i32, i32)> = meta
                .rooms
                .iter()
                .filter(|rm| {
                    rm.x + rm.w >= view_x - 8
                        && rm.x <= view_x + 328
                        && rm.y + rm.h >= view_y - 8
                        && rm.y <= view_y + 208
                })
                .map(|rm| (hd_map_asset_key(map_id, &rm.file), rm.x, rm.y, rm.w, rm.h))
                .collect();

            let visible_strips: Vec<(String, HdOccluderStrip)> = meta
                .strips
                .iter()
                .filter(|st| {
                    st.dx + st.dw >= view_x - 4
                        && st.dx <= view_x + 324
                        && st.dy + st.dh >= view_y - 4
                        && st.dy <= view_y + 204
                })
                .filter_map(|st| {
                    meta.occluder_pages
                        .get(st.page as usize)
                        .map(|pf| (hd_map_asset_key(map_id, pf), *st))
                })
                .collect();
            (visible_rooms, visible_strips)
        });

        if let Some((visible_rooms, visible_strips)) = hd_snapshot {
            // Request all visible room chunks & spatial occluder pages (only intersecting tiles!)
            let mut all_rooms_ready = !visible_rooms.is_empty();
            for (rkey, _, _, _, _) in &visible_rooms {
                if cache.get_or_request_texture(rkey).is_none() {
                    all_rooms_ready = false;
                }
            }
            let mut seen_pages: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for (pkey, _) in &visible_strips {
                if seen_pages.insert(pkey.as_str()) {
                    let _ = cache.get_or_request_texture(pkey);
                }
            }

            if all_rooms_ready {
                hd_fully_covered = true;
            }

            // Render GOP ground tiles underneath HD rooms whenever any part of the viewport
            // extends past `maps_hd` room bounds (or rooms have transparent edge cutouts, e.g.
            // Map 2 boat ride to Fairy Island past x=1432) so no black bars ever appear.
            let any_room_ready = visible_rooms
                .iter()
                .any(|(rkey, _, _, _, _)| cache.get_texture(rkey).is_some());
            if any_room_ready || visible_rooms.is_empty() {
                render_gop_tiles(
                    fb,
                    cache,
                    map_id,
                    &gop_base,
                    view_x,
                    view_y,
                    scale,
                    hd,
                    &mut depth_queue,
                    false,
                );
            }

            // Blit all ready HD ground chunks at exact 1:1 4x integer pixels (0px seam tearing!)
            for (rkey, rx, ry, rw, rh) in &visible_rooms {
                if let Some(tex) = cache.get_texture(rkey) {
                    let dx = (*rx - view_x) * scale;
                    let dy = (*ry - view_y) * scale;
                    let dw = *rw * scale;
                    let dh = *rh * scale;
                    fb.blit_region_scaled(
                        &tex.pixels,
                        tex.width,
                        tex.height,
                        0,
                        0,
                        tex.width as i32,
                        tex.height as i32,
                        dx,
                        dy,
                        dw,
                        dh,
                        255,
                    );
                    hd_used = true;
                }
            }

            // Queue Y-sorted Track-B occluder strips (only when HD chunks are drawn)
            if hd_used {
                for (pkey, strip) in visible_strips {
                    depth_queue.push(DepthItem::HdOccluder {
                        sort_y: strip.sort_y,
                        page_key: pkey,
                        strip,
                    });
                }
            } else {
                // Keep HD chunks loading without pulling a 45 MB legacy `gop_*_4x` atlas into LRU
                hd_meta_pending = true;
            }
        } else if !cache.is_missing_permanent(crate::cache::AssetKind::HdMapMeta, &meta_key) {
            // Wait for `meta.json` probe (single lightweight JSON) before fetching legacy `gop_*_4x`
            hd_meta_pending = true;
        }
    }

    // 2. Legacy 1995 GOP Tile Renderer (retro 1x mode, or un-remastered maps 59..221 where meta.json is absent)
    if !hd_fully_covered && !hd_used && !hd_meta_pending {
        render_gop_tiles(fb, cache, map_id, &gop_base, view_x, view_y, scale, hd, &mut depth_queue, true);
    }

    // 3. Optional HD click-to-walk ripple on ground
    if hd {
        if let Some((rx, ry, age)) = world.click_ripple {
            if (0.0..1.0).contains(&age) {
                let cx = (rx - view_x) * scale;
                let cy = (ry - view_y) * scale;
                let rad_x = ((6.0 + age * 10.0) * scale as f32).round() as i32;
                let rad_y = ((3.0 + age * 5.0) * scale as f32).round() as i32;
                let alpha = ((1.0 - age) * 204.0).round().clamp(0.0, 255.0) as u8;
                fb.stroke_ellipse(cx, cy, rad_x, rad_y, 2, Color::rgba(130, 235, 210, alpha));
            }
        }
    }

    // 4. Collect Y-sorted sprites (party + scene NPCs) via pal-core
    let sprites: Vec<DrawSprite> = if world.ev_end < world.ev_start {
        // Empty scene event range (e.g. Scene 164): render party only, never flood all events.
        build_field_draw_list(state, u16::MAX, 0)
    } else {
        let ev_end = if world.ev_end > 0 {
            world.ev_end
        } else {
            state.events.len().saturating_sub(1) as u16
        };
        build_field_draw_list(state, world.ev_start, ev_end)
    };
    for sp in sprites {
        if sp.sprite_id == 0 {
            continue;
        }
        if sp.world_x < view_x - 96
            || sp.world_x > view_x + 416
            || sp.world_y < view_y - 64
            || sp.world_y > view_y + 320
        {
            continue;
        }
        let mgo_base = format!("mgo_{:03}", sp.sprite_id);
        let meta_key = atlas_meta_key("mgo", &mgo_base);
        let total_frames = cache
            .get_or_request_atlas(&meta_key)
            .map(|m| m.frames.len())
            .unwrap_or(0);
        let _ = cache.get_or_request_texture(&atlas_tex_key("mgo", &mgo_base, hd));

        let is_party_like = sp.kind == 0 || sp.kind == 2;
        let frame_idx = if is_party_like {
            if sp.frame >= 3 && total_frames > 0 {
                (sp.frame as usize).min(total_frames - 1)
            } else if total_frames >= 12 {
                ((sp.dir & 3) * 3 + (sp.frame % 3)) as usize
            } else if total_frames > 0 {
                (sp.frame as usize) % total_frames
            } else {
                sp.frame as usize
            }
        } else {
            let ev_sf = state
                .events
                .get((sp.id as usize).saturating_sub(1))
                .map(|e| e.sprite_frames as usize)
                .unwrap_or(0);
            resolve_npc_frame_idx(sp.dir, sp.frame, ev_sf, total_frames)
        };

        let cast_shadow = is_party_like || (sp.layer >= 0 && total_frames >= 3);
        // Symmetric Y-sort baseline matching DOS PAL PAL_AddSpriteToDrawList (#53):
        // party (layer=0) and events share `world_y + layer * 8 + 8` without NPC +1 bias.
        let sort_y = sp.world_y + sp.layer * 8 + 8;

        depth_queue.push(DepthItem::Sprite {
            sort_y,
            sprite_id: sp.sprite_id,
            frame: frame_idx,
            screen_x: (sp.world_x - view_x) * scale,
            screen_y: (sp.world_y - view_y) * scale,
            cast_shadow,
            layer: sp.layer,
        });
    }

    // Painter's algorithm Y-sort
    depth_queue.sort_by_key(|item| match item {
        DepthItem::Tile { sort_y, .. } => (*sort_y, 0),
        DepthItem::HdOccluder { sort_y, .. } => (*sort_y, 0),
        DepthItem::Sprite { sort_y, .. } => (*sort_y, 1),
    });

    for item in depth_queue {
        match item {
            DepthItem::Tile {
                sprite, dx, dy, ..
            } => {
                blit_atlas_frame(fb, cache, "gop", &gop_base, sprite, dx, dy, scale, hd);
            }
            DepthItem::HdOccluder {
                page_key, strip, ..
            } => {
                if let Some(tex) = cache.get_texture(&page_key) {
                    let dx = (strip.dx - view_x) * scale;
                    let dy = (strip.dy - view_y) * scale;
                    let dw = strip.dw * scale;
                    let dh = strip.dh * scale;
                    fb.blit_region_scaled(
                        &tex.pixels,
                        tex.width,
                        tex.height,
                        strip.sx,
                        strip.sy,
                        strip.sw,
                        strip.sh,
                        dx,
                        dy,
                        dw,
                        dh,
                        255,
                    );
                }
            }
            DepthItem::Sprite {
                sprite_id,
                frame,
                screen_x,
                screen_y,
                cast_shadow,
                layer,
                ..
            } => {
                let mgo_base = format!("mgo_{:03}", sprite_id);
                let meta_key = atlas_meta_key("mgo", &mgo_base);
                let (fw, fh, shadow_stripped) = cache
                    .get_atlas(&meta_key)
                    .map(|m| {
                        let (w, h) = if m.frames.is_empty() {
                            (24, 40)
                        } else {
                            let f = m.frames[frame.min(m.frames.len() - 1)];
                            (f[2], f[3])
                        };
                        (w, h, m.shadow_stripped)
                    })
                    .unwrap_or((24, 40, false));
                if hd && cast_shadow && shadow_stripped {
                    let shadow_w = ((fw as f32) * 0.36).clamp(7.0, 13.0);
                    let rx = (shadow_w * scale as f32).round() as i32;
                    let ry = (shadow_w * 0.42 * scale as f32).round().max(2.0) as i32;
                    fb.draw_soft_ellipse_shadow(screen_x, screen_y + 3 * scale, rx, ry, 118);
                }
                let anchor_pad = if layer < 0 { 5 } else { 6 };
                let dx = screen_x - (fw * scale) / 2;
                let dy = screen_y - (fh - anchor_pad) * scale;
                blit_atlas_frame(fb, cache, "mgo", &mgo_base, frame, dx, dy, scale, hd);
            }
        }
    }

    apply_post_fx(fb, world, hd);
}

#[allow(clippy::too_many_arguments)]
fn render_gop_tiles(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    map_id: u16,
    gop_base: &str,
    view_x: i32,
    view_y: i32,
    scale: i32,
    hd: bool,
    depth_queue: &mut Vec<DepthItem>,
    queue_elevated: bool,
) {
    let map_key = map_bin_key(map_id);
    let gop_meta_key = atlas_meta_key("gop", gop_base);
    let gop_tex_key_hd = atlas_tex_key("gop", gop_base, true);
    let gop_tex_key_1x = atlas_tex_key("gop", gop_base, false);
    // In HD mode, prefer `gop_XXX_4x.webp`, but automatically fall back to `gop_XXX_1x.webp` if
    // the 4x GOP atlas is permanently missing (#178/#189).
    let gop_tex_key = if hd
        && !cache.is_missing_permanent(crate::cache::AssetKind::Texture, &gop_tex_key_hd)
    {
        gop_tex_key_hd
    } else {
        gop_tex_key_1x
    };

    let map_bytes_opt = cache.get_or_request_map(&map_key).map(|s| s.to_vec());
    let _ = cache.get_or_request_atlas(&gop_meta_key);
    let _ = cache.get_or_request_texture(&gop_tex_key);

    if let Some(map_bytes) = map_bytes_opt {
        let gop_meta = cache.get_atlas(&gop_meta_key);
        let gop_tex = cache.get_texture(&gop_tex_key);
        let gop_tex_scale = match (gop_meta, gop_tex) {
            (Some(meta), Some(tex)) if meta.w > 0 && tex.width >= meta.w * 2 => {
                (tex.width / meta.w).max(1) as i32
            }
            _ => 1,
        };
        let blit_gop = |fb: &mut Framebuffer, sprite_idx: usize, dx: i32, dy: i32| {
            let (Some(meta), Some(tex)) = (gop_meta, gop_tex) else {
                return;
            };
            if meta.frames.is_empty() {
                return;
            }
            let [fx, fy, fw, fh] = meta.frames[sprite_idx.min(meta.frames.len() - 1)];
            if fw <= 0 || fh <= 0 {
                return;
            }
            fb.blit_region_scaled(
                &tex.pixels,
                tex.width,
                tex.height,
                fx * gop_tex_scale,
                fy * gop_tex_scale,
                fw * gop_tex_scale,
                fh * gop_tex_scale,
                dx,
                dy,
                fw * scale,
                fh * scale,
                255,
            );
        };

        let min_tx = ((view_x - 32) / 32).clamp(0, 63);
        let max_tx = ((view_x + 352) / 32).clamp(0, 63);
        let min_ty = ((view_y - 32) / 16).clamp(0, 127);
        let max_ty = ((view_y + 240) / 16).clamp(0, 127);

        for ty in min_ty..=max_ty {
            for tx in min_tx..=max_tx {
                for h in 0..2 {
                    let off = (((ty * 64 + tx) * 2 + h) * 4) as usize;
                    if off + 3 >= map_bytes.len() {
                        continue;
                    }
                    let b0 = map_bytes[off] as usize;
                    let b1 = map_bytes[off + 1] as usize;
                    let b2 = map_bytes[off + 2] as usize;
                    let b3 = map_bytes[off + 3] as usize;

                    let btm_sprite = b0 | ((b1 & 0x10) << 4);
                    let btm_layer = (b1 & 0x0f) as i32;
                    let raw_top = b2 | ((b3 & 0x10) << 4);
                    let top_layer = (b3 & 0x0f) as i32;

                    let world_tx = tx * 32 + h * 16 - 16;
                    let world_ty = ty * 16 + h * 8 - 8;
                    let dx = (world_tx - view_x) * scale;
                    let dy = (world_ty - view_y) * scale;

                    if btm_layer == 0 {
                        blit_gop(fb, btm_sprite, dx, dy);
                    } else if queue_elevated {
                        depth_queue.push(DepthItem::Tile {
                            sort_y: ty * 16 + h * 8 + 7 + btm_layer * 8,
                            sprite: btm_sprite,
                            dx,
                            dy,
                        });
                    }

                    if raw_top > 0 {
                        let top_sprite = raw_top - 1;
                        if top_layer == 0 {
                            blit_gop(fb, top_sprite, dx, dy);
                        } else if queue_elevated {
                            depth_queue.push(DepthItem::Tile {
                                sort_y: ty * 16 + h * 8 + 7 + top_layer * 8,
                                sprite: top_sprite,
                                dx,
                                dy,
                            });
                        }
                    }
                }
            }
        }
    }
}

/// Render authentic 1995 DOS PAL battle backdrop + ABC enemies + F heroes + fire VFX + floating damage text.
pub fn render_battle(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    battle: &BattleVisuals,
    hd: bool,
) {
    let scale = if hd { 4 } else { 1 };
    let (sh_x, sh_y) = (battle.shake_offset.0 * scale, battle.shake_offset.1 * scale);
    if !blit_fbp_fullscreen(fb, cache, battle.battlefield_fbp, hd) {
        // Fallback atmospheric battle gradient if FBP not yet uploaded
        fb.fill_rect_vgrad(
            0,
            0,
            fb.width as i32,
            fb.height as i32,
            Color::rgba(36, 48, 62, 255),
            Color::rgba(18, 22, 28, 255),
        );
    }

    // Subtle dramatic backdrop dimming during spell casting (authentic SDLPAL magic mood)
    if battle.dim_bg {
        fb.fill_rect(
            0,
            0,
            fb.width as i32,
            fb.height as i32,
            Color::rgba(6, 8, 18, 108),
        );
    }

    // Draw enemies (top-left diagonal formation, 1995 DOS PAL style)
    for (idx, enemy) in battle.enemies.iter().enumerate() {
        if !enemy.alive && enemy.fade_out == 0 {
            continue;
        }
        let alpha = if enemy.alive { 255 } else { enemy.fade_out };
        let base = format!("abc_{:03}", enemy.sprite_id);
        let meta_key = atlas_meta_key("abc", &base);
        let _ = cache.get_or_request_atlas(&meta_key);
        let (fw, fh) = cache
            .get_atlas(&meta_key)
            .and_then(|m| {
                m.frames
                    .get(enemy.frame.min(m.frames.len().saturating_sub(1)))
                    .map(|f| (f[2], f[3]))
            })
            .unwrap_or((48, 48));
        let dx = enemy.x * scale - (fw * scale) / 2 + sh_x;
        // Clamp tall boss sprites (Snake Demon, Fox Demon, Master Zhixiu) so heads/weapons are
        // never clipped off above the top viewport edge (#180).
        let dy = (enemy.y * scale - fh * scale + sh_y).max(2 * scale);
        let foot_y = dy + fh * scale;
        if hd && enemy.alive {
            fb.draw_soft_ellipse_shadow(
                enemy.x * scale + sh_x,
                foot_y - 4 * scale,
                16 * scale,
                6 * scale,
                88,
            );
        }
        blit_atlas_frame_alpha(fb, cache, "abc", &base, enemy.frame, dx, dy, scale, hd, enemy.flash, alpha);

        // Compact status badges above alive enemies (poison / sleep / silence / etc.)
        if enemy.alive && !enemy.statuses.is_empty() {
            let mut bx = enemy.x * scale - ((enemy.statuses.len() as i32) * 7 * scale) + sh_x;
            let by = (dy - 12 * scale).max(2 * scale);
            for &st in enemy.statuses.iter().take(3) {
                let (tag, col) = match st {
                    99 => ("毒", Color::rgba(92, 232, 112, 255)),
                    0 => ("乱", Color::rgba(232, 130, 255, 255)),
                    1 => ("定", Color::rgba(255, 196, 86, 255)),
                    2 => ("眠", Color::rgba(130, 196, 255, 255)),
                    3 => ("封", Color::rgba(255, 118, 118, 255)),
                    _ => continue,
                };
                fb.fill_rect(bx - scale, by - scale, 11 * scale, 10 * scale, Color::rgba(12, 12, 16, 185));
                fonts.draw_text_outlined(fb, bx, by, tag, (8 * scale) as u16, col, Color::BLACK);
                bx += 13 * scale;
            }
        }

        // Draw target cursor arrow above selected enemy ONLY while actively prompting for a target
        // (#172), clamped cleanly below the top target banner (y=6..36).
        if enemy.alive && battle.prompting && battle.selected_enemy == Some(idx) {
            let cx = enemy.x * scale + sh_x;
            let cy = (dy - 8 * scale).max(38 * scale);
            let sz = 5 * scale;
            for row in 0..sz {
                let half = sz - row;
                fb.fill_rect(cx - half, cy + row, half * 2 + 1, 1, Color::GOLD_HI);
            }
            fb.stroke_rect(cx - sz - 1, cy - 1, sz * 2 + 3, sz + 2, 1, Color::BLACK);
        }
    }

    // Draw party heroes (bottom-right diagonal formation)
    for (idx, hero) in battle.heroes.iter().enumerate() {
        let base = format!("f_{:03}", hero.sprite_id);
        let meta_key = atlas_meta_key("f", &base);
        let _ = cache.get_or_request_atlas(&meta_key);
        let (fw, fh) = cache
            .get_atlas(&meta_key)
            .and_then(|m| {
                m.frames
                    .get(hero.frame.min(m.frames.len().saturating_sub(1)))
                    .map(|f| (f[2], f[3]))
            })
            .unwrap_or((40, 52));
        let dx = hero.x * scale - (fw * scale) / 2 + sh_x;
        let dy = hero.y * scale - fh * scale + sh_y;
        // F.MKF hero frames carry ~12px transparent bottom padding; anchor shadow & active-turn bar
        // right beneath the boots instead of floating below (#190).
        let boot_y = (hero.y - 12) * scale + sh_y;
        if hd {
            fb.draw_soft_ellipse_shadow(
                hero.x * scale + sh_x,
                boot_y,
                13 * scale,
                5 * scale,
                88,
            );
        }
        blit_atlas_frame_ex(fb, cache, "f", &base, hero.frame, dx, dy, scale, hd, hero.flash);

        // Subtle golden underline under active hero during command prompt
        if battle.prompting && idx == battle.active_hero {
            fb.fill_rect(
                hero.x * scale - 16 * scale + sh_x,
                boot_y + 2 * scale,
                32 * scale,
                2 * scale,
                Color::GOLD_HI,
            );
        }

        // Green/gold target arrow above selected ally (healing/revive spell targeting)
        if battle.selected_ally == Some(idx) {
            let cx = hero.x * scale + sh_x;
            let cy = (dy - 12 * scale).max(4 * scale);
            let sz = 5 * scale;
            for row in 0..sz {
                let half = sz - row;
                fb.fill_rect(
                    cx - half,
                    cy + row,
                    half * 2 + 1,
                    1,
                    Color::rgba(110, 248, 152, 255),
                );
            }
            fb.stroke_rect(cx - sz - 1, cy - 1, sz * 2 + 3, sz + 2, 1, Color::BLACK);
        }
    }

    // Draw active spell/attack fire VFX overlay (`graphics/fire/fire_XXX`)
    if let Some(fx) = &battle.active_effect {
        if fx.is_summon {
            // Dramatic dark sky veil + golden flash pulse for Summon Spirits (酒神/剑神/雷神/山神/雪妖)
            fb.fill_rect(0, 0, fb.width as i32, fb.height as i32, Color::rgba(4, 6, 18, 92));
            if fx.frame == 7 || fx.frame == 8 {
                fb.fill_rect(0, 0, fb.width as i32, fb.height as i32, Color::rgba(255, 240, 190, 68));
            }
        }
        let base = format!("fire_{:03}", fx.fire_id);
        let meta_key = atlas_meta_key("fire", &base);
        let _ = cache.get_or_request_atlas(&meta_key);
        let (actual_frame, fw, fh) = cache
            .get_atlas(&meta_key)
            .and_then(|m| {
                let n = m.frames.len();
                if n == 0 {
                    None
                } else {
                    let idx = ((fx.frame * n) / 14).min(n - 1);
                    let f = m.frames[idx];
                    Some((idx, f[2], f[3]))
                }
            })
            .unwrap_or((fx.frame, 64, 64));

        if !fx.is_summon && fx.target_all && fw < 180 {
            if fx.target_enemy {
                for e in battle.enemies.iter().filter(|e| e.alive) {
                    let dx = e.x * scale - (fw * scale) / 2 + sh_x;
                    let dy = (e.y - 16) * scale - (fh * scale) / 2 + sh_y;
                    blit_atlas_frame_alpha(fb, cache, "fire", &base, actual_frame, dx, dy, scale, hd, false, fx.alpha);
                }
            } else {
                for h in &battle.heroes {
                    let dx = h.x * scale - (fw * scale) / 2 + sh_x;
                    let dy = (h.y - 20) * scale - (fh * scale) / 2 + sh_y;
                    blit_atlas_frame_alpha(fb, cache, "fire", &base, actual_frame, dx, dy, scale, hd, false, fx.alpha);
                }
            }
        } else {
            let (tx, ty) = if fx.target_all {
                (160, 100)
            } else {
                (fx.x, fx.y - 14)
            };
            let dx = tx * scale - (fw * scale) / 2 + sh_x;
            let dy = ty * scale - (fh * scale) / 2 + sh_y;
            blit_atlas_frame_alpha(fb, cache, "fire", &base, actual_frame, dx, dy, scale, hd, false, fx.alpha);
        }
    }

    // Draw floating damage/heal numbers
    let text_px = if hd { 32u16 } else { 10u16 };
    for ft in &battle.float_texts {
        fonts.draw_text_centered(
            fb,
            ft.x * scale,
            ft.y * scale,
            &ft.text,
            text_px,
            ft.color,
            Some(Color::BLACK),
        );
    }
}

fn apply_post_fx(fb: &mut Framebuffer, world: &WorldRenderState, hd: bool) {
    if world.wave_amplitude > 0.01 {
        let scale = if hd { 4.0 } else { 1.0 };
        fb.apply_horizontal_wave(
            world.wave_amplitude * scale,
            (world.frame_count as f32) * 0.15,
            0.08 / scale,
        );
    }
    if world.night_palette {
        fb.fill_rect(
            0,
            0,
            fb.width as i32,
            fb.height as i32,
            Color::rgba(18, 32, 68, 72),
        );
    }
    if world.fade_alpha > 0.001 {
        fb.apply_fade(world.fade_color, world.fade_alpha);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_night_palette_applies_in_both_hd_and_retro() {
        let mut fb_retro = Framebuffer::new(320, 200);
        fb_retro.clear(Color::rgba(200, 180, 140, 255));
        let mut fb_hd = Framebuffer::new(1280, 800);
        fb_hd.clear(Color::rgba(200, 180, 140, 255));

        let world = WorldRenderState {
            night_palette: true,
            ..Default::default()
        };
        apply_post_fx(&mut fb_retro, &world, false);
        apply_post_fx(&mut fb_hd, &world, true);

        assert_ne!(
            &fb_retro.pixels[0..4],
            &[200, 180, 140, 255],
            "Retro mode must apply night_palette atmospheric tint"
        );
        assert_eq!(
            &fb_retro.pixels[0..4],
            &fb_hd.pixels[0..4],
            "Retro and HD night_palette tint must match identically"
        );
    }

    #[test]
    fn test_screen_wave_hd_matches_retro_spatial_freq_and_amplitude() {
        let mut fb_retro = Framebuffer::new(320, 200);
        fb_retro.clear(Color::BLACK);
        fb_retro.fill_rect(160, 0, 1, 200, Color::WHITE);

        let mut fb_hd = Framebuffer::new(1280, 800);
        fb_hd.clear(Color::BLACK);
        fb_hd.fill_rect(640, 0, 4, 800, Color::WHITE);

        let world = WorldRenderState {
            wave_amplitude: 5.0,
            frame_count: 3,
            ..Default::default()
        };
        apply_post_fx(&mut fb_retro, &world, false);
        apply_post_fx(&mut fb_hd, &world, true);

        let first_white_x = |fb: &Framebuffer, y: u32| -> i32 {
            let row = (y as usize) * (fb.width as usize) * 4;
            for x in 0..(fb.width as usize) {
                if fb.pixels[row + x * 4] > 200 {
                    return x as i32;
                }
            }
            panic!("No white pixel found on row {}", y);
        };

        let peak_hd_shift = (0..800u32)
            .map(|y| (first_white_x(&fb_hd, y) - 640).abs())
            .max()
            .unwrap();
        assert!(
            peak_hd_shift >= 16,
            "HD wave amplitude must scale by 4x (expected >=16px peak, got {}px)",
            peak_hd_shift
        );

        for y_r in [10u32, 30, 50, 75, 100, 130, 160, 185] {
            let shift_retro = (first_white_x(&fb_retro, y_r) - 160) as f32;
            let shift_hd_norm = (first_white_x(&fb_hd, y_r * 4) - 640) as f32 / 4.0;
            assert!(
                (shift_hd_norm - shift_retro).abs() <= 1.0,
                "Row y_r={} wave mismatch: HD/4={:.2} vs Retro={:.2}",
                y_r,
                shift_hd_norm,
                shift_retro
            );
        }
    }

    #[test]
    fn test_resolve_npc_frame_idx_directional_and_ambient() {
        // 4-direction standing market vendors (sprite_frames = 1, total_frames = 4, #77)
        assert_eq!(resolve_npc_frame_idx(0, 0, 1, 4), 0);
        assert_eq!(resolve_npc_frame_idx(1, 0, 1, 4), 1);
        assert_eq!(resolve_npc_frame_idx(2, 0, 1, 4), 2);
        assert_eq!(resolve_npc_frame_idx(3, 0, 1, 4), 3);

        // 12-frame walking villagers (sprite_frames = 3, total_frames = 12):
        // SDLPAL 4-phase cycle: 0 -> 0 (stand), 1 -> 1 (left), 2 -> 0 (stand), 3 -> 2 (right).
        assert_eq!(resolve_npc_frame_idx(2, 0, 3, 12), 6);
        assert_eq!(resolve_npc_frame_idx(2, 1, 3, 12), 7);
        assert_eq!(resolve_npc_frame_idx(2, 2, 3, 12), 6);
        assert_eq!(resolve_npc_frame_idx(3, 3, 3, 12), 11);

        // 16-frame 4-dir animals (sprite_frames = 4, total_frames = 16)
        assert_eq!(resolve_npc_frame_idx(2, 3, 4, 16), 11);

        // Fisherman Ev#113 absolute pose frames (sprite_frames = 1, frame = 4..5, total_frames = 6)
        assert_eq!(resolve_npc_frame_idx(2, 4, 1, 6), 4);
        assert_eq!(resolve_npc_frame_idx(2, 5, 1, 6), 5);

        // Non-directional ambient props (sprite_frames = 0, total_frames = 16)
        assert_eq!(resolve_npc_frame_idx(3, 2, 0, 16), 2);
    }

    #[test]
    fn test_blit_atlas_frame_empty_sentinel_falls_back_to_valid_frame() {
        let mut fb = Framebuffer::new(32, 32);
        fb.clear(Color::BLACK);
        let mut cache = AssetCache::new(4 * 1024 * 1024);
        assert!(cache.upload_atlas_json(
            atlas_meta_key("mgo", "mgo_035"),
            r#"{"w":8,"h":8,"frames":[[0,0,4,4],[4,0,4,4],[0,0,0,0],[0,0,0,0]]}"#,
        ));
        let mut pixels = vec![0u8; 8 * 8 * 4];
        for px in pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&[200, 120, 60, 255]);
        }
        cache.upload_texture(atlas_tex_key("mgo", "mgo_035", false), 8, 8, pixels, false);

        // Indexing into empty sentinel slot 2 must fall back instead of returning None.
        let res = blit_atlas_frame(&mut fb, &mut cache, "mgo", "mgo_035", 2, 4, 4, 1, false);
        assert_eq!(res, Some((4, 4)));
        let non_black = fb.pixels.chunks_exact(4).filter(|p| p[0] > 0).count();
        assert_eq!(non_black, 16);
    }
}
