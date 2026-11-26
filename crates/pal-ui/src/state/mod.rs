//! Pure-Rust 1995 DOS PAL UI State Machine & Input Controller.


mod battle;
mod keys;
mod pointer;
use crate::input::{HitRect, HitTag, PointerEvent, PointerKind, UiAction, UiKey};
use pal_core::state::GameState;
use pal_render::world::BattleVisuals;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleSelectTarget {
    Status,
    Magic,
    Equip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveSlotMode {
    LoadFromTitle,
    LoadInGame,
    SaveInGame,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BattleItemFilter {
    #[default]
    All,
    UseOnly,
    ThrowOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BattleSubPhase {
    CommandWheel,
    MiscMenu,
    SelectEnemyForAttack,
    SelectEnemyForCoop,
    SelectMagic,
    SelectEnemyForMagic(u16),
    SelectAllyForMagic(u16),
    SelectItem,
    SelectEnemyForItem(u16),
    SelectAllyForItem(u16),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BattleVictoryInfo {
    pub exp: i32,
    pub cash: i32,
    pub level_ups: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiView {
    None,
    Title,
    TitleOptions,
    Dialog,
    RootMenu,
    RoleSelect(RoleSelectTarget),
    StatusFbp000,
    EquipFbp001,
    MagicGrid,
    ItemSubmenu,
    ItemGrid,
    SystemSubmenu,
    RemakeSubmenu,
    SaveLoadSlots(SaveSlotMode),
    ReplayChapters,
    ReplayFinished,
    Shop,
    PawnshopSell,
    BattleHud(BattleSubPhase),
    BattleVictory,
}

#[derive(Clone, Debug, Default)]
pub struct DialogModel {
    pub speaker: String,
    pub text: String,
    pub portrait_rgm: Option<u16>,
    pub at_top: bool,
    pub choices: Vec<String>,
    pub choice_cursor: usize,
    /// Current 0-based page index (max 3 lines per page; speaker name stays visible on every page).
    pub page: usize,
    /// Total pages computed deterministically via `layout_dialog_pages` (at least 1, #217).
    pub total_pages: usize,
    /// Pre-wrapped visible text for each page (length == `total_pages`), shared by renderer & Text Mode (#217).
    pub pages: Vec<String>,
}

/// Snapshot of the opposite speaker's last dialogue line, kept visible on-screen in authentic
/// 1995 SDLPAL dual-speaker conversations (upper-left + lower-right) until overwritten or closed.
#[derive(Clone, Debug)]
pub struct PrevDialogSnapshot {
    pub speaker: String,
    pub lines: Vec<String>,
    pub portrait_rgm: Option<u16>,
    pub at_top: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ShopEntry {
    pub item_id: u16,
    pub name: String,
    pub price: u32,
    pub count: u16,
}

#[derive(Clone, Debug, Default)]
pub struct SaveSlotSummary {
    pub slot: usize,
    pub label: String,
    pub exists: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Default)]
pub struct ReplayChapterSummary {
    pub index: usize,
    pub start_tick: u64,
    pub scene_id: u16,
    pub label: String,
    /// Decoded RGBA8 thumbnail `(width, height, rgba_bytes)` for chapter preview.
    pub thumb_rgba: Option<(u32, u32, Vec<u8>)>,
}

/// Optional display-name catalog (populated from `objects.json` / `magics.json` or defaults).
#[derive(Clone, Debug, Default)]
pub struct UiCatalog {
    pub role_names: [String; 6],
    pub item_names: HashMap<u16, String>,
    pub item_descs: HashMap<u16, String>,
    pub item_prices: HashMap<u16, u32>,
    pub item_icons: HashMap<u16, u16>,
    pub item_equip_slot: HashMap<u16, u8>,
    pub item_equip_mask: HashMap<u16, u8>,
    pub item_usable: HashMap<u16, bool>,
    pub item_throwable: HashMap<u16, bool>,
    pub item_target_all: HashMap<u16, bool>,
    pub magic_names: HashMap<u16, String>,
    pub magic_descs: HashMap<u16, String>,
    pub magic_mp_cost: HashMap<u16, u16>,
    pub magic_target_enemy: HashMap<u16, bool>,
    pub magic_target_all: HashMap<u16, bool>,
}

impl UiCatalog {
    pub fn with_defaults() -> Self {
        Self {
            role_names: [
                "李逍遙".into(),
                "趙靈兒".into(),
                "林月如".into(),
                "巫后".into(),
                "阿奴".into(),
                "蓋羅嬌".into(),
            ],
            item_names: HashMap::new(),
            item_descs: HashMap::new(),
            item_prices: HashMap::new(),
            item_icons: HashMap::new(),
            item_equip_slot: HashMap::new(),
            item_equip_mask: HashMap::new(),
            item_usable: HashMap::new(),
            item_throwable: HashMap::new(),
            item_target_all: HashMap::new(),
            magic_names: HashMap::new(),
            magic_descs: HashMap::new(),
            magic_mp_cost: HashMap::new(),
            magic_target_enemy: HashMap::new(),
            magic_target_all: HashMap::new(),
        }
    }

    pub fn role_name(&self, role_id: u16) -> &str {
        self.role_names
            .get(role_id as usize)
            .filter(|s| !s.is_empty())
            .map(|s| s.as_str())
            .unwrap_or("俠客")
    }

    pub fn item_name(&self, item_id: u16) -> String {
        if item_id == 0 {
            return "—".into();
        }
        self.item_names
            .get(&item_id)
            .cloned()
            .unwrap_or_else(|| format!("物品#{:03}", item_id))
    }

    pub fn item_icon(&self, item_id: u16) -> usize {
        self.item_icons
            .get(&item_id)
            .copied()
            .map(|id| id as usize)
            .unwrap_or_else(|| (item_id as usize) % 231)
    }

    pub fn magic_name(&self, magic_id: u16) -> String {
        if magic_id == 0 {
            return "—".into();
        }
        self.magic_names
            .get(&magic_id)
            .cloned()
            .unwrap_or_else(|| format!("仙術#{:03}", magic_id))
    }

    pub fn magic_cost(&self, magic_id: u16) -> u16 {
        self.magic_mp_cost.get(&magic_id).copied().unwrap_or(5)
    }
}

/// Remembered battle selections for a single playable role (`0..6`), matching the 1995 DOS
/// behavior where each character's last command/spell/item/target is recalled on their next turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoleBattleMemory {
    /// Last Bagua wheel selection: 0=Attack, 1=Magic, 2=Coop, 3=Misc
    pub cmd_cursor: usize,
    /// Last Misc submenu selection: 0=Item, 1=Defend, 2=Flee
    pub misc_cursor: usize,
    /// Last targeted enemy index
    pub enemy_cursor: usize,
    /// Last magic ID cast by this role in battle
    pub last_magic_id: u16,
    /// Last item ID used by this role in battle
    pub last_item_id: u16,
}

pub struct UiState {
    pub view: UiView,
    pub dirty: bool,
    pub title_cursor: usize,
    pub title_options_cursor: usize,
    pub replay_finished_cursor: usize,
    pub root_cursor: usize,
    pub role_cursor: usize,
    pub active_role_idx: usize,
    pub item_sub_cursor: usize,
    pub sys_sub_cursor: usize,
    pub remake_sub_cursor: usize,
    pub grid_cursor: usize,
    pub equip_slot_cursor: usize,
    pub save_slot_cursor: usize,
    pub shop_cursor: usize,
    /// Armed row index in `UiView::PawnshopSell` awaiting a 2nd Confirm to sell (#181).
    pub pawnshop_confirm_idx: Option<usize>,
    pub battle_cmd_cursor: usize,
    pub battle_misc_cursor: usize,
    pub battle_enemy_cursor: usize,
    pub battle_ally_cursor: usize,
    pub battle_item_filter: BattleItemFilter,
    /// True when `UiView::StatusFbp000` was opened from battle (`S` shortcut, #274) so Cancel returns to BattleHud.
    pub status_from_battle: bool,
    pub battle_victory: BattleVictoryInfo,
    pub battle_memory: [RoleBattleMemory; 6],
    pub field_magic_memory: [u16; 6],
    pub field_item_memory: u16,
    pub music_on: bool,
    pub sfx_on: bool,
    pub hd_mode: bool,
    pub fullscreen: bool,
    /// 0 = Off, 1 = Local (127.0.0.1), 2 = LAN (0.0.0.0)
    pub agent_mode: u8,
    /// Active per-session LAN auth token displayed persistently in menus when `agent_mode == 2` (#236).
    pub agent_token: Option<String>,
    pub traditional_chinese: bool,
    pub lang: pal_core::Language,
    pub dialog: DialogModel,
    pub prev_dialog: Option<PrevDialogSnapshot>,
    pub shop_items: Vec<ShopEntry>,
    pub save_slots: Vec<SaveSlotSummary>,
    /// Exclusive replay mode: replaces gameplay RootMenu with ReplayMenu (Chapters/TakeOver/System/Remake).
    pub replay_mode: bool,
    pub replay_chapters: Vec<ReplayChapterSummary>,
    pub replay_chapter_cursor: usize,
    pub catalog: UiCatalog,
    pub hit_rects: Vec<HitRect>,
    /// Optional available update version tag (e.g. `Some("v0.3.10")`) shown in Title & TitleOptions.
    pub update_available: Option<String>,
    /// Host-provided state of the "download all assets" item (Title Options + in-game Remake
    /// menu); `None` hides the item (loose-asset mode / no pack manager, #18). The label is
    /// formatted at draw time so it follows language switches.
    pub download_all: Option<DownloadAllState>,
    pub toast_text: Option<String>,
    /// Current AI thought/commentary subtitle (`think <text>`), rendered only in 4x HD mode.
    pub ai_thought: Option<String>,
    /// Spectator toggle (`F7` on Desktop / HUD button on Web) for showing/hiding the HD AI thought subtitle bar.
    pub show_ai_thought: bool,
    /// True after 12 s wall-clock without a new thought so the subtitle dims to ~45% opacity.
    pub ai_thought_dimmed: bool,
    /// True when `debug_mode` is active (live session or replaying a debug-mode `.repal`, #235).
    /// Renders an unmistakable `DEBUG ENABLED` watermark in both HD and Retro modes.
    pub debug_mode: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self::new()
    }
}

/// Total number of save slots (20 pages × 5 slots per page in the DOS Save/Load UI).
pub const SAVE_SLOT_COUNT: usize = 100;

impl UiState {
    pub fn new() -> Self {
        let save_slots = (0..SAVE_SLOT_COUNT)
            .map(|i| SaveSlotSummary {
                slot: i,
                label: format!("進度 {:02}", i + 1),
                exists: i == 0,
                detail: if i == 0 {
                    "餘杭客棧 Lv.1".into()
                } else {
                    "空".into()
                },
            })
            .collect();

        Self {
            view: UiView::Title,
            dirty: true,
            title_cursor: 0,
            title_options_cursor: 0,
            replay_finished_cursor: 0,
            root_cursor: 0,
            role_cursor: 0,
            active_role_idx: 0,
            item_sub_cursor: 0,
            sys_sub_cursor: 0,
            remake_sub_cursor: 0,
            grid_cursor: 0,
            equip_slot_cursor: 0,
            save_slot_cursor: 0,
            shop_cursor: 0,
            pawnshop_confirm_idx: None,
            battle_cmd_cursor: 0,
            battle_misc_cursor: 0,
            battle_enemy_cursor: 0,
            battle_ally_cursor: 0,
            battle_item_filter: BattleItemFilter::All,
            status_from_battle: false,
            battle_victory: BattleVictoryInfo::default(),
            battle_memory: [RoleBattleMemory::default(); 6],
            field_magic_memory: [0; 6],
            field_item_memory: 0,
            music_on: true,
            sfx_on: true,
            hd_mode: true,
            fullscreen: false,
            agent_mode: 0,
            agent_token: None,
            traditional_chinese: true,
            lang: pal_core::Language::ZhHant,
            dialog: DialogModel::default(),
            prev_dialog: None,
            shop_items: Vec::new(),
            save_slots,
            replay_mode: false,
            replay_chapters: Vec::new(),
            replay_chapter_cursor: 0,
            catalog: UiCatalog::with_defaults(),
            hit_rects: Vec::new(),
            update_available: None,
            download_all: None,
            toast_text: None,
            ai_thought: None,
            show_ai_thought: true,
            ai_thought_dimmed: false,
            debug_mode: false,
        }
    }

    pub fn set_view(&mut self, view: UiView) {
        if self.view != view {
            self.pawnshop_confirm_idx = None;
        }
        if view != UiView::Dialog {
            self.prev_dialog = None;
        }
        self.view = view;
        self.dirty = true;
    }

    pub fn is_modal_open(&self) -> bool {
        self.view != UiView::None
    }

    pub fn set_lang(&mut self, lang: pal_core::Language) {
        self.lang = lang;
        self.traditional_chinese = lang.is_hant();
        self.dirty = true;
    }

    pub fn cycle_lang(&mut self) -> pal_core::Language {
        let next = self.lang.next();
        self.set_lang(next);
        next
    }

    /// Strip stray unmatched trailing ASCII `(` or `)` (original 1995 DOS PAL `M.MSG` wait/icon
    /// control markers that leaked into extracted script text, #182) without altering balanced parentheticals.
    pub fn sanitize_dialog_text(raw: &str) -> String {
        raw.lines()
            .map(|line| {
                let trimmed_end = line.trim_end_matches(' ');
                if trimmed_end.ends_with('(') && !trimmed_end.contains(')') {
                    trimmed_end.trim_end_matches('(')
                } else if trimmed_end.ends_with(')') && !trimmed_end.contains('(') {
                    trimmed_end.trim_end_matches(')')
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn open_dialog(&mut self, speaker: impl Into<String>, text: impl Into<String>, portrait_rgm: Option<u16>, at_top: bool) {
        let speaker_str = speaker.into();
        let clean = Self::sanitize_dialog_text(&text.into());
        let new_is_unnamed = speaker_str.is_empty() && portrait_rgm.is_none();
        let new_is_notice = new_is_unnamed
            && !clean.trim().contains('\n')
            && clean.trim().chars().count() <= 26
            && crate::draw::is_system_notice_text(clean.trim(), true);

        if new_is_notice {
            self.prev_dialog = None;
        } else if self.view == UiView::Dialog && self.dialog.at_top != at_top {
            let prev_trimmed = self.dialog.text.trim();
            let prev_unnamed = self.dialog.speaker.is_empty() && self.dialog.portrait_rgm.is_none();
            let prev_is_notice = prev_unnamed
                && !prev_trimmed.contains('\n')
                && prev_trimmed.chars().count() <= 26
                && crate::draw::is_system_notice_text(prev_trimmed, true);
            if !prev_trimmed.is_empty() && !prev_is_notice {
                let page_text = self
                    .dialog
                    .pages
                    .get(self.dialog.page)
                    .map(|s| s.as_str())
                    .unwrap_or(prev_trimmed);
                let lines: Vec<String> = page_text.lines().map(String::from).collect();
                self.prev_dialog = Some(PrevDialogSnapshot {
                    speaker: self.dialog.speaker.clone(),
                    lines,
                    portrait_rgm: self.dialog.portrait_rgm,
                    at_top: self.dialog.at_top,
                });
            }
        } else if self.prev_dialog.as_ref().is_some_and(|p| p.at_top == at_top) {
            self.prev_dialog = None;
        }

        self.toast_text = None;
        self.dialog = DialogModel {
            speaker: speaker_str,
            text: clean.clone(),
            portrait_rgm,
            at_top,
            choices: Vec::new(),
            choice_cursor: 0,
            page: 0,
            total_pages: 1,
            pages: vec![clean],
        };
        self.set_view(UiView::Dialog);
    }

    /// Deterministically compute `dialog.total_pages` and `dialog.pages` using the exact same
    /// line-wrapping layout as `draw_dialog` so headless and GUI pagination never diverge (#217).
    pub fn sync_dialog_layout(&mut self, fonts: &mut pal_render::font::FontEngine, hd: bool) {
        if self.view != UiView::Dialog {
            return;
        }
        let page_lines = crate::draw::layout_dialog_pages(&self.dialog, fonts, hd);
        self.dialog.total_pages = page_lines.len().max(1);
        self.dialog.pages = page_lines
            .into_iter()
            .map(|lines| lines.join("\n"))
            .collect();
        if self.dialog.pages.is_empty() {
            self.dialog.pages.push(self.dialog.text.clone());
        }
        self.dialog.page = self.dialog.page.min(self.dialog.total_pages - 1);
    }

    /// Return the visible text for the current dialog page (#217).
    pub fn current_dialog_page_text(&self) -> &str {
        self.dialog
            .pages
            .get(self.dialog.page)
            .map(|s| s.as_str())
            .unwrap_or(&self.dialog.text)
    }

    pub fn open_pawnshop_from_inventory(&mut self, state: &GameState) {
        self.shop_items = state
            .inventory
            .iter()
            .filter(|(_, &qty)| qty > 0)
            .map(|(&item_id, &count)| {
                let price = self
                    .catalog
                    .item_prices
                    .get(&item_id)
                    .copied()
                    .unwrap_or(100)
                    / 2;
                ShopEntry {
                    item_id,
                    name: self.catalog.item_name(item_id),
                    price: price.max(10),
                    count,
                }
            })
            .collect();
        self.shop_cursor = 0;
        self.set_view(UiView::PawnshopSell);
    }

    pub fn open_battle(&mut self) {
        self.open_battle_for_role(0);
    }

    /// Open the battle command wheel for `role_id`, recalling that role's last battle selection.
    pub fn open_battle_for_role(&mut self, role_id: usize) {
        let mem = self.battle_memory[role_id.min(5)];
        self.battle_cmd_cursor = mem.cmd_cursor;
        self.battle_misc_cursor = mem.misc_cursor;
        self.battle_enemy_cursor = mem.enemy_cursor;
        self.battle_item_filter = BattleItemFilter::All;
        self.status_from_battle = false;
        self.set_view(UiView::BattleHud(BattleSubPhase::CommandWheel));
    }

    fn restore_field_item_cursor(&mut self, state: &GameState) {
        let items: Vec<u16> = state
            .inventory
            .iter()
            .filter(|(_, &q)| q > 0)
            .map(|(&id, _)| id)
            .collect();
        self.grid_cursor = items
            .iter()
            .position(|&id| id == self.field_item_memory)
            .unwrap_or(0);
    }

    fn restore_field_magic_cursor(&mut self, state: &GameState) {
        let role_id = state
            .party
            .get(self.active_role_idx)
            .map(|p| p.role_id as usize)
            .unwrap_or(0)
            .min(5);
        let remembered = self.field_magic_memory[role_id];
        self.grid_cursor = state.roles[role_id]
            .magics
            .iter()
            .position(|&mid| mid == remembered)
            .unwrap_or(0);
    }

    /// Items that can be used or thrown in battle (respects `battle_item_filter`: All / UseOnly / ThrowOnly, #274).
    pub fn battle_usable_items(&self, state: &GameState) -> Vec<(u16, u16)> {
        let has_flags = !self.catalog.item_usable.is_empty() || !self.catalog.item_throwable.is_empty();
        state
            .inventory
            .iter()
            .filter(|(&id, &q)| {
                if q == 0 {
                    return false;
                }
                if !has_flags {
                    return true;
                }
                let usable = self.catalog.item_usable.get(&id).copied().unwrap_or(false);
                let throwable = self.catalog.item_throwable.get(&id).copied().unwrap_or(false);
                match self.battle_item_filter {
                    BattleItemFilter::All => usable || throwable,
                    BattleItemFilter::UseOnly => usable,
                    BattleItemFilter::ThrowOnly => throwable,
                }
            })
            .map(|(&id, &q)| (id, q))
            .collect()
    }
}

/// Host-provided "download all assets" status (#18).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DownloadAllState {
    /// Bytes of packages not cached yet (0 = everything downloaded).
    pub remaining_bytes: u64,
    /// `Some(0.0..=1.0)` while the background download runs.
    pub progress: Option<f32>,
}

/// Localized "download all assets" label for the given status.
pub fn download_all_text(
    lang: pal_core::Language,
    remaining_bytes: u64,
    progress: Option<f32>,
) -> String {
    use pal_core::Language;
    if let Some(f) = progress {
        let pct = (f.clamp(0.0, 1.0) * 100.0).round() as u32;
        return match lang {
            Language::En => format!("Downloading all… {pct}%"),
            Language::ZhHant => format!("下載全部資源… {pct}%"),
            Language::ZhHans => format!("下载全部资源… {pct}%"),
        };
    }
    if remaining_bytes == 0 {
        return match lang {
            Language::En => "All downloaded ✓".to_string(),
            Language::ZhHant => "資源已全部下載 ✓".to_string(),
            Language::ZhHans => "资源已全部下载 ✓".to_string(),
        };
    }
    let m = remaining_bytes as f64 / (1024.0 * 1024.0);
    match lang {
        Language::En => format!("Download all ({m:.0} MB)"),
        Language::ZhHant => format!("下載全部資源 ({m:.0} MB)"),
        Language::ZhHans => format!("下载全部资源 ({m:.0} MB)"),
    }
}

impl UiState {
    /// Current "download all assets" item label, or `None` when the item is hidden.
    pub fn download_all_label(&self) -> Option<String> {
        self.download_all
            .map(|d| download_all_text(self.lang, d.remaining_bytes, d.progress))
    }
}
