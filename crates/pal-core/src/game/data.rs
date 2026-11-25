//! Static game database decoded from `site/assets/data/*.yaml`.
//!
//! The host (browser JS or a native test) parses YAML into JSON values and hands each document to
//! [`GameData::load_table`]; the core never parses YAML itself.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

fn de<T: for<'a> Deserialize<'a> + Default>(v: &Value) -> T {
    serde_json::from_value(v.clone()).unwrap_or_default()
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SceneDef {
    pub id: u16,
    pub name: String,
    pub map_id: u16,
    pub script_enter: String,
    pub script_teleport: String,
    pub event_start: u16,
    pub event_end: u16,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct EventDef {
    pub id: u16,
    pub vanish_time: i32,
    pub x: i32,
    pub y: i32,
    pub layer: i32,
    pub script_trigger: String,
    pub script_auto: String,
    pub state: i32,
    pub trigger_mode: i32,
    pub sprite_id: u16,
    pub sprite_frames: i32,
    pub direction: i32,
    pub current_frame: i32,
    pub script_idle_frame: i32,
    pub sprite_frames_auto: i32,
    pub interact_anchor: Vec<i32>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct EquipSlots {
    pub hat: u16,
    pub cloak: u16,
    pub armor: u16,
    pub weapon: u16,
    pub boots: u16,
    pub ornament: u16,
}

impl EquipSlots {
    /// `[hat, cloak, armor, weapon, boots, ornament]`.
    pub fn to_array(&self) -> [u16; 6] {
        [self.hat, self.cloak, self.armor, self.weapon, self.boots, self.ornament]
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct StatBlock {
    pub attack: i32,
    pub magic_power: i32,
    pub defense: i32,
    pub dexterity: i32,
    pub flee: i32,
}

impl StatBlock {
    /// `[attack, magic_power, defense, dexterity, flee]`.
    pub fn to_array(&self) -> [i32; 5] {
        [self.attack, self.magic_power, self.defense, self.dexterity, self.flee]
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RoleSfx {
    pub death: u16,
    pub attack: u16,
    pub weapon: u16,
    pub critical: u16,
    pub magic: u16,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RoleDef {
    pub role_id: u16,
    pub name: String,
    pub name_hant: String,
    pub avatar_rgm: u16,
    pub battle_sprite_f: u16,
    pub walk_sprite_mgo: u16,
    pub level: i32,
    pub max_hp: i32,
    pub max_mp: i32,
    pub hp: i32,
    pub mp: i32,
    pub equipment: EquipSlots,
    pub stats: StatBlock,
    pub initial_magics: Vec<u16>,
    pub coop_magic: u16,
    pub sfx: RoleSfx,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct LevelupMagicEntry {
    pub level: i32,
    pub magic_id: u16,
    pub magic_name: String,
}

/// Role keys used by `roles.yaml::levelup_magic` rows, indexed by role id.
pub const ROLE_KEYS: [&str; 6] = ["li_xiaoyao", "zhao_linger", "lin_yueru", "wu_hou", "a_nu", "gai_luojiao"];

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ItemDef {
    pub id: u16,
    pub name: String,
    pub name_hant: String,
    pub icon_id: u16,
    pub price: i32,
    pub script_use: String,
    pub script_equip: String,
    pub script_throw: String,
    pub flags: u16,
    pub usable: bool,
    pub equipable: bool,
    pub throwable: bool,
    pub consumable: bool,
    pub target_all: bool,
    pub sellable: bool,
    pub desc: String,
    pub desc_hant: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct MagicDef {
    pub obj_id: u16,
    pub name: String,
    pub name_hant: String,
    pub script_success: String,
    pub script_use: String,
    pub usable_outside_battle: bool,
    pub usable_in_battle: bool,
    pub target_enemy: bool,
    pub target_all: bool,
    pub fire_effect_id: i32,
    pub cost_mp: i32,
    pub base_damage: i32,
    pub elemental: i32,
    pub sfx_id: u16,
    #[serde(rename = "type")]
    pub magic_type: i32,
    pub summon_effect: i32,
    pub desc: String,
    pub desc_hant: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct EnemyDef {
    pub enemy_id: u16,
    pub abc_sprite_id: u16,
    pub name: String,
    pub name_hant: String,
    pub idle_frames: i32,
    pub magic_frames: i32,
    pub attack_frames: i32,
    pub idle_anim_speed: i32,
    pub sfx_attack: i32,
    pub sfx_magic: i32,
    pub sfx_death: i32,
    pub hp: i32,
    pub exp: i32,
    pub cash: i32,
    pub level: i32,
    pub magic_id: u16,
    pub magic_rate: i32,
    pub attack_equiv_item: u16,
    pub attack_equiv_rate: i32,
    pub steal_item: u16,
    pub steal_count: i32,
    /// Raw signed-16 stat offsets as stored in the data file; see [`EnemyDef::effective`].
    pub attack: i32,
    pub magic_power: i32,
    pub defense: i32,
    pub dexterity: i32,
    pub flee: i32,
    pub resist_poison: i32,
    pub resist_elem: Vec<i32>,
    pub resist_phys: i32,
    pub dual_move: i32,
    pub collect_value: i32,
    pub script_pre_turn: String,
    pub script_in_turn: String,
    pub script_post_battle: String,
}

impl EnemyDef {
    /// Effective DOS stat `(level + 6) * 6 + s16(raw)` for attack/magic/dexterity/flee.
    pub fn effective(&self, raw: i32, min: i32) -> i32 {
        ((self.level + 6) * 6 + (raw as u16 as i16 as i32)).max(min)
    }

    /// Effective DOS defense `(level + 6) * 4 + s16(defense)` (SDLPAL `fight.c`).
    pub fn effective_defense(&self) -> i32 {
        ((self.level + 6) * 4 + (self.defense as u16 as i16 as i32)).max(0)
    }

    /// Legacy heuristic boss flag (kept for parity with `wasm-bridge.js::_packEnemyTemplate`).
    pub fn is_boss(&self) -> bool {
        self.dual_move > 0 || (self.hp >= 1000 && self.collect_value == 0)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ShopEntry {
    pub item_id: u16,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ShopDef {
    pub shop_id: u16,
    pub items: Vec<ShopEntry>,
}

/// Visual-only appearance metadata for an MGO sprite used strictly in `--obs fair` mode (#278).
///
/// IMPORTANT: Must contain ONLY what is visually observable in frame 0 (never dialogue/script
/// identities such as character names or destinations).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SpriteVisualDef {
    pub id: u16,
    #[serde(alias = "visual_zh")]
    pub visual: String,
    pub visual_hant: String,
    pub visual_en: String,
    pub visual_kind: String,
}

/// Every static table the game core needs.
#[derive(Clone, Debug, Default)]
pub struct GameData {
    pub scenes: BTreeMap<u16, SceneDef>,
    pub events: BTreeMap<u16, EventDef>,
    pub roles: Vec<RoleDef>,
    pub levelup_exp: Vec<i32>,
    pub levelup_magic: Vec<BTreeMap<String, LevelupMagicEntry>>,
    pub items: BTreeMap<u16, ItemDef>,
    pub magics: BTreeMap<u16, MagicDef>,
    pub enemies: BTreeMap<u16, EnemyDef>,
    pub teams: BTreeMap<u16, Value>,
    pub battlefields: BTreeMap<u16, Value>,
    pub shops: BTreeMap<u16, ShopDef>,
    /// Optional visual-only sprite descriptions for `--obs fair` mode (#278).
    pub sprite_visuals: BTreeMap<u16, SpriteVisualDef>,
    /// Decoupled English translation overlay (`site/assets/data/i18n/en/*.json`).
    pub i18n_en: crate::I18nOverlay,
}

impl GameData {
    /// Merge an English i18n JSON overlay (`tables.json`, `scripts_*.json`).
    pub fn load_i18n_en(&mut self, doc: &Value) {
        self.i18n_en.merge_json(doc);
    }

    /// Load visual-only sprite appearance entries from a YAML/JSON document (#278).
    ///
    /// Supports `{ "sprites": [ { "id": 62, "visual": "...", "visual_kind": "door" }, ... ] }`
    /// (matching both standalone `sprite_visuals.yaml` and `tools/mgo_catalog.yaml`), a top-level
    /// array, or an id-keyed object `{ "62": { "visual": "...", "visual_kind": "door" } }`.
    /// Deliberately ignores `name`, `speaker_hints`, and all non-visual fields so script identities
    /// can never leak into `--obs fair` mode.
    pub fn load_sprite_visuals(&mut self, doc: &Value, overwrite: bool) {
        let mut insert_one = |mut sv: SpriteVisualDef| {
            sv.visual = sv.visual.trim().to_string();
            sv.visual_hant = sv.visual_hant.trim().to_string();
            sv.visual_en = sv.visual_en.trim().to_string();
            sv.visual_kind = sv.visual_kind.trim().to_ascii_lowercase();
            if sv.id == 0
                || (sv.visual.is_empty()
                    && sv.visual_hant.is_empty()
                    && sv.visual_en.is_empty()
                    && sv.visual_kind.is_empty())
            {
                return;
            }
            if overwrite || !self.sprite_visuals.contains_key(&sv.id) {
                self.sprite_visuals.insert(sv.id, sv);
            }
        };
        if let Some(arr) = doc.get("sprites").and_then(Value::as_array) {
            for v in arr {
                insert_one(de::<SpriteVisualDef>(v));
            }
        } else if let Some(arr) = doc.as_array() {
            for v in arr {
                insert_one(de::<SpriteVisualDef>(v));
            }
        } else if let Some(obj) = doc.as_object() {
            for (k, v) in obj {
                if let Ok(id) = k.parse::<u16>() {
                    let mut sv: SpriteVisualDef = de(v);
                    if sv.id == 0 {
                        sv.id = id;
                    }
                    insert_one(sv);
                }
            }
        }
    }
    /// Load one decoded YAML document. `table` is the file stem (`scenes`, `events`, `roles`,
    /// `items`, `magic`, `enemies`, `shops`). Unknown tables are ignored.
    pub fn load_table(&mut self, table: &str, doc: &Value) {
        let list = |k: &str| doc.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
        match table {
            "scenes" => {
                for v in list("scenes") {
                    let s: SceneDef = de(&v);
                    self.scenes.insert(s.id, s);
                }
            }
            "events" => {
                for v in list("events") {
                    let e: EventDef = de(&v);
                    self.events.insert(e.id, e);
                }
            }
            "roles" => {
                self.roles = list("roles").iter().map(de).collect();
                self.levelup_exp = list("levelup_exp").iter().map(|v| v.as_i64().unwrap_or(0) as i32).collect();
                self.levelup_magic = list("levelup_magic").iter().map(de).collect();
            }
            "items" => {
                for v in list("items") {
                    let it: ItemDef = de(&v);
                    self.items.insert(it.id, it);
                }
            }
            "magic" => {
                for v in list("magics") {
                    let m: MagicDef = de(&v);
                    self.magics.insert(m.obj_id, m);
                }
            }
            "enemies" => {
                for v in list("enemies") {
                    let e: EnemyDef = de(&v);
                    self.enemies.insert(e.enemy_id, e);
                }
                for v in list("teams") {
                    if let Some(id) = v.get("team_id").and_then(Value::as_u64) {
                        self.teams.insert(id as u16, v.clone());
                    }
                }
                for v in list("battlefields") {
                    if let Some(id) = v.get("id").and_then(Value::as_u64) {
                        self.battlefields.insert(id as u16, v.clone());
                    }
                }
            }
            "shops" => {
                for v in list("shops") {
                    let s: ShopDef = de(&v);
                    self.shops.insert(s.shop_id, s);
                }
            }
            "sprite_visuals" => {
                self.load_sprite_visuals(doc, true);
            }
            _ => {}
        }
    }

    /// EXP needed to advance from `level` (legacy `GameState.getNextLevelExp`).
    pub fn next_level_exp(&self, level: i32) -> i32 {
        let lv = level.max(1) as usize;
        match self.levelup_exp.get(lv) {
            Some(&v) if v > 0 => v,
            _ => self.levelup_exp.last().copied().filter(|&v| v > 0).unwrap_or(32000),
        }
    }
}
