//! `pal-runtime`: Shared platform-neutral PalRemake runtime wiring together
//! `pal-core` (deterministic game VM + audio synth), `pal-render` (software framebuffer +
//! Track-B HD compositor), and `pal-ui` (1995 DOS UI state machine).
//!
//! Used by all three frontends:
//! - `pal-wasm` (browser static site / GitHub Pages)
//! - `pal-desktop` (native desktop app)
//! - `pal-headless` (browserless visual E2E & CLI replay runner)

pub mod crypto;
#[cfg(feature = "disk")]
pub mod disk;
#[cfg(feature = "disk")]
pub mod pack_vfs;
#[cfg(feature = "disk")]
pub mod repal_archive;
pub mod replay_player;
pub mod rpk;
pub mod rpk_manifest;

use pal_core::game::data::GameData;
use pal_core::game::{BattleCmd, Game, HostCmd, HostReply, TickInput, Wait};
use pal_core::state::GameState;
use pal_core::synth::DualSynth;
use pal_render::framebuffer::Color;
use pal_render::world::{
    BattleEffectVisual, BattleEnemyVisual, BattleFloatText, BattleHeroVisual, WorldRenderState,
};
use pal_render::PalCompositor;
use pal_ui::{draw_ui, PointerEvent, ShopEntry, UiAction, UiKey, UiState, UiView};
use std::collections::VecDeque;

/// Structured gameplay notice (toast, scene change, battle event, RNG movie) queued for Text Mode (#218).
#[derive(Clone, Debug)]
pub struct TextNotice {
    pub seq: u64,
    pub tick: u64,
    pub kind: String,
    pub text: String,
}

pub struct PalRuntime {
    pub data: GameData,
    pub game: Option<Game>,
    /// Render/UI projection of the core's world (filled by `project()`).
    pub state: GameState,
    pub world: WorldRenderState,
    pub ui: UiState,
    pub compositor: PalCompositor,
    pub synth: DualSynth,
    /// Host commands left for the platform shell (audio, RNG playback, scene asset load, title).
    pub out_cmds: VecDeque<HostCmd>,
    /// Bounded ring buffer of gameplay notices delivered to Text Mode clients (#218).
    pub text_events: VecDeque<TextNotice>,
    next_notice_seq: u64,
    pub toast_ticks: u32,
    pub shop_open: bool,
    pub victory_open: bool,
    pub render_dirty: bool,
    /// Inputs of the last rendered frame, for [`PalRuntime::frame_unchanged`].
    last_render_key: Option<Box<RenderKey>>,
    /// Scene ID when the current AI thought was set, so scene transitions clear only older thoughts.
    pub ai_thought_scene_id: Option<u16>,
    /// Monotonic counter bumped whenever `set_ai_thought` changes the active thought (for host 12s auto-dim).
    pub ai_thought_gen: u64,
}

fn is_menu_view(v: &UiView) -> bool {
    !matches!(
        v,
        UiView::None
            | UiView::Title
            | UiView::Dialog
            | UiView::Shop
            | UiView::PawnshopSell
            | UiView::BattleHud(_)
            | UiView::BattleVictory
    )
}

fn rgb(c: [u8; 3]) -> Color {
    Color::rgba(c[0], c[1], c[2], 255)
}

impl PalRuntime {
    pub fn new(hd_mode: bool) -> Self {
        let mut world_render = WorldRenderState::default();
        world_render.fbp_override = Some(60);
        let mut ui = UiState::new();
        ui.hd_mode = hd_mode;
        let mut synth = DualSynth::new(44100);
        synth.hd_mode = hd_mode;
        Self {
            data: GameData::default(),
            game: None,
            state: GameState::new(),
            world: world_render,
            ui,
            compositor: PalCompositor::new(hd_mode),
            synth,
            out_cmds: VecDeque::new(),
            text_events: VecDeque::new(),
            next_notice_seq: 0,
            toast_ticks: 0,
            shop_open: false,
            victory_open: false,
            render_dirty: false,
            last_render_key: None,
            ai_thought_scene_id: None,
            ai_thought_gen: 0,
        }
    }

    pub fn active(&self) -> bool {
        self.game.is_some()
    }

    /// Construct the `Game` from the tables loaded into `self.data`, register UI catalog, and
    /// enter title mode.
    pub fn init_game(&mut self) {
        let data = std::mem::take(&mut self.data);
        let mut g = Game::new(data);
        g.in_title = true;
        g.world.set_lang(self.ui.lang);
        self.compositor.fonts.set_prefer_latin(self.ui.lang.is_en());
        self.game = Some(g);
        self.register_catalog();
        self.project();
    }

    /// Merge an English i18n JSON overlay (`tables.json`, `scripts_*.json`).
    pub fn load_i18n_en(&mut self, doc: &serde_json::Value) {
        if let Some(g) = self.game.as_mut() {
            g.load_i18n_en(doc);
            self.register_catalog();
            self.compositor.mark_ui_dirty();
        } else {
            self.data.load_i18n_en(doc);
        }
    }

    /// Populate item / magic / role names, icons, descriptions, and equip slots into `ui.catalog`.
    pub fn register_catalog(&mut self) {
        let Some(g) = self.game.as_ref() else { return };
        let w = &g.world;
        let cat = &mut self.ui.catalog;
        cat.item_equip_slot.clear();
        cat.item_equip_mask.clear();
        for it in g.data.items.values() {
            let name = g.item_display_name(it.id);
            if !name.is_empty() {
                cat.item_names.insert(it.id, name);
            }
            if it.price > 0 {
                cat.item_prices.insert(it.id, it.price as u32);
            }
            cat.item_icons.insert(it.id, it.icon_id);
            cat.item_usable.insert(it.id, it.usable);
            cat.item_throwable.insert(it.id, it.throwable);
            cat.item_target_all.insert(it.id, it.target_all);
            if let Some(slot) = g.item_slot(it.id) {
                cat.item_equip_slot.insert(it.id, slot as u8);
                cat.item_equip_mask
                    .insert(it.id, ((it.flags >> 6) & 0x3f) as u8);
            }
            let desc = g.item_display_desc(it.id);
            if !desc.is_empty() {
                cat.item_descs.insert(it.id, desc);
            }
        }
        for m in g.data.magics.values() {
            let name = g.magic_display_name(m.obj_id);
            if !name.is_empty() {
                cat.magic_names.insert(m.obj_id, name);
            }
            let desc = g.magic_display_desc(m.obj_id);
            if !desc.is_empty() {
                cat.magic_descs.insert(m.obj_id, desc);
            }
            cat.magic_mp_cost
                .insert(m.obj_id, m.cost_mp.clamp(0, u16::MAX as i32) as u16);
            cat.magic_target_enemy.insert(m.obj_id, m.target_enemy);
            cat.magic_target_all.insert(m.obj_id, m.target_all);
        }
        for (i, _r) in w.roles.iter().take(6).enumerate() {
            cat.role_names[i] = g.role_display_name(i);
        }
    }

    /// Clear transient host/render modal & battle state across session transitions.
    pub fn reset_session(&mut self) {
        self.shop_open = false;
        self.victory_open = false;
        self.out_cmds.clear();
        self.world.active_rng = None;
        self.world.fbp_override = None;
        self.world.battle.active = false;
        self.world.battle.active_effect = None;
        self.world.battle.float_texts.clear();
        self.ui.ai_thought = None;
        self.ui.ai_thought_dimmed = false;
        self.ai_thought_scene_id = None;
        self.ai_thought_gen = self.ai_thought_gen.wrapping_add(1);
        self.compositor.mark_ui_dirty();
    }

    /// Record a structured gameplay notice in `text_events` (bounded to 128 entries, #218).
    pub fn push_notice(&mut self, kind: &str, text: impl Into<String>) {
        self.next_notice_seq += 1;
        let tick = self.game.as_ref().map(|g| g.tick_count).unwrap_or(0);
        self.text_events.push_back(TextNotice {
            seq: self.next_notice_seq,
            tick,
            kind: kind.to_string(),
            text: text.into(),
        });
        while self.text_events.len() > 128 {
            self.text_events.pop_front();
        }
    }

    /// Return all notices emitted strictly after `after_seq`, along with the latest sequence number (#218).
    pub fn notices_since(&self, after_seq: u64) -> (Vec<TextNotice>, u64) {
        let items = self
            .text_events
            .iter()
            .filter(|n| n.seq > after_seq)
            .cloned()
            .collect();
        (items, self.next_notice_seq)
    }

    pub fn latest_notice_seq(&self) -> u64 {
        self.next_notice_seq
    }

    pub fn set_lang(&mut self, lang: pal_core::Language) {
        self.ui.set_lang(lang);
        self.compositor.fonts.set_prefer_latin(lang.is_en());
        if let Some(g) = self.game.as_mut() {
            g.world.set_lang(lang);
        }
        self.register_catalog();
        // Refresh active Shop / Pawnshop item labels & Dialog Yes/No choices immediately (#176).
        for entry in &mut self.ui.shop_items {
            entry.name = self.ui.catalog.item_name(entry.item_id);
        }
        if !self.ui.dialog.choices.is_empty() && self.ui.dialog.choices.len() == 2 {
            let is_yn = matches!(
                (self.ui.dialog.choices[0].as_str(), self.ui.dialog.choices[1].as_str()),
                ("是", "否") | ("Yes", "No")
            );
            if is_yn {
                self.ui.dialog.choices = if lang.is_en() {
                    vec!["Yes".into(), "No".into()]
                } else {
                    vec!["是".into(), "否".into()]
                };
            }
        }
        self.ui.sync_dialog_layout(&mut self.compositor.fonts, self.ui.hd_mode);
        self.project();
        self.compositor.mark_ui_dirty();
    }

    pub fn set_hd_mode(&mut self, hd: bool) {
        self.ui.hd_mode = hd;
        self.synth.hd_mode = hd;
        self.compositor.set_hd_mode(hd);
        self.ui.sync_dialog_layout(&mut self.compositor.fonts, self.ui.hd_mode);
    }

    pub fn set_in_title(&mut self, in_title: bool) {
        if in_title {
            self.reset_session();
            self.world.fbp_override = Some(60);
            self.ui.set_view(UiView::Title);
            self.ui.debug_mode = false;
        }
        if let Some(g) = self.game.as_mut() {
            g.in_title = in_title;
            if in_title {
                g.abort_foreground();
                g.debug_mode = false;
                g.god_mode = false;
                g.recorder.debug_mode = false;
                g.recorder.tainted_by_debug = false;
            }
        }
    }

    /// Single source of truth for whether the runtime is currently on the Title screen flow.
    pub fn is_in_title(&self) -> bool {
        self.game.as_ref().is_some_and(|g| g.in_title)
            || matches!(
                self.ui.view,
                UiView::Title
                    | UiView::TitleOptions
                    | UiView::SaveLoadSlots(pal_ui::SaveSlotMode::LoadFromTitle)
            )
    }

    /// Refresh the Pawnshop sell inventory list after programmatic sells.
    pub fn refresh_pawnshop(&mut self) {
        if self.game.is_some() {
            self.fill_pawnshop();
            self.compositor.mark_ui_dirty();
        }
    }

    pub fn new_game(&mut self, seed: u32) {
        self.reset_session();
        let lang = self.ui.lang;
        let Some(g) = self.game.as_mut() else { return };
        g.start_new_game(seed);
        g.world.set_lang(lang);
        self.ui.set_view(UiView::None);
        self.route_cmds();
        self.project();
    }

    pub fn load_save_json_str(&mut self, text: &str) -> bool {
        let lang = self.ui.lang;
        let Some(g) = self.game.as_mut() else { return false };
        let ok = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .map(|v| g.load_save_json(&v).is_ok())
            .unwrap_or(false);
        if ok {
            g.world.set_lang(lang);
            self.reset_session();
            self.ui.set_view(UiView::None);
            self.route_cmds();
            self.project();
        }
        ok
    }

    pub fn load_dos_rpg(&mut self, bytes: &[u8]) -> bool {
        let lang = self.ui.lang;
        let Some(g) = self.game.as_mut() else { return false };
        let ok = g.load_dos_rpg(bytes).is_ok();
        if ok {
            g.world.set_lang(lang);
            self.reset_session();
            self.ui.set_view(UiView::None);
            self.route_cmds();
            self.project();
        }
        ok
    }

    pub fn save_json_str(&self, note: &str, ts: &str) -> Option<String> {
        let g = self.game.as_ref()?;
        Some(g.save_json(note, ts).to_string())
    }

    pub fn scene_sprite_ids(&self) -> Vec<u16> {
        let Some(g) = self.game.as_ref() else {
            return Vec::new();
        };
        let mut ids: Vec<u16> = g
            .world
            .current_event_ids()
            .iter()
            .filter_map(|&id| g.world.event(id))
            .map(|e| e.sprite_id)
            .filter(|&s| s > 0)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    pub fn show_toast(&mut self, text: impl Into<String>, ticks: u32) {
        let msg = text.into();
        self.push_notice("toast", msg.clone());
        self.ui.toast_text = Some(msg);
        self.toast_ticks = ticks;
        self.compositor.mark_ui_dirty();
    }

    /// Update or clear the persistent HD-only AI thought/commentary subtitle (`think <text>`).
    pub fn set_ai_thought(&mut self, text: Option<String>) {
        let next = text.filter(|t| !t.is_empty());
        self.ui.ai_thought = next;
        self.ui.ai_thought_dimmed = false;
        self.ai_thought_scene_id = if self.ui.ai_thought.is_some() {
            self.game.as_ref().map(|g| g.world.scene_id)
        } else {
            None
        };
        self.ai_thought_gen = self.ai_thought_gen.wrapping_add(1);
    }

    /// Returns `true` when the present-time HD AI thought overlay would draw visible pixels.
    pub fn has_visible_ai_thought_overlay(&self) -> bool {
        let battle_ref = if self.world.battle.active {
            Some(&self.world.battle)
        } else {
            None
        };
        pal_ui::should_draw_ai_thought_overlay(&self.ui, battle_ref, self.compositor.hd_mode)
    }

    /// Composite the HD-only AI thought subtitle bar onto `fb` at present time (or `shot --with-thought`),
    /// then redraw the `DEBUG ENABLED` watermark last if active so cosmetic overlays can never hide it (#245).
    /// Never called inside `render_frame()`, keeping `self.compositor.fb` structurally thought-free.
    pub fn draw_ai_thought_overlay_onto(&mut self, fb: &mut pal_render::framebuffer::Framebuffer) {
        let battle_ref = if self.world.battle.active {
            Some(&self.world.battle)
        } else {
            None
        };
        pal_ui::draw_ai_thought_overlay(
            fb,
            &mut self.compositor.fonts,
            &self.ui,
            battle_ref,
            self.compositor.hd_mode,
        );
        if self.ui.debug_mode {
            let s = if self.compositor.hd_mode { 4 } else { 1 };
            pal_ui::draw_debug_watermark(fb, &mut self.compositor.fonts, s);
        }
    }

    /// Presentation-only toast decay for host loops that pause `tick()` (RNG cutscene,
    /// paused `.repal` replay, replay menu, #205).
    pub fn decay_toast_presentation(&mut self, ticks: u32) {
        if self.toast_ticks > 0 {
            self.toast_ticks = self.toast_ticks.saturating_sub(ticks);
            if self.toast_ticks == 0 {
                self.ui.toast_text = None;
                self.compositor.mark_ui_dirty();
            }
        }
    }

    /// Advance the game core by one fixed 20 ms tick.
    pub fn tick(&mut self, input: TickInput) {
        if self.toast_ticks > 0 {
            self.toast_ticks -= 1;
            if self.toast_ticks == 0 {
                self.ui.toast_text = None;
                self.compositor.mark_ui_dirty();
            }
        }
        let Some(g) = self.game.as_mut() else { return };
        g.menu_open = is_menu_view(&self.ui.view);
        g.tick(&input);
        if self.world.battle.active || matches!(self.ui.view, UiView::BattleHud(_)) {
            // Battle HP/MP/ATB, spell banner, and status badges update mid-turn during animations.
            self.compositor.mark_ui_dirty();
        }
        self.route_cmds();
        self.render_dirty = true;
    }

    pub fn reply(&mut self, r: HostReply) {
        let Some(g) = self.game.as_mut() else { return };
        g.reply(r);
        self.route_cmds();
        self.project();
    }

    /// Handle a UI key event. Returns `Some(action)` when the platform host must execute it
    /// (e.g. `NewGame`, `SaveSlot`, `LoadSlot`, `ToggleHdMode`, `ToggleMusic`, `ToggleSfx`,
    /// `ToggleLanguage`, `ReturnToTitle`).
    pub fn on_key(&mut self, key: UiKey) -> Option<UiAction> {
        self.flush_projection();
        let action = {
            let battle_opt = if self.world.battle.active {
                Some(&mut self.world.battle)
            } else {
                None
            };
            self.ui.on_key(key, &self.state, battle_opt)
        };
        self.apply_internal_ui_action(&action);
        self.compositor.mark_ui_dirty();
        if self.active() && self.consume_ui_action(&action) {
            return None;
        }
        if action == UiAction::None {
            None
        } else {
            Some(action)
        }
    }

    /// Handle a UI pointer event in logical `320x200` coordinates.
    pub fn on_pointer(&mut self, ev: PointerEvent) -> Option<UiAction> {
        self.flush_projection();
        self.ui.dirty = false;
        let action = {
            let battle_opt = if self.world.battle.active {
                Some(&mut self.world.battle)
            } else {
                None
            };
            self.ui.on_pointer(ev, &self.state, battle_opt)
        };
        self.apply_internal_ui_action(&action);
        if self.ui.dirty || action != UiAction::None {
            self.compositor.mark_ui_dirty();
            if self.active() && self.consume_ui_action(&action) {
                return None;
            }
        }
        if action == UiAction::None {
            None
        } else {
            Some(action)
        }
    }

    fn apply_internal_ui_action(&mut self, action: &UiAction) {
        match action {
            UiAction::ToggleHdMode => {
                self.compositor.set_hd_mode(self.ui.hd_mode);
            }
            UiAction::ReturnToTitle => {
                self.world.battle.active = false;
                self.world.fbp_override = Some(60);
            }
            UiAction::NewGame => {
                self.world.fbp_override = None;
            }
            _ => {}
        }
    }

    /// Handle commands emitted by `pal_core::game::Game`: UI-facing ones directly here,
    /// platform-facing ones queued in `self.out_cmds`.
    pub fn route_cmds(&mut self) {
        let Some(g) = self.game.as_mut() else { return };
        let cmds = g.drain_cmds();
        let mut ui_changed = false;
        for c in cmds {
            match c {
                HostCmd::ShowDialogue {
                    speaker,
                    text,
                    side,
                    portrait,
                } => {
                    let p = if portrait > 0 { Some(portrait as u16) } else { None };
                    self.ui.open_dialog(speaker, text, p, side == 1);
                    self.ui.sync_dialog_layout(&mut self.compositor.fonts, self.ui.hd_mode);
                    ui_changed = true;
                }
                HostCmd::HideDialogue => {
                    self.ui.dialog.choices.clear();
                    self.ui.dialog.choice_cursor = 0;
                    if matches!(self.ui.view, UiView::Dialog) {
                        self.ui.set_view(UiView::None);
                    }
                    ui_changed = true;
                }
                HostCmd::AskYesNo => {
                    self.ui.dialog.choices = if self.ui.lang.is_en() {
                        vec!["Yes".into(), "No".into()]
                    } else {
                        vec!["是".into(), "否".into()]
                    };
                    self.ui.dialog.choice_cursor = 0;
                    self.ui.set_view(UiView::Dialog);
                    ui_changed = true;
                }
                HostCmd::OpenShop(id) => {
                    let g = self.game.as_ref().unwrap();
                    self.ui.shop_items.clear();
                    self.ui.shop_cursor = 0;
                    if let Some(shop) = g.data.shops.get(&(id.max(0) as u16)) {
                        for e in shop.items.iter().filter(|e| e.item_id > 0) {
                            let it = g.data.items.get(&e.item_id);
                            let name = {
                                let n = g.item_display_name(e.item_id);
                                if n.is_empty() { format!("Item #{}", e.item_id) } else { n }
                            };
                            let price = it
                                .map(|i| i.price)
                                .filter(|&p| p != 0)
                                .unwrap_or(100)
                                .max(10) as u32;
                            self.ui.shop_items.push(ShopEntry {
                                item_id: e.item_id,
                                name,
                                price,
                                count: 1,
                            });
                        }
                    }
                    self.ui.set_view(UiView::Shop);
                    self.shop_open = true;
                    ui_changed = true;
                }
                HostCmd::OpenPawnshop => {
                    self.fill_pawnshop();
                    self.ui.set_view(UiView::PawnshopSell);
                    self.shop_open = true;
                    ui_changed = true;
                }
                HostCmd::Toast(t) => {
                    self.push_notice("toast", t.clone());
                    self.ui.toast_text = Some(t);
                    self.toast_ticks = 90;
                    ui_changed = true;
                }
                HostCmd::BattleStarted => {
                    self.push_notice("battle_started", "battle started");
                    self.world.battle.active = true;
                    self.world.fbp_override = None;
                    // Clear any stale overworld toast so it never hides the battle Spell Name Banner.
                    self.ui.toast_text = None;
                    self.toast_ticks = 0;
                    self.ui.open_battle();
                    ui_changed = true;
                }
                HostCmd::BattleEnded => {
                    self.push_notice("battle_ended", "battle ended");
                    self.world.battle.active = false;
                    self.world.battle.active_effect = None;
                    self.world.battle.spell_banner = None;
                    self.world.battle.dim_bg = false;
                    self.world.battle.prompting = false;
                    self.world.battle.float_texts.clear();
                    if matches!(self.ui.view, UiView::BattleHud(_) | UiView::BattleVictory) {
                        self.ui.set_view(UiView::None);
                    }
                    ui_changed = true;
                }
                HostCmd::BattlePrompt { hero } => {
                    self.world.battle.active_hero = hero;
                    let role_id = self
                        .game
                        .as_ref()
                        .and_then(|g| g.world.party.get(hero))
                        .map(|m| m.role_id as usize)
                        .unwrap_or(0);
                    self.ui.open_battle_for_role(role_id);
                    ui_changed = true;
                }
                HostCmd::BattleVictory {
                    exp,
                    cash,
                    level_ups,
                } => {
                    let lv_summary = if level_ups.is_empty() {
                        format!("EXP +{exp}, Wen +{cash}")
                    } else {
                        format!("EXP +{exp}, Wen +{cash}; {}", level_ups.join("; "))
                    };
                    self.push_notice("battle_victory", lv_summary);
                    self.ui.battle_victory = pal_ui::BattleVictoryInfo {
                        exp,
                        cash,
                        level_ups,
                    };
                    self.ui.set_view(UiView::BattleVictory);
                    self.victory_open = true;
                    ui_changed = true;
                }
                other => {
                    match &other {
                        HostCmd::LoadScene { scene_id, .. } => {
                            let sname = self
                                .game
                                .as_ref()
                                .and_then(|g| g.data.scenes.get(scene_id))
                                .map(|s| s.name.clone())
                                .unwrap_or_else(|| format!("Scene #{scene_id}"));
                            self.push_notice("scene_changed", format!("#{scene_id} {sname}"));
                            // Clear any transient toast from the previous room/battle on scene transition.
                            self.ui.toast_text = None;
                            self.toast_ticks = 0;
                            // Clear an older AI thought when entering a different scene (only if it predates scene entry).
                            if self.ui.ai_thought.is_some() && self.ai_thought_scene_id != Some(*scene_id) {
                                self.ui.ai_thought = None;
                                self.ui.ai_thought_dimmed = false;
                                self.ai_thought_scene_id = None;
                                self.ai_thought_gen = self.ai_thought_gen.wrapping_add(1);
                            }
                            ui_changed = true;
                        }
                        HostCmd::PlayRng { id, .. } => {
                            self.push_notice("rng", format!("RNG #{id}"));
                        }
                        HostCmd::GameOver => {
                            self.push_notice("game_over", "game over");
                        }
                        HostCmd::ReturnToTitle => {
                            self.push_notice("return_to_title", "return to title");
                        }
                        _ => {}
                    }
                    self.out_cmds.push_back(other);
                }
            }
        }
        if ui_changed {
            self.compositor.mark_ui_dirty();
        }
    }

    fn fill_pawnshop(&mut self) {
        let g = self.game.as_ref().unwrap();
        self.ui.shop_items.clear();
        for (&id, &n) in g.world.inventory.iter().filter(|(_, &n)| n > 0) {
            let Some(it) = g.data.items.get(&id) else { continue };
            // Protect unsellable quest/key items and zero-price story items.
            if !it.sellable || it.price <= 0 {
                continue;
            }
            let name = g.item_display_name(id);
            let price = (it.price / 2).max(1) as u32;
            self.ui.shop_items.push(ShopEntry {
                item_id: id,
                name,
                price,
                count: n.clamp(0, u16::MAX as i32) as u16,
            });
        }
        if self.ui.shop_items.is_empty() {
            self.ui.shop_cursor = 0;
        } else if self.ui.shop_cursor >= self.ui.shop_items.len() {
            self.ui.shop_cursor = self.ui.shop_items.len() - 1;
        }
    }

    pub fn consume_ui_action(&mut self, action: &UiAction) -> bool {
        let Some(g) = self.game.as_mut() else { return false };
        let party_role = |g: &Game, idx: usize| {
            g.world
                .party
                .get(idx)
                .map(|m| m.role_id as usize)
                .unwrap_or(g.world.leader_role() as usize)
        };
        let handled = match *action {
            UiAction::DialogAdvance => {
                if std::mem::take(&mut self.victory_open) {
                    g.reply(HostReply::ModalDone);
                } else if matches!(g.waiting_on(), Some(Wait::Dialogue)) {
                    g.reply(HostReply::DialogueDone);
                }
                if matches!(self.ui.view, UiView::Dialog) {
                    self.ui.set_view(UiView::None);
                }
                true
            }
            UiAction::DialogPickChoice(idx) => {
                self.ui.dialog.choices.clear();
                if matches!(self.ui.view, UiView::Dialog) {
                    self.ui.set_view(UiView::None);
                }
                g.reply(HostReply::Choice(idx == 0));
                true
            }
            UiAction::UseItem { item_id, role_idx } => {
                self.ui.set_view(UiView::None);
                let r = party_role(g, role_idx);
                g.menu_open = false;
                g.use_item(item_id, Some(r));
                true
            }
            UiAction::EquipItem {
                item_id, role_idx, ..
            } => {
                let r = party_role(g, role_idx);
                g.equip_from_menu(r, item_id);
                true
            }
            UiAction::CastMagic { magic_id, role_idx } => {
                let r = party_role(g, role_idx);
                g.cast_field_magic(r, magic_id, None);
                true
            }
            UiAction::BuyItem { item_id, price } => {
                g.buy_item(item_id, price);
                true
            }
            UiAction::SellItem { item_id, price } => {
                g.sell_item(item_id, price);
                self.fill_pawnshop();
                true
            }
            UiAction::BattleAttack { enemy_idx } => {
                g.battle_command(BattleCmd::Attack { target: enemy_idx })
            }
            UiAction::BattleCastMagic {
                magic_id,
                enemy_idx,
            } => g.battle_command(BattleCmd::Magic {
                magic: magic_id,
                target: enemy_idx,
            }),
            UiAction::BattleCoop { enemy_idx } => {
                g.battle_command(BattleCmd::Coop { target: enemy_idx })
            }
            UiAction::BattleUseItem { item_id } => {
                g.battle_command(BattleCmd::Item { item: item_id })
            }
            UiAction::BattleUseItemTarget { item_id, target } => {
                g.battle_command(BattleCmd::ItemTarget {
                    item: item_id,
                    target,
                })
            }
            UiAction::BattleDefend => g.battle_command(BattleCmd::Defend),
            UiAction::BattleFlee => {
                let ok = g.battle_command(BattleCmd::Flee);
                if ok && matches!(self.ui.view, UiView::BattleHud(_)) {
                    self.ui.set_view(UiView::None);
                }
                ok
            }
            UiAction::BattleAuto => g.battle_command(BattleCmd::Auto),
            UiAction::BattleCancelAuto => g.battle_cancel_auto(),
            _ => false,
        };
        self.after_ui();
        handled
    }

    pub fn after_ui(&mut self) {
        let Some(g) = self.game.as_mut() else { return };
        if self.shop_open && !matches!(self.ui.view, UiView::Shop | UiView::PawnshopSell) {
            self.shop_open = false;
            g.reply(HostReply::ModalDone);
        }
        g.menu_open = is_menu_view(&self.ui.view);
        self.route_cmds();
        self.project();
    }

    pub fn flush_projection(&mut self) {
        if std::mem::take(&mut self.render_dirty) {
            self.project();
        }
    }

    pub fn project(&mut self) {
        self.render_dirty = false;
        let Some(g) = self.game.as_ref() else { return };
        g.project_render_state(&mut self.state);
        let w = &g.world;
        let new_dbg = g.debug_mode || g.recorder.tainted_by_debug;
        if self.ui.debug_mode != new_dbg {
            self.ui.debug_mode = new_dbg;
            self.compositor.mark_ui_dirty();
        }
        let wr = &mut self.world;
        if let Some(sc) = w.scenes.get(&w.scene_id) {
            wr.map_id_override = (sc.map_id > 0).then_some(sc.map_id);
            wr.ev_start = sc.event_start;
            wr.ev_end = sc.event_end;
        }
        let v = &w.view;
        if matches!(
            self.ui.view,
            UiView::Title
                | UiView::TitleOptions
                | UiView::SaveLoadSlots(pal_ui::SaveSlotMode::LoadFromTitle)
        ) {
            wr.fbp_override = Some(60);
        } else {
            wr.fbp_override = (v.active_fbp >= 0).then_some(v.active_fbp as u16);
        }
        wr.cam_override = v.cam_override;
        wr.fade_alpha = v.fade_alpha.clamp(0.0, 1.0);
        wr.fade_color = Color::rgba(v.fade_color[0], v.fade_color[1], v.fade_color[2], 255);
        wr.crossfade_alpha = v.crossfade_alpha.clamp(0.0, 1.0);
        wr.crossfade_seq = v.crossfade_seq;
        wr.night_palette = w.night_palette;
        wr.wave_amplitude = if w.screen_wave > 0 {
            (w.screen_wave as f32 * 1.8).min(16.0)
        } else {
            0.0
        };
        if v.shake_ticks > 0 {
            let t = g.tick_count as f32 * 20.0;
            let lvl = v.shake_level as f32;
            wr.shake_offset = (
                ((t * 0.09).sin() * (lvl * 2.0 + 2.0)).round() as i32,
                ((t * 0.12).cos() * (lvl * 2.0 + 1.0)).round() as i32,
            );
        } else {
            wr.shake_offset = (0, 0);
        }
        self.project_battle();
    }

    fn project_battle(&mut self) {
        use pal_core::game::battle::{EnemyPose, HeroPose};
        let Some(g) = self.game.as_ref() else { return };
        let Some(b) = g.battle.as_ref() else {
            if !matches!(self.ui.view, UiView::BattleHud(_)) {
                let bv = &mut self.world.battle;
                bv.active = false;
                bv.prompting = false;
                bv.dim_bg = false;
                bv.spell_banner = None;
                bv.shake_offset = (0, 0);
                bv.active_effect = None;
                bv.float_texts.clear();
            }
            return;
        };
        let bv = &mut self.world.battle;
        bv.active = true;
        bv.battlefield_fbp = g.world.battlefield_id.max(0) as u16;
        bv.active_hero = b.active_hero;
        bv.prompting = b.prompting.is_some();
        bv.auto_battle = b.is_auto_repeat();
        bv.dim_bg = b.dim_bg;
        bv.spell_banner = b.spell_banner.clone();
        // Clear target cursor whenever not actively choosing an enemy/ally target (#172).
        if !matches!(
            self.ui.view,
            UiView::BattleHud(
                pal_ui::BattleSubPhase::SelectEnemyForAttack
                    | pal_ui::BattleSubPhase::SelectEnemyForCoop
                    | pal_ui::BattleSubPhase::SelectEnemyForMagic(_)
                    | pal_ui::BattleSubPhase::SelectEnemyForItem(_)
            )
        ) {
            bv.selected_enemy = None;
        }
        if !matches!(
            self.ui.view,
            UiView::BattleHud(
                pal_ui::BattleSubPhase::SelectAllyForMagic(_)
                    | pal_ui::BattleSubPhase::SelectAllyForItem(_)
            )
        ) {
            bv.selected_ally = None;
        }
        if b.shake_ticks > 0 {
            let t = b.anim_tick as f32 * 1.4;
            let amp = (b.shake_level as f32).max(1.0) * 1.6;
            bv.shake_offset = (
                (t.sin() * amp).round() as i32,
                ((t * 1.3).cos() * amp * 0.7).round() as i32,
            );
        } else {
            bv.shake_offset = (0, 0);
        }
        let u16c = |v: i32| v.clamp(0, u16::MAX as i32) as u16;
        bv.enemies = b
            .enemies
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let idle_n = e.idle_frames.max(1) as u32;
                let speed = (e.idle_anim_speed.max(1) as u32).saturating_mul(2).max(6);
                let frame = match e.pose {
                    EnemyPose::Idle => ((b.anim_tick / speed) % idle_n) as usize,
                    EnemyPose::Attack => {
                        let atk_n = e.attack_frames.max(1) as u32;
                        let step = ((14u32.saturating_sub(e.pose_ticks as u32)) / 4) % atk_n;
                        (idle_n + step) as usize
                    }
                    EnemyPose::Magic => {
                        let mag_n = e.magic_frames.max(e.attack_frames).max(1) as u32;
                        let step = ((12u32.saturating_sub(e.pose_ticks as u32)) / 4) % mag_n;
                        (idle_n + step) as usize
                    }
                    EnemyPose::Hurt => 0,
                };
                let mut statuses: Vec<u16> = e
                    .statuses
                    .iter()
                    .filter(|(_, &turns)| turns > 0)
                    .map(|(&st, _)| st)
                    .collect();
                if !e.poisons.is_empty() {
                    statuses.push(99);
                }
                let recoil = if e.pose == EnemyPose::Hurt && e.flash > 0 { (-4, -2) } else { (0, 0) };
                BattleEnemyVisual {
                    slot: i,
                    sprite_id: e.sprite_id,
                    frame,
                    x: e.x + e.lunge.0 + recoil.0,
                    y: e.y + e.lunge.1 + recoil.1,
                    hp: u16c(e.hp),
                    max_hp: u16c(e.max_hp.max(1)),
                    alive: e.hp > 0,
                    flash: e.flash > 0,
                    name: g.enemy_display_name(e.enemy_id, &e.name, &e.name_hant),
                    fade_out: if e.hp <= 0 { e.death_fade } else { 0 },
                    statuses,
                }
            })
            .collect();
        bv.heroes = b
            .heroes
            .iter()
            .filter_map(|h| {
                let r = g.world.roles.get(h.role)?;
                // Authentic 1995 DOS PAL `F.MKF` frame indices:
                // 0=Idle, 1=LowHP(<20%), 2=KO/Dead, 3=Defend, 4=Hurt,
                // 5=CastPrep, 6=CastRelease, 7=AtkPrep, 8=AtkLunge, 9=AtkSlash.
                let frame = if r.hp <= 0 {
                    2
                } else {
                    match h.pose {
                        HeroPose::Hurt => 4,
                        HeroPose::AttackPrep => 7,
                        HeroPose::AttackLunge => 8,
                        HeroPose::AttackSlash => 9,
                        HeroPose::CastPrep => 5,
                        HeroPose::CastRelease => 6,
                        HeroPose::Defend => 3,
                        HeroPose::Idle => {
                            if h.defending {
                                3
                            } else if r.hp * 5 < r.max_hp.max(1) {
                                1
                            } else {
                                0
                            }
                        }
                    }
                };
                let mut statuses: Vec<u16> = r
                    .statuses
                    .iter()
                    .filter(|(_, &turns)| turns > 0)
                    .map(|(&st, _)| st)
                    .collect();
                if !r.poisons.is_empty() {
                    statuses.push(99);
                }
                let recoil = if h.pose == HeroPose::Hurt && h.flash > 0 { (4, 2) } else { (0, 0) };
                Some(BattleHeroVisual {
                    role_id: h.role as u16,
                    sprite_id: r.battle_sprite_f,
                    frame,
                    x: h.x + h.lunge.0 + recoil.0,
                    y: h.y + h.lunge.1 + recoil.1,
                    hp: u16c(r.hp),
                    max_hp: u16c(r.max_hp.max(1)),
                    mp: u16c(r.mp),
                    max_mp: u16c(r.max_mp.max(1)),
                    flash: h.flash > 0,
                    name: g.role_display_name(h.role),
                    avatar_rgm: r.avatar_rgm,
                    atb_pct: h.atb_pct,
                    defending: h.defending,
                    statuses,
                })
            })
            .collect();
        bv.active_effect = b.fire.map(|f| BattleEffectVisual {
            fire_id: f.fire_id,
            frame: f.frame as usize,
            x: f.x,
            y: f.y,
            alpha: 255,
            target_all: f.target_all,
            target_enemy: f.target_enemy,
            is_summon: f.is_summon,
        });
        bv.float_texts = b
            .float_texts
            .iter()
            .map(|ft| BattleFloatText {
                x: ft.x,
                y: ft.y - ft.age / 2,
                text: ft.text.clone(),
                color: rgb(ft.color),
            })
            .collect();
    }

    /// `true` when rendering now would reproduce the last rendered frame exactly, so a host may skip
    /// drawing and presenting. Compares everything the compositor reads: the `GameState` /
    /// `WorldRenderState` projections (the tick-driven `frame_count` only while the screen wave
    /// uses it), HD/retro mode, the asset cache generation (pop-in) and the UI dirty flags.
    /// Host-side overlays drawn outside the compositor are the host's responsibility.
    pub fn frame_unchanged(&mut self) -> bool {
        self.flush_projection();
        if self.ui.dirty || self.compositor.ui_dirty {
            return false;
        }
        self.last_render_key
            .as_deref()
            .is_some_and(|k| k.matches(&self.state, &self.world, self.compositor.hd_mode, &self.compositor.cache))
    }

    /// Render the current world + UI frame into `self.compositor.fb`.
    pub fn render_frame(&mut self) {
        self.render_frame_inner();
        // Generation is read after rendering: requests issued by this render that fail/arrive
        // later bump it and force the next frame.
        self.last_render_key = Some(Box::new(RenderKey::new(
            &self.state,
            &self.world,
            self.compositor.hd_mode,
            &self.compositor.cache,
        )));
    }

    fn render_frame_inner(&mut self) {
        self.flush_projection();
        // Animation clock for render-only effects (screen wave): follow the 50 Hz game tick so
        // effects keep their speed whatever the host's draw rate (desktop/web cap drawing at 15 fps).
        self.world.frame_count = match self.game.as_ref() {
            Some(g) => g.tick_count,
            None => self.world.frame_count + 1,
        };
        let state = &self.state;
        let world = &self.world;
        let ui = &mut self.ui;
        let battle_ref = if world.battle.active {
            Some(&world.battle)
        } else {
            None
        };
        self.compositor
            .render_frame(state, world, |fb, cache, fonts, hd| {
                draw_ui(fb, cache, fonts, ui, state, battle_ref, hd);
            });
    }
}

/// Snapshot of the compositor's inputs for [`PalRuntime::frame_unchanged`]. Only what is on screen
/// counts: in the field view, the map, the view origin and the viewport-culled sprite list (the
/// same selection `render_world` draws) instead of the whole `GameState`, so off-screen NPC
/// animation, scripts or cash changes don't force a redraw. Battles, RNG cutscenes and full-screen
/// FBPs only read `WorldRenderState`.
struct RenderKey {
    field: Option<pal_render::world::FieldFrameKey>,
    world: WorldRenderState,
    hd: bool,
    cache_gen: u64,
}

impl RenderKey {
    fn field_key(
        state: &GameState,
        world: &WorldRenderState,
        cache: &pal_render::cache::AssetCache,
    ) -> Option<pal_render::world::FieldFrameKey> {
        let field_view = world.active_rng.is_none() && world.fbp_override.is_none() && !world.battle.active;
        field_view.then(|| pal_render::world::FieldFrameKey::new(state, world, cache))
    }

    /// `frame_count` advances every tick but only feeds the screen-wave effect.
    fn normalized_world(world: &WorldRenderState) -> WorldRenderState {
        let mut w = world.clone();
        if w.wave_amplitude <= 0.01 {
            w.frame_count = 0;
        }
        w
    }

    fn new(state: &GameState, world: &WorldRenderState, hd: bool, cache: &pal_render::cache::AssetCache) -> Self {
        Self {
            field: Self::field_key(state, world, cache),
            world: Self::normalized_world(world),
            hd,
            cache_gen: cache.generation,
        }
    }

    fn matches(&self, state: &GameState, world: &WorldRenderState, hd: bool, cache: &pal_render::cache::AssetCache) -> bool {
        self.hd == hd
            && self.cache_gen == cache.generation
            && Self::normalized_world(world) == self.world
            && Self::field_key(state, world, cache) == self.field
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_lang_and_hd_sync() {
        let mut rt = PalRuntime::new(true);
        rt.init_game();
        rt.set_lang(pal_core::Language::ZhHans);
        assert!(!rt.ui.traditional_chinese);
        assert_eq!(rt.ui.lang, pal_core::Language::ZhHans);
        assert!(!rt.game.as_ref().unwrap().world.hant);
        rt.set_lang(pal_core::Language::En);
        assert_eq!(rt.ui.lang, pal_core::Language::En);
        assert_eq!(rt.game.as_ref().unwrap().world.lang, pal_core::Language::En);
        rt.set_hd_mode(false);
        assert!(!rt.ui.hd_mode);
        assert!(!rt.synth.hd_mode);
        assert_eq!(rt.compositor.fb.width, 320);
        assert_eq!(rt.compositor.fb.height, 200);
    }
}
