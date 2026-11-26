//! `pal-ui` — Pure-Rust 1995 DOS PAL UI State Machine, Input Router & Software Framebuffer Renderer.

pub mod draw;
pub mod input;
pub mod state;

pub use draw::{
    draw_ai_thought_overlay, draw_debug_watermark, draw_ui, layout_dialog_pages,
    should_draw_ai_thought_overlay,
};
pub use input::{HitRect, HitTag, PointerEvent, PointerKind, UiAction, UiKey};
pub use state::{
    download_all_text, BattleSubPhase, BattleVictoryInfo, DialogModel, DownloadAllState,
    ReplayChapterSummary, RoleSelectTarget,
    SaveSlotMode, SaveSlotSummary, ShopEntry, UiCatalog, UiState, UiView,
};

#[cfg(test)]
mod tests {
    use super::*;
    use pal_core::state::GameState;
    use pal_render::cache::AssetCache;
    use pal_render::font::FontEngine;
    use pal_render::framebuffer::Framebuffer;
    use pal_render::world::{BattleEnemyVisual, BattleVisuals};

    #[test]
    fn test_cascading_menu_keyboard_and_cancel_close() {
        let state = GameState::new();
        let mut ui = UiState::new();
        ui.set_view(UiView::None);

        // Open RootMenu with MenuToggle (Alt/Esc)
        ui.on_key(UiKey::MenuToggle, &state, None);
        assert_eq!(ui.view, UiView::RootMenu);

        // Enter Status (FBP_000) directly since party len == 1
        ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(ui.view, UiView::StatusFbp000);

        // Esc back to RootMenu
        ui.on_key(UiKey::Cancel, &state, None);
        assert_eq!(ui.view, UiView::RootMenu);

        // Move down to 物品 (index 2), enter ItemSubmenu, select 裝備 -> EquipFbp001
        ui.on_key(UiKey::Down, &state, None);
        ui.on_key(UiKey::Down, &state, None);
        ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(ui.view, UiView::ItemSubmenu);

        ui.on_key(UiKey::Down, &state, None);
        ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(ui.view, UiView::EquipFbp001);

        // Back out with Cancel all the way to Field (UiView::None)
        ui.on_key(UiKey::Cancel, &state, None);
        assert_eq!(ui.view, UiView::ItemSubmenu);
        ui.on_key(UiKey::Cancel, &state, None);
        assert_eq!(ui.view, UiView::RootMenu);
        ui.on_key(UiKey::Cancel, &state, None);
        assert_eq!(ui.view, UiView::None);
    }

    #[test]
    fn test_title_pointer_hit_test() {
        let state = GameState::new();
        let mut ui = UiState::new();
        let mut fb = Framebuffer::new(320, 200);
        let mut cache = AssetCache::default();
        let mut fonts = FontEngine::new();

        draw_ui(&mut fb, &mut cache, &mut fonts, &mut ui, &state, None, false);
        assert_eq!(ui.hit_rects.len(), 3);

        // Click "Options" pill on Title screen -> opens TitleOptions menu
        let r_opt = ui.hit_rects[2];
        let act_opt = ui.on_pointer(
            PointerEvent {
                kind: PointerKind::Down,
                x: r_opt.x + 10,
                y: r_opt.y + 5,
            },
            &state,
            None,
        );
        assert_eq!(act_opt, UiAction::None);
        assert_eq!(ui.view, UiView::TitleOptions);

        // Inside TitleOptions, item 2 toggles language
        ui.title_options_cursor = 2;
        assert_eq!(ui.on_key(UiKey::Confirm, &state, None), UiAction::ToggleLanguage);
        assert_eq!(ui.lang, pal_core::Language::ZhHans);

        // Cancel returns to Title screen
        ui.on_key(UiKey::Cancel, &state, None);
        assert_eq!(ui.view, UiView::Title);
        draw_ui(&mut fb, &mut cache, &mut fonts, &mut ui, &state, None, false);

        // Click "新的故事"
        let r0 = ui.hit_rects[0];
        let action = ui.on_pointer(
            PointerEvent {
                kind: PointerKind::Down,
                x: r0.x + 10,
                y: r0.y + 5,
            },
            &state,
            None,
        );
        assert_eq!(action, UiAction::NewGame);
        assert_eq!(ui.view, UiView::None);
    }

    #[test]
    fn test_battle_target_selection_keyboard() {
        let state = GameState::new();
        let mut ui = UiState::new();
        ui.open_battle();

        let mut battle = BattleVisuals {
            active: true,
            battlefield_fbp: 10,
            enemies: vec![
                BattleEnemyVisual {
                    slot: 0,
                    sprite_id: 1,
                    alive: true,
                    name: "苗人頭領".into(),
                    ..Default::default()
                },
                BattleEnemyVisual {
                    slot: 1,
                    sprite_id: 2,
                    alive: true,
                    name: "黑苗戰士".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        // Confirm Attack -> enters SelectEnemyForAttack with enemy 0 selected
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::SelectEnemyForAttack));
        assert_eq!(battle.selected_enemy, Some(0));

        // Right -> selects enemy 1
        ui.on_key(UiKey::Right, &state, Some(&mut battle));
        assert_eq!(battle.selected_enemy, Some(1));

        // Confirm -> emits BattleAttack { enemy_idx: 1 }
        let act = ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        assert_eq!(act, UiAction::BattleAttack { enemy_idx: 1 });
        assert_eq!(battle.selected_enemy, None);
    }

    #[test]
    fn test_system_submenu_language_and_replay_issue_64() {
        let state = GameState::new();
        let mut ui = UiState::new();

        // Classic System submenu has 5 items (#91)
        ui.set_view(UiView::SystemSubmenu);
        ui.sys_sub_cursor = 2; // 音樂
        assert_eq!(ui.on_key(UiKey::Confirm, &state, None), UiAction::ToggleMusic);
        ui.sys_sub_cursor = 4; // 回到標題
        assert_eq!(ui.on_key(UiKey::Confirm, &state, None), UiAction::ReturnToTitle);

        // Dedicated Remake submenu holds Remake-only tools (#91, #85, #78, #64)
        ui.set_view(UiView::RemakeSubmenu);
        ui.remake_sub_cursor = 0; // 匯出存檔 (#78)
        assert_eq!(ui.on_key(UiKey::Confirm, &state, None), UiAction::ExportSave);

        ui.remake_sub_cursor = 1; // 匯入存檔 (#78)
        assert_eq!(ui.on_key(UiKey::Confirm, &state, None), UiAction::ImportSave);

        ui.remake_sub_cursor = 3; // 文字：繁體/簡體/English (#64)
        assert!(ui.traditional_chinese);
        assert_eq!(ui.lang, pal_core::Language::ZhHant);
        let a1 = ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(a1, UiAction::ToggleLanguage);
        assert!(!ui.traditional_chinese);
        assert_eq!(ui.lang, pal_core::Language::ZhHans);
        let a2 = ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(a2, UiAction::ToggleLanguage);
        assert_eq!(ui.lang, pal_core::Language::En);
        let a3 = ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(a3, UiAction::ToggleLanguage);
        assert_eq!(ui.lang, pal_core::Language::ZhHant);

        ui.remake_sub_cursor = 4; // 錄像：複製報告 (#64, #76)
        assert_eq!(ui.on_key(UiKey::Confirm, &state, None), UiAction::CopyReplayReport);

        ui.remake_sub_cursor = 5; // 畫面截圖 [F10] (#85)
        assert_eq!(ui.on_key(UiKey::Confirm, &state, None), UiAction::TakeScreenshot);
    }

    #[test]
    fn test_battle_magic_item_defend_issue_82() {
        let mut state = GameState::new();
        state.party.push(pal_core::state::PartyMember {
            role_id: 0,
            x: 0,
            y: 0,
            dir: 0,
            frame: 0,
        });
        state.roles[0].magics = vec![301, 315];
        state.inventory.insert(99, 3);
        let mut battle = BattleVisuals::default();
        battle.active = true;
        battle.enemies.push(pal_render::world::BattleEnemyVisual {
            sprite_id: 1,
            name: "蜂巢".into(),
            hp: 100,
            max_hp: 100,
            alive: true,
            ..Default::default()
        });

        let mut ui = UiState::new();
        ui.open_battle();

        // Authentic 4-way Bagua diamond cross: Left selects 仙術 (cursor 1) -> Confirm opens SelectMagic
        ui.on_key(UiKey::Left, &state, Some(&mut battle));
        assert_eq!(ui.battle_cmd_cursor, 1);
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::SelectMagic));
        ui.on_key(UiKey::Right, &state, Some(&mut battle));
        let act_m = ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        assert_eq!(act_m, UiAction::BattleCastMagic { magic_id: 315, enemy_idx: 0 });

        // Down selects 雜項 (cursor 3) -> Confirm opens MiscMenu -> Item (0) opens SelectItem -> Confirm uses item 99
        ui.on_key(UiKey::Down, &state, Some(&mut battle));
        assert_eq!(ui.battle_cmd_cursor, 3);
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::MiscMenu));
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::SelectItem));
        let act_i = ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        assert_eq!(act_i, UiAction::BattleUseItem { item_id: 99 });

        // MiscMenu -> Down selects 防禦 (1) -> Confirm emits BattleDefend
        ui.on_key(UiKey::Down, &state, Some(&mut battle));
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::MiscMenu));
        ui.on_key(UiKey::Down, &state, Some(&mut battle));
        let act_d = ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        assert_eq!(act_d, UiAction::BattleDefend);
    }

    #[test]
    fn test_dialog_pagination_three_lines_per_page_keeps_speaker() {
        let state = GameState::new();
        let mut ui = UiState::new();
        ui.set_lang(pal_core::Language::En);
        let long_text = "Xiaoyao, stop lazing around in bed! Guests are already knocking at the inn door, and you haven't even swept the courtyard or heated the morning tea yet! If you keep daydreaming about becoming some famous swordsman, who is going to help me run this place?";
        ui.open_dialog("Auntie Li", long_text, Some(55), false);
        let mut fb = pal_render::framebuffer::Framebuffer::new(320, 200);
        let mut cache = pal_render::cache::AssetCache::new(1024 * 1024);
        let mut fonts = pal_render::font::FontEngine::new();
        draw::draw_ui(&mut fb, &mut cache, &mut fonts, &mut ui, &state, None, false);

        assert!(ui.dialog.total_pages >= 2, "Long English dialog must paginate into >=2 pages");
        assert_eq!(ui.dialog.page, 0);
        assert_eq!(ui.dialog.speaker, "Auntie Li");

        // Confirm on page 0 advances to page 1 without closing dialog
        let act1 = ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(act1, UiAction::None);
        assert_eq!(ui.view, UiView::Dialog);
        assert_eq!(ui.dialog.page, 1);
        assert_eq!(ui.dialog.speaker, "Auntie Li");

        // Advance to last page, then Confirm dismisses dialog
        ui.dialog.page = ui.dialog.total_pages - 1;
        let act_end = ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(act_end, UiAction::DialogAdvance);
        assert_eq!(ui.view, UiView::None);
    }

    #[test]
    fn test_per_role_battle_and_field_cursor_memory() {
        let mut state = GameState::new();
        state.party.push(pal_core::state::PartyMember {
            role_id: 1,
            x: 100,
            y: 100,
            dir: 0,
            frame: 0,
        });
        state.roles[0].magics = vec![296, 297];
        state.roles[1].magics = vec![300, 301];
        state.inventory.insert(99, 3);
        state.inventory.insert(101, 2);

        let mut ui = UiState::new();
        let mut battle = BattleVisuals::default();
        battle.active = true;
        battle.enemies = vec![
            BattleEnemyVisual {
                slot: 0,
                alive: true,
                ..Default::default()
            },
            BattleEnemyVisual {
                slot: 1,
                alive: true,
                ..Default::default()
            },
        ];

        // Turn 1, Role 0 casts 2nd magic (#297)
        battle.active_hero = 0;
        ui.open_battle_for_role(0);
        ui.on_key(UiKey::Left, &state, Some(&mut battle));
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        ui.on_key(UiKey::Right, &state, Some(&mut battle));
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, Some(&mut battle)),
            UiAction::BattleCastMagic {
                magic_id: 297,
                enemy_idx: 0
            }
        );

        // Turn 1, Role 1 uses 2nd item (#101)
        battle.active_hero = 1;
        ui.open_battle_for_role(1);
        ui.on_key(UiKey::Down, &state, Some(&mut battle));
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        ui.on_key(UiKey::Right, &state, Some(&mut battle));
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, Some(&mut battle)),
            UiAction::BattleUseItem { item_id: 101 }
        );

        // Turn 2, Role 0: just press Confirm twice to repeat magic #297!
        battle.active_hero = 0;
        ui.open_battle_for_role(0);
        assert_eq!(ui.battle_cmd_cursor, 1);
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, Some(&mut battle)),
            UiAction::BattleCastMagic {
                magic_id: 297,
                enemy_idx: 0
            }
        );

        // Turn 2, Role 1: just press Confirm 3 times (Misc -> Item -> #101) to repeat item #101!
        battle.active_hero = 1;
        ui.open_battle_for_role(1);
        assert_eq!(ui.battle_cmd_cursor, 3);
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, Some(&mut battle)),
            UiAction::BattleUseItem { item_id: 101 }
        );

        // Field menu: ItemGrid remembers #101 across close/reopen
        ui.set_view(UiView::ItemSubmenu);
        ui.item_sub_cursor = 0;
        ui.on_key(UiKey::Confirm, &state, None);
        ui.on_key(UiKey::Right, &state, None);
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, None),
            UiAction::UseItem {
                item_id: 101,
                role_idx: 0
            }
        );
        ui.set_view(UiView::ItemSubmenu);
        ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, None),
            UiAction::UseItem {
                item_id: 101,
                role_idx: 0
            }
        );

        // Field menu: MagicGrid remembers per-role spell across reopen via RoleSelect (#274: no Tab cycling)
        ui.active_role_idx = 0;
        ui.set_view(UiView::MagicGrid);
        ui.grid_cursor = 1;
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, None),
            UiAction::CastMagic {
                magic_id: 297,
                role_idx: 0
            }
        );
        // Tab no longer cycles roles in gameplay menus
        ui.on_key(UiKey::Tab, &state, None);
        assert_eq!(ui.active_role_idx, 0);
        // Re-open MagicGrid via RoleSelect for role 1, then back to role 0
        ui.set_view(UiView::RoleSelect(RoleSelectTarget::Magic));
        ui.role_cursor = 1;
        ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(ui.view, UiView::MagicGrid);
        assert_eq!(ui.active_role_idx, 1);
        assert_eq!(ui.grid_cursor, 0);
        ui.set_view(UiView::RoleSelect(RoleSelectTarget::Magic));
        ui.role_cursor = 0;
        ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(ui.active_role_idx, 0);
        assert_eq!(ui.grid_cursor, 1);
    }

    #[test]
    fn test_exclusive_replay_menu_and_chapter_selector() {
        let state = GameState::new();
        let mut ui = UiState::new();
        ui.replay_mode = true;
        ui.replay_chapters = vec![
            ReplayChapterSummary {
                index: 0,
                start_tick: 0,
                scene_id: 1,
                label: "余杭客栈".into(),
                thumb_rgba: Some((2, 2, vec![255; 16])),
            },
            ReplayChapterSummary {
                index: 1,
                start_tick: 1500,
                scene_id: 2,
                label: "余杭镇".into(),
                thumb_rgba: None,
            },
        ];
        ui.set_view(UiView::RootMenu);

        // Item 0 -> Chapters
        assert_eq!(ui.on_key(UiKey::Confirm, &state, None), UiAction::None);
        assert_eq!(ui.view, UiView::ReplayChapters);

        // Select chapter 1 and confirm -> ReplayJumpChapter(1)
        ui.on_key(UiKey::Down, &state, None);
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, None),
            UiAction::ReplayJumpChapter(1)
        );
        assert_eq!(ui.view, UiView::None);

        // Item 1 -> Take Over
        ui.set_view(UiView::RootMenu);
        ui.root_cursor = 1;
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, None),
            UiAction::ReplayTakeOver
        );

        // System submenu in replay mode hides Save Game (item 0 is Load Game)
        ui.set_view(UiView::RootMenu);
        ui.root_cursor = 2;
        ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(ui.view, UiView::SystemSubmenu);
        ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(ui.view, UiView::SaveLoadSlots(SaveSlotMode::LoadInGame));
    }

    #[test]
    fn test_battle_ally_and_enemy_magic_targeting_and_victory_modal() {
        let mut state = GameState::new();
        state.party = vec![
            pal_core::state::PartyMember { role_id: 0, x: 0, y: 0, dir: 0, frame: 0 },
            pal_core::state::PartyMember { role_id: 1, x: 0, y: 0, dir: 0, frame: 0 },
        ];
        state.roles[0].magics = vec![297, 305]; // 297=观音咒 (ally single), 305=御剑术 (enemy single)

        let mut ui = UiState::new();
        ui.catalog.magic_names.insert(297, "观音咒".into());
        ui.catalog.magic_target_enemy.insert(297, false);
        ui.catalog.magic_target_all.insert(297, false);
        ui.catalog.magic_names.insert(305, "御剑术".into());
        ui.catalog.magic_target_enemy.insert(305, true);
        ui.catalog.magic_target_all.insert(305, false);

        let mut battle = BattleVisuals {
            active: true,
            prompting: true,
            heroes: vec![
                pal_render::world::BattleHeroVisual {
                    role_id: 0, hp: 100, max_hp: 100, name: "李逍遥".into(), ..Default::default()
                },
                pal_render::world::BattleHeroVisual {
                    role_id: 1, hp: 20, max_hp: 100, name: "赵灵儿".into(), ..Default::default()
                },
            ],
            enemies: vec![
                BattleEnemyVisual { slot: 0, alive: true, name: "苗人A".into(), ..Default::default() },
                BattleEnemyVisual { slot: 1, alive: true, name: "苗人B".into(), ..Default::default() },
            ],
            ..Default::default()
        };

        // Pick 观音咒 (#297) -> enters SelectAllyForMagic defaulting to lowest HP% ally (idx 1 = 赵灵儿)
        ui.open_battle();
        ui.on_key(UiKey::Left, &state, Some(&mut battle));
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle));
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::SelectMagic));
        assert_eq!(ui.on_key(UiKey::Confirm, &state, Some(&mut battle)), UiAction::None);
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::SelectAllyForMagic(297)));
        assert_eq!(battle.selected_ally, Some(1));
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, Some(&mut battle)),
            UiAction::BattleCastMagic { magic_id: 297, enemy_idx: 1 }
        );
        assert_eq!(battle.selected_ally, None);

        // Pick 御剑术 (#305) with 2 alive enemies -> enters SelectEnemyForMagic
        ui.open_battle();
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle)); // opens SelectMagic (remembered Magic)
        ui.on_key(UiKey::Right, &state, Some(&mut battle)); // cursor -> #305
        assert_eq!(ui.on_key(UiKey::Confirm, &state, Some(&mut battle)), UiAction::None);
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::SelectEnemyForMagic(305)));
        ui.on_key(UiKey::Right, &state, Some(&mut battle)); // enemy 1
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, Some(&mut battle)),
            UiAction::BattleCastMagic { magic_id: 305, enemy_idx: 1 }
        );

        // BattleVictory modal dismisses on Confirm -> UiAction::DialogAdvance
        ui.battle_victory = BattleVictoryInfo { exp: 120, cash: 80, level_ups: vec!["李逍遥 Lv.3".into()] };
        ui.set_view(UiView::BattleVictory);
        assert_eq!(ui.on_key(UiKey::Confirm, &state, Some(&mut battle)), UiAction::DialogAdvance);
        assert_eq!(ui.view, UiView::None);
    }

    #[test]
    fn test_battle_coop_item_targeting_and_auto_cancel() {
        let mut state = GameState::new();
        state.party = vec![
            pal_core::state::PartyMember { role_id: 0, x: 0, y: 0, dir: 0, frame: 0 },
            pal_core::state::PartyMember { role_id: 1, x: 0, y: 0, dir: 0, frame: 0 },
        ];
        state.inventory.insert(71, 3); // 止血草 (usable ally heal)
        state.inventory.insert(140, 2); // 蜂巢 (throwable enemy weapon)
        state.inventory.insert(166, 1); // 木剑 (equipment - should be filtered out in battle!)

        let mut ui = UiState::new();
        ui.catalog.item_usable.insert(71, true);
        ui.catalog.item_throwable.insert(71, false);
        ui.catalog.item_usable.insert(140, false);
        ui.catalog.item_throwable.insert(140, true);
        ui.catalog.item_usable.insert(166, false);
        ui.catalog.item_throwable.insert(166, false);

        let mut battle = BattleVisuals {
            active: true,
            prompting: true,
            heroes: vec![
                pal_render::world::BattleHeroVisual {
                    role_id: 0, hp: 100, max_hp: 100, name: "李逍遥".into(), ..Default::default()
                },
                pal_render::world::BattleHeroVisual {
                    role_id: 1, hp: 15, max_hp: 100, name: "赵灵儿".into(), ..Default::default()
                },
            ],
            enemies: vec![
                BattleEnemyVisual { slot: 0, alive: true, name: "苗人A".into(), ..Default::default() },
                BattleEnemyVisual { slot: 1, alive: true, name: "苗人B".into(), ..Default::default() },
            ],
            ..Default::default()
        };

        // 1. Coop Magic (Right on wheel -> Confirm -> SelectEnemyForCoop -> Confirm)
        ui.open_battle();
        ui.on_key(UiKey::Right, &state, Some(&mut battle));
        assert_eq!(ui.on_key(UiKey::Confirm, &state, Some(&mut battle)), UiAction::None);
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::SelectEnemyForCoop));
        ui.on_key(UiKey::Right, &state, Some(&mut battle)); // target enemy 1
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, Some(&mut battle)),
            UiAction::BattleCoop { enemy_idx: 1 }
        );

        // 2. Battle usable item list filters out equipment (#166), heals lowest HP% ally (#71 -> ally 1)
        assert_eq!(ui.battle_usable_items(&state).len(), 2);
        ui.open_battle();
        ui.on_key(UiKey::Down, &state, Some(&mut battle));
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle)); // MiscMenu
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle)); // SelectItem (item 0 = #71)
        assert_eq!(ui.on_key(UiKey::Confirm, &state, Some(&mut battle)), UiAction::None);
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::SelectAllyForItem(71)));
        assert_eq!(battle.selected_ally, Some(1));
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, Some(&mut battle)),
            UiAction::BattleUseItemTarget { item_id: 71, target: 1 }
        );

        // 3. Throwable item (#140) selects enemy target
        ui.open_battle();
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle)); // MiscMenu (remembered Misc)
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle)); // SelectItem
        ui.on_key(UiKey::Right, &state, Some(&mut battle)); // cursor -> #140
        assert_eq!(ui.on_key(UiKey::Confirm, &state, Some(&mut battle)), UiAction::None);
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::SelectEnemyForItem(140)));
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, Some(&mut battle)),
            UiAction::BattleUseItemTarget { item_id: 140, target: 1 }
        );

        // 4. MiscMenu -> Auto (cursor 2) emits BattleAuto; Cancel while auto_battle emits BattleCancelAuto
        ui.open_battle();
        ui.on_key(UiKey::Confirm, &state, Some(&mut battle)); // MiscMenu
        ui.on_key(UiKey::Down, &state, Some(&mut battle)); // Defend (1)
        ui.on_key(UiKey::Down, &state, Some(&mut battle)); // Auto (2)
        assert_eq!(ui.on_key(UiKey::Confirm, &state, Some(&mut battle)), UiAction::BattleAuto);
        battle.auto_battle = true;
        assert_eq!(ui.on_key(UiKey::Cancel, &state, Some(&mut battle)), UiAction::BattleCancelAuto);
    }

    #[test]
    fn test_batch3_dialog_sanitize_pawnshop_confirm_and_title_options() {
        // #182: Unmatched trailing '(' and ')' stripped, balanced parens kept.
        assert_eq!(UiState::sanitize_dialog_text("什么事？)"), "什么事？");
        assert_eq!(UiState::sanitize_dialog_text("啊．．("), "啊．．");
        assert_eq!(UiState::sanitize_dialog_text("说明(完整)"), "说明(完整)");

        // #181: PawnshopSell requires 2-step confirmation; Cancel disarms first, Up/Down disarms.
        let mut state = GameState::new();
        state.inventory.insert(101, 3);
        state.inventory.insert(102, 2);
        let mut ui = UiState::new();
        ui.open_pawnshop_from_inventory(&state);
        assert_eq!(ui.view, UiView::PawnshopSell);
        assert_eq!(ui.pawnshop_confirm_idx, None);

        // 1st Confirm arms row 0 without selling.
        assert_eq!(ui.on_key(UiKey::Confirm, &state, None), UiAction::None);
        assert_eq!(ui.pawnshop_confirm_idx, Some(0));

        // Cancel disarms without closing pawnshop.
        assert_eq!(ui.on_key(UiKey::Cancel, &state, None), UiAction::None);
        assert_eq!(ui.view, UiView::PawnshopSell);
        assert_eq!(ui.pawnshop_confirm_idx, None);

        // Re-arm then move Down -> disarms.
        assert_eq!(ui.on_key(UiKey::Confirm, &state, None), UiAction::None);
        assert_eq!(ui.pawnshop_confirm_idx, Some(0));
        assert_eq!(ui.on_key(UiKey::Down, &state, None), UiAction::None);
        assert_eq!(ui.pawnshop_confirm_idx, None);

        // Confirm twice on row 1 -> emits SellItem and clears armed state.
        let expected_id = ui.shop_items[1].item_id;
        let expected_price = ui.shop_items[1].price;
        assert_eq!(ui.on_key(UiKey::Confirm, &state, None), UiAction::None);
        assert_eq!(ui.pawnshop_confirm_idx, Some(1));
        assert_eq!(
            ui.on_key(UiKey::Confirm, &state, None),
            UiAction::SellItem {
                item_id: expected_id,
                price: expected_price,
            }
        );
        assert_eq!(ui.pawnshop_confirm_idx, None);

        // #161 & #187: TitleOptions renders without Title buttons; 4-digit HP & Gai Luojiao (role_id=5) draw cleanly.
        let mut fb = Framebuffer::new(320, 200);
        let mut cache = AssetCache::default();
        let mut fonts = FontEngine::new();
        ui.set_view(UiView::TitleOptions);
        draw_ui(&mut fb, &mut cache, &mut fonts, &mut ui, &state, None, false);
        assert!(
            ui.hit_rects.iter().all(|r| matches!(r.tag, HitTag::TitleOptionsItem(_))),
            "TitleOptions must not expose Title buttons underneath"
        );

        // LAN Agent Mode exposes clickable Copy + Refresh buttons in the bottom token bar.
        ui.agent_mode = 2;
        ui.agent_token = Some("0123456789abcdef0123456789abcdef".into());
        draw_ui(&mut fb, &mut cache, &mut fonts, &mut ui, &state, None, false);
        assert!(ui.hit_rects.iter().any(|r| r.tag == HitTag::CopyAgentToken));
        assert!(ui.hit_rects.iter().any(|r| r.tag == HitTag::RefreshAgentToken));
        assert_eq!(
            ui.on_pointer(PointerEvent { kind: PointerKind::Down, x: 255, y: 190 }, &state, None),
            UiAction::CopyAgentToken
        );
        assert_eq!(
            ui.on_pointer(PointerEvent { kind: PointerKind::Down, x: 295, y: 190 }, &state, None),
            UiAction::RefreshAgentToken
        );
        ui.agent_mode = 0;
        ui.agent_token = None;

        let battle = BattleVisuals {
            active: true,
            prompting: true,
            heroes: vec![pal_render::world::BattleHeroVisual {
                role_id: 5,
                avatar_rgm: 44,
                hp: 3600,
                max_hp: 3600,
                mp: 1200,
                max_mp: 1200,
                name: "蓋羅嬌".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        ui.set_view(UiView::BattleHud(BattleSubPhase::CommandWheel));
        draw_ui(&mut fb, &mut cache, &mut fonts, &mut ui, &state, Some(&battle), false);
    }

    #[test]
    fn test_sdlpal_navigation_paging_and_battle_shortcuts_274() {
        let mut state = GameState::new();
        state.party.push(pal_core::state::PartyMember {
            role_id: 1,
            x: 100,
            y: 100,
            dir: 0,
            frame: 0,
        });
        // Populate 30 items (some usable, some throwable) to test 7-row paging and E/W filtering
        for id in 60u16..90u16 {
            state.inventory.insert(id, 2);
        }
        let mut ui = UiState::new();
        ui.set_view(UiView::None);
        for id in 60u16..90u16 {
            ui.catalog.item_usable.insert(id, id % 2 == 0);
            ui.catalog.item_throwable.insert(id, id % 2 == 1);
        }

        // 1. Vertical menu Left/Right = prev/next (no close on Left!) & Root/System cursor memory
        ui.on_key(UiKey::MenuToggle, &state, None);
        assert_eq!(ui.view, UiView::RootMenu);
        assert_eq!(ui.root_cursor, 0);
        ui.on_key(UiKey::Right, &state, None); // moves next -> 1 (Magic)
        assert_eq!(ui.view, UiView::RootMenu);
        assert_eq!(ui.root_cursor, 1);
        ui.on_key(UiKey::Left, &state, None); // moves prev -> 0 (Status), does NOT close!
        assert_eq!(ui.view, UiView::RootMenu);
        assert_eq!(ui.root_cursor, 0);
        ui.on_key(UiKey::Left, &state, None); // wraps to last (Remake = 4)
        assert_eq!(ui.root_cursor, 4);
        ui.on_key(UiKey::Up, &state, None); // 3 (System)
        assert_eq!(ui.root_cursor, 3);
        // Close and reopen RootMenu -> remembers cursor 3 (System)!
        ui.on_key(UiKey::Cancel, &state, None);
        assert_eq!(ui.view, UiView::None);
        ui.on_key(UiKey::MenuToggle, &state, None);
        assert_eq!(ui.view, UiView::RootMenu);
        assert_eq!(ui.root_cursor, 3);

        // Enter SystemSubmenu, move to index 2, exit, re-enter -> remembers sys_sub_cursor = 2!
        ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(ui.view, UiView::SystemSubmenu);
        ui.on_key(UiKey::Down, &state, None);
        ui.on_key(UiKey::Right, &state, None);
        assert_eq!(ui.sys_sub_cursor, 2);
        ui.on_key(UiKey::Left, &state, None); // Left moves prev, does NOT close SystemSubmenu
        assert_eq!(ui.view, UiView::SystemSubmenu);
        assert_eq!(ui.sys_sub_cursor, 1);
        ui.on_key(UiKey::Cancel, &state, None);
        assert_eq!(ui.view, UiView::RootMenu);
        ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(ui.view, UiView::SystemSubmenu);
        assert_eq!(ui.sys_sub_cursor, 1);
        ui.on_key(UiKey::Cancel, &state, None);

        // 2. ItemGrid: Left clamps at 0 (no exit!) + PgDn/PgUp (7 rows = 21 items) + Home/End
        ui.root_cursor = 2; // Items
        ui.on_key(UiKey::Confirm, &state, None);
        assert_eq!(ui.view, UiView::ItemSubmenu);
        ui.on_key(UiKey::Confirm, &state, None); // Use -> ItemGrid
        assert_eq!(ui.view, UiView::ItemGrid);
        ui.grid_cursor = 0;
        ui.on_key(UiKey::Left, &state, None);
        assert_eq!(ui.view, UiView::ItemGrid, "Left at index 0 must clamp, not exit");
        assert_eq!(ui.grid_cursor, 0);
        ui.on_key(UiKey::PageDown, &state, None);
        assert_eq!(ui.grid_cursor, 21, "PgDn advances 7 rows x 3 cols = 21");
        ui.on_key(UiKey::End, &state, None);
        assert_eq!(ui.grid_cursor, 29, "End jumps to last item");
        ui.on_key(UiKey::PageUp, &state, None);
        assert_eq!(ui.grid_cursor, 8, "PgUp retreats 21 items");
        ui.on_key(UiKey::Home, &state, None);
        assert_eq!(ui.grid_cursor, 0, "Home jumps to first item");

        // 3. Battle shortcuts: R, A, D, E (UseOnly), W (ThrowOnly), Q, F, S (Status roundtrip)
        let mut battle = BattleVisuals {
            active: true,
            prompting: true,
            active_hero: 0,
            enemies: vec![BattleEnemyVisual {
                slot: 0,
                alive: true,
                name: "苗人".into(),
                ..Default::default()
            }],
            heroes: vec![
                pal_render::world::BattleHeroVisual {
                    role_id: 0,
                    hp: 100,
                    max_hp: 100,
                    name: "李逍遥".into(),
                    ..Default::default()
                },
                pal_render::world::BattleHeroVisual {
                    role_id: 1,
                    hp: 80,
                    max_hp: 80,
                    name: "赵灵儿".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        ui.open_battle_for_role(0);
        assert_eq!(
            ui.on_key(UiKey::BattleRepeat, &state, Some(&mut battle)),
            UiAction::BattleAttack { enemy_idx: 0 }
        );
        assert_eq!(ui.on_key(UiKey::BattleAuto, &state, Some(&mut battle)), UiAction::BattleAuto);
        assert_eq!(ui.on_key(UiKey::BattleDefend, &state, Some(&mut battle)), UiAction::BattleDefend);
        // Repeat after Defend repeats Defend!
        assert_eq!(ui.on_key(UiKey::BattleRepeat, &state, Some(&mut battle)), UiAction::BattleDefend);
        assert_eq!(ui.on_key(UiKey::BattleFlee, &state, Some(&mut battle)), UiAction::BattleFlee);
        assert_eq!(
            ui.on_key(UiKey::BattleForce, &state, Some(&mut battle)),
            UiAction::BattleAttack { enemy_idx: 0 }
        );

        // E -> SelectItem filtered to usable items only (even IDs: 60, 62, ...)
        assert_eq!(ui.on_key(UiKey::BattleUseItem, &state, Some(&mut battle)), UiAction::None);
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::SelectItem));
        let usable = ui.battle_usable_items(&state);
        assert!(!usable.is_empty() && usable.iter().all(|(id, _)| id % 2 == 0));
        ui.on_key(UiKey::Cancel, &state, Some(&mut battle)); // back to CommandWheel
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::CommandWheel));

        // W -> SelectItem filtered to throwable items only (odd IDs: 61, 63, ...)
        assert_eq!(ui.on_key(UiKey::BattleThrowItem, &state, Some(&mut battle)), UiAction::None);
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::SelectItem));
        let throwable = ui.battle_usable_items(&state);
        assert!(!throwable.is_empty() && throwable.iter().all(|(id, _)| id % 2 == 1));
        ui.on_key(UiKey::Cancel, &state, Some(&mut battle));
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::CommandWheel));

        // S -> opens StatusFbp000 from battle; Left/Right cycles party; Cancel returns to BattleHud!
        assert_eq!(ui.on_key(UiKey::BattleStatus, &state, Some(&mut battle)), UiAction::None);
        assert_eq!(ui.view, UiView::StatusFbp000);
        assert_eq!(ui.active_role_idx, 0);
        ui.on_key(UiKey::Right, &state, Some(&mut battle));
        assert_eq!(ui.active_role_idx, 1);
        ui.on_key(UiKey::Cancel, &state, Some(&mut battle));
        assert_eq!(ui.view, UiView::BattleHud(BattleSubPhase::CommandWheel));
    }

    #[test]
    fn test_dual_speaker_dialog_persistence() {
        let mut ui = UiState::new();
        ui.open_dialog("李大娘", "逍遙，快起床！", Some(26), true);
        assert!(ui.prev_dialog.is_none());

        // Switching to the opposite side (bottom) preserves the top speaker on screen.
        ui.open_dialog("李逍遙", "嬸嬸，我這就起來。", Some(2), false);
        let prev = ui.prev_dialog.as_ref().expect("top speaker should be preserved");
        assert_eq!(prev.speaker, "李大娘");
        assert!(prev.at_top);
        assert_eq!(prev.portrait_rgm, Some(26));

        // A centered system notice clears dual-speaker history.
        ui.open_dialog("", "獲得 止血草", None, false);
        assert!(ui.prev_dialog.is_none());

        // Closing the dialog clears prev_dialog as well.
        ui.open_dialog("李大娘", "去招呼客人吧。", Some(26), true);
        ui.open_dialog("李逍遙", "好的！", Some(2), false);
        assert!(ui.prev_dialog.is_some());
        ui.set_view(UiView::None);
        assert!(ui.prev_dialog.is_none());
    }
}
