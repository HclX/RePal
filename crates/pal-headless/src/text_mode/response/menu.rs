//! Structured JSON snapshot builders for every `pal_ui::UiView` screen.

use super::*;

/// Complete snapshot of the active `pal-ui` menu / HUD matching everything the GUI renders (#220, #221).
pub fn build_ui_menu(rt: &PalRuntime) -> Option<serde_json::Value> {
    let ui = &rt.ui;
    let lang = ui.lang;
    let g = rt.game.as_ref()?;
    let on_off = |v: bool| {
        if v {
            tr_lang(lang, "开", "開", "On")
        } else {
            tr_lang(lang, "关", "關", "Off")
        }
    };
    let hd_val = if ui.hd_mode {
        "HD"
    } else {
        tr_lang(lang, "复古", "復古", "Retro")
    };
    let lang_val = match ui.lang {
        pal_core::Language::ZhHant => "繁體",
        pal_core::Language::ZhHans => "简体",
        pal_core::Language::En => "English",
    };
    let agent_val = match ui.agent_mode {
        1 => tr_lang(lang, "本机", "本機", "Local"),
        2 => tr_lang(lang, "局域网", "區網", "LAN"),
        _ => tr_lang(lang, "关", "關", "Off"),
    };

    match &ui.view {
        UiView::None | UiView::Dialog => None,
        UiView::Title => Some(serde_json::json!({
            "screen": "title",
            "cursor": ui.title_cursor,
            "items": [
                tr_lang(lang, "新的故事", "新的故事", "New Story"),
                tr_lang(lang, "旧的回忆", "舊的回憶", "Continue"),
                tr_lang(lang, "更多选项", "更多選項", "Options"),
            ],
        })),
        UiView::TitleOptions => Some(serde_json::json!({
            "screen": "title_options",
            "cursor": ui.title_options_cursor,
            "items": [
                { "label": tr_lang(lang, "录像回放", "錄像回放", "Replays"), "value": serde_json::Value::Null },
                { "label": tr_lang(lang, "画质", "畫質", "Mode"), "value": hd_val },
                { "label": tr_lang(lang, "文字", "文字", "Lang"), "value": lang_val },
                { "label": tr_lang(lang, "音乐", "音樂", "Music"), "value": on_off(ui.music_on) },
                { "label": tr_lang(lang, "音效", "音效", "SFX"), "value": on_off(ui.sfx_on) },
                { "label": tr_lang(lang, "全屏", "全螢幕", "Fullscreen"), "value": on_off(ui.fullscreen) },
                { "label": tr_lang(lang, "AI 旁观", "AI 旁觀", "Agent"), "value": agent_val },
                { "label": tr_lang(lang, "检查更新", "檢查更新", "Check Update"), "value": ui.update_available },
            ],
        })),
        UiView::ReplayFinished => Some(serde_json::json!({
            "screen": "replay_finished",
            "cursor": ui.replay_finished_cursor,
            "items": [
                tr_lang(lang, "重新播放", "重新播放", "Replay Again"),
                tr_lang(lang, "从此接管", "從此接管", "Take Over"),
                tr_lang(lang, "打开其他录像", "打開其他錄像", "Open Replay"),
                tr_lang(lang, "返回标题", "返回標題", "Title Screen"),
            ],
        })),
        UiView::RootMenu => Some(serde_json::json!({
            "screen": "root",
            "cursor": ui.root_cursor,
            "items": if ui.replay_mode {
                vec![
                    tr_lang(lang, "章节", "章節", "Chapters"),
                    tr_lang(lang, "接管", "接管", "Take Over"),
                    tr_lang(lang, "系统", "系統", "System"),
                    tr_lang(lang, "重制", "重製", "Remake"),
                ]
            } else {
                vec![
                    tr_lang(lang, "状态", "狀態", "Status"),
                    tr_lang(lang, "仙术", "仙術", "Magic"),
                    tr_lang(lang, "物品", "物品", "Items"),
                    tr_lang(lang, "系统", "系統", "System"),
                    tr_lang(lang, "重制", "重製", "Remake"),
                ]
            },
        })),
        UiView::RoleSelect(target) => Some(serde_json::json!({
            "screen": match target {
                RoleSelectTarget::Status => "role_select_status",
                RoleSelectTarget::Magic => "role_select_magic",
                RoleSelectTarget::Equip => "role_select_equip",
            },
            "cursor": ui.role_cursor,
            "items": g.world.party.iter().map(|m| g.role_display_name(m.role_id as usize)).collect::<Vec<_>>(),
        })),
        UiView::StatusFbp000 => Some(serde_json::json!({
            "screen": "status",
            "active_role_idx": ui.active_role_idx,
        })),
        UiView::ItemSubmenu => Some(serde_json::json!({
            "screen": "item_submenu",
            "cursor": ui.item_sub_cursor,
            "items": [
                tr_lang(lang, "使用", "使用", "Use"),
                tr_lang(lang, "装备", "裝備", "Equip"),
            ],
        })),
        UiView::ItemGrid => {
            let cols = if ui.lang.is_en() { 2usize } else { 3usize };
            let rows = 6usize;
            let per_page = cols * rows;
            let items: Vec<serde_json::Value> = g
                .world
                .inventory
                .iter()
                .filter(|(_, &q)| q > 0)
                .enumerate()
                .map(|(i, (&id, &qty))| {
                    let it = g.data.items.get(&id);
                    serde_json::json!({
                        "idx": i,
                        "id": id,
                        "name": g.item_display_name(id),
                        "qty": qty,
                        "desc": g.item_display_desc(id),
                        "usable": it.map(|x| x.usable).unwrap_or(false),
                        "throwable": it.map(|x| x.throwable).unwrap_or(false),
                        "target": it.map(item_target_kind).unwrap_or("none"),
                    })
                })
                .collect();
            let total_pages = items.len().max(1).div_ceil(per_page);
            let page = ui.grid_cursor / per_page;
            Some(serde_json::json!({
                "screen": "item_grid",
                "cursor": ui.grid_cursor,
                "active_role_idx": ui.active_role_idx,
                "cols": cols,
                "rows": rows,
                "page": page,
                "total_pages": total_pages,
                "items": items,
            }))
        }
        UiView::EquipFbp001 => {
            let rid = g
                .world
                .party
                .get(ui.active_role_idx)
                .map(|p| p.role_id as usize)
                .unwrap_or(0)
                .min(5);
            let slot_names = [
                tr_lang(lang, "头戴", "頭戴", "Head"),
                tr_lang(lang, "披挂", "披掛", "Cloak"),
                tr_lang(lang, "身穿", "身穿", "Armor"),
                tr_lang(lang, "手持", "手持", "Weapon"),
                tr_lang(lang, "脚穿", "腳穿", "Boots"),
                tr_lang(lang, "佩带", "配帶", "Charm"),
            ];
            let equip_arr = g
                .world
                .roles
                .get(rid)
                .map(|r| r.equipment)
                .unwrap_or([0; 6]);
            let slots: Vec<serde_json::Value> = (0..6)
                .map(|s| {
                    let iid = equip_arr[s];
                    serde_json::json!({
                        "slot": s,
                        "name": slot_names[s],
                        "item_id": iid,
                        "item_name": if iid > 0 { g.item_display_name(iid) } else { tr_lang(lang, "无", "無", "None").to_string() },
                    })
                })
                .collect();
            let cur_slot = ui.equip_slot_cursor.min(5);
            let cur_eq = equip_arr[cur_slot];
            let old_b = if cur_eq > 0 {
                g.item_stat_bonuses(cur_eq)
            } else {
                [0; 5]
            };
            let mut candidates: Vec<serde_json::Value> = g
                .world
                .inventory
                .iter()
                .filter(|(_, &q)| q > 0)
                .filter_map(|(&iid, &qty)| {
                    if g.item_slot(iid) != Some(cur_slot) || !g.can_role_equip(rid, iid) {
                        return None;
                    }
                    let new_b = g.item_stat_bonuses(iid);
                    Some(serde_json::json!({
                        "item_id": iid,
                        "name": g.item_display_name(iid),
                        "qty": qty,
                        "stat_deltas": {
                            "atk": new_b[0] - old_b[0],
                            "mag": new_b[1] - old_b[1],
                            "def": new_b[2] - old_b[2],
                            "dex": new_b[3] - old_b[3],
                            "flee": new_b[4] - old_b[4],
                        },
                    }))
                })
                .collect();
            candidates.sort_by_key(|v| v["item_id"].as_u64().unwrap_or(0));
            Some(serde_json::json!({
                "screen": "equip",
                "active_role_idx": ui.active_role_idx,
                "role_id": rid,
                "cursor": ui.equip_slot_cursor,
                "slots": slots,
                "candidates": candidates,
            }))
        }
        UiView::MagicGrid => {
            let cols = if ui.lang.is_en() { 2usize } else { 3usize };
            let rows = 6usize;
            let per_page = cols * rows;
            let rid = g
                .world
                .party
                .get(ui.active_role_idx)
                .map(|p| p.role_id as usize)
                .unwrap_or(0);
            let cur_mp = g.world.roles.get(rid).map(|r| r.mp).unwrap_or(0);
            let magics: Vec<serde_json::Value> = g
                .world
                .roles
                .get(rid)
                .map(|r| {
                    r.magics
                        .iter()
                        .copied()
                        .filter(|&m| m > 0)
                        .enumerate()
                        .map(|(i, mid)| {
                            let md = g.data.magics.get(&mid);
                            let cost = md.map(|m| m.cost_mp).unwrap_or(0);
                            serde_json::json!({
                                "idx": i,
                                "id": mid,
                                "name": g.magic_display_name(mid),
                                "cost_mp": cost,
                                "desc": g.magic_display_desc(mid),
                                "target": md.map(magic_target_kind).unwrap_or("enemy"),
                                "usable_field": md.map(|m| m.usable_outside_battle).unwrap_or(false),
                                "usable_battle": true,
                                "affordable": cur_mp >= cost,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let total_pages = magics.len().max(1).div_ceil(per_page);
            let page = ui.grid_cursor / per_page;
            Some(serde_json::json!({
                "screen": "magic_grid",
                "active_role_idx": ui.active_role_idx,
                "cursor": ui.grid_cursor,
                "cols": cols,
                "rows": rows,
                "page": page,
                "total_pages": total_pages,
                "magics": magics,
            }))
        }
        UiView::SystemSubmenu => {
            let mut items = Vec::new();
            if !ui.replay_mode {
                items.push(serde_json::json!({
                    "label": tr_lang(lang, "存储进度", "儲存進度", "Save Game"),
                    "value": serde_json::Value::Null,
                }));
            }
            items.push(serde_json::json!({
                "label": tr_lang(lang, "读取进度", "讀取進度", "Load Game"),
                "value": serde_json::Value::Null,
            }));
            items.push(serde_json::json!({
                "label": tr_lang(lang, "音乐", "音樂", "Music"),
                "value": on_off(ui.music_on),
            }));
            items.push(serde_json::json!({
                "label": tr_lang(lang, "音效", "音效", "SFX"),
                "value": on_off(ui.sfx_on),
            }));
            items.push(serde_json::json!({
                "label": tr_lang(lang, "返回标题", "返回標題", "Title Screen"),
                "value": serde_json::Value::Null,
            }));
            Some(serde_json::json!({
                "screen": "system",
                "cursor": ui.sys_sub_cursor,
                "items": items,
            }))
        }
        UiView::RemakeSubmenu => {
            let items: Vec<serde_json::Value> = if ui.replay_mode {
                vec![
                    serde_json::json!({ "label": tr_lang(lang, "画质", "畫質", "Mode"), "value": hd_val }),
                    serde_json::json!({ "label": tr_lang(lang, "文字", "文字", "Lang"), "value": lang_val }),
                    serde_json::json!({ "label": tr_lang(lang, "打开录像 [F8]", "開啟錄像 [F8]", "Open Replay [F8]"), "value": serde_json::Value::Null }),
                    serde_json::json!({ "label": tr_lang(lang, "画面截图 [F10]", "畫面截圖 [F10]", "Screenshot [F10]"), "value": serde_json::Value::Null }),
                    serde_json::json!({ "label": tr_lang(lang, "全屏", "全螢幕", "Fullscreen"), "value": on_off(ui.fullscreen) }),
                ]
            } else {
                vec![
                    serde_json::json!({ "label": tr_lang(lang, "导出存档", "匯出存檔", "Export Save"), "value": serde_json::Value::Null }),
                    serde_json::json!({ "label": tr_lang(lang, "导入存档", "匯入存檔", "Import Save"), "value": serde_json::Value::Null }),
                    serde_json::json!({ "label": tr_lang(lang, "画质", "畫質", "Mode"), "value": hd_val }),
                    serde_json::json!({ "label": tr_lang(lang, "文字", "文字", "Lang"), "value": lang_val }),
                    serde_json::json!({ "label": tr_lang(lang, "错误报告 [F9]", "錯誤報告 [F9]", "Bug Report [F9]"), "value": serde_json::Value::Null }),
                    serde_json::json!({ "label": tr_lang(lang, "画面截图 [F10]", "畫面截圖 [F10]", "Screenshot [F10]"), "value": serde_json::Value::Null }),
                    serde_json::json!({ "label": tr_lang(lang, "全屏", "全螢幕", "Fullscreen"), "value": on_off(ui.fullscreen) }),
                    serde_json::json!({ "label": tr_lang(lang, "AI 旁观", "AI 旁觀", "Agent"), "value": agent_val }),
                    serde_json::json!({ "label": tr_lang(lang, "重新开始录制", "重新開始錄製", "New Recording"), "value": serde_json::Value::Null }),
                    serde_json::json!({ "label": tr_lang(lang, "导出录像", "匯出錄像", "Export Replay"), "value": serde_json::Value::Null }),
                ]
            };
            Some(serde_json::json!({
                "screen": "remake",
                "cursor": ui.remake_sub_cursor,
                "items": items,
            }))
        }
        UiView::SaveLoadSlots(mode) => {
            let page_size = 5usize;
            let total = ui.save_slots.len().max(1);
            let total_pages = total.div_ceil(page_size);
            let page = ui.save_slot_cursor / page_size;
            let page_start = page * page_size;
            let page_items: Vec<serde_json::Value> = ui
                .save_slots
                .iter()
                .skip(page_start)
                .take(page_size)
                .map(|s| {
                    serde_json::json!({
                        "slot": s.slot,
                        "label": s.label,
                        "exists": s.exists,
                        "detail": s.detail,
                    })
                })
                .collect();
            Some(serde_json::json!({
                "screen": match mode {
                    SaveSlotMode::SaveInGame => "save_slots",
                    SaveSlotMode::LoadFromTitle => "load_slots_title",
                    SaveSlotMode::LoadInGame => "load_slots",
                },
                "cursor": ui.save_slot_cursor,
                "page": page + 1,
                "total_pages": total_pages,
                "items": page_items,
            }))
        }
        UiView::ReplayChapters => {
            let items: Vec<serde_json::Value> = ui
                .replay_chapters
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "index": c.index,
                        "scene_id": c.scene_id,
                        "start_tick": c.start_tick,
                        "label": c.label,
                    })
                })
                .collect();
            Some(serde_json::json!({
                "screen": "replay_chapters",
                "cursor": ui.replay_chapter_cursor,
                "total": ui.replay_chapters.len(),
                "items": items,
            }))
        }
        UiView::Shop | UiView::PawnshopSell => Some(serde_json::json!({
            "screen": if ui.view == UiView::PawnshopSell { "pawnshop" } else { "shop" },
            "cursor": ui.shop_cursor,
            "confirm_armed_idx": ui.pawnshop_confirm_idx,
            "items": ui.shop_items.iter().enumerate().map(|(i, e)| serde_json::json!({
                "idx": i, "id": e.item_id, "name": e.name, "price": e.price, "count": e.count
            })).collect::<Vec<_>>(),
        })),
        UiView::BattleHud(phase) => {
            let active_hero = g.battle.as_ref().and_then(|b| b.prompting).unwrap_or(0);
            let rid = g
                .battle
                .as_ref()
                .and_then(|b| b.heroes.get(active_hero))
                .map(|h| h.role)
                .unwrap_or(0);
            let mut obj = serde_json::json!({
                "screen": ui_view_id(&UiView::BattleHud(*phase)),
                "cmd_cursor": ui.battle_cmd_cursor,
                "misc_cursor": ui.battle_misc_cursor,
                "enemy_cursor": ui.battle_enemy_cursor,
                "ally_cursor": ui.battle_ally_cursor,
                "grid_cursor": ui.grid_cursor,
                "wheel": ["Up:Attack", "Left:Magic", "Right:Coop", "Down:Misc"],
            });
            if let Some(map) = obj.as_object_mut() {
                match phase {
                    BattleSubPhase::MiscMenu => {
                        map.insert(
                            "items".into(),
                            serde_json::json!([
                                tr_lang(lang, "物品", "物品", "Item"),
                                tr_lang(lang, "防御", "防禦", "Defend"),
                                tr_lang(lang, "围攻", "圍攻", "Auto"),
                                tr_lang(lang, "逃跑", "逃跑", "Flee"),
                            ]),
                        );
                    }
                    BattleSubPhase::SelectMagic => {
                        let cur_mp = g.world.roles.get(rid).map(|r| r.mp).unwrap_or(0);
                        let magics: Vec<serde_json::Value> = g
                            .world
                            .roles
                            .get(rid)
                            .map(|r| {
                                r.magics
                                    .iter()
                                    .copied()
                                    .filter(|&m| m > 0)
                                    .enumerate()
                                    .map(|(i, mid)| {
                                        let md = g.data.magics.get(&mid);
                                        let cost = md.map(|m| m.cost_mp).unwrap_or(0);
                                        serde_json::json!({
                                            "idx": i,
                                            "id": mid,
                                            "name": g.magic_display_name(mid),
                                            "cost_mp": cost,
                                            "desc": g.magic_display_desc(mid),
                                            "target": md.map(magic_target_kind).unwrap_or("enemy"),
                                            "affordable": cur_mp >= cost,
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        map.insert("cols".into(), serde_json::json!(2));
                        map.insert("rows".into(), serde_json::json!(6));
                        map.insert("magics".into(), serde_json::Value::Array(magics));
                    }
                    BattleSubPhase::SelectItem => {
                        let items: Vec<serde_json::Value> = g
                            .world
                            .inventory
                            .iter()
                            .filter(|(&id, &q)| {
                                q > 0
                                    && g.data
                                        .items
                                        .get(&id)
                                        .is_some_and(|it| it.usable || it.throwable)
                            })
                            .enumerate()
                            .map(|(i, (&id, &qty))| {
                                let it = g.data.items.get(&id);
                                serde_json::json!({
                                    "idx": i,
                                    "id": id,
                                    "name": g.item_display_name(id),
                                    "qty": qty,
                                    "desc": g.item_display_desc(id),
                                    "usable": it.map(|x| x.usable).unwrap_or(false),
                                    "throwable": it.map(|x| x.throwable).unwrap_or(false),
                                    "target": it.map(item_target_kind).unwrap_or("none"),
                                })
                            })
                            .collect();
                        map.insert("cols".into(), serde_json::json!(2));
                        map.insert("rows".into(), serde_json::json!(6));
                        map.insert("items".into(), serde_json::Value::Array(items));
                    }
                    _ => {}
                }
            }
            Some(obj)
        }
        UiView::BattleVictory => Some(serde_json::json!({
            "screen": "battle_victory",
            "exp": ui.battle_victory.exp,
            "cash": ui.battle_victory.cash,
            "level_ups": ui.battle_victory.level_ups,
        })),
    }
}
