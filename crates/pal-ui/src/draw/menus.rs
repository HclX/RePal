//! Root menu, role picker, item/system/remake submenus, Status (FBP000), and Equip (FBP001).

use super::overlays::draw_agent_token_bar;
use super::*;
use pal_render::world::blit_fbp_fullscreen;

/// Authentic 1995 DOS Root Menu:
/// Top-left Cash single-line scroll box `(0,0)` + Left 9-slice Style 1 command list `(3,37)` (`状态/法术/物品/系统`).
/// No bulky right-side party cards blocking the HD world map!
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_root_menu(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    state: &GameState,
    s: i32,
    hd: bool,
    interactive: bool,
) {
    if ui.replay_mode {
        // Exclusive Replay Mode Root Menu: top-left Replay badge + 4-item scroll box
        draw_pal_single_line_box(fb, cache, 0, 0, 5, s, hd);
        fonts.draw_text_centered(
            fb,
            48 * s,
            11 * s,
            tr(ui, "錄像回放", "录像回放", "Replay"),
            (10 * s) as u16,
            Color::GOLD_HI,
            Some(Color::BLACK),
        );
        let bx = 3;
        let by = 37;
        let n_cols = if ui.lang.is_en() { 4 } else { 3 };
        let (bw, _) = draw_pal_box(fb, cache, bx, by, 3, n_cols, 0, s, hd);
        let replay_labels = [
            tr(ui, "選擇章節", "选择章节", "Chapters"),
            tr(ui, "從此接管", "从此接管", "Take Over"),
            tr(ui, "系統", "系统", "System"),
            tr(ui, "重製", "重制", "Remake"),
        ];
        for (i, label) in replay_labels.iter().enumerate() {
            let ry = by + 13 + (i as i32) * 18;
            let sel = ui.root_cursor == i;
            let color = if sel {
                if interactive { Color::GOLD_HI } else { CONFIRMED_GREEN }
            } else {
                Color::PAPER_TEXT
            };
            fonts.draw_text_centered(
                fb,
                (bx + bw / 2 - 4) * s,
                ry * s,
                label,
                (11 * s) as u16,
                color,
                Some(Color::BLACK),
            );
            if interactive {
                ui.hit_rects.push(HitRect {
                    x: bx + 8,
                    y: ry - 2,
                    w: bw - 18,
                    h: 17,
                    tag: HitTag::RootMenuItem(i),
                });
            }
        }
        return;
    }

    // Top-left Cash single-line scroll box (`PAL_ShowCash`)
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
    let digits = if state.cash >= 1_000_000 { 8 } else { 6 };
    draw_pal_number(fb, cache, state.cash, digits, 46, 13, NumColor::Yellow, true, s, hd);

    // Left 9-slice Style 0 scroll box at `(3, 37)` (`PAL_InGameMenu`)
    let bx = 3;
    let by = 37;
    let n_cols = if ui.lang.is_en() { 2 } else { 1 };
    let (bw, _) = draw_pal_box(fb, cache, bx, by, 4, n_cols, 0, s, hd);

    let root_labels = [
        tr(ui, "狀態", "状态", "Status"),
        tr(ui, "仙術", "仙术", "Magic"),
        tr(ui, "物品", "物品", "Items"),
        tr(ui, "系統", "系统", "System"),
        tr(ui, "重製", "重制", "Remake"),
    ];
    for (i, label) in root_labels.iter().enumerate() {
        let ry = by + 13 + (i as i32) * 18;
        let sel = ui.root_cursor == i;
        let color = if sel {
            if interactive { Color::GOLD_HI } else { CONFIRMED_GREEN }
        } else {
            Color::PAPER_TEXT
        };
        fonts.draw_text_centered(
            fb,
            (bx + bw / 2 - 4) * s,
            ry * s,
            label,
            (11 * s) as u16,
            color,
            Some(Color::BLACK),
        );
        if interactive {
            ui.hit_rects.push(HitRect {
                x: bx + 8,
                y: ry - 2,
                w: bw - 18,
                h: 17,
                tag: HitTag::RootMenuItem(i),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_role_select(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    state: &GameState,
    s: i32,
    hd: bool,
) {
    // Authentic SDLPAL: show compact PLAYERINFOBOX cards along the bottom while choosing a party member
    for (idx, pm) in state.party.iter().take(3).enumerate() {
        let role = &state.roles[(pm.role_id as usize).min(5)];
        draw_player_info_box(
            fb,
            cache,
            fonts,
            45 + (idx as i32) * 78,
            165,
            role.role_id,
            role.avatar_rgm,
            role.hp,
            role.max_hp,
            role.mp,
            role.max_mp,
            100,
            ui.role_cursor == idx,
            s,
            hd,
        );
    }

    let bx = 38;
    let by = 58;
    let n = state.party.len().max(1) as i32;
    let n_cols = if ui.lang.is_en() { 4 } else { 2 };
    let (bw, _) = draw_pal_box(fb, cache, bx, by, (n - 1).max(1), n_cols, 0, s, hd);

    for (i, pm) in state.party.iter().enumerate() {
        let ry = by + 13 + (i as i32) * 18;
        let sel = ui.role_cursor == i;
        let name = ui.catalog.role_name(pm.role_id);
        fonts.draw_text_outlined(
            fb,
            (bx + 15) * s,
            ry * s,
            name,
            (11 * s) as u16,
            if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
            Color::BLACK,
        );
        ui.hit_rects.push(HitRect {
            x: bx + 8,
            y: ry - 2,
            w: bw - 18,
            h: 17,
            tag: HitTag::RoleSelectItem(i),
        });
    }
}

pub(super) fn draw_item_submenu(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    s: i32,
    hd: bool,
) {
    let bx = 40;
    let by = 60;
    let n_cols = if ui.lang.is_en() { 2 } else { 1 };
    let (bw, _) = draw_pal_box(fb, cache, bx, by, 1, n_cols, 0, s, hd);

    let item_sub_labels = [tr(ui, "使用", "使用", "Use"), tr(ui, "裝備", "装备", "Equip")];
    for (i, label) in item_sub_labels.iter().enumerate() {
        let ry = by + 13 + (i as i32) * 18;
        let sel = ui.item_sub_cursor == i;
        fonts.draw_text_centered(
            fb,
            (bx + bw / 2 - 4) * s,
            ry * s,
            label,
            (11 * s) as u16,
            if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
            Some(Color::BLACK),
        );
        ui.hit_rects.push(HitRect {
            x: bx + 8,
            y: ry - 2,
            w: bw - 18,
            h: 17,
            tag: HitTag::ItemSubmenuItem(i),
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_system_submenu(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    s: i32,
    hd: bool,
    interactive: bool,
) {
    let bx = if ui.replay_mode { 62 } else { 40 };
    let by = 60;
    let n_cols = if ui.lang.is_en() { 5 } else { 3 };
    let mut items = Vec::with_capacity(5);
    if !ui.replay_mode {
        items.push(tr(ui, "儲存進度", "储存进度", "Save Game").to_string());
    }
    items.push(tr(ui, "讀取進度", "读取进度", "Load Game").to_string());
    items.push(format!(
        "{}: {}",
        tr(ui, "音樂", "音乐", "Music"),
        if ui.music_on { tr(ui, "開", "开", "On") } else { tr(ui, "關", "关", "Off") }
    ));
    items.push(format!(
        "{}: {}",
        tr(ui, "音效", "音效", "SFX"),
        if ui.sfx_on { tr(ui, "開", "开", "On") } else { tr(ui, "關", "关", "Off") }
    ));
    items.push(tr(ui, "結束遊戲", "结束游戏", "Title Screen").to_string());

    let n_rows = (items.len() as i32 - 1).max(1);
    let (bw, _) = draw_pal_box(fb, cache, bx, by, n_rows, n_cols, 0, s, hd);

    for (i, label) in items.iter().enumerate() {
        let ry = by + 13 + (i as i32) * 18;
        let sel = ui.sys_sub_cursor == i;
        let color = if sel {
            if interactive { Color::GOLD_HI } else { CONFIRMED_GREEN }
        } else {
            Color::PAPER_TEXT
        };
        fonts.draw_text_outlined(
            fb,
            (bx + 14) * s,
            ry * s,
            label,
            (10 * s) as u16,
            color,
            Color::BLACK,
        );
        if interactive {
            ui.hit_rects.push(HitRect {
                x: bx + 8,
                y: ry - 2,
                w: bw - 18,
                h: 17,
                tag: HitTag::SystemSubmenuItem(i),
            });
        }
    }
}

pub(super) fn draw_remake_submenu(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    s: i32,
    hd: bool,
) {
    // Position clear of top-left Cash box (x=0..96, y=0..30) and RootMenu (x=3..64) (#168).
    let bx = if ui.replay_mode { 68 } else { 98 };
    let by = if ui.replay_mode { 48 } else { 10 };
    let n_cols = if ui.lang.is_en() { 7 } else { 6 };

    let items: Vec<String> = if ui.replay_mode {
        vec![
            format!(
                "{}: {}",
                tr(ui, "畫質", "画质", "Mode"),
                if ui.hd_mode { "HD" } else { tr(ui, "復古", "复古", "Retro") }
            ),
            format!(
                "{}: {}",
                tr(ui, "文字", "文字", "Lang"),
                match ui.lang {
                    pal_core::Language::ZhHant => "繁體",
                    pal_core::Language::ZhHans => "简体",
                    pal_core::Language::En => "English",
                }
            ),
            tr(ui, "開啟錄像 [F8]", "打开录像 [F8]", "Open Replay [F8]").to_string(),
            tr(ui, "畫面截圖 [F10]", "画面截图 [F10]", "Screenshot [F10]").to_string(),
            format!(
                "{}: {}",
                tr(ui, "全螢幕", "全屏", "Fullscreen"),
                if ui.fullscreen { tr(ui, "開", "开", "On") } else { tr(ui, "關", "关", "Off") }
            ),
        ]
    } else {
        vec![
            tr(ui, "匯出存檔", "导出存档", "Export Save").to_string(),
            tr(ui, "匯入存檔", "导入存档", "Import Save").to_string(),
            format!(
                "{}: {}",
                tr(ui, "畫質", "画质", "Mode"),
                if ui.hd_mode { "HD" } else { tr(ui, "復古", "复古", "Retro") }
            ),
            format!(
                "{}: {}",
                tr(ui, "文字", "文字", "Lang"),
                match ui.lang {
                    pal_core::Language::ZhHant => "繁體",
                    pal_core::Language::ZhHans => "简体",
                    pal_core::Language::En => "English",
                }
            ),
            tr(ui, "錯誤報告 [F9]", "错误报告 [F9]", "Bug Report [F9]").to_string(),
            tr(ui, "畫面截圖 [F10]", "画面截图 [F10]", "Screenshot [F10]").to_string(),
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
            tr(ui, "重新開始錄製", "重新开始录制", "New Recording").to_string(),
            tr(ui, "匯出錄像", "导出录像", "Export Replay").to_string(),
        ]
    };

    let (n_rows, row_pitch, font_sz) = if ui.replay_mode {
        ((items.len() as i32 - 1).max(1), 17, (10 * s) as u16)
    } else {
        // Fit all 10 gameplay remake options cleanly within y=10..190 without bottom clipping (#168).
        (8, 16, (9 * s) as u16)
    };
    let (bw, _) = draw_pal_box(fb, cache, bx, by, n_rows, n_cols, 0, s, hd);

    for (i, label) in items.iter().enumerate() {
        let ry = by + 11 + (i as i32) * row_pitch;
        let sel = ui.remake_sub_cursor == i;
        fonts.draw_text_outlined(
            fb,
            (bx + 14) * s,
            ry * s,
            label,
            font_sz,
            if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
            Color::BLACK,
        );
        ui.hit_rects.push(HitRect {
            x: bx + 8,
            y: ry - 2,
            w: bw - 18,
            h: row_pitch,
            tag: HitTag::RemakeSubmenuItem(i),
        });
    }
    draw_agent_token_bar(fb, fonts, ui, s);
}

/// Authentic 1995 DOS PAL `狀態` screen backed by `FBP_000`.
pub(super) fn draw_status_fbp000(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    state: &GameState,
    s: i32,
    hd: bool,
) {
    let has_fbp = blit_fbp_fullscreen(fb, cache, 0, hd);
    if !has_fbp {
        fb.fill_rect(0, 0, fb.width as i32, fb.height as i32, Color::rgba(28, 22, 16, 245));
    }

    let role_id = state
        .party
        .get(ui.active_role_idx)
        .map(|p| p.role_id as usize)
        .unwrap_or(0)
        .min(5);
    let role = &state.roles[role_id];
    let name = ui.catalog.role_name(role.role_id).to_string();

    fonts.draw_text_centered(
        fb,
        136 * s,
        10 * s,
        &format!("{}  Lv.{}", name, role.level),
        (11 * s) as u16,
        Color::GOLD_HI,
        Some(Color::BLACK),
    );

    blit_atlas_frame_fit(
        fb,
        cache,
        "ui",
        "rgm",
        role.avatar_rgm as usize,
        106 * s,
        34 * s,
        64 * s,
        64 * s,
        hd,
    );

    // Authentic SDLPAL stat ordering: EXP, LV, HP, MP, ATK, MAG, DEF, AGI, LUK
    let stats_rows = [
        (tr(ui, "經驗", "经验", "EXP"), format!("{}", role.exp)),
        (tr(ui, "修行", "修行", "LV"), format!("{}", role.level)),
        (tr(ui, "體力", "体力", "HP"), format!("{}/{}", role.hp, role.max_hp)),
        (tr(ui, "真氣", "真气", "MP"), format!("{}/{}", role.mp, role.max_mp)),
        (tr(ui, "武術", "武术", "ATK"), format!("{}", role.stats[0])),
        (tr(ui, "靈力", "灵力", "MAG"), format!("{}", role.stats[1])),
        (tr(ui, "防禦", "防御", "DEF"), format!("{}", role.stats[2])),
        (tr(ui, "身法", "身法", "AGI"), format!("{}", role.stats[3])),
        (tr(ui, "吉運", "吉运", "LUK"), format!("{}", role.stats[4])),
    ];
    for (i, (label, val)) in stats_rows.iter().enumerate() {
        let y = 16 + (i as i32) * 19;
        fonts.draw_text_outlined(fb, 8 * s, y * s, label, (9 * s) as u16, Color::GOLD_HI, Color::BLACK);
        fonts.draw_text_outlined(fb, 38 * s, y * s, val, (9 * s) as u16, Color::PAPER_TEXT, Color::BLACK);
    }

    const FBP000_SLOTS: [(i32, i32); 6] = [
        (190, 2),
        (248, 40),
        (252, 102),
        (202, 134),
        (142, 142),
        (82, 126),
    ];
    for (slot, &(sx, sy)) in FBP000_SLOTS.iter().enumerate() {
        if !has_fbp {
            fb.draw_bevel_box(sx * s, sy * s, 48 * s, 47 * s, (s.max(1) + 1) / 2);
        }
        let item_id = role.equipment[slot];
        if item_id > 0 {
            blit_atlas_frame_fit(
                fb,
                cache,
                "ui",
                "ball",
                ui.catalog.item_icon(item_id),
                (sx + 6) * s,
                (sy + 4) * s,
                36 * s,
                28 * s,
                hd,
            );
            let item_name = ui.catalog.item_name(item_id);
            let name_px = if ui.lang.is_en() && item_name.chars().count() > 8 {
                (6 * s) as u16
            } else {
                (7 * s) as u16
            };
            fonts.draw_text_centered(
                fb,
                (sx + 24) * s,
                (sy + 35) * s,
                &item_name,
                name_px,
                Color::PAPER_TEXT,
                Some(Color::BLACK),
            );
        } else {
            let slot_lbls = [
                tr(ui, "頭戴", "头戴", "Head"),
                tr(ui, "披掛", "披挂", "Cloak"),
                tr(ui, "身穿", "身穿", "Armor"),
                tr(ui, "手持", "手持", "Weapon"),
                tr(ui, "腳穿", "脚穿", "Boots"),
                tr(ui, "配帶", "佩带", "Charm"),
            ];
            fonts.draw_text_centered(
                fb,
                (sx + 24) * s,
                (sy + 19) * s,
                slot_lbls[slot],
                (8 * s) as u16,
                Color::MUTED_TEXT,
                Some(Color::BLACK),
            );
        }
    }
}

/// Authentic 1995 DOS PAL `物品 -> 裝備` screen backed by `FBP_001`.
pub(super) fn draw_equip_fbp001(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    state: &GameState,
    s: i32,
    hd: bool,
) {
    let has_fbp = blit_fbp_fullscreen(fb, cache, 1, hd);
    if !has_fbp {
        fb.fill_rect(0, 0, fb.width as i32, fb.height as i32, Color::rgba(28, 22, 16, 245));
    }

    let role_id = state
        .party
        .get(ui.active_role_idx)
        .map(|p| p.role_id as usize)
        .unwrap_or(0)
        .min(5);
    let role = &state.roles[role_id];
    let name = ui.catalog.role_name(role.role_id).to_string();

    let sel_item_id = role.equipment[ui.equip_slot_cursor.min(5)];
    if sel_item_id > 0 {
        blit_atlas_frame_fit(
            fb,
            cache,
            "ui",
            "ball",
            ui.catalog.item_icon(sel_item_id),
            18 * s,
            14 * s,
            46 * s,
            46 * s,
            hd,
        );
    }
    fonts.draw_text_centered(
        fb,
        40 * s,
        74 * s,
        &name,
        (11 * s) as u16,
        Color::GOLD_HI,
        Some(Color::BLACK),
    );

    // Always draw crisp gold labels in both HD & Retro so localized/1x FBP_001 never looks black (#169).
    let draw_dynamic_labels = true;
    let equip_labels = [
        tr(ui, "頭戴", "头戴", "Head"),
        tr(ui, "披掛", "披挂", "Cloak"),
        tr(ui, "身穿", "身穿", "Armor"),
        tr(ui, "手持", "手持", "Weapon"),
        tr(ui, "腳穿", "脚穿", "Boots"),
        tr(ui, "配帶", "佩带", "Charm"),
    ];
    let is_en = ui.lang.is_en();
    let lbl_x = if is_en { 82 } else { 88 };
    let lbl_px = if is_en { (9 * s) as u16 } else { (11 * s) as u16 };
    let val_x = if is_en { 134 } else { 132 };
    let val_px = if is_en { (9 * s) as u16 } else { (10 * s) as u16 };
    let sel_w = if is_en { 86 } else { 64 };
    for slot in 0..6 {
        let ry = 12 + (slot as i32) * 22;
        let sel = ui.equip_slot_cursor == slot;
        if sel {
            fb.fill_rect(128 * s, (ry - 1) * s, sel_w * s, 18 * s, Color::SEL_BG);
        }
        if draw_dynamic_labels {
            fonts.draw_text_outlined(
                fb,
                lbl_x * s,
                (ry + 2) * s,
                equip_labels[slot],
                lbl_px,
                Color::GOLD_HI,
                Color::BLACK,
            );
        }
        let item_name = if role.equipment[slot] == 0 {
            tr(ui, "無", "无", "None").to_string()
        } else {
            ui.catalog.item_name(role.equipment[slot])
        };
        fonts.draw_text_outlined(
            fb,
            val_x * s,
            (ry + 2) * s,
            &item_name,
            val_px,
            if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
            Color::BLACK,
        );
        ui.hit_rects.push(HitRect {
            x: 84,
            y: ry - 1,
            w: 130,
            h: 18,
            tag: HitTag::EquipSlot(slot),
        });
    }

    let right_stats = [
        (tr(ui, "武術", "武术", "ATK"), role.stats[0]),
        (tr(ui, "靈力", "灵力", "MAG"), role.stats[1]),
        (tr(ui, "防禦", "防御", "DEF"), role.stats[2]),
        (tr(ui, "身法", "身法", "AGI"), role.stats[3]),
        (tr(ui, "吉運", "吉运", "LUK"), role.stats[4]),
    ];
    for (i, (label, val)) in right_stats.iter().enumerate() {
        let y = 14 + (i as i32) * 23;
        if draw_dynamic_labels {
            fonts.draw_text_outlined(fb, 224 * s, y * s, label, (11 * s) as u16, Color::GOLD_HI, Color::BLACK);
        }
        fonts.draw_text_outlined(
            fb,
            268 * s,
            y * s,
            &val.to_string(),
            (11 * s) as u16,
            Color::PAPER_TEXT,
            Color::BLACK,
        );
    }
}
