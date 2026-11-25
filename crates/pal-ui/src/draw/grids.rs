//! Magic grid, item grid, save/load slots, replay chapter gallery, and shop/pawnshop UI.

use super::*;
use crate::state::SaveSlotMode;

/// Authentic 1995 DOS 3-column Magic Grid using Style 1 crimson-gold ornate 9-slice box + bottom MP bar.
pub(super) fn draw_magic_grid(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    state: &GameState,
    s: i32,
    hd: bool,
) {
    let role_id = state
        .party
        .get(ui.active_role_idx)
        .map(|p| p.role_id as usize)
        .unwrap_or(0)
        .min(5);
    let role = &state.roles[role_id];
    let role_name = ui.catalog.role_name(role.role_id).to_string();

    // Top ornate brocade box (`PAL_MagicSelectionMenu`): 3 columns for CJK, 2 columns for English so long spell names never overlap
    draw_pal_box(fb, cache, 2, 0, 6, 17, 1, s, hd);
    let cols = if ui.lang.is_en() { 2usize } else { 3usize };
    let per_page = cols * 6;
    let col_w = if ui.lang.is_en() { 146 } else { 99 };
    let font_px = if ui.lang.is_en() { (9 * s) as u16 } else { (10 * s) as u16 };
    let num_dx = if ui.lang.is_en() { 120 } else { 74 };
    let page_start = (ui.grid_cursor / per_page) * per_page;
    for slot in 0..per_page {
        let idx = page_start + slot;
        let Some(&magic_id) = role.magics.get(idx) else {
            break;
        };
        let col = (slot % cols) as i32;
        let row = (slot / cols) as i32;
        let cx = 14 + col * col_w;
        let cy = 13 + row * 19;
        let sel = idx == ui.grid_cursor;
        let mname = ui.catalog.magic_name(magic_id);
        fonts.draw_text_outlined(
            fb,
            cx * s,
            cy * s,
            &mname,
            font_px,
            if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
            Color::BLACK,
        );
        let cost = ui.catalog.magic_cost(magic_id);
        draw_pal_number(fb, cache, cost as u32, 3, cx + num_dx, cy + 3, NumColor::Cyan, true, s, hd);
        ui.hit_rects.push(HitRect {
            x: cx - 2,
            y: cy - 2,
            w: col_w - 4,
            h: 18,
            tag: HitTag::GridCell(idx),
        });
    }

    let total_magic_pages = role.magics.len().max(1).div_ceil(per_page);
    if total_magic_pages > 1 {
        let cur_page = (ui.grid_cursor / per_page) + 1;
        let pg_txt = format!("{cur_page}/{total_magic_pages}");
        fonts.draw_text_outlined(
            fb,
            286 * s,
            130 * s,
            &pg_txt,
            (8 * s) as u16,
            Color::GOLD_HI,
            Color::BLACK,
        );
    }

    // Selected spell borderless description + bottom single-line scroll box showing caster & MP / Cost
    let selected_magic = role.magics.get(ui.grid_cursor).copied().unwrap_or(0);
    if selected_magic > 0 {
        if let Some(desc) = ui.catalog.magic_descs.get(&selected_magic) {
            let clean = desc.replace('\r', "").replace('\n', if ui.lang.is_en() { " " } else { "" });
            let wrapped = fonts.wrap_text(&clean, (9 * s) as u16, 296 * s);
            if let Some(line) = wrapped.first() {
                fonts.draw_text_outlined(
                    fb,
                    12 * s,
                    143 * s,
                    line,
                    (9 * s) as u16,
                    Color::PAPER_TEXT,
                    Color::BLACK,
                );
            }
        }
    }
    let mp_cost = if selected_magic > 0 { ui.catalog.magic_cost(selected_magic) } else { 0 };
    draw_pal_single_line_box(fb, cache, 10, 160, 17, s, hd);
    fonts.draw_text_outlined(
        fb,
        22 * s,
        171 * s,
        &format!(
            "{} {}   {} {}/{}   {} {}",
            role_name,
            tr(ui, "仙術", "仙术", "Magic"),
            tr(ui, "真氣", "真气", "MP"),
            role.mp,
            role.max_mp,
            tr(ui, "耗氣", "耗气", "Cost"),
            mp_cost
        ),
        (10 * s) as u16,
        Color::GOLD_HI,
        Color::BLACK,
    );
}

/// Authentic 1995 DOS 3-column Item Grid (`PAL_ItemSelectMenu`):
/// Style 1 crimson-gold ornate 9-slice list at TOP `(2,0)`, `ITEMBOX` (frame 70) at bottom-left `(5,132)`,
/// and item description at bottom-right.
pub(super) fn draw_item_grid(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    state: &GameState,
    s: i32,
    hd: bool,
) {
    let items: Vec<(u16, u16)> = state
        .inventory
        .iter()
        .filter(|(_, &q)| q > 0)
        .map(|(&id, &q)| (id, q))
        .collect();

    // Top crimson-gold brocade box: 3 columns for CJK, 2 columns for English so long item names never clip
    draw_pal_box(fb, cache, 2, 0, 6, 17, 1, s, hd);
    if items.is_empty() {
        fonts.draw_text_centered(
            fb,
            160 * s,
            66 * s,
            tr(ui, "行囊空空如也", "行囊空空如也", "Inventory is empty"),
            (11 * s) as u16,
            Color::MUTED_TEXT,
            Some(Color::BLACK),
        );
    } else {
        let cols = if ui.lang.is_en() { 2usize } else { 3usize };
        let per_page = cols * 6;
        let col_w = if ui.lang.is_en() { 146 } else { 99 };
        let font_px = if ui.lang.is_en() { (9 * s) as u16 } else { (10 * s) as u16 };
        let num_dx = if ui.lang.is_en() { 126 } else { 80 };
        let page_start = (ui.grid_cursor / per_page) * per_page;
        for slot in 0..per_page {
            let idx = page_start + slot;
            let Some(&(item_id, qty)) = items.get(idx) else {
                break;
            };
            let col = (slot % cols) as i32;
            let row = (slot / cols) as i32;
            let cx = 14 + col * col_w;
            let cy = 13 + row * 19;
            let sel = idx == ui.grid_cursor;
            let name = ui.catalog.item_name(item_id);
            fonts.draw_text_outlined(
                fb,
                cx * s,
                cy * s,
                &name,
                font_px,
                if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
                Color::BLACK,
            );
            draw_pal_number(fb, cache, qty as u32, 2, cx + num_dx, cy + 3, NumColor::Cyan, true, s, hd);
            ui.hit_rects.push(HitRect {
                x: cx - 2,
                y: cy - 2,
                w: col_w - 4,
                h: 18,
                tag: HitTag::GridCell(idx),
            });
        }
        let total_item_pages = items.len().max(1).div_ceil(per_page);
        if total_item_pages > 1 {
            let cur_page = (ui.grid_cursor / per_page) + 1;
            let pg_txt = format!("{cur_page}/{total_item_pages}");
            fonts.draw_text_outlined(
                fb,
                286 * s,
                130 * s,
                &pg_txt,
                (8 * s) as u16,
                Color::GOLD_HI,
                Color::BLACK,
            );
        }
    }

    // Authentic 1995 SDLPAL borderless bottom-left `SPRITENUM_ITEMBOX` (frame 70, 64x64) +
    // heavy-outlined 2-line description floating directly over the live scene (no dark band).
    if blit_atlas_frame(fb, cache, "ui", "spriteui", SPR_ITEMBOX, 5 * s, 134 * s, s, hd).is_none() {
        fb.draw_bevel_box(5 * s, 134 * s, 64 * s, 64 * s, s.max(1));
    }
    if let Some(&(item_id, qty)) = items.get(ui.grid_cursor) {
        blit_atlas_frame_fit(
            fb,
            cache,
            "ui",
            "ball",
            ui.catalog.item_icon(item_id),
            13 * s,
            142 * s,
            48 * s,
            48 * s,
            hd,
        );
        let title = format!("{}  ×{}", ui.catalog.item_name(item_id), qty);
        fonts.draw_text_outlined(fb, 76 * s, 152 * s, &title, (11 * s) as u16, Color::GOLD_HI, Color::BLACK);
        if let Some(desc) = ui.catalog.item_descs.get(&item_id) {
            let wrapped = fonts.wrap_text(desc, (9 * s) as u16, 234 * s);
            for (li, line) in wrapped.iter().take(2).enumerate() {
                fonts.draw_text_outlined(
                    fb,
                    76 * s,
                    (168 + (li as i32) * 12) * s,
                    line,
                    (9 * s) as u16,
                    Color::PAPER_TEXT,
                    Color::BLACK,
                );
            }
        }
    }
}

/// Authentic 1995 DOS Save/Load Slots (`PAL_SaveSlotMenu`):
/// 5 independent single-line scroll boxes stacked on the right side `(148, 7 + 38 * i)`.
pub(super) fn draw_save_load_slots(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    mode: SaveSlotMode,
    s: i32,
    hd: bool,
) {
    let page_size = 5usize;
    let total_slots = ui.save_slots.len().max(1);
    let total_pages = (total_slots + page_size - 1) / page_size;
    let page = (ui.save_slot_cursor / page_size).min(total_pages - 1);
    let page_start = page * page_size;

    let page_txt = match ui.lang {
        pal_core::Language::En => format!("Page {}/{}", page + 1, total_pages),
        pal_core::Language::ZhHans => format!("第 {}/{} 页", page + 1, total_pages),
        pal_core::Language::ZhHant => format!("第 {}/{} 頁", page + 1, total_pages),
    };

    if mode == SaveSlotMode::LoadFromTitle {
        // Standalone Title-screen load: show top-left mode badge + page indicator.
        let header = tr(ui, "讀取進度", "读取进度", "Load Game");
        draw_pal_single_line_box(fb, cache, 8, 7, 5, s, hd);
        fonts.draw_text_centered(
            fb,
            56 * s,
            18 * s,
            header,
            (11 * s) as u16,
            Color::GOLD_HI,
            Some(Color::BLACK),
        );
        if total_pages > 1 {
            draw_pal_single_line_box(fb, cache, 8, 45, 5, s, hd);
            fonts.draw_text_centered(
                fb,
                56 * s,
                56 * s,
                &page_txt,
                (9 * s) as u16,
                Color::PAPER_TEXT,
                Some(Color::BLACK),
            );
            let hint_txt = tr(ui, "←/→ 翻頁", "←/→ 翻页", "←/→ Page");
            fonts.draw_text_centered(
                fb,
                56 * s,
                84 * s,
                hint_txt,
                (8 * s) as u16,
                Color::GOLD_HI,
                Some(Color::BLACK),
            );
        }
    } else if total_pages > 1 {
        // In-game Save/Load: Cash + RootMenu + SystemSubmenu stay visible on the left;
        // anchor the compact page readout cleanly at bottom-left clear of all menus.
        let compact = format!("{}  {}", page_txt, tr(ui, "←/→", "←/→", "←/→"));
        fonts.draw_text_outlined(
            fb,
            8 * s,
            184 * s,
            &compact,
            (8 * s) as u16,
            Color::GOLD_HI,
            Color::BLACK,
        );
    }

    let bx = 136;
    let n_len = 10; // 176px wide -> fits both localized slot label + scene/level cleanly
    for row in 0..page_size {
        let i = page_start + row;
        let Some(slot) = ui.save_slots.get(i) else {
            break;
        };
        let ry = 7 + (row as i32) * 38;
        let (bw, bh) = draw_pal_single_line_box(fb, cache, bx, ry, n_len, s, hd);
        let sel = ui.save_slot_cursor == i;
        let slot_label = match ui.lang {
            pal_core::Language::En => format!("Slot {:02}", i + 1),
            pal_core::Language::ZhHans => format!("进度{:02}", i + 1),
            pal_core::Language::ZhHant => format!("進度{:02}", i + 1),
        };
        let detail = if !slot.exists || slot.detail.is_empty() || slot.detail == "空" {
            tr(ui, "空", "空", "Empty")
        } else if ui.lang.is_en() && slot.detail == "餘杭客棧 Lv.1" {
            "Yuhang Inn Lv.1"
        } else {
            slot.detail.as_str()
        };
        let line = format!("{}  {}", slot_label, detail);
        fonts.draw_text_outlined(
            fb,
            (bx + 12) * s,
            (ry + 11) * s,
            &line,
            (10 * s) as u16,
            if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
            Color::BLACK,
        );
        ui.hit_rects.push(HitRect {
            x: bx,
            y: ry,
            w: bw,
            h: bh,
            tag: HitTag::SaveSlot(i),
        });
    }
}

/// Blit an RGBA8 thumbnail scaled via nearest-neighbor into `fb` at `(dx, dy, dw, dh)`.
pub(super) fn blit_rgba_scaled(
    fb: &mut Framebuffer,
    src_w: u32,
    src_h: u32,
    src: &[u8],
    dx: i32,
    dy: i32,
    dw: i32,
    dh: i32,
) {
    if src_w == 0 || src_h == 0 || dw <= 0 || dh <= 0 {
        return;
    }
    let fbw = fb.width as i32;
    let fbh = fb.height as i32;
    for y in 0..dh {
        let py = dy + y;
        if py < 0 || py >= fbh {
            continue;
        }
        let sy = ((y as u64 * src_h as u64) / dh as u64).min(src_h as u64 - 1) as usize;
        for x in 0..dw {
            let px = dx + x;
            if px < 0 || px >= fbw {
                continue;
            }
            let sx = ((x as u64 * src_w as u64) / dw as u64).min(src_w as u64 - 1) as usize;
            let si = (sy * src_w as usize + sx) * 4;
            if si + 3 < src.len() {
                let di = (py as usize * fb.width as usize + px as usize) * 4;
                if di + 3 < fb.pixels.len() {
                    fb.pixels[di] = src[si];
                    fb.pixels[di + 1] = src[si + 1];
                    fb.pixels[di + 2] = src[si + 2];
                    fb.pixels[di + 3] = 255;
                }
            }
        }
    }
}

/// Replay Chapter Selector (styled identically to `PAL_SaveSlotMenu`, with thumbnail preview on the left).
pub(super) fn draw_replay_chapters(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    s: i32,
    hd: bool,
) {
    let header = tr(ui, "選擇章節", "选择章节", "Chapters");
    draw_pal_single_line_box(fb, cache, 8, 7, 5, s, hd);
    fonts.draw_text_centered(
        fb,
        56 * s,
        18 * s,
        header,
        (11 * s) as u16,
        Color::GOLD_HI,
        Some(Color::BLACK),
    );

    let page_size = 5usize;
    let total = ui.replay_chapters.len();
    let total_pages = total.max(1).div_ceil(page_size);
    let page = if total == 0 {
        0
    } else {
        (ui.replay_chapter_cursor.min(total - 1) / page_size).min(total_pages - 1)
    };
    let page_start = page * page_size;

    draw_pal_single_line_box(fb, cache, 8, 45, 5, s, hd);
    let page_txt = match ui.lang {
        pal_core::Language::En => format!("Page {}/{}", page + 1, total_pages),
        pal_core::Language::ZhHans => format!("第 {}/{} 页", page + 1, total_pages),
        pal_core::Language::ZhHant => format!("第 {}/{} 頁", page + 1, total_pages),
    };
    fonts.draw_text_centered(
        fb,
        56 * s,
        56 * s,
        &page_txt,
        (9 * s) as u16,
        Color::PAPER_TEXT,
        Some(Color::BLACK),
    );

    // Left-side thumbnail preview box (120x75 logical = 480x300 HD) for the selected chapter
    let tx = 8;
    let ty = 86;
    let tw = 120;
    let th = 75;
    fb.fill_rect((tx - 2) * s, (ty - 2) * s, (tw + 4) * s, (th + 4) * s, Color::rgba(14, 18, 26, 235));
    fb.draw_bevel_box((tx - 2) * s, (ty - 2) * s, (tw + 4) * s, (th + 4) * s, s.max(1));
    if let Some(ch) = ui.replay_chapters.get(ui.replay_chapter_cursor) {
        if let Some((w, h, rgba)) = &ch.thumb_rgba {
            blit_rgba_scaled(fb, *w, *h, rgba, tx * s, ty * s, tw * s, th * s);
        } else {
            fonts.draw_text_centered(
                fb,
                (tx + tw / 2) * s,
                (ty + th / 2 - 5) * s,
                tr(ui, "無縮圖", "无缩略图", "No Preview"),
                (9 * s) as u16,
                Color::PAPER_TEXT,
                Some(Color::BLACK),
            );
        }
        let secs = ch.start_tick / 50;
        let cap = format!("#{:02}  {:02}:{:02}", ch.index + 1, secs / 60, secs % 60);
        fonts.draw_text_centered(
            fb,
            (tx + tw / 2) * s,
            (ty + th + 7) * s,
            &cap,
            (9 * s) as u16,
            Color::GOLD_HI,
            Some(Color::BLACK),
        );
    }

    let bx = 136;
    let n_len = 10;
    for row in 0..page_size {
        let i = page_start + row;
        let Some(ch) = ui.replay_chapters.get(i) else {
            break;
        };
        let ry = 7 + (row as i32) * 38;
        let (bw, bh) = draw_pal_single_line_box(fb, cache, bx, ry, n_len, s, hd);
        let sel = ui.replay_chapter_cursor == i;
        let secs = ch.start_tick / 50;
        let line = format!("#{:02} {:02}:{:02} {}", ch.index + 1, secs / 60, secs % 60, ch.label);
        fonts.draw_text_outlined(
            fb,
            (bx + 12) * s,
            (ry + 11) * s,
            &line,
            (10 * s) as u16,
            if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
            Color::BLACK,
        );
        ui.hit_rects.push(HitRect {
            x: bx,
            y: ry,
            w: bw,
            h: bh,
            tag: HitTag::ReplayChapter(i),
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_shop(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    state: &GameState,
    is_sell: bool,
    s: i32,
    hd: bool,
) {
    // Top-left Cash single-line scroll box
    let cash_len = if state.cash >= 1_000_000 { 7 } else { 5 };
    draw_pal_single_line_box(fb, cache, 0, 0, cash_len, s, hd);
    fonts.draw_text_outlined(
        fb,
        10 * s,
        11 * s,
        tr(ui, "金錢", "金钱", "Wen"),
        (10 * s) as u16,
        Color::PAPER_TEXT,
        Color::BLACK,
    );
    draw_pal_number(fb, cache, state.cash, if state.cash >= 1_000_000 { 8 } else { 6 }, 46, 13, NumColor::Yellow, true, s, hd);

    // Top-right shop title badge
    let title = if is_sell {
        tr(ui, "當鋪變賣", "当铺变卖", "Pawnshop")
    } else {
        tr(ui, "商店購買", "商店购买", "Shop")
    };
    draw_pal_single_line_box(fb, cache, 198, 0, 6, s, hd);
    fonts.draw_text_centered(fb, 254 * s, 11 * s, title, (10 * s) as u16, Color::GOLD_HI, Some(Color::BLACK));

    // Right-side Style 1 brocade item list + bottom-left ITEMBOX
    draw_pal_box(fb, cache, 76, 36, 6, 12, 1, s, hd);
    let page_start = (ui.shop_cursor / 6) * 6;
    for (row_idx, entry) in ui.shop_items.iter().skip(page_start).take(6).enumerate() {
        let idx = page_start + row_idx;
        let ry = 49 + (row_idx as i32) * 19;
        let sel = ui.shop_cursor == idx;
        fonts.draw_text_outlined(
            fb,
            90 * s,
            ry * s,
            &entry.name,
            (10 * s) as u16,
            if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
            Color::BLACK,
        );
        let right = if is_sell {
            format!("{}  (×{})", entry.price, entry.count)
        } else {
            format!("{}", entry.price)
        };
        fonts.draw_text_outlined(
            fb,
            234 * s,
            ry * s,
            &right,
            (10 * s) as u16,
            if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
            Color::BLACK,
        );
        ui.hit_rects.push(HitRect {
            x: 86,
            y: ry - 2,
            w: 220,
            h: 18,
            tag: HitTag::ShopRow(idx),
        });
    }

    // Left ITEMBOX showing selected shop item icon
    if blit_atlas_frame(fb, cache, "ui", "spriteui", SPR_ITEMBOX, 6 * s, 54 * s, s, hd).is_none() {
        fb.draw_bevel_box(6 * s, 54 * s, 64 * s, 64 * s, s.max(1));
    }
    if let Some(entry) = ui.shop_items.get(ui.shop_cursor) {
        blit_atlas_frame_fit(
            fb,
            cache,
            "ui",
            "ball",
            ui.catalog.item_icon(entry.item_id),
            14 * s,
            62 * s,
            48 * s,
            48 * s,
            hd,
        );
        // Show clear 2-step sell confirmation banner when armed in PawnshopSell (#181).
        if is_sell && ui.pawnshop_confirm_idx == Some(ui.shop_cursor) {
            let prompt = match ui.lang {
                pal_core::Language::En => format!("Confirm sell {} (+{} Wen)? [Enter/Esc]", entry.name, entry.price),
                pal_core::Language::ZhHans => format!("再次确认变卖「{}」(+{} 文)？ [确认/取消]", entry.name, entry.price),
                pal_core::Language::ZhHant => format!("再次確認變賣「{}」(+{} 文)？ [確認/取消]", entry.name, entry.price),
            };
            fb.fill_rect(12 * s, 174 * s, 296 * s, 18 * s, Color::rgba(28, 18, 10, 225));
            fb.stroke_rect(12 * s, 174 * s, 296 * s, 18 * s, s.max(1), Color::GOLD_HI);
            fonts.draw_text_centered(
                fb,
                160 * s,
                178 * s,
                &prompt,
                (9 * s) as u16,
                Color::GOLD_HI,
                Some(Color::BLACK),
            );
        }
    }
}
