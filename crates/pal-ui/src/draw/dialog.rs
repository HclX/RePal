//! Dialogue pagination and authentic 1995 DOS PAL dialogue / choice box renderer.

use super::*;

/// Reflow legacy 1995 DOS PAL hard-wrapped dialogue lines (single `\n` every ~13 CJK chars)
/// so text fills the full dialogue width cleanly, while preserving explicit `\n\n` paragraphs.
pub(super) fn reflow_dialog_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_newline = false;
    for ch in raw.chars() {
        if ch == '\r' {
            continue;
        }
        if ch == '\n' {
            if prev_newline && !out.ends_with('\n') {
                out.push('\n');
            }
            prev_newline = true;
        } else {
            if prev_newline {
                let prev_ch = out.chars().last();
                if prev_ch.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, ',' | '.' | ';' | ':' | '!' | '?'))
                    && ch.is_ascii_alphanumeric()
                {
                    out.push(' ');
                }
            }
            prev_newline = false;
            out.push(ch);
        }
    }
    out
}

pub(super) fn is_system_notice_text(trimmed: &str, is_unnamed: bool) -> bool {
    (trimmed.starts_with('『') && trimmed.ends_with('』'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        || (is_unnamed
            && (trimmed.starts_with("获得")
                || trimmed.starts_with("獲得")
                || trimmed.starts_with("得到")
                || trimmed.starts_with("学会")
                || trimmed.starts_with("學會")
                || trimmed.starts_with("失去")
                || trimmed.starts_with("遗忘")
                || trimmed.starts_with("遺忘")
                || trimmed.starts_with("Obtained ")
                || trimmed.starts_with("Received ")
                || trimmed.starts_with("Learned ")
                || trimmed.starts_with("Gained ")))
}

/// Pure deterministic dialog pagination shared by both `UiState::sync_dialog_layout` (headless &
/// runtime) and `draw_dialog` (GUI) so `total_pages` and page contents are 100% identical (#217).
pub fn layout_dialog_pages(dialog: &crate::DialogModel, fonts: &mut FontEngine, hd: bool) -> Vec<Vec<String>> {
    const MAX_LINES_PER_PAGE: usize = 3;
    let s = if hd { 4 } else { 1 };
    let has_speaker = !dialog.speaker.is_empty();
    let has_portrait = dialog.portrait_rgm.is_some();
    let is_unnamed = !has_speaker && !has_portrait;
    let reflowed = reflow_dialog_text(&dialog.text);
    let trimmed_all = reflowed.trim();

    if is_unnamed
        && dialog.choices.is_empty()
        && !trimmed_all.contains('\n')
        && trimmed_all.chars().count() <= 26
        && is_system_notice_text(trimmed_all, true)
    {
        return vec![vec![trimmed_all.to_string()]];
    }

    let portrait_on_left = has_portrait && dialog.at_top;
    let portrait_on_right = has_portrait && !dialog.at_top;
    let text_x = if portrait_on_left { 66 } else { 14 };
    let text_right = if portrait_on_right { 250 } else { 306 };
    let font_px = (10 * s) as u16;
    let max_w = (text_right - text_x) * s;
    let lines = fonts.wrap_text(&reflowed, font_px, max_w);
    if lines.is_empty() {
        return vec![vec![String::new()]];
    }
    lines.chunks(MAX_LINES_PER_PAGE).map(|c| c.to_vec()).collect()
}

pub(super) fn draw_dialog(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    s: i32,
    hd: bool,
) {
    const MAX_LINES_PER_PAGE: usize = 3;
    let has_speaker = !ui.dialog.speaker.is_empty();
    let has_portrait = ui.dialog.portrait_rgm.is_some();
    let is_unnamed = !has_speaker && !has_portrait;
    let reflowed = reflow_dialog_text(&ui.dialog.text);
    let trimmed_all = reflowed.trim();

    // Authentic SDLPAL centered single-line scroll box (`kDialogCenterWindow`) for short system notices!
    if is_unnamed
        && ui.dialog.choices.is_empty()
        && !trimmed_all.contains('\n')
        && trimmed_all.chars().count() <= 26
        && is_system_notice_text(trimmed_all, true)
    {
        ui.dialog.total_pages = 1;
        ui.dialog.page = 0;
        let est_px = if ui.lang.is_en() {
            (trimmed_all.chars().count() as i32) * 6 + 18
        } else {
            (trimmed_all.chars().count() as i32) * 11 + 20
        };
        let n_len = ((est_px + 15) / 16).clamp(4, 17);
        let box_w = 16 + n_len * 16;
        let bx = (320 - box_w) / 2;
        let by = 32;
        let (_, bh) = draw_pal_single_line_box(fb, cache, bx, by, n_len, s, hd);
        fonts.draw_text_centered(
            fb,
            (bx + box_w / 2) * s,
            (by + 11) * s,
            trimmed_all,
            (10 * s) as u16,
            Color::GOLD_HI,
            Some(Color::BLACK),
        );
        ui.hit_rects.push(HitRect {
            x: 0,
            y: 0,
            w: 320,
            h: 200,
            tag: HitTag::DialogConfirm,
        });
        let _ = bh;
        return;
    }

    // Authentic SDLPAL portrait placement:
    //   Upper dialog (`at_top`): portrait on LEFT, text on RIGHT.
    //   Lower dialog (`!at_top`): portrait on RIGHT (270, 144), text on LEFT!
    let portrait_on_left = has_portrait && ui.dialog.at_top;
    let portrait_on_right = has_portrait && !ui.dialog.at_top;

    let text_x = if portrait_on_left { 66 } else { 14 };
    let text_right = if portrait_on_right { 250 } else { 306 };
    let font_px = (10 * s) as u16;
    let pages = layout_dialog_pages(&ui.dialog, fonts, hd);
    let total_pages = pages.len().max(1);
    ui.dialog.total_pages = total_pages;
    ui.dialog.pages = pages.iter().map(|pl| pl.join("\n")).collect();
    let page = ui.dialog.page.min(total_pages - 1);
    ui.dialog.page = page;
    let page_lines: &[String] = pages.get(page).map(|v| v.as_slice()).unwrap_or(&[]);
    let has_more_pages = page + 1 < total_pages;

    let shown_lines = page_lines.len().clamp(1, MAX_LINES_PER_PAGE) as i32;
    let speaker_h = if has_speaker { 13 } else { 0 };
    let line_h = 13;
    let min_h = if has_portrait { 60 } else { 46 };
    let bh = (12 + speaker_h + shown_lines * line_h).max(min_h).min(78);
    let by = if ui.dialog.at_top { 0 } else { 200 - bh };

    // Soft borderless translucent dark gradient band (open 1995 feel + crisp HD readability)
    if ui.dialog.at_top {
        fb.fill_rect_vgrad(
            0,
            0,
            320 * s,
            (bh + 6) * s,
            Color::rgba(8, 7, 6, 178),
            Color::rgba(8, 7, 6, 24),
        );
    } else {
        fb.fill_rect_vgrad(
            0,
            (by - 8).max(0) * s,
            320 * s,
            (bh + 8) * s,
            Color::rgba(8, 7, 6, 24),
            Color::rgba(8, 7, 6, 184),
        );
    }

    if let Some(rgm_id) = ui.dialog.portrait_rgm {
        let px = if portrait_on_left { 8 } else { 256 };
        let py = if ui.dialog.at_top { 5 } else { (196 - 56).max(by + 2) };
        blit_atlas_frame_fit(
            fb,
            cache,
            "ui",
            "rgm",
            rgm_id as usize,
            px * s,
            py * s,
            56 * s,
            56 * s,
            hd,
        );
    }

    // Speaker name is ALWAYS displayed on every page of a paginated dialogue.
    let mut cur_y = by + 6;
    if has_speaker {
        let header = if ui.lang.is_en() {
            format!("{}:", ui.dialog.speaker)
        } else {
            format!("{}：", ui.dialog.speaker)
        };
        fonts.draw_text_outlined(
            fb,
            (text_x - 4).max(6) * s,
            cur_y * s,
            &header,
            font_px,
            Color::CYAN_SPEAKER,
            Color::BLACK,
        );
        cur_y += speaker_h;
    }

    for line in page_lines {
        let trimmed = line.trim();
        let line_color = if is_system_notice_text(trimmed, is_unnamed) {
            Color::GOLD_HI
        } else {
            Color::PAPER_TEXT
        };
        fonts.draw_text_outlined(
            fb,
            text_x * s,
            cur_y * s,
            line,
            font_px,
            line_color,
            Color::BLACK,
        );
        cur_y += line_h;
    }

    if has_more_pages {
        let hint = format!("{}/{} ▼", page + 1, total_pages);
        fonts.draw_text_outlined(
            fb,
            (text_right - 28) * s,
            (by + bh - 12) * s,
            &hint,
            (8 * s) as u16,
            Color::GOLD_HI,
            Color::BLACK,
        );
    }

    ui.hit_rects.push(HitRect {
        x: 0,
        y: by,
        w: 320,
        h: bh,
        tag: HitTag::DialogConfirm,
    });

    // Authentic SDLPAL side-by-side single-line scroll boxes for Yes/No & choices
    if !has_more_pages && !ui.dialog.choices.is_empty() {
        let cy_base = if ui.dialog.at_top { by + bh + 12 } else { (by - 44).max(76) };
        for (i, choice) in ui.dialog.choices.iter().enumerate() {
            let col = (i % 2) as i32;
            let row = (i / 2) as i32;
            let chars = choice.chars().count() as i32;
            let n_len = if ui.lang.is_en() {
                ((chars * 7 + 12) / 16).clamp(2, 6)
            } else {
                (chars + 1).clamp(2, 6)
            };
            let cx = 114 + col * 84;
            let cy = cy_base + row * 40;
            let (cw, ch) = draw_pal_single_line_box(fb, cache, cx, cy, n_len, s, hd);
            let sel = ui.dialog.choice_cursor == i;
            fonts.draw_text_centered(
                fb,
                (cx + cw / 2) * s,
                (cy + 11) * s,
                choice,
                (11 * s) as u16,
                if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
                Some(Color::BLACK),
            );
            ui.hit_rects.push(HitRect {
                x: cx,
                y: cy,
                w: cw,
                h: ch,
                tag: HitTag::DialogChoice(i),
            });
        }
    }
}
