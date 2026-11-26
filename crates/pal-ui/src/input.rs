//! Input events, hit-test rectangles, and output UI actions for `pal-ui`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum UiKey {
    Up = 1,
    Down = 2,
    Left = 3,
    Right = 4,
    Confirm = 5,         // Enter / Space / Z / Left Ctrl / Numpad Enter
    Cancel = 6,          // Escape / Backspace / X / Insert / Alt / Numpad 0
    MenuToggle = 7,      // Alt / Insert / Numpad 0
    Tab = 8,             // Legacy / Replay Theater chapters only (no gameplay binding, #274)
    PageUp = 9,          // PgUp / Numpad 9 / Gamepad LB
    PageDown = 10,       // PgDn / Numpad 3 / Gamepad RB
    Home = 11,           // Home / Numpad 7
    End = 12,            // End / Numpad 1
    BattleRepeat = 13,   // R — Repeat last action
    BattleAuto = 14,     // A — Auto battle (围攻)
    BattleDefend = 15,   // D — Defend (防御)
    BattleUseItem = 16,  // E — Use item in battle (使用物品)
    BattleThrowItem = 17,// W — Throw item in battle (投掷物品)
    BattleFlee = 18,     // Q — Flee (逃跑)
    BattleForce = 19,    // F — Force attack (普攻/全体普攻)
    BattleStatus = 20,   // S — Battle status (状态)
}

impl UiKey {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::Up),
            2 => Some(Self::Down),
            3 => Some(Self::Left),
            4 => Some(Self::Right),
            5 => Some(Self::Confirm),
            6 => Some(Self::Cancel),
            7 => Some(Self::MenuToggle),
            8 => Some(Self::Tab),
            9 => Some(Self::PageUp),
            10 => Some(Self::PageDown),
            11 => Some(Self::Home),
            12 => Some(Self::End),
            13 => Some(Self::BattleRepeat),
            14 => Some(Self::BattleAuto),
            15 => Some(Self::BattleDefend),
            16 => Some(Self::BattleUseItem),
            17 => Some(Self::BattleThrowItem),
            18 => Some(Self::BattleFlee),
            19 => Some(Self::BattleForce),
            20 => Some(Self::BattleStatus),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum PointerKind {
    Move = 0,
    Down = 1,
    Up = 2,
    RightClick = 3,
}

impl PointerKind {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Down,
            2 => Self::Up,
            3 => Self::RightClick,
            _ => Self::Move,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerEvent {
    pub kind: PointerKind,
    /// Coordinates in logical `320x200` space (host converts canvas pixels to `0..320, 0..200`).
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitTag {
    TitleOption(usize),
    TitleLangToggle,
    TitleOptionsItem(usize),
    ReplayFinishedItem(usize),
    RootMenuItem(usize),
    RoleSelectItem(usize),
    ItemSubmenuItem(usize),
    SystemSubmenuItem(usize),
    RemakeSubmenuItem(usize),
    GridCell(usize),
    EquipSlot(usize),
    SaveSlot(usize),
    DialogConfirm,
    DialogChoice(usize),
    BattleCommand(usize),
    BattleMiscItem(usize),
    BattleEnemy(usize),
    BattleHero(usize),
    BattleCancelAuto,
    BattleVictoryConfirm,
    ShopRow(usize),
    ReplayChapter(usize),
    CopyAgentToken,
    RefreshAgentToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HitRect {
    /// Logical `320x200` coordinates
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub tag: HitTag,
}

impl HitRect {
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// High-level action emitted by `UiState` for the host/engine to execute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiAction {
    None,
    NewGame,
    LoadSlot(usize),
    SaveSlot(usize),
    ToggleHdMode,
    ToggleMusic,
    ToggleSfx,
    ToggleLanguage,
    ToggleFullscreen,
    ToggleAgentMode,
    CopyAgentToken,
    RefreshAgentToken,
    CheckForUpdate,
    /// Download every asset package not cached yet (desktop packs mode, #18).
    DownloadAllAssets,
    RestartRecording,
    ExportReplay,
    OpenReplay,
    ReplayJumpChapter(usize),
    ReplayTakeOver,
    ReplayRestart,
    CopyReplayReport,
    ExportSave,
    ImportSave,
    TakeScreenshot,
    ReturnToTitle,
    DialogAdvance,
    DialogPickChoice(usize),
    UseItem { item_id: u16, role_idx: usize },
    EquipItem { item_id: u16, slot: usize, role_idx: usize },
    CastMagic { magic_id: u16, role_idx: usize },
    BuyItem { item_id: u16, price: u32 },
    SellItem { item_id: u16, price: u32 },
    BattleAttack { enemy_idx: usize },
    BattleCastMagic { magic_id: u16, enemy_idx: usize },
    BattleCoop { enemy_idx: usize },
    BattleUseItem { item_id: u16 },
    BattleUseItemTarget { item_id: u16, target: usize },
    BattleDefend,
    BattleFlee,
    BattleAuto,
    BattleCancelAuto,
}
