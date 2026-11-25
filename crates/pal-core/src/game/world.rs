//! Authoritative mutable game state (`World`).
//!
//! This is the single source of truth for everything that persists in a save or affects game
//! logic. Rendering and UI read a projection of it (see `Game::project_render_state`), and hosts
//! never mutate it directly — they feed inputs to `Game::tick`.

use super::data::{GameData, RoleSfx};
use crate::rng::GameRng;
use crate::script::{LabelId, Program, ScriptPos, NO_LABEL};
use std::collections::{BTreeMap, HashMap, VecDeque};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartyMember {
    pub role_id: u16,
    pub x: i32,
    pub y: i32,
    pub dir: u8,
    pub frame: u8,
}

/// Equipment slot order used everywhere in the core.
pub const EQUIP_SLOTS: [&str; 6] = ["hat", "cloak", "armor", "weapon", "boots", "ornament"];
/// Stat order used everywhere in the core.
pub const STAT_KEYS: [&str; 5] = ["attack", "magic_power", "defense", "dexterity", "flee"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Role {
    pub role_id: u16,
    pub name: String,
    pub name_hant: String,
    pub avatar_rgm: u16,
    pub battle_sprite_f: u16,
    pub walk_sprite_mgo: u16,
    pub level: i32,
    pub exp: i32,
    pub max_hp: i32,
    pub max_mp: i32,
    pub hp: i32,
    pub mp: i32,
    /// `[hat, cloak, armor, weapon, boots, ornament]`
    pub equipment: [u16; 6],
    /// Effective `[attack, magic_power, defense, dexterity, flee]` = `(base_stats + equip_stat_bonuses).clamp(1, u16::MAX)`.
    pub stats: [i32; 5],
    /// Base `[attack, magic_power, defense, dexterity, flee]` excluding equipment bonuses (#253).
    pub base_stats: [i32; 5],
    /// Cached sum of equipped item stat bonuses `[atk, mag, def, dex, flee]` (#253).
    pub equip_stat_bonuses: [i32; 5],
    pub magics: Vec<u16>,
    /// Effective co-op magic (`equip_coop_override.unwrap_or(base_coop_magic)`).
    pub coop_magic: u16,
    /// Base/script-assigned co-op magic preserved across equipment recomputation (#264).
    pub base_coop_magic: u16,
    /// Optional co-op magic override granted by an equipped item (e.g. Five Spirit Pearls).
    pub equip_coop_override: Option<u16>,
    pub sfx: RoleSfx,
    pub statuses: BTreeMap<u16, i32>,
    pub poisons: Vec<u16>,
    /// `role_set_attr_equip` effects: `(slot, attribute) -> value`.
    pub equip_effects: BTreeMap<(i32, i32), i32>,
}

impl Role {
    fn clamp16(v: i32) -> i32 {
        v.clamp(0, u16::MAX as i32)
    }

    /// Recompute derived effective `stats` and `coop_magic` from base + equipment bonuses (#253, #264).
    pub fn recompute_effective_stats(&mut self) {
        for k in 0..5 {
            self.stats[k] = (self.base_stats[k] + self.equip_stat_bonuses[k]).clamp(1, u16::MAX as i32);
        }
        self.coop_magic = self.equip_coop_override.unwrap_or(self.base_coop_magic);
    }

    /// Legacy `GameState::apply_role_attr` (`role_add_attr` / `role_set_attr`).
    pub fn apply_attr(&mut self, attr_idx: i32, val: i32, is_set: bool) {
        let upd = |cur: i32| Self::clamp16(if is_set { val } else { cur + val });
        match attr_idx {
            0 => self.avatar_rgm = upd(self.avatar_rgm as i32) as u16,
            1 => self.battle_sprite_f = upd(self.battle_sprite_f as i32) as u16,
            2 => self.walk_sprite_mgo = upd(self.walk_sprite_mgo as i32) as u16,
            6 => self.level = upd(self.level).max(1),
            7 => self.max_hp = upd(self.max_hp).max(1),
            8 => self.max_mp = upd(self.max_mp),
            9 => self.hp = upd(self.hp).min(self.max_hp),
            10 => self.mp = upd(self.mp).min(self.max_mp),
            17..=21 => {
                let i = (attr_idx - 17) as usize;
                self.base_stats[i] = if is_set {
                    Self::clamp16(val)
                } else {
                    (self.base_stats[i] + val).clamp(-32768, u16::MAX as i32)
                };
                self.recompute_effective_stats();
            }
            65 => {
                self.base_coop_magic = upd(self.base_coop_magic as i32) as u16;
                self.recompute_effective_stats();
            }
            _ => {}
        }
    }

    pub fn boost_attr_pct(&mut self, attr_idx: i32, pct: i32) {
        if (17..=21).contains(&attr_idx) {
            let i = (attr_idx - 17) as usize;
            let eff = self.stats[i];
            let delta = ((eff * (100 + pct)) / 100).clamp(1, u16::MAX as i32) - eff;
            self.base_stats[i] = (self.base_stats[i] + delta).clamp(-32768, u16::MAX as i32);
            self.recompute_effective_stats();
        }
    }

    pub fn revive(&mut self, tenths: i32) -> bool {
        if self.hp == 0 {
            let t = tenths.clamp(1, 10);
            self.hp = ((self.max_hp * t) / 10).max(1);
            return true;
        }
        false
    }

    pub fn halve_hp(&mut self) {
        if self.hp > 1 {
            self.hp = (self.hp / 2).max(1);
        }
    }

    pub fn apply_poison(&mut self, poison_id: u16) {
        if poison_id > 0 && !self.poisons.contains(&poison_id) {
            self.poisons.push(poison_id);
        }
    }

    pub fn cure_poison(&mut self, poison_id: u16) {
        if poison_id == 0xFFFF {
            self.poisons.clear();
        } else {
            self.poisons.retain(|&p| p != poison_id);
        }
    }

    pub fn clear_status(&mut self, status_id: u16) {
        if status_id == 0xFFFF {
            self.statuses.clear();
        } else {
            self.statuses.remove(&status_id);
        }
    }

    /// Total elemental resistance (0..=100%) from equipped items (`role_set_attr_equip` attrs 23..=27:
    /// 0=Wind, 1=Thunder, 2=Water, 3=Fire, 4=Earth).
    pub fn elem_resist(&self, elem_idx: usize) -> i32 {
        if elem_idx >= 5 {
            return 0;
        }
        let attr = 23 + elem_idx as i32;
        self.equip_effects
            .iter()
            .filter(|(&(_, a), _)| a == attr)
            .map(|(_, &v)| v)
            .sum::<i32>()
            .clamp(0, 100)
    }

    /// Total poison resistance (0..=100%) from equipped items (`role_set_attr_equip` attr 22, e.g. 五毒珠=100).
    pub fn poison_resist(&self) -> i32 {
        self.equip_effects
            .iter()
            .filter(|(&(_, a), _)| a == 22)
            .map(|(_, &v)| v)
            .sum::<i32>()
            .clamp(0, 100)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scene {
    pub id: u16,
    pub map_id: u16,
    /// Enter script; `idx` is the resume offset (legacy `enterOffset`).
    pub enter: ScriptPos,
    pub teleport: LabelId,
    pub event_start: u16,
    pub event_end: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub id: u16,
    pub vanish_time: i32,
    pub x: i32,
    pub y: i32,
    pub layer: i32,
    /// Trigger script and resume offset (legacy `script_trigger` + `triggerOffset`).
    pub trigger: ScriptPos,
    /// Auto script and resume offset (legacy `script_auto` + `autoOffset`).
    pub auto: ScriptPos,
    pub state: i32,
    pub trigger_mode: i32,
    pub sprite_id: u16,
    pub sprite_frames: i32,
    pub direction: i32,
    pub current_frame: i32,
    /// Frame count discovered from the sprite atlas metadata (`mgo_XXX.json`).
    pub sprite_frames_auto: i32,
    pub interact_anchor: Vec<i32>,
    /// Spawn position remembered by the monster chase AI.
    pub spawn: Option<(i32, i32)>,
}

impl Event {
    pub fn anim_frames(&self, fallback: i32) -> i32 {
        let f = if self.sprite_frames != 0 { self.sprite_frames } else { self.sprite_frames_auto };
        if f != 0 { f.max(1) } else { fallback.max(1) }
    }

    pub fn advance_frame(&mut self, fallback: i32) {
        let total = self.anim_frames(fallback);
        self.current_frame = (self.current_frame + 1).rem_euclid(total);
    }
}

/// Key for `replace_and_yield` / `wait_ticks` counters (legacy `state.scriptCounters`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CounterKey {
    pub wait: bool,
    pub event_id: u16,
    pub pos: ScriptPos,
}

/// Presentation state driven by scripts (fades, camera, full-screen pictures).
#[derive(Clone, Debug, PartialEq)]
pub struct ViewState {
    pub fade_alpha: f32,
    pub fade_color: [u8; 3],
    pub active_fbp: i32,
    pub cam_override: Option<(i32, i32)>,
    pub shake_ticks: i32,
    pub shake_level: i32,
    pub rng_id: i32,
    pub screen_backed_up: bool,
    pub backed_fbp: i32,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            fade_alpha: 0.0,
            fade_color: [0, 0, 0],
            active_fbp: -1,
            cam_override: None,
            shake_ticks: 0,
            shake_level: 0,
            rng_id: 0,
            screen_backed_up: false,
            backed_fbp: -1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DialogueStyle {
    /// `0 = center`, `1 = left (upper)`, `2 = right (lower)`.
    pub side: u8,
    pub portrait_id: i32,
    pub font_color: i32,
    /// Last explicit speaker `(zh_cn, zh_tw)` in the current dialogue position block (#123).
    pub last_speaker: (String, String),
}

#[derive(Clone, Debug)]
pub struct World {
    pub scene_id: u16,
    pub music_track: i32,
    pub battle_music_track: i32,
    pub battlefield_id: i32,
    pub cash: u32,
    pub party: Vec<PartyMember>,
    /// Party vertical Z-sort layer (`gpGlobals->wLayer`, set by 3-arg `party_walk_step`, #186).
    pub party_layer: i32,
    pub trail: VecDeque<(i32, i32, u8)>,
    pub follower_sprites: Vec<u16>,
    pub roles: Vec<Role>,
    pub inventory: BTreeMap<u16, i32>,
    pub scenes: BTreeMap<u16, Scene>,
    /// Indexed by `id - 1`.
    pub events: Vec<Event>,
    pub script_counters: HashMap<CounterKey, i32>,
    pub object_scripts: BTreeMap<i32, LabelId>,
    /// Persistent `script_success` resume position for stateful magics (e.g. 酒神 #370 9-use limit).
    pub magic_script_pos: HashMap<u16, ScriptPos>,
    pub gourd_essences: Vec<u16>,
    pub night_palette: bool,
    pub palette_id: i32,
    pub screen_wave: i32,
    pub chase_alert_range: i32,
    pub chase_alert_cycles: i32,
    pub chase_disabled_ticks: i32,
    pub rng: GameRng,
    pub hant: bool,
    pub lang: crate::Language,
    pub view: ViewState,
    pub ending_triggered: bool,
    pub game_quit: bool,
    pub last_save_load_requested: bool,
}

impl World {
    pub fn empty() -> Self {
        Self {
            scene_id: 1,
            music_track: 0,
            battle_music_track: 4,
            battlefield_id: 0,
            cash: 0,
            party: vec![PartyMember { role_id: 0, x: 1312, y: 288, dir: 0, frame: 0 }],
            party_layer: 0,
            trail: VecDeque::new(),
            follower_sprites: Vec::new(),
            roles: Vec::new(),
            inventory: BTreeMap::new(),
            scenes: BTreeMap::new(),
            events: Vec::new(),
            script_counters: HashMap::new(),
            object_scripts: BTreeMap::new(),
            magic_script_pos: HashMap::new(),
            gourd_essences: Vec::new(),
            night_palette: false,
            palette_id: 0,
            screen_wave: 0,
            chase_alert_range: 3,
            chase_alert_cycles: 0,
            chase_disabled_ticks: 0,
            rng: GameRng::default(),
            hant: false,
            lang: crate::Language::ZhHans,
            view: ViewState::default(),
            ending_triggered: false,
            game_quit: false,
            last_save_load_requested: false,
        }
    }

    /// Fresh new-game state from the static tables (legacy `GameState.resetNewGame`).
    pub fn new_game(data: &GameData, prog: &mut Program, seed: u32) -> Self {
        let mut w = Self::empty();
        w.rng = GameRng::new(seed);
        w.music_track = 31;
        w.battle_music_track = 37;
        w.party = vec![PartyMember { role_id: 0, x: 41 * 32, y: 18 * 16, dir: 0, frame: 0 }];
        w.roles = data
            .roles
            .iter()
            .map(|r| Role {
                role_id: r.role_id,
                name: r.name.clone(),
                name_hant: r.name_hant.clone(),
                avatar_rgm: r.avatar_rgm,
                battle_sprite_f: r.battle_sprite_f,
                walk_sprite_mgo: r.walk_sprite_mgo,
                level: r.level,
                exp: 0,
                max_hp: r.max_hp,
                max_mp: r.max_mp,
                hp: r.hp,
                mp: r.mp,
                equipment: r.equipment.to_array(),
                stats: r.stats.to_array(),
                base_stats: r.stats.to_array(),
                equip_stat_bonuses: [0; 5],
                magics: r.initial_magics.clone(),
                coop_magic: r.coop_magic,
                base_coop_magic: r.coop_magic,
                equip_coop_override: None,
                sfx: r.sfx.clone(),
                ..Default::default()
            })
            .collect();
        for sc in data.scenes.values() {
            w.scenes.insert(
                sc.id,
                Scene {
                    id: sc.id,
                    map_id: sc.map_id,
                    enter: ScriptPos::new(prog.intern(&sc.script_enter), 0),
                    teleport: prog.intern(&sc.script_teleport),
                    event_start: sc.event_start,
                    event_end: sc.event_end,
                },
            );
        }
        let max_id = data.events.keys().next_back().copied().unwrap_or(0) as usize;
        w.events = (1..=max_id as u16)
            .map(|id| match data.events.get(&id) {
                Some(e) => Event {
                    id,
                    vanish_time: e.vanish_time,
                    x: e.x,
                    y: e.y,
                    layer: e.layer,
                    trigger: ScriptPos::new(prog.intern(&e.script_trigger), 0),
                    auto: ScriptPos::new(prog.intern(&e.script_auto), 0),
                    state: e.state,
                    trigger_mode: e.trigger_mode,
                    sprite_id: e.sprite_id,
                    sprite_frames: e.sprite_frames,
                    direction: e.direction,
                    current_frame: e.current_frame,
                    sprite_frames_auto: 0,
                    interact_anchor: e.interact_anchor.clone(),
                    spawn: None,
                },
                None => Event { id, ..Default::default() },
            })
            .collect();
        w
    }

    pub fn set_lang(&mut self, lang: crate::Language) {
        self.lang = lang;
        self.hant = lang.is_hant();
    }

    pub fn t<'a>(&self, hans: &'a str, hant: &'a str) -> &'a str {
        if self.hant {
            if hant.is_empty() { hans } else { hant }
        } else if hans.is_empty() {
            hant
        } else {
            hans
        }
    }

    pub fn t3<'a>(&self, hans: &'a str, hant: &'a str, en: &'a str) -> &'a str {
        match self.lang {
            crate::Language::En => {
                if en.is_empty() {
                    self.t(hans, hant)
                } else {
                    en
                }
            }
            crate::Language::ZhHant => {
                if hant.is_empty() { hans } else { hant }
            }
            crate::Language::ZhHans => {
                if hans.is_empty() { hant } else { hans }
            }
        }
    }

    pub fn leader(&self) -> &PartyMember {
        &self.party[0]
    }

    pub fn leader_mut(&mut self) -> &mut PartyMember {
        &mut self.party[0]
    }

    pub fn leader_role(&self) -> u16 {
        self.party.first().map(|p| p.role_id).unwrap_or(0)
    }

    pub fn event(&self, id: u16) -> Option<&Event> {
        if id == 0 { None } else { self.events.get(id as usize - 1) }
    }

    pub fn event_mut(&mut self, id: u16) -> Option<&mut Event> {
        if id == 0 { None } else { self.events.get_mut(id as usize - 1) }
    }

    pub fn current_scene(&self) -> Option<&Scene> {
        self.scenes.get(&self.scene_id)
    }

    /// Inclusive event id range of `scene_id` (empty when unknown).
    pub fn scene_event_range(&self, scene_id: u16) -> std::ops::RangeInclusive<u16> {
        match self.scenes.get(&scene_id) {
            Some(s) if s.event_start > 0 && s.event_end >= s.event_start => s.event_start..=s.event_end,
            #[allow(clippy::reversed_empty_ranges)]
            _ => 1..=0,
        }
    }

    pub fn current_event_ids(&self) -> Vec<u16> {
        self.scene_event_range(self.scene_id)
            .filter(|&id| self.event(id).is_some())
            .collect()
    }

    pub fn is_event_in_current_scene(&self, id: u16) -> bool {
        self.scene_event_range(self.scene_id).contains(&id)
    }

    // ---- Party movement -------------------------------------------------------------------

    pub fn set_party_tile_pos(&mut self, tx: i32, ty: i32, h: i32) {
        self.set_party_world_pos(tx * 32 + h * 16, ty * 16 + h * 8);
    }

    pub fn set_party_world_pos(&mut self, px: i32, py: i32) {
        let lead_dir = self.party.first().map(|m| m.dir).unwrap_or(0);
        self.trail.clear();
        self.trail.push_front((px, py, lead_dir));
        // Authentic SDLPAL `0x0013` (`PAL_SetPartyPos`): place all members on the guaranteed-walkable
        // leader spawn tile so followers never clip into adjacent walls/cliffs on scene entry (#170).
        for m in &mut self.party {
            m.x = px;
            m.y = py;
            m.frame = 0;
        }
    }

    /// Move the leader one step and let followers trail behind (legacy `stepPartyTo`).
    pub fn step_party_to(&mut self, nx: i32, ny: i32, dir: u8) {
        let lead = self.party[0].clone();
        self.trail.push_front((lead.x, lead.y, lead.dir));
        if self.trail.len() > 24 {
            self.trail.pop_back();
        }
        let l = &mut self.party[0];
        l.x = nx;
        l.y = ny;
        l.dir = dir & 3;
        l.frame = (l.frame + 1) % 3;
        let frame = l.frame;
        for i in 1..self.party.len() {
            let target_k = (i * 2 - 1).min(self.trail.len().saturating_sub(1));
            // If a follower loaded from a DOS `.rpg` save starts further back along `rgTrail`
            // (e.g. `45.rpg` doorway), advance smoothly 1 trail slot per step instead of
            // teleporting across intervening walls (#170).
            let cur_k = self
                .trail
                .iter()
                .enumerate()
                .skip(target_k)
                .find(|(_, &(tx, ty, _))| tx == self.party[i].x && ty == self.party[i].y)
                .map(|(idx, _)| idx);
            let k = match cur_k {
                Some(idx) if idx > target_k => idx - 1,
                _ => target_k,
            };
            if let Some(&(x, y, d)) = self.trail.get(k) {
                let m = &mut self.party[i];
                m.x = x;
                m.y = y;
                m.dir = d;
                m.frame = frame;
            }
        }
    }

    // ---- Cash & inventory -----------------------------------------------------------------

    pub fn add_cash(&mut self, delta: i32) {
        self.cash = if delta >= 0 {
            self.cash.saturating_add(delta as u32)
        } else {
            self.cash.saturating_sub(delta.unsigned_abs())
        };
    }

    pub fn halve_cash(&mut self) {
        self.cash /= 2;
    }

    pub fn add_item(&mut self, item_id: u16, count: i32) {
        let prev = self.inventory.get(&item_id).copied().unwrap_or(0);
        let next = (prev + count).max(0);
        if next == 0 {
            self.inventory.remove(&item_id);
        } else {
            self.inventory.insert(item_id, next);
        }
    }

    /// Inventory count plus copies equipped by party members (legacy `getItemCount`).
    pub fn item_count(&self, item_id: u16) -> i32 {
        let mut n = self.inventory.get(&item_id).copied().unwrap_or(0);
        for p in &self.party {
            if let Some(r) = self.roles.get(p.role_id as usize) {
                n += r.equipment.iter().filter(|&&e| e == item_id).count() as i32;
            }
        }
        n
    }

    pub fn remove_item(&mut self, item_id: u16, count: i32) -> bool {
        if count <= 0 {
            return false;
        }
        let have = self.inventory.get(&item_id).copied().unwrap_or(0);
        if have < count {
            return false;
        }
        if have - count <= 0 {
            self.inventory.remove(&item_id);
        } else {
            self.inventory.insert(item_id, have - count);
        }
        true
    }

    /// Remove `count` copies of `item_id` from inventory first, then unequip remaining copies from
    /// party members (exact SDLPAL `0x0020` `item_remove` semantics).
    pub fn remove_item_including_equipped(&mut self, item_id: u16, count: i32) -> Vec<(usize, u16)> {
        if count <= 0 || self.item_count(item_id) < count {
            return Vec::new();
        }
        let mut need = count;
        let inv = self.inventory.get(&item_id).copied().unwrap_or(0);
        let take_inv = inv.min(need);
        if take_inv > 0 {
            if inv - take_inv <= 0 {
                self.inventory.remove(&item_id);
            } else {
                self.inventory.insert(item_id, inv - take_inv);
            }
            need -= take_inv;
        }
        let mut unequipped = Vec::new();
        if need > 0 {
            let pids = self.party_role_ids();
            for rid in pids {
                if need <= 0 {
                    break;
                }
                if let Some(r) = self.roles.get_mut(rid) {
                    for slot in 0..r.equipment.len() {
                        if need <= 0 {
                            break;
                        }
                        if r.equipment[slot] == item_id {
                            r.equipment[slot] = 0;
                            unequipped.push((rid, item_id));
                            need -= 1;
                        }
                    }
                }
            }
        }
        unequipped
    }

    // ---- Roles ----------------------------------------------------------------------------

    pub fn party_role_ids(&self) -> Vec<usize> {
        self.party.iter().map(|m| m.role_id as usize).collect()
    }

    /// Verbatim SDLPAL `PAL_PlayerLevelUp` (`global.c:2347-2409`): randomized stat growth
    /// (capped at 999) plus `levelup_magic` learning. Returns newly learned magic names.
    pub fn apply_level_up(&mut self, data: &GameData, role_idx: usize, gain: i32, learn: bool) -> Vec<String> {
        if role_idx >= self.roles.len() {
            return Vec::new();
        }
        let g = gain.max(1);
        let prev = self.roles[role_idx].level;
        let new_level = (prev + g).min(99);
        let steps = (new_level - prev).max(0);
        let mut hp_add = 0;
        let mut mp_add = 0;
        let mut stat_add = [0i32; 5];
        for _ in 0..steps {
            hp_add += 10 + self.rng.random_long(0, 7);
            mp_add += 8 + self.rng.random_long(0, 5);
            stat_add[0] += 4 + self.rng.random_long(0, 1); // attack
            stat_add[1] += 4 + self.rng.random_long(0, 1); // magic
            stat_add[2] += 2 + self.rng.random_long(0, 1); // defense
            stat_add[3] += 2 + self.rng.random_long(0, 1); // dexterity
            stat_add[4] += 2; // flee_rate
        }
        let r = &mut self.roles[role_idx];
        r.level = new_level;
        r.max_hp = (r.max_hp + hp_add).min(999);
        r.max_mp = (r.max_mp + mp_add).min(999);
        r.hp = r.max_hp;
        r.mp = r.max_mp;
        for (b, inc) in r.base_stats.iter_mut().zip(stat_add) {
            *b = (*b + inc).min(999);
        }
        r.recompute_effective_stats();
        let mut learned = Vec::new();
        if learn {
            if let Some(key) = super::data::ROLE_KEYS.get(role_idx) {
                for row in &data.levelup_magic {
                    if let Some(e) = row.get(*key) {
                        if e.level > prev && e.level <= r.level && e.magic_id > 0 && !r.magics.contains(&e.magic_id) {
                            r.magics.push(e.magic_id);
                            learned.push(e.magic_name.clone());
                        }
                    }
                }
            }
        }
        learned
    }

    pub fn object_script(&self, obj_id: i32) -> LabelId {
        self.object_scripts.get(&obj_id).copied().unwrap_or(NO_LABEL)
    }
}
