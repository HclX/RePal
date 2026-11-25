//! Mouse/touch pointer hit-testing and click dispatch for all UI screens.

use super::*;

impl UiState {
    /// Process a mouse/touch pointer event in logical `320x200` space.
    pub fn on_pointer(&mut self, ev: PointerEvent, state: &GameState, mut battle: Option<&mut BattleVisuals>) -> UiAction {
        if ev.kind == PointerKind::RightClick {
            return self.on_key(UiKey::Cancel, state, battle);
        }
        let hit = self
            .hit_rects
            .iter()
            .rev()
            .find(|r| r.contains(ev.x, ev.y))
            .map(|r| r.tag);

        let Some(tag) = hit else {
            return UiAction::None;
        };

        let is_click = ev.kind == PointerKind::Down;
        match tag {
            HitTag::TitleOption(i) => {
                if self.title_cursor != i {
                    self.title_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::TitleLangToggle => {
                if is_click {
                    self.title_options_cursor = 0;
                    self.view = UiView::TitleOptions;
                    self.dirty = true;
                }
            }
            HitTag::TitleOptionsItem(i) => {
                if self.title_options_cursor != i {
                    self.title_options_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::ReplayFinishedItem(i) => {
                if self.replay_finished_cursor != i {
                    self.replay_finished_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::RootMenuItem(i) => {
                if self.root_cursor != i {
                    self.root_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::RoleSelectItem(i) => {
                if self.role_cursor != i {
                    self.role_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::ItemSubmenuItem(i) => {
                if self.item_sub_cursor != i {
                    self.item_sub_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::SystemSubmenuItem(i) => {
                if self.sys_sub_cursor != i {
                    self.sys_sub_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::RemakeSubmenuItem(i) => {
                if self.remake_sub_cursor != i {
                    self.remake_sub_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::GridCell(i) => {
                if self.grid_cursor != i {
                    self.grid_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::EquipSlot(i) => {
                if self.equip_slot_cursor != i {
                    self.equip_slot_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::SaveSlot(i) => {
                if self.save_slot_cursor != i {
                    self.save_slot_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::DialogConfirm => {
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::DialogChoice(i) => {
                if self.dialog.choice_cursor != i {
                    self.dialog.choice_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::BattleCommand(i) => {
                if self.battle_cmd_cursor != i {
                    self.battle_cmd_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    if let Some(b) = battle.as_deref_mut() {
                        b.selected_enemy = None;
                    }
                    self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::BattleMiscItem(i) => {
                if self.battle_misc_cursor != i {
                    self.battle_misc_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::BattleEnemy(i) => {
                if self.battle_enemy_cursor != i {
                    self.battle_enemy_cursor = i;
                    self.dirty = true;
                }
                if let Some(b) = battle.as_deref_mut() {
                    if is_click {
                        b.selected_enemy = None;
                    } else if b.selected_enemy != Some(i) {
                        b.selected_enemy = Some(i);
                        self.dirty = true;
                    }
                }
                if is_click {
                    match self.view {
                        UiView::BattleHud(BattleSubPhase::SelectEnemyForMagic(magic_id)) => {
                            self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                            self.dirty = true;
                            return UiAction::BattleCastMagic {
                                magic_id,
                                enemy_idx: i,
                            };
                        }
                        UiView::BattleHud(BattleSubPhase::SelectEnemyForCoop) => {
                            self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                            self.dirty = true;
                            return UiAction::BattleCoop { enemy_idx: i };
                        }
                        UiView::BattleHud(BattleSubPhase::SelectEnemyForItem(item_id)) => {
                            self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                            self.dirty = true;
                            return UiAction::BattleUseItemTarget { item_id, target: i };
                        }
                        _ => {
                            self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                            self.dirty = true;
                            return UiAction::BattleAttack { enemy_idx: i };
                        }
                    }
                }
            }
            HitTag::BattleHero(i) => {
                if self.battle_ally_cursor != i {
                    self.battle_ally_cursor = i;
                    self.dirty = true;
                }
                if let Some(b) = battle.as_deref_mut() {
                    if is_click {
                        b.selected_ally = None;
                    } else if b.selected_ally != Some(i) {
                        b.selected_ally = Some(i);
                        self.dirty = true;
                    }
                }
                if is_click {
                    match self.view {
                        UiView::BattleHud(BattleSubPhase::SelectAllyForMagic(magic_id)) => {
                            self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                            self.dirty = true;
                            return UiAction::BattleCastMagic {
                                magic_id,
                                enemy_idx: i,
                            };
                        }
                        UiView::BattleHud(BattleSubPhase::SelectAllyForItem(item_id)) => {
                            self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                            self.dirty = true;
                            return UiAction::BattleUseItemTarget { item_id, target: i };
                        }
                        _ => {}
                    }
                }
            }
            HitTag::BattleCancelAuto => {
                if is_click {
                    return UiAction::BattleCancelAuto;
                }
            }
            HitTag::BattleVictoryConfirm => {
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::ShopRow(i) => {
                if self.shop_cursor != i {
                    self.shop_cursor = i;
                    self.pawnshop_confirm_idx = None;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
            HitTag::ReplayChapter(i) => {
                if self.replay_chapter_cursor != i {
                    self.replay_chapter_cursor = i;
                    self.dirty = true;
                }
                if is_click {
                    return self.on_key(UiKey::Confirm, state, battle);
                }
            }
        }
        UiAction::None
    }
}
