//! Pure-Rust 1995 DOS PAL UI Framebuffer Renderer (SDLPAL / `DATA.MKF` chunk 9 `gpSpriteUI` parity).
//!
//! Draws all UI views into `ui_layer: &mut Framebuffer` using logical `320x200` coordinates
//! scaled by `s = if hd { 4 } else { 1 }`, using authentic 1995 `spriteui` 9-slice ornate scroll
//! boxes, single-line boxes, 6px drop shadows, `PLAYERINFOBOX`, 4-way Bagua battle icons, `ITEMBOX`,
//! and bitmap digits — while preserving 4x HD crisp TTF typography, 3-line paginated dialogue, and
//! live 繁體 / 简体 / English localization.

mod battle;
mod dialog;
mod grids;
mod menus;
mod overlays;

pub use dialog::layout_dialog_pages;
pub use overlays::{draw_ai_thought_overlay, draw_debug_watermark, should_draw_ai_thought_overlay};
use battle::{draw_battle_hud, draw_battle_party_strip, draw_battle_victory};
use dialog::draw_dialog;
use grids::{draw_item_grid, draw_magic_grid, draw_replay_chapters, draw_save_load_slots, draw_shop};
use menus::{
    draw_equip_fbp001, draw_item_submenu, draw_remake_submenu, draw_role_select, draw_root_menu,
    draw_status_fbp000, draw_system_submenu,
};
use overlays::{draw_replay_finished, draw_title, draw_title_options};

use crate::input::{HitRect, HitTag};
use crate::state::{BattleSubPhase, UiState, UiView};
use pal_core::state::GameState;
use pal_render::cache::AssetCache;
use pal_render::font::FontEngine;
use pal_render::framebuffer::{Color, Framebuffer};
use pal_render::world::{
    atlas_frame_size, blit_atlas_frame, blit_atlas_frame_fit, blit_atlas_frame_mono,
    blit_atlas_frame_shadow, BattleVisuals,
};

// Authentic DATA.MKF chunk 9 (`gpSpriteUI`) frame indices (matching SDLPAL ui.h / uibattle.h)
const SPR_PLAYERINFOBOX: usize = 18;
const SPR_NUM_YELLOW_0: usize = 19;
const SPR_NUM_BLUE_0: usize = 29;
const SPR_SLASH: usize = 39;
const SPR_BATTLE_ATTACK: usize = 40;
const SPR_BATTLE_MAGIC: usize = 41;
const SPR_BATTLE_COOP: usize = 42;
const SPR_BATTLE_MISC: usize = 43;
const SPR_SINGLE_LEFT: usize = 44;
const SPR_SINGLE_MID: usize = 45;
const SPR_SINGLE_RIGHT: usize = 46;
const SPR_PLAYERFACE_FIRST: usize = 48;
const SPR_NUM_CYAN_0: usize = 56;
const SPR_CURSOR_DOWN: usize = 69;
const SPR_ITEMBOX: usize = 70;

const CONFIRMED_GREEN: Color = Color::rgba(116, 232, 148, 255);
const SHADOW_ALPHA: u8 = 136;

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum NumColor {
    Yellow,
    Blue,
    Cyan,
}

#[inline]
fn tr<'a>(ui: &UiState, hant: &'a str, hans: &'a str, en: &'a str) -> &'a str {
    match ui.lang {
        pal_core::Language::En => en,
        pal_core::Language::ZhHant => hant,
        pal_core::Language::ZhHans => hans,
    }
}

/// Draw an authentic SDLPAL 9-slice ornate box (`PAL_CreateBoxWithShadow`, style 0 = scroll, style 1 = crimson brocade).
/// Returns logical `(total_w, total_h)`. Falls back cleanly to `draw_bevel_box` if `spriteui` is still streaming.
#[allow(clippy::too_many_arguments)]
fn draw_pal_box(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    x: i32,
    y: i32,
    n_rows: i32,
    n_cols: i32,
    style: usize,
    s: i32,
    hd: bool,
) -> (i32, i32) {
    let base = style * 9;
    let mut sizes = [[(16, 18); 3]; 3];
    let mut ready = true;
    for r in 0..3 {
        for c in 0..3 {
            if let Some(sz) = atlas_frame_size(cache, "ui", "spriteui", base + r * 3 + c) {
                sizes[r][c] = sz;
            } else {
                ready = false;
            }
        }
    }
    let total_w = sizes[0][0].0 + sizes[0][1].0 * n_cols + sizes[0][2].0;
    let total_h = sizes[0][0].1 + sizes[1][0].1 * n_rows + sizes[2][0].1;
    if !ready {
        fb.draw_bevel_box(x * s, y * s, total_w * s, total_h * s, s.max(1));
        return (total_w, total_h);
    }

    let rows = n_rows + 2;
    let cols = n_cols + 2;
    // Pass 1: 6px drop shadow
    let mut cy = y;
    for i in 0..rows {
        let m = if i == 0 { 0 } else if i == rows - 1 { 2 } else { 1 };
        let mut cx = x;
        for j in 0..cols {
            let n = if j == 0 { 0 } else if j == cols - 1 { 2 } else { 1 };
            let fidx = base + m * 3 + n;
            blit_atlas_frame_shadow(
                fb,
                cache,
                "ui",
                "spriteui",
                fidx,
                (cx + 6) * s,
                (cy + 6) * s,
                s,
                hd,
                SHADOW_ALPHA,
            );
            cx += sizes[m][n].0;
        }
        cy += sizes[m][0].1;
    }
    // Pass 2: ornate 9-slice foreground
    let mut cy = y;
    for i in 0..rows {
        let m = if i == 0 { 0 } else if i == rows - 1 { 2 } else { 1 };
        let mut cx = x;
        for j in 0..cols {
            let n = if j == 0 { 0 } else if j == cols - 1 { 2 } else { 1 };
            let fidx = base + m * 3 + n;
            blit_atlas_frame(fb, cache, "ui", "spriteui", fidx, cx * s, cy * s, s, hd);
            cx += sizes[m][n].0;
        }
        cy += sizes[m][0].1;
    }
    (total_w, total_h)
}

/// Draw an authentic SDLPAL single-line scroll box (`PAL_CreateSingleLineBoxWithShadow`, frames 44/45/46).
/// Returns logical `(total_w, total_h)`.
#[allow(clippy::too_many_arguments)]
fn draw_pal_single_line_box(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    x: i32,
    y: i32,
    n_len: i32,
    s: i32,
    hd: bool,
) -> (i32, i32) {
    let l_sz = atlas_frame_size(cache, "ui", "spriteui", SPR_SINGLE_LEFT);
    let m_sz = atlas_frame_size(cache, "ui", "spriteui", SPR_SINGLE_MID);
    let r_sz = atlas_frame_size(cache, "ui", "spriteui", SPR_SINGLE_RIGHT);
    let (Some((lw, lh)), Some((mw, _)), Some((rw, _))) = (l_sz, m_sz, r_sz) else {
        let w = 16 + 16 * n_len.max(1);
        fb.draw_bevel_box(x * s, y * s, w * s, 30 * s, s.max(1));
        return (w, 30);
    };
    let total_w = lw + mw * n_len.max(1) + rw;
    // Pass 1: 6px shadow
    let mut cx = x;
    blit_atlas_frame_shadow(fb, cache, "ui", "spriteui", SPR_SINGLE_LEFT, (cx + 6) * s, (y + 6) * s, s, hd, SHADOW_ALPHA);
    cx += lw;
    for _ in 0..n_len.max(1) {
        blit_atlas_frame_shadow(fb, cache, "ui", "spriteui", SPR_SINGLE_MID, (cx + 6) * s, (y + 6) * s, s, hd, SHADOW_ALPHA);
        cx += mw;
    }
    blit_atlas_frame_shadow(fb, cache, "ui", "spriteui", SPR_SINGLE_RIGHT, (cx + 6) * s, (y + 6) * s, s, hd, SHADOW_ALPHA);
    // Pass 2: foreground
    let mut cx = x;
    blit_atlas_frame(fb, cache, "ui", "spriteui", SPR_SINGLE_LEFT, cx * s, y * s, s, hd);
    cx += lw;
    for _ in 0..n_len.max(1) {
        blit_atlas_frame(fb, cache, "ui", "spriteui", SPR_SINGLE_MID, cx * s, y * s, s, hd);
        cx += mw;
    }
    blit_atlas_frame(fb, cache, "ui", "spriteui", SPR_SINGLE_RIGHT, cx * s, y * s, s, hd);
    (total_w, lh)
}

/// Draw authentic 1995 DOS `6x8` bitmap digits (`PAL_DrawNumber`).
#[allow(clippy::too_many_arguments)]
fn draw_pal_number(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    num: u32,
    width_digits: i32,
    x: i32,
    y: i32,
    color: NumColor,
    right_align: bool,
    s: i32,
    hd: bool,
) {
    let base = match color {
        NumColor::Yellow => SPR_NUM_YELLOW_0,
        NumColor::Blue => SPR_NUM_BLUE_0,
        NumColor::Cyan => SPR_NUM_CYAN_0,
    };
    let text = num.to_string();
    let n = text.len() as i32;
    let mut dx = if right_align {
        x + 6 * (width_digits - n)
    } else {
        x
    };
    for b in text.bytes() {
        if b.is_ascii_digit() {
            let d = (b - b'0') as usize;
            blit_atlas_frame(fb, cache, "ui", "spriteui", base + d, dx * s, y * s, s, hd);
        }
        dx += 6;
    }
}

/// Draw authentic SDLPAL `PAL_PlayerInfoBox` (`75x35` card + mini portrait + ATB bar + bitmap HP/MP).
#[allow(clippy::too_many_arguments)]
fn draw_player_info_box(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    x: i32,
    y: i32,
    role_id: u16,
    avatar_rgm: u16,
    hp: u16,
    max_hp: u16,
    mp: u16,
    max_mp: u16,
    atb_pct: i32,
    active: bool,
    s: i32,
    hd: bool,
) {
    if blit_atlas_frame(fb, cache, "ui", "spriteui", SPR_PLAYERINFOBOX, x * s, y * s, s, hd).is_none() {
        fb.draw_bevel_box(x * s, y * s, 75 * s, 35 * s, s.max(1));
    }
    if role_id < 5 {
        let face_idx = SPR_PLAYERFACE_FIRST + (role_id as usize);
        if hp == 0 {
            blit_atlas_frame_mono(
                fb,
                cache,
                "ui",
                "spriteui",
                face_idx,
                (x - 2) * s,
                (y - 4) * s,
                s,
                hd,
                [140, 140, 145],
            );
        } else {
            blit_atlas_frame(fb, cache, "ui", "spriteui", face_idx, (x - 2) * s, (y - 4) * s, s, hd);
        }
    } else {
        // Role #5 (Gai Luojiao) has no mini-face in SPRITEUI (only 0..4 exist); draw her RGM portrait (#187).
        blit_atlas_frame_fit(
            fb,
            cache,
            "ui",
            "rgm",
            avatar_rgm as usize,
            (x + 1) * s,
            (y + 2) * s,
            28 * s,
            30 * s,
            hd,
        );
        if hp == 0 {
            fb.fill_rect((x + 1) * s, (y + 2) * s, 28 * s, 30 * s, Color::rgba(20, 20, 24, 150));
        }
    }

    // ATB bar border + fill (exact SDLPAL uibattle.c:168-204)
    let border_c = Color::rgba(196, 172, 126, 255);
    fb.fill_rect((x + 31) * s, (y + 4) * s, s, 6 * s, border_c);
    fb.fill_rect((x + 70) * s, (y + 4) * s, s, 6 * s, border_c);
    fb.fill_rect((x + 32) * s, (y + 3) * s, 38 * s, s, border_c);
    fb.fill_rect((x + 32) * s, (y + 10) * s, 38 * s, s, border_c);
    if hp > 0 {
        if active || atb_pct >= 100 {
            fb.fill_rect((x + 33) * s, (y + 6) * s, 36 * s, 2 * s, Color::GOLD_HI);
        } else if atb_pct > 0 {
            let fill_w = ((atb_pct.clamp(0, 100) * 36) / 100).max(1);
            fb.fill_rect((x + 33) * s, (y + 5) * s, fill_w * s, 4 * s, Color::rgba(220, 108, 92, 255));
        }
    }

    // Compact outlined numbers for 4-digit HP/MP so digits never collide with portrait or slash (#161).
    if hp >= 1000 || max_hp >= 1000 {
        fonts.draw_text_centered(
            fb,
            (x + 51) * s,
            (y + 13) * s,
            &format!("{hp}/{max_hp}"),
            (7 * s) as u16,
            Color::GOLD_HI,
            Some(Color::BLACK),
        );
    } else {
        blit_atlas_frame(fb, cache, "ui", "spriteui", SPR_SLASH, (x + 49) * s, (y + 14) * s, s, hd);
        draw_pal_number(fb, cache, max_hp as u32, 4, x + 47, y + 16, NumColor::Yellow, true, s, hd);
        draw_pal_number(fb, cache, hp as u32, 4, x + 26, y + 13, NumColor::Yellow, true, s, hd);
    }

    if mp >= 1000 || max_mp >= 1000 {
        fonts.draw_text_centered(
            fb,
            (x + 51) * s,
            (y + 23) * s,
            &format!("{mp}/{max_mp}"),
            (7 * s) as u16,
            Color::CYAN_SPEAKER,
            Some(Color::BLACK),
        );
    } else {
        blit_atlas_frame(fb, cache, "ui", "spriteui", SPR_SLASH, (x + 49) * s, (y + 24) * s, s, hd);
        draw_pal_number(fb, cache, max_mp as u32, 4, x + 47, y + 26, NumColor::Cyan, true, s, hd);
        draw_pal_number(fb, cache, mp as u32, 4, x + 26, y + 23, NumColor::Cyan, true, s, hd);
    }
}

/// Draw the active UI view onto `fb` and update `ui.hit_rects`.
pub fn draw_ui(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    state: &GameState,
    battle: Option<&BattleVisuals>,
    hd: bool,
) {
    ui.hit_rects.clear();
    fonts.set_prefer_latin(ui.lang.is_en());
    let s = if hd { 4 } else { 1 };

    // Always draw the bottom-right `PLAYERINFOBOX` strip & top-center Spell Name Banner
    // throughout the entire battle, EXCEPT hide the bottom strip when a bottom mid-battle
    // dialogue is active so dialogue text & portrait never overlap party cards (#173).
    if let Some(b) = battle.filter(|b| b.active) {
        let hide_bottom_strip = ui.view == UiView::Dialog && !ui.dialog.at_top;
        if !hide_bottom_strip {
            draw_battle_party_strip(fb, cache, fonts, b, s, hd);
        }
        if b.auto_battle {
            let lx = 6;
            let ly = 176;
            let lw = 78;
            let lh = 16;
            fb.fill_rect(lx * s, ly * s, lw * s, lh * s, Color::rgba(28, 22, 12, 210));
            fb.stroke_rect(lx * s, ly * s, lw * s, lh * s, s.max(1), Color::GOLD_HI);
            let txt = tr(ui, "圍攻中 ESC停", "围攻中 ESC停", "AUTO [ESC]");
            fonts.draw_text_centered(
                fb,
                (lx + lw / 2) * s,
                (ly + 3) * s,
                txt,
                (8 * s) as u16,
                Color::GOLD_HI,
                Some(Color::BLACK),
            );
            ui.hit_rects.push(HitRect {
                x: lx,
                y: ly,
                w: lw,
                h: lh,
                tag: HitTag::BattleCancelAuto,
            });
        }
        if ui.toast_text.is_none() {
            if let Some(spell_name) = b.spell_banner.as_deref() {
                let chars = spell_name.chars().count() as i32;
                let n_len = ((chars * 10 + 18) / 16).clamp(4, 14);
                let box_w = 16 + n_len * 16;
                let bx = (320 - box_w) / 2;
                let by = 6;
                draw_pal_single_line_box(fb, cache, bx, by, n_len, s, hd);
                fonts.draw_text_centered(
                    fb,
                    (bx + box_w / 2) * s,
                    (by + 11) * s,
                    spell_name,
                    (10 * s) as u16,
                    Color::GOLD_HI,
                    Some(Color::BLACK),
                );
            }
        }
    }

    match ui.view.clone() {
        UiView::None => {}
        UiView::Title => draw_title(fb, cache, fonts, ui, s, hd),
        UiView::TitleOptions => {
            // Render options modal directly over FBP_060 without Title buttons bleeding through (#161).
            draw_title_options(fb, cache, fonts, ui, s, hd);
        }
        UiView::Dialog => draw_dialog(fb, cache, fonts, ui, s, hd),
        UiView::RootMenu => {
            draw_root_menu(fb, cache, fonts, ui, state, s, hd, true);
        }
        UiView::RoleSelect(_) => {
            draw_root_menu(fb, cache, fonts, ui, state, s, hd, false);
            draw_role_select(fb, cache, fonts, ui, state, s, hd);
        }
        UiView::ItemSubmenu => {
            draw_root_menu(fb, cache, fonts, ui, state, s, hd, false);
            draw_item_submenu(fb, cache, fonts, ui, s, hd);
        }
        UiView::SystemSubmenu => {
            draw_root_menu(fb, cache, fonts, ui, state, s, hd, false);
            draw_system_submenu(fb, cache, fonts, ui, s, hd);
        }
        UiView::RemakeSubmenu => {
            draw_root_menu(fb, cache, fonts, ui, state, s, hd, false);
            draw_remake_submenu(fb, cache, fonts, ui, s, hd);
        }
        UiView::StatusFbp000 => draw_status_fbp000(fb, cache, fonts, ui, state, s, hd),
        UiView::EquipFbp001 => draw_equip_fbp001(fb, cache, fonts, ui, state, s, hd),
        UiView::MagicGrid => draw_magic_grid(fb, cache, fonts, ui, state, s, hd),
        UiView::ItemGrid => draw_item_grid(fb, cache, fonts, ui, state, s, hd),
        UiView::SaveLoadSlots(mode) => draw_save_load_slots(fb, cache, fonts, ui, mode, s, hd),
        UiView::ReplayChapters => draw_replay_chapters(fb, cache, fonts, ui, s, hd),
        UiView::ReplayFinished => draw_replay_finished(fb, cache, fonts, ui, s, hd),
        UiView::Shop => draw_shop(fb, cache, fonts, ui, state, false, s, hd),
        UiView::PawnshopSell => draw_shop(fb, cache, fonts, ui, state, true, s, hd),
        UiView::BattleHud(phase) => draw_battle_hud(fb, cache, fonts, ui, state, battle, phase, s, hd),
        UiView::BattleVictory => draw_battle_victory(fb, cache, fonts, ui, s, hd),
    }

    if let Some(toast) = ui.toast_text.clone() {
        let chars = toast.chars().count() as i32;
        let n_len = ((chars * 10 + 20) / 16).clamp(5, 16);
        let box_w = 16 + n_len * 16;
        let bx = (320 - box_w) / 2;
        // Anchor toast at the bottom in Shop/Pawnshop/Equip/Magic views so it never obscures top headers/rows (#169, #181).
        let by = if matches!(
            ui.view,
            UiView::Shop | UiView::PawnshopSell | UiView::EquipFbp001 | UiView::MagicGrid
        ) {
            164
        } else {
            8
        };
        draw_pal_single_line_box(fb, cache, bx, by, n_len, s, hd);
        fonts.draw_text_centered(
            fb,
            (bx + box_w / 2) * s,
            (by + 11) * s,
            &toast,
            (10 * s) as u16,
            Color::GOLD_HI,
            Some(Color::BLACK),
        );
    }

    if ui.debug_mode {
        draw_debug_watermark(fb, fonts, s);
    }
}
