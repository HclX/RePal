//! Non-battle keyboard navigation for title, dialogue, menus, grids, save slots, and shops.

use super::*;

impl UiState {
    /// Process a keyboard event in the current UI view and return any resulting `UiAction`.
    pub fn on_key(&mut self, key: UiKey, state: &GameState, battle: Option<&mut BattleVisuals>) -> UiAction {
        self.dirty = true;
        // Allow Esc / Cancel to immediately stop Auto-Battle (围攻) at any time!
        if key == UiKey::Cancel
            && battle.as_ref().map(|b| b.active && b.auto_battle).unwrap_or(false)
        {
            return UiAction::BattleCancelAuto;
        }
        match self.view.clone() {
            UiView::None => {
                if matches!(key, UiKey::MenuToggle | UiKey::Cancel) {
                    // Authentic SDLPAL (`iCurMainMenuItem`): preserve last RootMenu cursor (#274).
                    self.view = UiView::RootMenu;
                }
                UiAction::None
            }
            UiView::Title => match key {
                UiKey::Up | UiKey::Left => {
                    self.title_cursor = (self.title_cursor + 2) % 3;
                    UiAction::None
                }
                UiKey::Down | UiKey::Right => {
                    self.title_cursor = (self.title_cursor + 1) % 3;
                    UiAction::None
                }
                UiKey::MenuToggle | UiKey::Cancel => {
                    self.title_options_cursor = 0;
                    self.view = UiView::TitleOptions;
                    UiAction::None
                }
                UiKey::Confirm => match self.title_cursor {
                    0 => {
                        self.root_cursor = 0;
                        self.sys_sub_cursor = 0;
                        self.field_item_memory = 0;
                        self.view = UiView::None;
                        UiAction::NewGame
                    }
                    1 => {
                        self.save_slot_cursor = 0;
                        self.view = UiView::SaveLoadSlots(SaveSlotMode::LoadFromTitle);
                        UiAction::None
                    }
                    _ => {
                        self.title_options_cursor = 0;
                        self.view = UiView::TitleOptions;
                        UiAction::None
                    }
                },
                _ => UiAction::None,
            },
            UiView::TitleOptions => match key {
                UiKey::Up | UiKey::Left => {
                    self.title_options_cursor = (self.title_options_cursor + 7) % 8;
                    UiAction::None
                }
                UiKey::Down | UiKey::Right => {
                    self.title_options_cursor = (self.title_options_cursor + 1) % 8;
                    UiAction::None
                }
                UiKey::Cancel | UiKey::MenuToggle => {
                    self.view = UiView::Title;
                    UiAction::None
                }
                UiKey::Confirm => match self.title_options_cursor {
                    0 => UiAction::OpenReplay,
                    1 => {
                        self.hd_mode = !self.hd_mode;
                        UiAction::ToggleHdMode
                    }
                    2 => {
                        self.cycle_lang();
                        UiAction::ToggleLanguage
                    }
                    3 => {
                        self.music_on = !self.music_on;
                        UiAction::ToggleMusic
                    }
                    4 => {
                        self.sfx_on = !self.sfx_on;
                        UiAction::ToggleSfx
                    }
                    5 => {
                        self.fullscreen = !self.fullscreen;
                        UiAction::ToggleFullscreen
                    }
                    6 => {
                        self.agent_mode = (self.agent_mode + 1) % 3;
                        UiAction::ToggleAgentMode
                    }
                    _ => UiAction::CheckForUpdate,
                },
                _ => UiAction::None,
            },
            UiView::ReplayFinished => match key {
                UiKey::Up | UiKey::Left => {
                    self.replay_finished_cursor = (self.replay_finished_cursor + 3) % 4;
                    UiAction::None
                }
                UiKey::Down | UiKey::Right => {
                    self.replay_finished_cursor = (self.replay_finished_cursor + 1) % 4;
                    UiAction::None
                }
                UiKey::Cancel => {
                    self.view = UiView::RootMenu;
                    UiAction::None
                }
                UiKey::Confirm => match self.replay_finished_cursor {
                    0 => {
                        self.view = UiView::None;
                        UiAction::ReplayRestart
                    }
                    1 => {
                        self.view = UiView::None;
                        UiAction::ReplayTakeOver
                    }
                    2 => UiAction::OpenReplay,
                    _ => {
                        self.title_cursor = 0;
                        self.view = UiView::Title;
                        UiAction::ReturnToTitle
                    }
                },
                _ => UiAction::None,
            },
            UiView::Dialog => {
                // Multi-page dialogue pagination: advance through pages before showing choices or dismissing.
                if self.dialog.page + 1 < self.dialog.total_pages {
                    if matches!(key, UiKey::Confirm | UiKey::Cancel) {
                        self.dialog.page += 1;
                    }
                    return UiAction::None;
                }
                if !self.dialog.choices.is_empty() {
                    let n = self.dialog.choices.len();
                    match key {
                        UiKey::Up | UiKey::Left => {
                            self.dialog.choice_cursor = (self.dialog.choice_cursor + n - 1) % n;
                            UiAction::None
                        }
                        UiKey::Down | UiKey::Right => {
                            self.dialog.choice_cursor = (self.dialog.choice_cursor + 1) % n;
                            UiAction::None
                        }
                        UiKey::Confirm => {
                            let idx = self.dialog.choice_cursor;
                            self.view = UiView::None;
                            UiAction::DialogPickChoice(idx)
                        }
                        UiKey::Cancel => {
                            self.view = UiView::None;
                            UiAction::DialogPickChoice(n.saturating_sub(1))
                        }
                        _ => UiAction::None,
                    }
                } else if matches!(key, UiKey::Confirm | UiKey::Cancel) {
                    self.view = UiView::None;
                    UiAction::DialogAdvance
                } else {
                    UiAction::None
                }
            }
            UiView::RootMenu => {
                let count = if self.replay_mode { 4 } else { 5 };
                if self.root_cursor >= count {
                    self.root_cursor = 0;
                }
                match key {
                    UiKey::Up | UiKey::Left => {
                        self.root_cursor = (self.root_cursor + count - 1) % count;
                        UiAction::None
                    }
                    UiKey::Down | UiKey::Right => {
                        self.root_cursor = (self.root_cursor + 1) % count;
                        UiAction::None
                    }
                    UiKey::Cancel | UiKey::MenuToggle => {
                        self.view = UiView::None;
                        UiAction::None
                    }
                    UiKey::Confirm => self.enter_root_selection(state),
                    _ => UiAction::None,
                }
            }
            UiView::RoleSelect(target) => {
                let n = state.party.len().max(1);
                match key {
                    UiKey::Up | UiKey::Left => {
                        self.role_cursor = (self.role_cursor + n - 1) % n;
                        UiAction::None
                    }
                    UiKey::Down | UiKey::Right => {
                        self.role_cursor = (self.role_cursor + 1) % n;
                        UiAction::None
                    }
                    UiKey::Cancel | UiKey::MenuToggle => {
                        self.view = UiView::RootMenu;
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        self.active_role_idx = self.role_cursor.min(n - 1);
                        self.grid_cursor = 0;
                        self.equip_slot_cursor = 0;
                        self.status_from_battle = false;
                        self.view = match target {
                            RoleSelectTarget::Status => UiView::StatusFbp000,
                            RoleSelectTarget::Magic => {
                                self.restore_field_magic_cursor(state);
                                UiView::MagicGrid
                            }
                            RoleSelectTarget::Equip => UiView::EquipFbp001,
                        };
                        UiAction::None
                    }
                    _ => UiAction::None,
                }
            }
            UiView::StatusFbp000 => {
                let n = state.party.len().max(1);
                match key {
                    UiKey::Left | UiKey::Up | UiKey::PageUp => {
                        self.active_role_idx = (self.active_role_idx + n - 1) % n;
                        UiAction::None
                    }
                    UiKey::Right | UiKey::Down | UiKey::PageDown => {
                        self.active_role_idx = (self.active_role_idx + 1) % n;
                        UiAction::None
                    }
                    UiKey::Cancel | UiKey::MenuToggle | UiKey::Confirm => {
                        if self.status_from_battle {
                            self.status_from_battle = false;
                            self.view = UiView::BattleHud(BattleSubPhase::CommandWheel);
                        } else {
                            self.view = UiView::RootMenu;
                        }
                        UiAction::None
                    }
                    _ => UiAction::None,
                }
            }
            UiView::EquipFbp001 => {
                match key {
                    UiKey::Up | UiKey::Left => {
                        self.equip_slot_cursor = (self.equip_slot_cursor + 5) % 6;
                        UiAction::None
                    }
                    UiKey::Down | UiKey::Right => {
                        self.equip_slot_cursor = (self.equip_slot_cursor + 1) % 6;
                        UiAction::None
                    }
                    UiKey::Cancel | UiKey::MenuToggle => {
                        self.view = UiView::ItemSubmenu;
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        let role_id = state
                            .party
                            .get(self.active_role_idx)
                            .map(|p| p.role_id as usize)
                            .unwrap_or(0)
                            .min(5);
                        let target_slot = self.equip_slot_cursor as u8;
                        let current_eq = state
                            .roles
                            .get(role_id)
                            .map(|r| r.equipment[self.equip_slot_cursor])
                            .unwrap_or(0);
                        let mut candidates: Vec<u16> = state
                            .inventory
                            .iter()
                            .filter(|(_, &q)| q > 0)
                            .map(|(&id, _)| id)
                            .filter(|id| {
                                if self.catalog.item_equip_slot.is_empty() {
                                    true
                                } else {
                                    let slot_ok =
                                        self.catalog.item_equip_slot.get(id) == Some(&target_slot);
                                    let mask =
                                        self.catalog.item_equip_mask.get(id).copied().unwrap_or(0);
                                    let role_ok = mask == 0 || (mask & (1 << role_id)) != 0;
                                    slot_ok && role_ok
                                }
                            })
                            .collect();
                        candidates.sort_unstable();
                        // Cycle to the next equippable item strictly greater than current_eq,
                        // wrapping back to candidates[0] so 3rd+ items for a slot are reachable.
                        let item_id = candidates
                            .iter()
                            .copied()
                            .find(|&id| id > current_eq)
                            .or_else(|| candidates.first().copied())
                            .unwrap_or(0);
                        if item_id > 0 {
                            UiAction::EquipItem {
                                item_id,
                                slot: self.equip_slot_cursor,
                                role_idx: self.active_role_idx,
                            }
                        } else {
                            UiAction::None
                        }
                    }
                    _ => UiAction::None,
                }
            }
            UiView::ItemSubmenu => match key {
                UiKey::Up | UiKey::Down | UiKey::Left | UiKey::Right => {
                    self.item_sub_cursor = 1 - (self.item_sub_cursor % 2);
                    UiAction::None
                }
                UiKey::Cancel | UiKey::MenuToggle => {
                    self.view = UiView::RootMenu;
                    UiAction::None
                }
                UiKey::Confirm => {
                    if self.item_sub_cursor == 0 {
                        self.restore_field_item_cursor(state);
                        self.view = UiView::ItemGrid;
                    } else if state.party.len() > 1 {
                        self.role_cursor = 0;
                        self.view = UiView::RoleSelect(RoleSelectTarget::Equip);
                    } else {
                        self.active_role_idx = 0;
                        self.equip_slot_cursor = 0;
                        self.view = UiView::EquipFbp001;
                    }
                    UiAction::None
                }
                _ => UiAction::None,
            },
            UiView::ItemGrid => {
                let items: Vec<u16> = state
                    .inventory
                    .iter()
                    .filter(|(_, &q)| q > 0)
                    .map(|(&id, _)| id)
                    .collect();
                let count = items.len();
                if count == 0 {
                    self.grid_cursor = 0;
                } else if self.grid_cursor >= count {
                    self.grid_cursor = count - 1;
                }
                let cols = if self.lang.is_en() { 2 } else { 3 };
                let page_jump = cols * 7; // Authentic SDLPAL 7-row PgUp/PgDn paging (#274)
                match key {
                    UiKey::Cancel | UiKey::MenuToggle => {
                        self.view = UiView::ItemSubmenu;
                        UiAction::None
                    }
                    UiKey::Left => {
                        // Authentic SDLPAL: clamp at index 0, never exit grid on Left (#274).
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
                        if self.grid_cursor >= cols {
                            self.grid_cursor -= cols;
                        }
                        UiAction::None
                    }
                    UiKey::Down => {
                        if self.grid_cursor + cols < count {
                            self.grid_cursor += cols;
                        }
                        UiAction::None
                    }
                    UiKey::PageUp => {
                        if count > 0 {
                            self.grid_cursor = self.grid_cursor.saturating_sub(page_jump);
                        }
                        UiAction::None
                    }
                    UiKey::PageDown => {
                        if count > 0 {
                            self.grid_cursor = (self.grid_cursor + page_jump).min(count - 1);
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
                        if let Some(&item_id) = items.get(self.grid_cursor) {
                            self.field_item_memory = item_id;
                            UiAction::UseItem {
                                item_id,
                                role_idx: self.active_role_idx,
                            }
                        } else {
                            UiAction::None
                        }
                    }
                    _ => UiAction::None,
                }
            }
            UiView::MagicGrid => {
                let role_id = state
                    .party
                    .get(self.active_role_idx)
                    .map(|p| p.role_id as usize)
                    .unwrap_or(0)
                    .min(5);
                let count = state.roles[role_id].magics.len();
                if count == 0 {
                    self.grid_cursor = 0;
                } else if self.grid_cursor >= count {
                    self.grid_cursor = count - 1;
                }
                let cols = if self.lang.is_en() { 2 } else { 3 };
                let page_jump = cols * 7;
                match key {
                    UiKey::Cancel | UiKey::MenuToggle => {
                        self.view = UiView::RootMenu;
                        UiAction::None
                    }
                    UiKey::Left => {
                        // Authentic SDLPAL: clamp at index 0, never exit grid on Left (#274).
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
                        if self.grid_cursor >= cols {
                            self.grid_cursor -= cols;
                        }
                        UiAction::None
                    }
                    UiKey::Down => {
                        if self.grid_cursor + cols < count {
                            self.grid_cursor += cols;
                        }
                        UiAction::None
                    }
                    UiKey::PageUp => {
                        if count > 0 {
                            self.grid_cursor = self.grid_cursor.saturating_sub(page_jump);
                        }
                        UiAction::None
                    }
                    UiKey::PageDown => {
                        if count > 0 {
                            self.grid_cursor = (self.grid_cursor + page_jump).min(count - 1);
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
                        if let Some(&magic_id) = state.roles[role_id].magics.get(self.grid_cursor) {
                            self.field_magic_memory[role_id] = magic_id;
                            UiAction::CastMagic {
                                magic_id,
                                role_idx: self.active_role_idx,
                            }
                        } else {
                            UiAction::None
                        }
                    }
                    _ => UiAction::None,
                }
            }
            UiView::SystemSubmenu => {
                let count = if self.replay_mode { 4 } else { 5 };
                if self.sys_sub_cursor >= count {
                    self.sys_sub_cursor = 0;
                }
                match key {
                    UiKey::Up | UiKey::Left => {
                        self.sys_sub_cursor = (self.sys_sub_cursor + count - 1) % count;
                        UiAction::None
                    }
                    UiKey::Down | UiKey::Right => {
                        self.sys_sub_cursor = (self.sys_sub_cursor + 1) % count;
                        UiAction::None
                    }
                    UiKey::Cancel | UiKey::MenuToggle => {
                        self.view = UiView::RootMenu;
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        // In replay mode, index 0 (Save Game) is hidden, so shift by +1.
                        let idx = if self.replay_mode {
                            self.sys_sub_cursor + 1
                        } else {
                            self.sys_sub_cursor
                        };
                        match idx {
                            0 => {
                                self.save_slot_cursor = 0;
                                self.view = UiView::SaveLoadSlots(SaveSlotMode::SaveInGame);
                                UiAction::None
                            }
                            1 => {
                                self.save_slot_cursor = 0;
                                self.view = UiView::SaveLoadSlots(SaveSlotMode::LoadInGame);
                                UiAction::None
                            }
                            2 => {
                                self.music_on = !self.music_on;
                                UiAction::ToggleMusic
                            }
                            3 => {
                                self.sfx_on = !self.sfx_on;
                                UiAction::ToggleSfx
                            }
                            _ => {
                                self.title_cursor = 0;
                                self.view = UiView::Title;
                                UiAction::ReturnToTitle
                            }
                        }
                    }
                    _ => UiAction::None,
                }
            }
            UiView::RemakeSubmenu => {
                let count = if self.replay_mode { 5 } else { 10 };
                if self.remake_sub_cursor >= count {
                    self.remake_sub_cursor = 0;
                }
                match key {
                    UiKey::Up | UiKey::Left => {
                        self.remake_sub_cursor = (self.remake_sub_cursor + count - 1) % count;
                        UiAction::None
                    }
                    UiKey::Down | UiKey::Right => {
                        self.remake_sub_cursor = (self.remake_sub_cursor + 1) % count;
                        UiAction::None
                    }
                    UiKey::Cancel | UiKey::MenuToggle => {
                        self.view = UiView::RootMenu;
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        if self.replay_mode {
                            match self.remake_sub_cursor {
                                0 => {
                                    self.hd_mode = !self.hd_mode;
                                    UiAction::ToggleHdMode
                                }
                                1 => {
                                    self.cycle_lang();
                                    UiAction::ToggleLanguage
                                }
                                2 => {
                                    self.view = UiView::None;
                                    UiAction::OpenReplay
                                }
                                3 => UiAction::TakeScreenshot,
                                _ => {
                                    self.fullscreen = !self.fullscreen;
                                    UiAction::ToggleFullscreen
                                }
                            }
                        } else {
                            match self.remake_sub_cursor {
                                0 => UiAction::ExportSave,
                                1 => UiAction::ImportSave,
                                2 => {
                                    self.hd_mode = !self.hd_mode;
                                    UiAction::ToggleHdMode
                                }
                                3 => {
                                    self.cycle_lang();
                                    UiAction::ToggleLanguage
                                }
                                4 => UiAction::CopyReplayReport,
                                5 => UiAction::TakeScreenshot,
                                6 => {
                                    self.fullscreen = !self.fullscreen;
                                    UiAction::ToggleFullscreen
                                }
                                7 => {
                                    self.agent_mode = (self.agent_mode + 1) % 3;
                                    UiAction::ToggleAgentMode
                                }
                                8 => UiAction::RestartRecording,
                                _ => UiAction::ExportReplay,
                            }
                        }
                    }
                    _ => UiAction::None,
                }
            }
            UiView::ReplayChapters => {
                let n = self.replay_chapters.len();
                let page_size = 5usize;
                if n > 0 && self.replay_chapter_cursor >= n {
                    self.replay_chapter_cursor = n - 1;
                }
                match key {
                    UiKey::Up => {
                        if n > 0 {
                            self.replay_chapter_cursor = (self.replay_chapter_cursor + n - 1) % n;
                        }
                        UiAction::None
                    }
                    UiKey::Down => {
                        if n > 0 {
                            self.replay_chapter_cursor = (self.replay_chapter_cursor + 1) % n;
                        }
                        UiAction::None
                    }
                    UiKey::Left | UiKey::PageUp => {
                        if n > 0 {
                            let step = page_size.min(n);
                            self.replay_chapter_cursor = (self.replay_chapter_cursor + n - step) % n;
                        }
                        UiAction::None
                    }
                    UiKey::Right | UiKey::PageDown => {
                        if n > 0 {
                            let step = page_size.min(n);
                            self.replay_chapter_cursor = (self.replay_chapter_cursor + step) % n;
                        }
                        UiAction::None
                    }
                    UiKey::Home => {
                        self.replay_chapter_cursor = 0;
                        UiAction::None
                    }
                    UiKey::End => {
                        if n > 0 {
                            self.replay_chapter_cursor = n - 1;
                        }
                        UiAction::None
                    }
                    UiKey::Cancel | UiKey::MenuToggle => {
                        self.view = UiView::RootMenu;
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        if n > 0 {
                            let idx = self.replay_chapter_cursor;
                            self.view = UiView::None;
                            UiAction::ReplayJumpChapter(idx)
                        } else {
                            UiAction::None
                        }
                    }
                    _ => UiAction::None,
                }
            }
            UiView::SaveLoadSlots(mode) => {
                let n = self.save_slots.len().max(1);
                let page_size = 5usize;
                match key {
                    UiKey::Up => {
                        self.save_slot_cursor = (self.save_slot_cursor + n - 1) % n;
                        UiAction::None
                    }
                    UiKey::Down => {
                        self.save_slot_cursor = (self.save_slot_cursor + 1) % n;
                        UiAction::None
                    }
                    UiKey::Left | UiKey::PageUp => {
                        let step = page_size.min(n);
                        self.save_slot_cursor = (self.save_slot_cursor + n - step) % n;
                        UiAction::None
                    }
                    UiKey::Right | UiKey::PageDown => {
                        let step = page_size.min(n);
                        self.save_slot_cursor = (self.save_slot_cursor + step) % n;
                        UiAction::None
                    }
                    UiKey::Home => {
                        self.save_slot_cursor = 0;
                        UiAction::None
                    }
                    UiKey::End => {
                        self.save_slot_cursor = n - 1;
                        UiAction::None
                    }
                    UiKey::Cancel | UiKey::MenuToggle => {
                        self.view = match mode {
                            SaveSlotMode::LoadFromTitle => UiView::Title,
                            _ => UiView::SystemSubmenu,
                        };
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        let slot = self.save_slot_cursor;
                        match mode {
                            SaveSlotMode::SaveInGame => {
                                self.view = UiView::None;
                                UiAction::SaveSlot(slot)
                            }
                            SaveSlotMode::LoadFromTitle | SaveSlotMode::LoadInGame => {
                                let exists = self
                                    .save_slots
                                    .get(slot)
                                    .map(|s| s.exists)
                                    .unwrap_or(false);
                                if !exists {
                                    UiAction::None
                                } else {
                                    self.view = UiView::None;
                                    UiAction::LoadSlot(slot)
                                }
                            }
                        }
                    }
                    _ => UiAction::None,
                }
            }
            UiView::Shop | UiView::PawnshopSell => {
                let is_sell = self.view == UiView::PawnshopSell;
                let n = self.shop_items.len();
                match key {
                    UiKey::Up | UiKey::Left => {
                        if n > 0 {
                            self.shop_cursor = (self.shop_cursor + n - 1) % n;
                        }
                        self.pawnshop_confirm_idx = None;
                        UiAction::None
                    }
                    UiKey::Down | UiKey::Right => {
                        if n > 0 {
                            self.shop_cursor = (self.shop_cursor + 1) % n;
                        }
                        self.pawnshop_confirm_idx = None;
                        UiAction::None
                    }
                    UiKey::PageUp => {
                        if n > 0 {
                            self.shop_cursor = self.shop_cursor.saturating_sub(6);
                        }
                        self.pawnshop_confirm_idx = None;
                        UiAction::None
                    }
                    UiKey::PageDown => {
                        if n > 0 {
                            self.shop_cursor = (self.shop_cursor + 6).min(n - 1);
                        }
                        self.pawnshop_confirm_idx = None;
                        UiAction::None
                    }
                    UiKey::Home => {
                        self.shop_cursor = 0;
                        self.pawnshop_confirm_idx = None;
                        UiAction::None
                    }
                    UiKey::End => {
                        if n > 0 {
                            self.shop_cursor = n - 1;
                        }
                        self.pawnshop_confirm_idx = None;
                        UiAction::None
                    }
                    UiKey::Cancel | UiKey::MenuToggle => {
                        if is_sell && self.pawnshop_confirm_idx.is_some() {
                            self.pawnshop_confirm_idx = None;
                            return UiAction::None;
                        }
                        self.pawnshop_confirm_idx = None;
                        self.view = UiView::None;
                        UiAction::None
                    }
                    UiKey::Confirm => {
                        if let Some(entry) = self.shop_items.get(self.shop_cursor) {
                            if is_sell {
                                // Require a 2nd confirm before selling to prevent accidental liquidation (#181).
                                if self.pawnshop_confirm_idx != Some(self.shop_cursor) {
                                    self.pawnshop_confirm_idx = Some(self.shop_cursor);
                                    UiAction::None
                                } else {
                                    self.pawnshop_confirm_idx = None;
                                    UiAction::SellItem {
                                        item_id: entry.item_id,
                                        price: entry.price,
                                    }
                                }
                            } else {
                                UiAction::BuyItem {
                                    item_id: entry.item_id,
                                    price: entry.price,
                                }
                            }
                        } else {
                            self.pawnshop_confirm_idx = None;
                            UiAction::None
                        }
                    }
                    _ => UiAction::None,
                }
            }
            UiView::BattleHud(phase) => self.on_battle_key(key, phase, state, battle),
            UiView::BattleVictory => match key {
                UiKey::Confirm | UiKey::Cancel => {
                    self.view = UiView::None;
                    UiAction::DialogAdvance
                }
                _ => UiAction::None,
            },
        }
    }

    fn enter_root_selection(&mut self, state: &GameState) -> UiAction {
        if self.replay_mode {
            match self.root_cursor {
                0 => {
                    self.view = UiView::ReplayChapters;
                    UiAction::None
                }
                1 => {
                    self.view = UiView::None;
                    UiAction::ReplayTakeOver
                }
                2 => {
                    // Authentic SDLPAL (`iCurSystemMenuItem`): remember cursor (#274).
                    self.view = UiView::SystemSubmenu;
                    UiAction::None
                }
                _ => {
                    self.remake_sub_cursor = 0;
                    self.view = UiView::RemakeSubmenu;
                    UiAction::None
                }
            }
        } else {
            match self.root_cursor {
                0 => {
                    self.status_from_battle = false;
                    if state.party.len() > 1 {
                        self.role_cursor = 0;
                        self.view = UiView::RoleSelect(RoleSelectTarget::Status);
                    } else {
                        self.active_role_idx = 0;
                        self.view = UiView::StatusFbp000;
                    }
                }
                1 => {
                    if state.party.len() > 1 {
                        self.role_cursor = 0;
                        self.view = UiView::RoleSelect(RoleSelectTarget::Magic);
                    } else {
                        self.active_role_idx = 0;
                        self.restore_field_magic_cursor(state);
                        self.view = UiView::MagicGrid;
                    }
                }
                2 => {
                    self.item_sub_cursor = 0;
                    self.view = UiView::ItemSubmenu;
                }
                3 => {
                    // Authentic SDLPAL (`iCurSystemMenuItem`): remember cursor (#274).
                    self.view = UiView::SystemSubmenu;
                }
                _ => {
                    self.remake_sub_cursor = 0;
                    self.view = UiView::RemakeSubmenu;
                }
            }
            UiAction::None
        }
    }

}
