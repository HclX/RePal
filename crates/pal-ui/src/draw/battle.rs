//! Battle HUD: party status strip, 4-way Bagua command wheel, target reticles, and victory overlay.

use super::*;

/// Always-visible bottom-right party `PLAYERINFOBOX` strip + status badges during battle.
pub(super) fn draw_battle_party_strip(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    b: &BattleVisuals,
    s: i32,
    hd: bool,
) {
    for (idx, hero) in b.heroes.iter().take(3).enumerate() {
        let hx = 91 + (idx as i32) * 77;
        let is_active = b.prompting && idx == b.active_hero;
        let atb = if is_active { 100 } else { hero.atb_pct as i32 };
        draw_player_info_box(
            fb,
            cache,
            fonts,
            hx,
            165,
            hero.role_id,
            hero.avatar_rgm,
            hero.hp,
            hero.max_hp,
            hero.mp,
            hero.max_mp,
            atb,
            is_active,
            s,
            hd,
        );
        // Compact status pills along the top edge of the card (poison/sleep/silence/defend/bravery/dual)
        let mut sx = hx + 2;
        let sy = 155;
        if hero.defending {
            fb.fill_rect(sx * s, sy * s, 11 * s, 9 * s, Color::rgba(18, 38, 56, 215));
            fonts.draw_text_outlined(
                fb,
                (sx + 1) * s,
                sy * s,
                "防",
                (8 * s) as u16,
                Color::CYAN_SPEAKER,
                Color::BLACK,
            );
            sx += 12;
        }
        for &st in hero.statuses.iter().take(4) {
            let (tag, col) = match st {
                99 => ("毒", Color::rgba(92, 232, 112, 255)),
                0 => ("乱", Color::rgba(232, 130, 255, 255)),
                1 => ("定", Color::rgba(255, 196, 86, 255)),
                2 => ("眠", Color::rgba(130, 196, 255, 255)),
                3 => ("封", Color::rgba(255, 118, 118, 255)),
                5 => ("气", Color::GOLD_HI),
                6 => ("护", Color::CYAN_SPEAKER),
                8 => ("双", Color::rgba(255, 168, 92, 255)),
                _ => continue,
            };
            fb.fill_rect(sx * s, sy * s, 11 * s, 9 * s, Color::rgba(14, 14, 20, 210));
            fonts.draw_text_outlined(fb, (sx + 1) * s, sy * s, tag, (8 * s) as u16, col, Color::BLACK);
            sx += 12;
        }
    }
}

/// Authentic 1995 SDLPAL dual stacked single-line scroll banners for Battle Victory
/// (EXP banner + Cash banner, plus optional compact Level-Up box below).
pub(super) fn draw_battle_victory(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    s: i32,
    hd: bool,
) {
    let info = &ui.battle_victory;
    let has_lv = !info.level_ups.is_empty();
    let top_y = if has_lv { 36 } else { 62 };

    let exp_line = match ui.lang {
        pal_core::Language::En => format!("Gained {} EXP", info.exp),
        pal_core::Language::ZhHans => format!("获得经验值 {}", info.exp),
        pal_core::Language::ZhHant => format!("獲得經驗值 {}", info.exp),
    };
    let cash_line = match ui.lang {
        pal_core::Language::En => format!("Gained {} Wen", info.cash),
        pal_core::Language::ZhHans => format!("获得金钱 {} 文", info.cash),
        pal_core::Language::ZhHant => format!("獲得金錢 {} 文", info.cash),
    };

    for (idx, text) in [&exp_line, &cash_line].iter().enumerate() {
        let est_px = if ui.lang.is_en() {
            (text.chars().count() as i32) * 7 + 22
        } else {
            (text.chars().count() as i32) * 11 + 24
        };
        let n_len = ((est_px + 15) / 16).clamp(7, 15);
        let box_w = 16 + n_len * 16;
        let bx = (320 - box_w) / 2;
        let by = top_y + (idx as i32) * 36;
        draw_pal_single_line_box(fb, cache, bx, by, n_len, s, hd);
        fonts.draw_text_centered(
            fb,
            (bx + box_w / 2) * s,
            (by + 11) * s,
            text,
            (10 * s) as u16,
            Color::GOLD_HI,
            Some(Color::BLACK),
        );
    }

    if has_lv {
        let extra_rows = info.level_ups.len().min(4) as i32;
        let n_cols = 10;
        let bx = 64;
        let by = top_y + 74;
        let (bw, _) = draw_pal_box(fb, cache, bx, by, (extra_rows - 1).max(0), n_cols, 0, s, hd);
        let cx = bx + bw / 2 - 2;
        for (i, lv) in info.level_ups.iter().take(4).enumerate() {
            let ry = by + 11 + (i as i32) * 17;
            let line = format!("★ {lv}");
            fonts.draw_text_centered(
                fb,
                cx * s,
                ry * s,
                &line,
                (10 * s) as u16,
                CONFIRMED_GREEN,
                Some(Color::BLACK),
            );
        }
    }

    ui.hit_rects.push(HitRect {
        x: 0,
        y: 0,
        w: 320,
        h: 200,
        tag: HitTag::BattleVictoryConfirm,
    });
}

/// Authentic 1995 DOS Battle HUD (`PAL_BattleUIUpdate`):
/// - Bottom-left 4-way Bagua diamond cross icons (`40..43`), shown ONLY when awaiting player input
/// - Active hero bouncing arrow (`69`)
/// - Misc submenu (`2, 20`) 9-slice Style 0 box
/// - Centered single-line scroll target banner & Style 1 Magic/Item pickers
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_battle_hud(
    fb: &mut Framebuffer,
    cache: &mut AssetCache,
    fonts: &mut FontEngine,
    ui: &mut UiState,
    state: &GameState,
    battle: Option<&BattleVisuals>,
    phase: BattleSubPhase,
    s: i32,
    hd: bool,
) {
    let Some(b) = battle else { return };
    // Hide the command wheel & menus during attack/spell/enemy animations (authentic SDLPAL behavior).
    if !b.prompting {
        return;
    }

    // 1. Bottom-left 4-way Bagua diamond cross icons (uibattle.c:813-816)
    let coop_valid = b.heroes.iter().filter(|h| h.hp > 0).count() > 1;
    let icons = [
        (SPR_BATTLE_ATTACK, 27, 140, true),
        (SPR_BATTLE_MAGIC, 0, 155, true),
        (SPR_BATTLE_COOP, 54, 155, coop_valid),
        (SPR_BATTLE_MISC, 27, 170, true),
    ];
    let in_cmd_or_misc = matches!(phase, BattleSubPhase::CommandWheel | BattleSubPhase::MiscMenu);
    for (idx, &(fidx, ix, iy, valid)) in icons.iter().enumerate() {
        let selected = in_cmd_or_misc && ui.battle_cmd_cursor == idx;
        if selected {
            blit_atlas_frame(fb, cache, "ui", "spriteui", fidx, ix * s, iy * s, s, hd);
        } else if valid {
            blit_atlas_frame_mono(fb, cache, "ui", "spriteui", fidx, ix * s, iy * s, s, hd, [132, 132, 140]);
        } else {
            blit_atlas_frame_mono(fb, cache, "ui", "spriteui", fidx, ix * s, iy * s, s, hd, [128, 44, 44]);
        }
        if matches!(
            phase,
            BattleSubPhase::CommandWheel
                | BattleSubPhase::MiscMenu
                | BattleSubPhase::SelectEnemyForAttack
                | BattleSubPhase::SelectEnemyForCoop
        ) {
            ui.hit_rects.push(HitRect {
                x: ix,
                y: iy,
                w: 30,
                h: 30,
                tag: HitTag::BattleCommand(idx),
            });
        }
    }

    // 2. Active hero indicator arrow above hero sprite
    if let Some(hero) = b.heroes.get(b.active_hero) {
        blit_atlas_frame(
            fb,
            cache,
            "ui",
            "spriteui",
            SPR_CURSOR_DOWN,
            (hero.x - 4) * s,
            (hero.y - 68).max(8) * s,
            s,
            hd,
        );
    }

    // Register clickable enemy hitboxes when choosing an enemy target
    if matches!(
        phase,
        BattleSubPhase::CommandWheel
            | BattleSubPhase::SelectEnemyForAttack
            | BattleSubPhase::SelectEnemyForCoop
            | BattleSubPhase::SelectEnemyForMagic(_)
            | BattleSubPhase::SelectEnemyForItem(_)
    ) {
        for (idx, enemy) in b.enemies.iter().enumerate() {
            if !enemy.alive {
                continue;
            }
            ui.hit_rects.push(HitRect {
                x: enemy.x - 24,
                y: enemy.y - 48,
                w: 48,
                h: 48,
                tag: HitTag::BattleEnemy(idx),
            });
        }
    }

    // Register clickable hero sprite + PLAYERINFOBOX hitboxes when choosing an ally target
    if matches!(
        phase,
        BattleSubPhase::SelectAllyForMagic(_) | BattleSubPhase::SelectAllyForItem(_)
    ) {
        for (idx, hero) in b.heroes.iter().take(3).enumerate() {
            ui.hit_rects.push(HitRect {
                x: hero.x - 24,
                y: hero.y - 54,
                w: 48,
                h: 56,
                tag: HitTag::BattleHero(idx),
            });
            let hx = 91 + (idx as i32) * 77;
            ui.hit_rects.push(HitRect {
                x: hx,
                y: 165,
                w: 75,
                h: 35,
                tag: HitTag::BattleHero(idx),
            });
        }
    }

    match phase {
        BattleSubPhase::CommandWheel => {}
        BattleSubPhase::MiscMenu => {
            // Authentic SDLPAL Misc submenu at `(2, 20)` (物品 / 防禦 / 圍攻 / 逃跑)
            let n_cols = if ui.lang.is_en() { 3 } else { 1 };
            let (bw, _) = draw_pal_box(fb, cache, 2, 20, 3, n_cols, 0, s, hd);
            let misc_labels = [
                tr(ui, "物品", "物品", "Item"),
                tr(ui, "防禦", "防御", "Defend"),
                tr(ui, "圍攻", "围攻", "Auto"),
                tr(ui, "逃跑", "逃跑", "Flee"),
            ];
            for (i, label) in misc_labels.iter().enumerate() {
                let ry = 33 + (i as i32) * 18;
                let sel = ui.battle_misc_cursor == i;
                fonts.draw_text_centered(
                    fb,
                    (2 + bw / 2 - 4) * s,
                    ry * s,
                    label,
                    (11 * s) as u16,
                    if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
                    Some(Color::BLACK),
                );
                ui.hit_rects.push(HitRect {
                    x: 10,
                    y: ry - 2,
                    w: bw - 18,
                    h: 17,
                    tag: HitTag::BattleMiscItem(i),
                });
            }
        }
        BattleSubPhase::SelectEnemyForAttack
        | BattleSubPhase::SelectEnemyForCoop
        | BattleSubPhase::SelectEnemyForMagic(_)
        | BattleSubPhase::SelectEnemyForItem(_) => {
            if let Some(sel_idx) = b.selected_enemy {
                if let Some(enemy) = b.enemies.get(sel_idx) {
                    let prefix = match phase {
                        BattleSubPhase::SelectEnemyForMagic(mid) => {
                            format!("{} → ", ui.catalog.magic_name(mid))
                        }
                        BattleSubPhase::SelectEnemyForItem(iid) => {
                            format!("{} → ", ui.catalog.item_name(iid))
                        }
                        BattleSubPhase::SelectEnemyForCoop => {
                            format!("{} → ", tr(ui, "合體技", "合体技", "Co-op"))
                        }
                        _ => tr(ui, "目標：", "目标：", "Target: ").to_string(),
                    };
                    let label = format!("{}{}", prefix, enemy.name);
                    let n_len = ((label.chars().count() as i32 * 10 + 18) / 16).clamp(5, 15);
                    let box_w = 16 + n_len * 16;
                    let bx = (320 - box_w) / 2;
                    draw_pal_single_line_box(fb, cache, bx, 6, n_len, s, hd);
                    fonts.draw_text_centered(
                        fb,
                        160 * s,
                        17 * s,
                        &label,
                        (10 * s) as u16,
                        Color::GOLD_HI,
                        Some(Color::BLACK),
                    );
                }
            }
        }
        BattleSubPhase::SelectAllyForMagic(id) | BattleSubPhase::SelectAllyForItem(id) => {
            if let Some(sel_idx) = b.selected_ally {
                if let Some(hero) = b.heroes.get(sel_idx) {
                    let skill_name = match phase {
                        BattleSubPhase::SelectAllyForMagic(_) => ui.catalog.magic_name(id),
                        _ => ui.catalog.item_name(id),
                    };
                    let label = format!("{} → {}", skill_name, hero.name);
                    let n_len = ((label.chars().count() as i32 * 10 + 18) / 16).clamp(5, 15);
                    let box_w = 16 + n_len * 16;
                    let bx = (320 - box_w) / 2;
                    draw_pal_single_line_box(fb, cache, bx, 6, n_len, s, hd);
                    fonts.draw_text_centered(
                        fb,
                        160 * s,
                        17 * s,
                        &label,
                        (10 * s) as u16,
                        CONFIRMED_GREEN,
                        Some(Color::BLACK),
                    );
                }
            }
        }
        BattleSubPhase::SelectMagic => {
            draw_pal_box(fb, cache, 2, 0, 5, 17, 1, s, hd);
            let role_id = state
                .party
                .get(b.active_hero)
                .map(|p| p.role_id as usize)
                .unwrap_or(b.active_hero);
            let magics: Vec<u16> = state
                .roles
                .get(role_id)
                .map(|r| r.magics.iter().copied().filter(|&m| m > 0).collect())
                .unwrap_or_default();
            if magics.is_empty() {
                fonts.draw_text_centered(
                    fb,
                    160 * s,
                    56 * s,
                    tr(ui, "（尚未習得任何仙術）", "（尚未习得任何仙术）", "(No spells learned)"),
                    (10 * s) as u16,
                    Color::PAPER_TEXT,
                    Some(Color::BLACK),
                );
            } else {
                let page_start = (ui.grid_cursor / 12) * 12;
                for (slot, &mid) in magics.iter().skip(page_start).take(12).enumerate() {
                    let idx = page_start + slot;
                    let col = (slot % 2) as i32;
                    let row = (slot / 2) as i32;
                    let cx = 16 + col * 144;
                    let cy = 13 + row * 17;
                    let sel = ui.grid_cursor == idx;
                    let label = ui.catalog.magic_name(mid);
                    fonts.draw_text_outlined(
                        fb,
                        cx * s,
                        cy * s,
                        &label,
                        (10 * s) as u16,
                        if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
                        Color::BLACK,
                    );
                    let cost = ui.catalog.magic_cost(mid);
                    draw_pal_number(fb, cache, cost as u32, 3, cx + 112, cy + 3, NumColor::Cyan, true, s, hd);
                    ui.hit_rects.push(HitRect {
                        x: cx - 2,
                        y: cy - 2,
                        w: 138,
                        h: 16,
                        tag: HitTag::GridCell(idx),
                    });
                }
                // Sleek description bar directly below the 2x6 spell picker (above PLAYERINFOBOX)
                if let Some(&mid) = magics.get(ui.grid_cursor) {
                    let cost = ui.catalog.magic_cost(mid);
                    let is_enemy = ui.catalog.magic_target_enemy.get(&mid).copied().unwrap_or(true);
                    let is_all = ui.catalog.magic_target_all.get(&mid).copied().unwrap_or(false);
                    let scope = match (is_enemy, is_all) {
                        (true, true) => tr(ui, "敵全體", "敌全体", "All Enemies"),
                        (true, false) => tr(ui, "敵單體", "敌单体", "1 Enemy"),
                        (false, true) => tr(ui, "我全體", "我全体", "All Allies"),
                        (false, false) => tr(ui, "我單體", "我单体", "1 Ally"),
                    };
                    let desc = ui.catalog.magic_descs.get(&mid).map(|s| s.as_str()).unwrap_or("");
                    let summary = if desc.is_empty() {
                        format!("[{}]  MP:{}", scope, cost)
                    } else {
                        format!("[{} · MP:{}] {}", scope, cost, desc)
                    };
                    let wrapped = fonts.wrap_text(&summary, (9 * s) as u16, 304 * s);
                    if let Some(line) = wrapped.first() {
                        fonts.draw_text_outlined(
                            fb,
                            8 * s,
                            136 * s,
                            line,
                            (9 * s) as u16,
                            Color::PAPER_TEXT,
                            Color::BLACK,
                        );
                    }
                }
            }
        }
        BattleSubPhase::SelectItem => {
            draw_pal_box(fb, cache, 2, 0, 5, 17, 1, s, hd);
            let items = ui.battle_usable_items(state);
            if items.is_empty() {
                fonts.draw_text_centered(
                    fb,
                    160 * s,
                    56 * s,
                    tr(ui, "（行囊中沒有可用物品）", "（行囊中没有可用物品）", "(No usable items)"),
                    (10 * s) as u16,
                    Color::PAPER_TEXT,
                    Some(Color::BLACK),
                );
            } else {
                let page_start = (ui.grid_cursor / 12) * 12;
                for (slot, &(item_id, qty)) in items.iter().skip(page_start).take(12).enumerate() {
                    let idx = page_start + slot;
                    let col = (slot % 2) as i32;
                    let row = (slot / 2) as i32;
                    let cx = 16 + col * 144;
                    let cy = 13 + row * 17;
                    let sel = ui.grid_cursor == idx;
                    let label = ui.catalog.item_name(item_id);
                    fonts.draw_text_outlined(
                        fb,
                        cx * s,
                        cy * s,
                        &label,
                        (10 * s) as u16,
                        if sel { Color::GOLD_HI } else { Color::PAPER_TEXT },
                        Color::BLACK,
                    );
                    draw_pal_number(fb, cache, qty as u32, 2, cx + 118, cy + 3, NumColor::Cyan, true, s, hd);
                    ui.hit_rects.push(HitRect {
                        x: cx - 2,
                        y: cy - 2,
                        w: 138,
                        h: 16,
                        tag: HitTag::GridCell(idx),
                    });
                }
                // Sleek description bar directly below the 2x6 item picker (above PLAYERINFOBOX)
                if let Some(&(item_id, _)) = items.get(ui.grid_cursor) {
                    let throwable = ui.catalog.item_throwable.get(&item_id).copied().unwrap_or(false);
                    let usable = ui.catalog.item_usable.get(&item_id).copied().unwrap_or(false);
                    let is_all = ui.catalog.item_target_all.get(&item_id).copied().unwrap_or(false);
                    let kind_tag = if throwable && !usable {
                        if is_all {
                            tr(ui, "暗器·全體", "暗器·全体", "Throw All")
                        } else {
                            tr(ui, "暗器·投擲", "暗器·投掷", "Throw")
                        }
                    } else if is_all {
                        tr(ui, "道具·全體", "道具·全体", "Use All")
                    } else {
                        tr(ui, "道具·單體", "道具·单体", "Use")
                    };
                    let raw_desc = ui.catalog.item_descs.get(&item_id).map(|s| s.as_str()).unwrap_or("");
                    let desc = raw_desc
                        .replace('\r', "")
                        .replace('\n', if ui.lang.is_en() { " " } else { "" });
                    let summary = if desc.is_empty() {
                        format!("[{}]", kind_tag)
                    } else {
                        format!("[{}] {}", kind_tag, desc)
                    };
                    let wrapped = fonts.wrap_text(&summary, (9 * s) as u16, 304 * s);
                    if let Some(line) = wrapped.first() {
                        fonts.draw_text_outlined(
                            fb,
                            8 * s,
                            136 * s,
                            line,
                            (9 * s) as u16,
                            Color::PAPER_TEXT,
                            Color::BLACK,
                        );
                    }
                }
            }
        }
    }
}
