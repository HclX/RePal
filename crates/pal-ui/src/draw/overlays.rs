//! Title screen, options, replay-finished overlay, AI thought subtitle bar, and debug watermark.

use super::*;

/// Subtle, unmistakable top-right `DEBUG ENABLED` pill rendered whenever `debug_mode` is active
/// (live gameplay or replaying a debug-mode `.repal` bundle, #235).
pub fn draw_debug_watermark(fb: &mut Framebuffer, fonts: &mut FontEngine, s: i32) {
    let label = "DEBUG ENABLED";
    let font_px = (6 * s).max(6) as u16;
    let text_w = fonts.measure_text(label, font_px);
    let pad_x = 4 * s;
    let pad_y = 2 * s;
    let pill_w = text_w + pad_x * 2;
    let pill_h = (6 * s) + pad_y * 2 + s;
    let x = (320 * s - pill_w - 4 * s).max(2 * s);
    let y = 3 * s;
    fb.fill_rect(x, y, pill_w, pill_h, Color::rgba(48, 12, 12, 185));
    let bw = (s / 2).max(1);
    let border = Color::rgba(245, 165, 65, 220);
    fb.fill_rect(x, y, pill_w, bw, border);
    fb.fill_rect(x, y + pill_h - bw, pill_w, bw, border);
    fb.fill_rect(x, y, bw, pill_h, border);
    fb.fill_rect(x + pill_w - bw, y, bw, pill_h, border);
    fonts.draw_text_outlined(
        fb,
        x + pad_x,
        y + pad_y,
        label,
        font_px,
        Color::rgba(255, 210, 110, 245),
        Color::rgba(0, 0, 0, 235),
    );
}

/// Returns `true` when [`draw_ai_thought_overlay`] would draw visible pixels for the current UI state.
/// Used by present-time paths to avoid copying `compositor.fb` when no overlay is visible.
pub fn should_draw_ai_thought_overlay(
    ui: &UiState,
    battle: Option<&BattleVisuals>,
    hd: bool,
) -> bool {
    if !hd || !ui.show_ai_thought || ui.toast_text.is_some() {
        return false;
    }
    if battle.is_some_and(|b| b.active && b.spell_banner.is_some()) {
        return false;
    }
    if matches!(
        ui.view,
        UiView::Title
            | UiView::TitleOptions
            | UiView::RootMenu
            | UiView::RoleSelect(_)
            | UiView::ItemSubmenu
            | UiView::SystemSubmenu
            | UiView::RemakeSubmenu
            | UiView::StatusFbp000
            | UiView::EquipFbp001
            | UiView::MagicGrid
            | UiView::ItemGrid
            | UiView::SaveLoadSlots(_)
            | UiView::Shop
            | UiView::PawnshopSell
            | UiView::ReplayChapters
            | UiView::ReplayFinished
    ) {
        return false;
    }
    if ui.view == UiView::Dialog && ui.dialog.at_top {
        return false;
    }
    ui.ai_thought.as_deref().is_some_and(|t| !t.is_empty())
}

/// Crisp top-bar subtitle displaying the AI agent's current thought (`think <text>`).
///
/// Intentionally **NOT** called by [`draw_ui`] so `compositor.fb` stays structurally thought-free
/// for `golden`, `bugreport`, `end_screenshot.png`, chapter `thumb.png`, and default `shot`.
/// Frontends invoke this only at present time (or for `shot --with-thought`).
///
/// HD-only (`4x`, `1280x800`) so `320x200` Retro mode stays pure 1995 pixel art, wraps and
/// ellipsizes cleanly to at most 2 lines, and dims to ~45% opacity when `ui.ai_thought_dimmed`.
pub fn draw_ai_thought_overlay(
    fb: &mut Framebuffer,
    fonts: &mut FontEngine,
    ui: &UiState,
    battle: Option<&BattleVisuals>,
    hd: bool,
) {
    if !should_draw_ai_thought_overlay(ui, battle, hd) {
        return;
    }
    let Some(thought) = ui.ai_thought.as_deref().filter(|t| !t.is_empty()) else {
        return;
    };
    fonts.set_prefer_latin(ui.lang.is_en());
    let s = 4i32;

    let font_px = (7 * s) as u16; // 28px at 4x HD — razor sharp, compact
    let prefix = "[AI] ";
    let full = format!("{prefix}{thought}");
    // Reserve right ~264 px when `DEBUG ENABLED` watermark is active so thought text never overlaps it (#245).
    let max_w = if ui.debug_mode { 238 * s } else { 304 * s };
    let mut lines = fonts.wrap_text(&full, font_px, max_w);
    if lines.is_empty() {
        return;
    }
    if lines.len() > 2 {
        lines.truncate(2);
        if let Some(last) = lines.last_mut() {
            while !last.is_empty() && fonts.measure_text(&format!("{last}…"), font_px) > max_w {
                last.pop();
            }
            if !last.ends_with('…') {
                last.push('…');
            }
        }
    }

    let (bg_a, border_a, text_a, outline_a) = if ui.ai_thought_dimmed {
        (88u8, 60u8, 115u8, 115u8)
    } else {
        (195u8, 135u8, 255u8, 255u8)
    };
    let prefix_color = Color::rgba(130, 235, 220, text_a);
    let body_color = Color::rgba(
        Color::PAPER_TEXT.0[0],
        Color::PAPER_TEXT.0[1],
        Color::PAPER_TEXT.0[2],
        text_a,
    );
    let outline_color = Color::rgba(0, 0, 0, outline_a);

    let line_h = 10 * s;
    let pad_y = 3 * s;
    let bar_h = pad_y * 2 + (lines.len() as i32) * line_h;
    fb.fill_rect(0, 0, 320 * s, bar_h, Color::rgba(8, 13, 20, bg_a));
    fb.fill_rect(
        0,
        bar_h - (s / 2).max(1),
        320 * s,
        (s / 2).max(1),
        Color::rgba(130, 215, 205, border_a),
    );

    for (idx, line) in lines.iter().enumerate() {
        let y = pad_y + (idx as i32) * line_h;
        if idx == 0 {
            if let Some(rest) = line.strip_prefix(prefix) {
                let prefix_w = fonts.measure_text(prefix, font_px);
                fonts.draw_text_outlined(fb, 8 * s, y, prefix, font_px, prefix_color, outline_color);
                fonts.draw_text_outlined(
                    fb,
                    8 * s + prefix_w,
                    y,
                    rest,
                    font_px,
                    body_color,
                    outline_color,
                );
                continue;
            }
        }
        fonts.draw_text_outlined(fb, 8 * s, y, line, font_px, body_color, outline_color);
    }
}

/// Authentic 1995 DOS Title Screen (`PAL_OpeningMenu`):
/// Borderless gold/white outlined options floating directly over `FBP_060`, plus a sleek bottom language toggle.
pub(super) fn draw_title(
    fb: &mut Framebuffer,
    _cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    s: i32,
    _hd: bool,
) {
    let options = [
        tr(ui, "新的故事", "新的故事", "New Story"),
        tr(ui, "舊的回憶", "旧的回忆", "Continue"),
    ];
    for (i, label) in options.iter().enumerate() {
        let ry = 94 + (i as i32) * 19;
        let selected = ui.title_cursor == i;
        if selected {
            // Subtle warm shadow pill behind active choice (no heavy box blocking the sword & gourd)
            fb.fill_rect(104 * s, (ry - 1) * s, 112 * s, 17 * s, Color::rgba(16, 12, 8, 120));
        }
        let color = if selected { Color::GOLD_HI } else { Color::PAPER_TEXT };
        fonts.draw_text_centered(
            fb,
            160 * s,
            (ry + 1) * s,
            label,
            (13 * s) as u16,
            color,
            Some(Color::BLACK),
        );
        ui.hit_rects.push(HitRect {
            x: 104,
            y: ry - 1,
            w: 112,
            h: 17,
            tag: HitTag::TitleOption(i),
        });
    }

    // Compact "More Options" button at the very bottom, clear of the sword & gourd artwork
    let lw = 116;
    let lh = 16;
    let lx = (320 - lw) / 2;
    let ly = 179;
    let opt_sel = ui.title_cursor == 2;
    fb.fill_rect(
        lx * s,
        ly * s,
        lw * s,
        lh * s,
        if opt_sel { Color::rgba(42, 34, 18, 210) } else { Color::rgba(14, 12, 10, 155) },
    );
    fb.stroke_rect(
        lx * s,
        ly * s,
        lw * s,
        lh * s,
        s.max(1),
        if opt_sel { Color::GOLD_HI } else { Color::rgba(196, 172, 126, 140) },
    );
    let opt_label = if let Some(ver) = ui.update_available.as_deref() {
        match ui.lang {
            pal_core::Language::En => format!("Options · {} ▸", ver),
            pal_core::Language::ZhHant => format!("更多選項 · {} ▸", ver),
            pal_core::Language::ZhHans => format!("更多选项 · {} ▸", ver),
        }
    } else {
        tr(ui, "更多選項 ▸", "更多选项 ▸", "Options ▸").to_string()
    };
    fonts.draw_text_centered(
        fb,
        (lx + lw / 2) * s,
        (ly + 3) * s,
        &opt_label,
        (9 * s) as u16,
        if opt_sel || ui.update_available.is_some() {
            Color::GOLD_HI
        } else {
            Color::CYAN_SPEAKER
        },
        Some(Color::BLACK),
    );
    ui.hit_rects.push(HitRect {
        x: lx,
        y: ly,
        w: lw,
        h: lh,
        tag: HitTag::TitleLangToggle,
    });
}

/// Centered ornate scroll menu opened from the Title Screen's bottom "Options" button.
pub(super) fn draw_title_options(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    s: i32,
    hd: bool,
) {
    ui.hit_rects.clear();
    fb.fill_rect(0, 0, 320 * s, 200 * s, Color::rgba(6, 10, 14, 145));
    let n_cols = if ui.lang.is_en() { 8 } else { 7 };
    let bx = if ui.lang.is_en() { 78 } else { 86 };
    let by = 14;
    let (bw, _) = draw_pal_box(fb, cache, bx, by, 7, n_cols, 0, s, hd);
    let update_item = if let Some(ver) = ui.update_available.as_deref() {
        format!("{} {} ★", tr(ui, "更新至", "更新至", "Update"), ver)
    } else {
        tr(ui, "檢查更新", "检查更新", "Check Update").to_string()
    };
    let items = [
        tr(ui, "錄像回放", "录像回放", "Replays").to_string(),
        format!(
            "{}: {}",
            tr(ui, "畫質", "画质", "Mode"),
            if ui.hd_mode { "HD" } else { tr(ui, "復古", "复古", "Retro") }
        ),
        format!(
            "{}: {}",
            tr(ui, "文字", "文字", "Lang"),
            match ui.lang {
                pal_core::Language::ZhHant => "繁體中文",
                pal_core::Language::ZhHans => "简体中文",
                pal_core::Language::En => "English",
            }
        ),
        format!(
            "{}: {}",
            tr(ui, "音樂", "音乐", "Music"),
            if ui.music_on { tr(ui, "開", "开", "On") } else { tr(ui, "關", "关", "Off") }
        ),
        format!(
            "{}: {}",
            tr(ui, "音效", "音效", "SFX"),
            if ui.sfx_on { tr(ui, "開", "开", "On") } else { tr(ui, "關", "关", "Off") }
        ),
        format!(
            "{}: {}",
            tr(ui, "全螢幕", "全屏", "Fullscreen"),
            if ui.fullscreen { tr(ui, "開", "开", "On") } else { tr(ui, "關", "关", "Off") }
        ),
        format!(
            "{}: {}",
            tr(ui, "AI 旁觀", "AI 旁观", "Agent"),
            match ui.agent_mode {
                1 => tr(ui, "本機", "本机", "Local"),
                2 => tr(ui, "區網", "局域网", "LAN"),
                _ => tr(ui, "關", "关", "Off"),
            }
        ),
        update_item,
    ];
    for (i, label) in items.iter().enumerate() {
        let ry = by + 13 + (i as i32) * 18;
        let sel = ui.title_options_cursor == i;
        fonts.draw_text_centered(
            fb,
            (bx + bw / 2 - 2) * s,
            ry * s,
            label,
            (10 * s) as u16,
            if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
            Some(Color::BLACK),
        );
        ui.hit_rects.push(HitRect {
            x: bx + 8,
            y: ry - 2,
            w: bw - 18,
            h: 17,
            tag: HitTag::TitleOptionsItem(i),
        });
    }
    draw_agent_token_bar(fb, fonts, ui, s);
}

/// Persistent bottom bar showing the LAN `auth <token>` secret with clickable `[Copy]` and
/// `[Refresh]` buttons while Agent Mode is LAN (`#236`).
pub(super) fn draw_agent_token_bar(
    fb: &mut Framebuffer,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    s: i32,
) {
    if ui.agent_mode != 2 {
        return;
    }
    let Some(tok) = ui
        .agent_token
        .as_deref()
        .filter(|t| !t.is_empty())
        .map(str::to_string)
    else {
        return;
    };
    let bar_y = 182;
    let bar_h = 18;
    fb.fill_rect(0, bar_y * s, 320 * s, bar_h * s, Color::rgba(8, 12, 18, 238));
    fb.fill_rect(0, bar_y * s, 320 * s, s, Color::rgba(140, 112, 64, 220));

    let label = format!("LAN: {tok}");
    fonts.draw_text_outlined(
        fb,
        6 * s,
        187 * s,
        &label,
        (7 * s) as u16,
        Color::GOLD_HI,
        Color::BLACK,
    );
    ui.hit_rects.push(HitRect {
        x: 0,
        y: bar_y,
        w: 240,
        h: bar_h,
        tag: HitTag::CopyAgentToken,
    });

    // [Copy] button
    let copy_x = 242;
    let btn_y = 184;
    let btn_w = 34;
    let btn_h = 14;
    fb.fill_rect(
        copy_x * s,
        btn_y * s,
        btn_w * s,
        btn_h * s,
        Color::rgba(36, 72, 108, 245),
    );
    fb.stroke_rect(
        copy_x * s,
        btn_y * s,
        btn_w * s,
        btn_h * s,
        s.max(1),
        Color::rgba(140, 200, 255, 255),
    );
    let copy_lbl = tr(ui, "複製", "复制", "Copy");
    fonts.draw_text_centered(
        fb,
        (copy_x + btn_w / 2) * s,
        187 * s,
        copy_lbl,
        (7 * s) as u16,
        Color::WHITE,
        Some(Color::BLACK),
    );
    ui.hit_rects.push(HitRect {
        x: copy_x,
        y: btn_y,
        w: btn_w,
        h: btn_h,
        tag: HitTag::CopyAgentToken,
    });

    // [Refresh] button
    let ref_x = 280;
    fb.fill_rect(
        ref_x * s,
        btn_y * s,
        btn_w * s,
        btn_h * s,
        Color::rgba(88, 48, 36, 245),
    );
    fb.stroke_rect(
        ref_x * s,
        btn_y * s,
        btn_w * s,
        btn_h * s,
        s.max(1),
        Color::rgba(245, 180, 120, 255),
    );
    let ref_lbl = tr(ui, "刷新", "刷新", "Reset");
    fonts.draw_text_centered(
        fb,
        (ref_x + btn_w / 2) * s,
        187 * s,
        ref_lbl,
        (7 * s) as u16,
        Color::rgba(255, 232, 190, 255),
        Some(Color::BLACK),
    );
    ui.hit_rects.push(HitRect {
        x: ref_x,
        y: btn_y,
        w: btn_w,
        h: btn_h,
        tag: HitTag::RefreshAgentToken,
    });
}

/// Centered ornate scroll overlay shown when a `.repal` replay finishes playing.
pub(super) fn draw_replay_finished(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    s: i32,
    hd: bool,
) {
    fb.fill_rect(0, 0, 320 * s, 200 * s, Color::rgba(6, 10, 14, 150));
    let n_cols = if ui.lang.is_en() { 8 } else { 7 };
    let bx = if ui.lang.is_en() { 78 } else { 86 };
    let by = 40;
    let (bw, _) = draw_pal_box(fb, cache, bx, by, 4, n_cols, 0, s, hd);
    let title = tr(ui, "── 錄像播放完畢 ──", "── 录像播放完毕 ──", "-- Replay Finished --");
    fonts.draw_text_centered(
        fb,
        (bx + bw / 2 - 2) * s,
        (by + 11) * s,
        title,
        (10 * s) as u16,
        Color::CYAN_SPEAKER,
        Some(Color::BLACK),
    );
    let items = [
        tr(ui, "重新播放", "重新播放", "Replay Again"),
        tr(ui, "從此接管", "从此接管", "Take Over"),
        tr(ui, "打開其他錄像", "打开其他录像", "Open Replay"),
        tr(ui, "返回標題", "返回标题", "Title Screen"),
    ];
    for (i, label) in items.iter().enumerate() {
        let ry = by + 31 + (i as i32) * 18;
        let sel = ui.replay_finished_cursor == i;
        fonts.draw_text_centered(
            fb,
            (bx + bw / 2 - 2) * s,
            ry * s,
            label,
            (10 * s) as u16,
            if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
            Some(Color::BLACK),
        );
        ui.hit_rects.push(HitRect {
            x: bx + 8,
            y: ry - 2,
            w: bw - 18,
            h: 17,
            tag: HitTag::ReplayFinishedItem(i),
        });
    }
}
