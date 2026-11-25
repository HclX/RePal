//! Battle HUD keyboard navigation (command wheel, misc menu, magic/item/target selectors).

use super::*;

impl UiState {
    pub(super) fn on_battle_key(
        &mut self,
        key: UiKey,
        phase: BattleSubPhase,
        state: &GameState,
        mut battle: Option<&mut BattleVisuals>,
    ) -> UiAction {
        let hero_idx = battle.as_ref().map(|b| b.active_hero).unwrap_or(0);
        let role_id = state
            .party
            .get(hero_idx)
            .map(|p| p.role_id as usize)
            .unwrap_or(hero_idx)
            .min(5);
        match phase {
            BattleSubPhase::CommandWheel => match key {
                // Authentic 1995 DOS 4-way Bagua diamond cross:
                // Up = Attack (0), Left = Magic (1), Right = Coop (2), Down = Misc (3)
                UiKey::Up => {
                    self.battle_cmd_cursor = 0;
                    UiAction::None
                }
                UiKey::Left => {
                    self.battle_cmd_cursor = 1;
                    UiAction::None
                }
                UiKey::Right => {
                    if state.party.len() > 1 {
                        self.battle_cmd_cursor = 2;
                    }
                    UiAction::None
                }
                UiKey::Down => {
                    self.battle_cmd_cursor = 3;
                    UiAction::None
                }
                UiKey::Confirm => match self.battle_cmd_cursor {
                    0 => {
                        let remembered = self.battle_memory[role_id].enemy_cursor;
                        if let Some(b) = battle.as_deref_mut() {
                            let target = b
                                .enemies
                                .get(remembered)
                                .filter(|e| e.alive)
                                .map(|_| remembered)
                                .or_else(|| b.enemies.iter().position(|e| e.alive));
                            if let Some(idx) = target {
                                self.battle_enemy_cursor = idx;
                                b.selected_enemy = Some(idx);
                            }
                        }
                        self.view = UiView::BattleHud(BattleSubPhase::SelectEnemyForAttack);
                        UiAction::None
                    }
                    1 => {
                        let magics: Vec<u16> = state
                            .roles
                            .get(role_id)
                            .map(|r| r.magics.iter().copied().filter(|&m| m > 0).collect())
                            .unwrap_or_default();
                        let last_mid = self.battle_memory[role_id].last_magic_id;
                        self.grid_cursor = magics
                            .iter()
                            .position(|&m| m == last_mid)
                            .unwrap_or(0);
                        self.view = UiView::BattleHud(BattleSubPhase::SelectMagic);
                        UiAction::None
                    }
                    2 => {
                        // Authentic SDLPAL Coop magic (when >= 2 alive heroes in party)
                        let alive_heroes = battle
                            .as_ref()
                            .map(|b| b.heroes.iter().filter(|h| h.hp > 0).count())
                            .unwrap_or(state.party.len());
                        if alive_heroes > 1 {
                            let remembered = self.battle_memory[role_id].enemy_cursor;
                            if let Some(b) = battle.as_deref_mut() {
                                let target = b
                                    .enemies
                                    .get(remembered)
                                    .filter(|e| e.alive)
                                    .map(|_| remembered)
                                    .or_else(|| b.enemies.iter().position(|e| e.alive));
                                if let Some(idx) = target {
                                    self.battle_enemy_cursor = idx;
                                    b.selected_enemy = Some(idx);
                                }
                            }
                            self.view = UiView::BattleHud(BattleSubPhase::SelectEnemyForCoop);
                        }
                        UiAction::None
                    }
                    _ => {
                        self.battle_misc_cursor = self.battle_memory[role_id].misc_cursor % 4;
                        self.view = UiView::BattleHud(BattleSubPhase::MiscMenu);
                        UiAction::None
                    }
                },
                UiKey::BattleRepeat => {
                    // Authentic SDLPAL 'R' key: repeat last action for active hero (#274)
                    let mem = self.battle_memory[role_id];
                    let target = battle
                        .as_ref()
                        .and_then(|b| {
                            b.enemies
                                .get(mem.enemy_cursor)
                                .filter(|e| e.alive)
                                .map(|_| mem.enemy_cursor)
                                .or_else(|| b.enemies.iter().position(|e| e.alive))
                        })
                        .unwrap_or(0);
                    match mem.cmd_cursor {
                        1 if mem.last_magic_id > 0 => UiAction::BattleCastMagic {
                            magic_id: mem.last_magic_id,
                            enemy_idx: target,
                        },
                        2 if state.party.len() > 1 => UiAction::BattleCoop { enemy_idx: target },
                        3 if mem.misc_cursor == 1 => UiAction::BattleDefend,
                        _ => UiAction::BattleAttack { enemy_idx: target },
                    }
                }
                UiKey::BattleAuto => {
                    // Authentic SDLPAL 'A' key: Auto-battle (围攻, #274)
                    self.battle_memory[role_id].cmd_cursor = 3;
                    self.battle_memory[role_id].misc_cursor = 2;
                    UiAction::BattleAuto
                }
                UiKey::BattleDefend => {
                    // Authentic SDLPAL 'D' key: Defend (防御, #274)
                    self.battle_memory[role_id].cmd_cursor = 3;
                    self.battle_memory[role_id].misc_cursor = 1;
                    UiAction::BattleDefend
                }
                UiKey::BattleUseItem => {
                    // Authentic SDLPAL 'E' key: open battle Use Item picker (#274)
                    self.battle_item_filter = BattleItemFilter::UseOnly;
                    let items = self.battle_usable_items(state);
                    let last_iid = self.battle_memory[role_id].last_item_id;
                    self.grid_cursor = items.iter().position(|&(id, _)| id == last_iid).unwrap_or(0);
                    self.view = UiView::BattleHud(BattleSubPhase::SelectItem);
                    UiAction::None
                }
                UiKey::BattleThrowItem => {
                    // Authentic SDLPAL 'W' key: open battle Throw Item picker (#274)
                    self.battle_item_filter = BattleItemFilter::ThrowOnly;
                    let items = self.battle_usable_items(state);
                    let last_iid = self.battle_memory[role_id].last_item_id;
                    self.grid_cursor = items.iter().position(|&(id, _)| id == last_iid).unwrap_or(0);
                    self.view = UiView::BattleHud(BattleSubPhase::SelectItem);
                    UiAction::None
                }
                UiKey::BattleFlee => {
                    // Authentic SDLPAL 'Q' key: Flee (逃跑, #274)
                    self.battle_memory[role_id].cmd_cursor = 3;
                    self.battle_memory[role_id].misc_cursor = 3;
                    UiAction::BattleFlee
                }
                UiKey::BattleForce => {
                    // Authentic SDLPAL 'F' key: Force physical attack immediately (#274)
                    let target = battle
                        .as_ref()
                        .and_then(|b| b.enemies.iter().position(|e| e.alive))
                        .unwrap_or(0);
                    self.battle_memory[role_id].cmd_cursor = 0;
                    self.battle_memory[role_id].enemy_cursor = target;
                    UiAction::BattleAttack { enemy_idx: target }
                }
                UiKey::BattleStatus => {
                    // Authentic SDLPAL 'S' key: inspect party status during battle (#274)
                    self.active_role_idx = hero_idx.min(state.party.len().saturating_sub(1));
                    self.status_from_battle = true;
                    self.view = UiView::StatusFbp000;
                    UiAction::None
                }
                UiKey::Cancel | UiKey::MenuToggle => {
                    self.battle_misc_cursor = self.battle_memory[role_id].misc_cursor % 4;
                    self.view = UiView::BattleHud(BattleSubPhase::MiscMenu);
                    UiAction::None
                }
                _ => UiAction::None,
            },
            BattleSubPhase::MiscMenu => match key {
                UiKey::Up | UiKey::Left => {
                    self.battle_misc_cursor = (self.battle_misc_cursor + 3) % 4;
                    UiAction::None
                }
                UiKey::Down | UiKey::Right => {
                    self.battle_misc_cursor = (self.battle_misc_cursor + 1) % 4;
                    UiAction::None
                }
                UiKey::Cancel | UiKey::MenuToggle => {
                    self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                    UiAction::None
                }
                UiKey::Confirm => match self.battle_misc_cursor {
                    0 => {
                        self.battle_item_filter = BattleItemFilter::All;
                        let items = self.battle_usable_items(state);
                        let last_iid = self.battle_memory[role_id].last_item_id;
                        self.grid_cursor = items
                            .iter()
                            .position(|&(id, _)| id == last_iid)
                            .unwrap_or(0);
                        self.view = UiView::BattleHud(BattleSubPhase::SelectItem);
                        UiAction::None
                    }
                    1 => {
                        self.battle_memory[role_id].cmd_cursor = 3;
                        self.battle_memory[role_id].misc_cursor = 1;
                        self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                        UiAction::BattleDefend
                    }
                    2 => {
                        self.battle_memory[role_id].cmd_cursor = 3;
                        self.battle_memory[role_id].misc_cursor = 2;
                        self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                        UiAction::BattleAuto
                    }
                    _ => {
                        self.battle_memory[role_id].cmd_cursor = 3;
                        self.battle_memory[role_id].misc_cursor = 3;
                        self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                        UiAction::BattleFlee
                    }
                },
                _ => UiAction::None,
            },
            BattleSubPhase::SelectEnemyForAttack | BattleSubPhase::SelectEnemyForCoop => {
                let is_coop = matches!(phase, BattleSubPhase::SelectEnemyForCoop);
                let alive_indices: Vec<usize> = battle
                    .as_ref()
                    .map(|b| {
                        b.enemies
                            .iter()
                            .enumerate()
                            .filter(|(_, e)| e.alive)
                            .map(|(i, _)| i)
                            .collect()
                    })
                    .unwrap_or_default();
                match key {
                    UiKey::Left | UiKey::Up | UiKey::Right | UiKey::Down => {
                        if !alive_indices.is_empty() {
                            let pos = alive_indices
                                .iter()
                                .position(|&i| i == self.battle_enemy_cursor)
                                .unwrap_or(0);
                            let next = if matches!(key, UiKey::Left | UiKey::Up) {
                                (pos + alive_indices.len() - 1) % alive_indices.len()
                            } else {
                                (pos + 1) % alive_indices.len()
                            };
                            self.battle_enemy_cursor = alive_indices[next];
                            if let Some(b) = battle.as_deref_mut() {
                                b.selected_enemy = Some(self.battle_enemy_cursor);
                            }
                        }
                        UiAction::None
                    }
                    UiKey::Cancel => {
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_enemy = None;
                        }
                        self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        let target = self.battle_enemy_cursor;
                        self.battle_memory[role_id].cmd_cursor = if is_coop { 2 } else { 0 };
                        self.battle_memory[role_id].enemy_cursor = target;
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_enemy = None;
                        }
                        self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                        if is_coop {
                            UiAction::BattleCoop { enemy_idx: target }
                        } else {
                            UiAction::BattleAttack { enemy_idx: target }
                        }
                    }
                    _ => UiAction::None,
                }
            }
            BattleSubPhase::SelectMagic => {
                let magics: Vec<u16> = state
                    .roles
                    .get(role_id)
                    .map(|r| r.magics.iter().copied().filter(|&m| m > 0).collect())
                    .unwrap_or_default();
                let count = magics.len();
                if count > 0 && self.grid_cursor >= count {
                    self.grid_cursor = count - 1;
                }
                match key {
                    UiKey::Cancel => {
                        self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                        UiAction::None
                    }
                    UiKey::Left => {
                        if count > 0 && self.grid_cursor > 0 {
                            self.grid_cursor -= 1;
                        }
                        UiAction::None
                    }
                    UiKey::Right => {
                        if count > 0 && self.grid_cursor + 1 < count {
                            self.grid_cursor += 1;
                        }
                        UiAction::None
                    }
                    UiKey::Up => {
                        if self.grid_cursor >= 2 {
                            self.grid_cursor -= 2;
                        }
                        UiAction::None
                    }
                    UiKey::Down => {
                        if self.grid_cursor + 2 < count {
                            self.grid_cursor += 2;
                        }
                        UiAction::None
                    }
                    UiKey::PageUp => {
                        if count > 0 {
                            self.grid_cursor = self.grid_cursor.saturating_sub(14);
                        }
                        UiAction::None
                    }
                    UiKey::PageDown => {
                        if count > 0 {
                            self.grid_cursor = (self.grid_cursor + 14).min(count - 1);
                        }
                        UiAction::None
                    }
                    UiKey::Home => {
                        self.grid_cursor = 0;
                        UiAction::None
                    }
                    UiKey::End => {
                        if count > 0 {
                            self.grid_cursor = count - 1;
                        }
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        if let Some(&magic_id) = magics.get(self.grid_cursor) {
                            let target_enemy = self.catalog.magic_target_enemy.get(&magic_id).copied();
                            let target_all = self.catalog.magic_target_all.get(&magic_id).copied().unwrap_or(false);
                            self.battle_memory[role_id].cmd_cursor = 1;
                            self.battle_memory[role_id].last_magic_id = magic_id;

                            // Single-target ally spell when party has >1 member -> let player pick ally!
                            if target_enemy == Some(false) && !target_all {
                                let party_len = battle
                                    .as_ref()
                                    .map(|b| b.heroes.len())
                                    .unwrap_or(state.party.len());
                                if party_len > 1 {
                                    // Default to lowest HP% ally (great UX for 观音咒 / 还魂咒!)
                                    let best_ally = battle
                                        .as_ref()
                                        .and_then(|b| {
                                            b.heroes
                                                .iter()
                                                .enumerate()
                                                .min_by_key(|(_, h)| {
                                                    (h.hp as u32 * 1000) / (h.max_hp.max(1) as u32)
                                                })
                                                .map(|(i, _)| i)
                                        })
                                        .unwrap_or(hero_idx);
                                    self.battle_ally_cursor = best_ally;
                                    if let Some(b) = battle.as_deref_mut() {
                                        b.selected_enemy = None;
                                        b.selected_ally = Some(best_ally);
                                    }
                                    self.view = UiView::BattleHud(BattleSubPhase::SelectAllyForMagic(magic_id));
                                    return UiAction::None;
                                }
                                self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                                return UiAction::BattleCastMagic {
                                    magic_id,
                                    enemy_idx: hero_idx,
                                };
                            }

                            let remembered = self.battle_memory[role_id].enemy_cursor;
                            let alive_count = battle
                                .as_ref()
                                .map(|b| b.enemies.iter().filter(|e| e.alive).count())
                                .unwrap_or(0);
                            let target = battle
                                .as_ref()
                                .and_then(|b| {
                                    b.enemies
                                        .get(remembered)
                                        .filter(|e| e.alive)
                                        .map(|_| remembered)
                                        .or_else(|| b.enemies.iter().position(|e| e.alive))
                                })
                                .unwrap_or(0);

                            // Single-target enemy spell when >1 enemies alive -> let player pick enemy!
                            if target_enemy == Some(true) && !target_all && alive_count > 1 {
                                self.battle_enemy_cursor = target;
                                if let Some(b) = battle.as_deref_mut() {
                                    b.selected_ally = None;
                                    b.selected_enemy = Some(target);
                                }
                                self.view = UiView::BattleHud(BattleSubPhase::SelectEnemyForMagic(magic_id));
                                return UiAction::None;
                            }

                            self.battle_memory[role_id].enemy_cursor = target;
                            self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                            UiAction::BattleCastMagic {
                                magic_id,
                                enemy_idx: target,
                            }
                        } else {
                            self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                            UiAction::None
                        }
                    }
                    _ => UiAction::None,
                }
            }
            BattleSubPhase::SelectEnemyForMagic(magic_id) => {
                let alive_indices: Vec<usize> = battle
                    .as_ref()
                    .map(|b| {
                        b.enemies
                            .iter()
                            .enumerate()
                            .filter(|(_, e)| e.alive)
                            .map(|(i, _)| i)
                            .collect()
                    })
                    .unwrap_or_default();
                match key {
                    UiKey::Left | UiKey::Up | UiKey::Right | UiKey::Down => {
                        if !alive_indices.is_empty() {
                            let pos = alive_indices
                                .iter()
                                .position(|&i| i == self.battle_enemy_cursor)
                                .unwrap_or(0);
                            let next = if matches!(key, UiKey::Left | UiKey::Up) {
                                (pos + alive_indices.len() - 1) % alive_indices.len()
                            } else {
                                (pos + 1) % alive_indices.len()
                            };
                            self.battle_enemy_cursor = alive_indices[next];
                            if let Some(b) = battle.as_deref_mut() {
                                b.selected_enemy = Some(self.battle_enemy_cursor);
                            }
                        }
                        UiAction::None
                    }
                    UiKey::Cancel => {
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_enemy = None;
                        }
                        self.view = UiView::BattleHud(BattleSubPhase::SelectMagic);
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        let target = self.battle_enemy_cursor;
                        self.battle_memory[role_id].enemy_cursor = target;
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_enemy = None;
                        }
                        self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                        UiAction::BattleCastMagic {
                            magic_id,
                            enemy_idx: target,
                        }
                    }
                    _ => UiAction::None,
                }
            }
            BattleSubPhase::SelectAllyForMagic(magic_id) => {
                let party_len = battle
                    .as_ref()
                    .map(|b| b.heroes.len())
                    .unwrap_or(state.party.len())
                    .max(1);
                match key {
                    UiKey::Left | UiKey::Up => {
                        self.battle_ally_cursor = (self.battle_ally_cursor + party_len - 1) % party_len;
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_ally = Some(self.battle_ally_cursor);
                        }
                        UiAction::None
                    }
                    UiKey::Right | UiKey::Down => {
                        self.battle_ally_cursor = (self.battle_ally_cursor + 1) % party_len;
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_ally = Some(self.battle_ally_cursor);
                        }
                        UiAction::None
                    }
                    UiKey::Cancel => {
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_ally = None;
                        }
                        self.view = UiView::BattleHud(BattleSubPhase::SelectMagic);
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        let target = self.battle_ally_cursor;
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_ally = None;
                        }
                        self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                        UiAction::BattleCastMagic {
                            magic_id,
                            enemy_idx: target,
                        }
                    }
                    _ => UiAction::None,
                }
            }
            BattleSubPhase::SelectItem => {
                let items = self.battle_usable_items(state);
                let count = items.len();
                if count > 0 && self.grid_cursor >= count {
                    self.grid_cursor = count - 1;
                }
                match key {
                    UiKey::Cancel => {
                        self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                        UiAction::None
                    }
                    UiKey::Left => {
                        if count > 0 && self.grid_cursor > 0 {
                            self.grid_cursor -= 1;
                        }
                        UiAction::None
                    }
                    UiKey::Right => {
                        if count > 0 && self.grid_cursor + 1 < count {
                            self.grid_cursor += 1;
                        }
                        UiAction::None
                    }
                    UiKey::Up => {
                        if self.grid_cursor >= 2 {
                            self.grid_cursor -= 2;
                        }
                        UiAction::None
                    }
                    UiKey::Down => {
                        if self.grid_cursor + 2 < count {
                            self.grid_cursor += 2;
                        }
                        UiAction::None
                    }
                    UiKey::PageUp => {
                        if count > 0 {
                            self.grid_cursor = self.grid_cursor.saturating_sub(14);
                        }
                        UiAction::None
                    }
                    UiKey::PageDown => {
                        if count > 0 {
                            self.grid_cursor = (self.grid_cursor + 14).min(count - 1);
                        }
                        UiAction::None
                    }
                    UiKey::Home => {
                        self.grid_cursor = 0;
                        UiAction::None
                    }
                    UiKey::End => {
                        if count > 0 {
                            self.grid_cursor = count - 1;
                        }
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        if let Some(&(item_id, _)) = items.get(self.grid_cursor) {
                            self.battle_memory[role_id].cmd_cursor = 3;
                            self.battle_memory[role_id].misc_cursor = 0;
                            self.battle_memory[role_id].last_item_id = item_id;
                            let usable = self.catalog.item_usable.get(&item_id).copied();
                            let throwable = self.catalog.item_throwable.get(&item_id).copied();
                            let target_all = self.catalog.item_target_all.get(&item_id).copied().unwrap_or(false);

                            // Throwable weapon/item with >1 alive enemies -> select enemy target!
                            if throwable == Some(true) && usable != Some(true) && !target_all {
                                let alive_count = battle
                                    .as_ref()
                                    .map(|b| b.enemies.iter().filter(|e| e.alive).count())
                                    .unwrap_or(0);
                                if alive_count > 1 {
                                    let remembered = self.battle_memory[role_id].enemy_cursor;
                                    let target = battle
                                        .as_ref()
                                        .and_then(|b| {
                                            b.enemies
                                                .get(remembered)
                                                .filter(|e| e.alive)
                                                .map(|_| remembered)
                                                .or_else(|| b.enemies.iter().position(|e| e.alive))
                                        })
                                        .unwrap_or(0);
                                    self.battle_enemy_cursor = target;
                                    if let Some(b) = battle.as_deref_mut() {
                                        b.selected_ally = None;
                                        b.selected_enemy = Some(target);
                                    }
                                    self.view = UiView::BattleHud(BattleSubPhase::SelectEnemyForItem(item_id));
                                    return UiAction::None;
                                }
                            }

                            // Single-target usable item (heal/revive/MP) with >1 party members -> select ally!
                            if usable == Some(true) && !target_all {
                                let party_len = battle
                                    .as_ref()
                                    .map(|b| b.heroes.len())
                                    .unwrap_or(state.party.len());
                                if party_len > 1 {
                                    let best_ally = battle
                                        .as_ref()
                                        .and_then(|b| {
                                            b.heroes
                                                .iter()
                                                .enumerate()
                                                .min_by_key(|(_, h)| {
                                                    (h.hp as u32 * 1000) / (h.max_hp.max(1) as u32)
                                                })
                                                .map(|(i, _)| i)
                                        })
                                        .unwrap_or(hero_idx);
                                    self.battle_ally_cursor = best_ally;
                                    if let Some(b) = battle.as_deref_mut() {
                                        b.selected_enemy = None;
                                        b.selected_ally = Some(best_ally);
                                    }
                                    self.view = UiView::BattleHud(BattleSubPhase::SelectAllyForItem(item_id));
                                    return UiAction::None;
                                }
                            }

                            self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                            UiAction::BattleUseItem { item_id }
                        } else {
                            self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                            UiAction::None
                        }
                    }
                    _ => UiAction::None,
                }
            }
            BattleSubPhase::SelectEnemyForItem(item_id) => {
                let alive_indices: Vec<usize> = battle
                    .as_ref()
                    .map(|b| {
                        b.enemies
                            .iter()
                            .enumerate()
                            .filter(|(_, e)| e.alive)
                            .map(|(i, _)| i)
                            .collect()
                    })
                    .unwrap_or_default();
                match key {
                    UiKey::Left | UiKey::Up | UiKey::Right | UiKey::Down => {
                        if !alive_indices.is_empty() {
                            let pos = alive_indices
                                .iter()
                                .position(|&i| i == self.battle_enemy_cursor)
                                .unwrap_or(0);
                            let next = if matches!(key, UiKey::Left | UiKey::Up) {
                                (pos + alive_indices.len() - 1) % alive_indices.len()
                            } else {
                                (pos + 1) % alive_indices.len()
                            };
                            self.battle_enemy_cursor = alive_indices[next];
                            if let Some(b) = battle.as_deref_mut() {
                                b.selected_enemy = Some(self.battle_enemy_cursor);
                            }
                        }
                        UiAction::None
                    }
                    UiKey::Cancel => {
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_enemy = None;
                        }
                        self.view = UiView::BattleHud(BattleSubPhase::SelectItem);
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        let target = self.battle_enemy_cursor;
                        self.battle_memory[role_id].enemy_cursor = target;
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_enemy = None;
                        }
                        self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                        UiAction::BattleUseItemTarget { item_id, target }
                    }
                    _ => UiAction::None,
                }
            }
            BattleSubPhase::SelectAllyForItem(item_id) => {
                let party_len = battle
                    .as_ref()
                    .map(|b| b.heroes.len())
                    .unwrap_or(state.party.len())
                    .max(1);
                match key {
                    UiKey::Left | UiKey::Up => {
                        self.battle_ally_cursor = (self.battle_ally_cursor + party_len - 1) % party_len;
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_ally = Some(self.battle_ally_cursor);
                        }
                        UiAction::None
                    }
                    UiKey::Right | UiKey::Down => {
                        self.battle_ally_cursor = (self.battle_ally_cursor + 1) % party_len;
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_ally = Some(self.battle_ally_cursor);
                        }
                        UiAction::None
                    }
                    UiKey::Cancel => {
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_ally = None;
                        }
                        self.view = UiView::BattleHud(BattleSubPhase::SelectItem);
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        let target = self.battle_ally_cursor;
                        if let Some(b) = battle.as_deref_mut() {
                            b.selected_ally = None;
                        }
                        self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                        UiAction::BattleUseItemTarget { item_id, target }
                    }
                    _ => UiAction::None,
                }
            }
        }
    }
}
