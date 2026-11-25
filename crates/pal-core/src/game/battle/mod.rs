//! Turn-based battle flow (port of the legacy `site/js/battle/battle-engine.js`).
//!
//! A battle runs inside the foreground script thread (`Wait::Battle`). [`Game::battle_tick`]
//! advances it one logic tick at a time; it pauses for player commands (`HostCmd::BattlePrompt`
//! answered with [`Game::battle_command`]), for battle scripts (run as nested foreground frames)
//! and for the victory screen (`HostCmd::BattleVictory` answered with `HostReply::ModalDone`).
//! Damage formulas come from `pal_core::battle` (shared with the legacy runtime).

mod actions;
mod flow;
mod formulas;

use super::{ms_to_ticks, BattleOutcome, Game, HostCmd};
use crate::script::{LabelId, ScriptPos, NO_LABEL};
use formulas::{
    calc_hero_attack_hits, sdlpal_calc_magic_damage, sdlpal_coop_magic_damage,
    sdlpal_enemy_action_dexterity, sdlpal_enemy_dexterity, sdlpal_enemy_flee_def,
    sdlpal_enemy_magic_hit, sdlpal_enemy_physical_hit, sdlpal_flee_check,
    sdlpal_hero_action_dexterity, sdlpal_hero_attack_mate, sdlpal_hero_physical_hit,
    sdlpal_is_player_dying, sdlpal_sort_action_queue, ActionActor, ActionSlot, HeroSpeedKind,
};
use std::collections::BTreeMap;

// SDLPAL 9 battle status indices (`palcommon.h` STATUS enum)
const STATUS_CONFUSED: u16 = 0;
const STATUS_PARALYZED: u16 = 1;
const STATUS_SLEEP: u16 = 2;
const STATUS_SILENCE: u16 = 3;
const STATUS_PUPPET: u16 = 4;
const STATUS_BRAVERY: u16 = 5;
const STATUS_PROTECT: u16 = 6;
#[allow(dead_code)]
const STATUS_HASTE: u16 = 7;
const STATUS_DUAL_ATTACK: u16 = 8;

const ENEMY_SLOTS: [(i32, i32); 5] = [(112, 92), (74, 116), (148, 74), (46, 94), (110, 62)];
const HERO_SLOTS: [(i32, i32); 3] = [(236, 146), (268, 124), (204, 168)];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EnemyPose {
    #[default]
    Idle,
    Attack,
    Magic,
    Hurt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HeroPose {
    #[default]
    Idle,
    Defend,
    Hurt,
    CastPrep,
    CastRelease,
    AttackPrep,
    AttackLunge,
    AttackSlash,
}

#[derive(Clone, Debug, Default)]
pub struct BattleEnemy {
    pub enemy_id: u16,
    pub obj_id: i32,
    pub sprite_id: u16,
    pub idle_frames: i32,
    pub magic_frames: i32,
    pub attack_frames: i32,
    pub idle_anim_speed: i32,
    pub name: String,
    pub name_hant: String,
    pub hp: i32,
    pub max_hp: i32,
    pub level: i32,
    pub raw_dexterity: i32,
    pub attack: i32,
    pub magic_power: i32,
    pub defense: i32,
    pub resist_phys: i32,
    pub resist_elem: [i32; 5],
    pub resist_poison: i32,
    pub magic_id: u16,
    pub magic_rate: i32,
    pub dual_move: i32,
    pub attack_equiv_item: u16,
    pub attack_equiv_rate: i32,
    pub exp: i32,
    pub cash: i32,
    pub is_boss: bool,
    pub steal_item: u16,
    pub steal_count: i32,
    pub sfx_attack: i32,
    pub sfx_magic: i32,
    pub sfx_death: i32,
    pub script_pre_turn: ScriptPos,
    pub script_in_turn: ScriptPos,
    pub script_post_battle: LabelId,
    pub poisons: Vec<u16>,
    pub statuses: BTreeMap<u16, i32>,
    pub selected_magic: u16,
    pub x: i32,
    pub y: i32,
    pub lunge: (i32, i32),
    pub flash: i32,
    pub pose: EnemyPose,
    pub pose_ticks: u16,
    pub death_fade: u8,
}

#[derive(Clone, Debug, Default)]
pub struct BattleHero {
    pub role: usize,
    pub defending: bool,
    pub x: i32,
    pub y: i32,
    pub lunge: (i32, i32),
    pub flash: i32,
    pub pose: HeroPose,
    pub pose_ticks: u16,
    pub atb_pct: u8,
}

/// Player battle command (from the battle HUD).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BattleCmd {
    Attack { target: usize },
    Magic { magic: u16, target: usize },
    Coop { target: usize },
    Item { item: u16 },
    ItemTarget { item: u16, target: usize },
    Defend,
    Flee,
    Auto,
}

/// Localized status name for SDLPAL battle status IDs (0..=8) and poison sentinel (99).
pub fn status_display_name(status_id: u16, lang: crate::Language) -> &'static str {
    match (status_id, lang) {
        (0, crate::Language::En) => "confused",
        (0, crate::Language::ZhHant) => "混亂",
        (0, _) => "混乱",
        (1, crate::Language::En) => "paralyzed",
        (1, crate::Language::ZhHant) => "定身",
        (1, _) => "定身",
        (2, crate::Language::En) => "sleep",
        (2, crate::Language::ZhHant) => "昏睡",
        (2, _) => "昏睡",
        (3, crate::Language::En) => "silence",
        (3, crate::Language::ZhHant) => "封魔",
        (3, _) => "封魔",
        (4, crate::Language::En) => "puppet",
        (4, crate::Language::ZhHant) => "傀儡",
        (4, _) => "傀儡",
        (5, crate::Language::En) => "bravery",
        (5, crate::Language::ZhHant) => "天罡",
        (5, _) => "天罡",
        (6, crate::Language::En) => "protect",
        (6, crate::Language::ZhHant) => "金剛",
        (6, _) => "金刚",
        (7, crate::Language::En) => "haste",
        (7, crate::Language::ZhHant) => "仙風",
        (7, _) => "仙风",
        (8, crate::Language::En) => "dual_attack",
        (8, crate::Language::ZhHant) => "醉仙",
        (8, _) => "醉仙",
        (99, crate::Language::En) => "poison",
        (99, _) => "中毒",
        _ => "status",
    }
}

/// Structured battle feedback entry emitted alongside floating damage/heal numbers and spell banners (#219).
#[derive(Clone, Debug)]
pub struct BattleFeedEntry {
    pub tick: u32,
    pub actor: String,
    pub action: String,
    pub name: String,
    pub target: String,
    pub value: Option<i32>,
    pub kind: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct FloatText {
    pub x: i32,
    pub y: i32,
    pub text: String,
    pub color: [u8; 3],
    pub age: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct FireAnim {
    pub fire_id: u16,
    pub frame: u16,
    pub x: i32,
    pub y: i32,
    pub target_all: bool,
    pub target_enemy: bool,
    pub is_summon: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    RoundStart,
    PreTurn(usize),
    Command(usize),
    /// Unified dexterity-sorted `PAL_CLASSIC` interleaved hero/enemy action queue (#255).
    ActQueue(usize, u8),
    PostBattle(usize),
    Rewards,
    AwaitVictoryAck,
    Finish,
}

#[derive(Clone, Debug)]
pub struct BattleCtx {
    pub enemies: Vec<BattleEnemy>,
    pub heroes: Vec<BattleHero>,
    pub current_target_enemy: usize,
    pub is_enemy_turn: bool,
    pub active: bool,
    pub can_flee: bool,
    pub outcome: Option<BattleOutcome>,
    pub auto_next: bool,
    pub hidden_roles: Vec<i32>,
    /// Remaining rounds the party is hidden from enemy attacks (SDLPAL `battle_hide_role` / 隐蛊, #215).
    pub party_hidden_turns: i32,
    pub magic_cast_anim: i32,
    pub base_magic_dmg_override: Option<i32>,
    pub fbp_id: i32,
    pub active_hero: usize,
    pub float_texts: Vec<FloatText>,
    /// Bounded structured battle action/damage/heal/miss/steal feed for Text Mode (#219).
    pub feed: Vec<BattleFeedEntry>,
    pub feed_total: u64,
    pub(crate) cur_actor: String,
    pub(crate) cur_action: String,
    pub(crate) cur_skill: String,
    pub fire: Option<FireAnim>,
    pub anim_tick: u32,
    /// Hero index currently being asked for a command (`None` when not prompting).
    pub prompting: Option<usize>,
    /// Active spell/skill name banner shown at top-center (authentic SDLPAL battle scroll banner).
    pub spell_banner: Option<String>,
    pub spell_banner_ticks: u16,
    /// Subtle backdrop dimming during spell casting.
    pub dim_bg: bool,
    /// Transient battle screen-shake ticks & intensity (purely visual).
    pub shake_ticks: u16,
    pub shake_level: u8,
    enemy_fire_ticks: u16,
    field_elem: [i32; 5],
    phase: Phase,
    actions: Vec<(usize, BattleCmd)>,
    action_queue: Vec<ActionSlot>,
    this_turn_coop: bool,
    pending_cmd: Option<BattleCmd>,
    auto_repeat: bool,
    sleep: u32,
    victory_ack: bool,
    enemy_target: usize,
    /// Script requested by the battle flow; the foreground runner executes it as a nested frame.
    pub(crate) script_request: Option<(ScriptPos, super::vm::Caller)>,
    pub(crate) script_running: bool,
}

impl Default for BattleCtx {
    fn default() -> Self {
        Self {
            enemies: Vec::new(),
            heroes: Vec::new(),
            current_target_enemy: 0,
            is_enemy_turn: false,
            active: false,
            can_flee: true,
            outcome: None,
            auto_next: false,
            hidden_roles: Vec::new(),
            party_hidden_turns: 0,
            magic_cast_anim: 0,
            base_magic_dmg_override: None,
            fbp_id: 0,
            active_hero: 0,
            float_texts: Vec::new(),
            feed: Vec::new(),
            feed_total: 0,
            cur_actor: String::new(),
            cur_action: String::new(),
            cur_skill: String::new(),
            fire: None,
            anim_tick: 0,
            prompting: None,
            spell_banner: None,
            spell_banner_ticks: 0,
            dim_bg: false,
            shake_ticks: 0,
            shake_level: 0,
            enemy_fire_ticks: 0,
            field_elem: [0; 5],
            phase: Phase::RoundStart,
            actions: Vec::new(),
            action_queue: Vec::new(),
            this_turn_coop: false,
            pending_cmd: None,
            auto_repeat: false,
            sleep: 0,
            victory_ack: false,
            enemy_target: 0,
            script_request: None,
            script_running: false,
        }
    }
}

impl BattleCtx {
    fn first_alive_enemy(&self) -> Option<usize> {
        self.enemies.iter().position(|e| e.hp > 0)
    }

    fn all_enemies_dead(&self) -> bool {
        self.enemies.iter().all(|e| e.hp <= 0)
    }

    pub(crate) fn push_feed(
        &mut self,
        actor: impl Into<String>,
        action: impl Into<String>,
        name: impl Into<String>,
        target: impl Into<String>,
        value: Option<i32>,
        kind: impl Into<String>,
        text: impl Into<String>,
    ) {
        self.feed_total += 1;
        self.feed.push(BattleFeedEntry {
            tick: self.anim_tick,
            actor: actor.into(),
            action: action.into(),
            name: name.into(),
            target: target.into(),
            value,
            kind: kind.into(),
            text: text.into(),
        });
        if self.feed.len() > 128 {
            let drop_n = self.feed.len() - 128;
            self.feed.drain(0..drop_n);
        }
    }

    pub(crate) fn set_banner(&mut self, actor: impl Into<String>, action: &str, name: impl Into<String>, ticks: u16) {
        let actor_s = actor.into();
        let name_s = name.into();
        self.cur_actor = actor_s.clone();
        self.cur_action = action.to_string();
        self.cur_skill = name_s.clone();
        if !name_s.is_empty() {
            self.spell_banner = Some(name_s.clone());
            self.spell_banner_ticks = ticks;
            self.push_feed(actor_s, action, name_s.clone(), "", None, "cast", name_s);
        }
    }

    pub(crate) fn float(&mut self, x: i32, y: i32, text: String, color: [u8; 3]) {
        // Infer structured kind/value/target from the floating text for Battle Feed parity (#219).
        let (kind, value) = if let Some(rest) = text.strip_prefix('-') {
            if let Ok(v) = rest.trim().parse::<i32>() {
                ("dmg", Some(v))
            } else {
                ("info", None)
            }
        } else if let Some(rest) = text.strip_prefix('+') {
            if let Ok(v) = rest.trim().parse::<i32>() {
                ("heal", Some(v))
            } else {
                ("info", None)
            }
        } else if text.contains("MISS")
            || text.contains("闪避")
            || text.contains("閃避")
            || text.contains("未发现")
            || text.contains("未發現")
            || text == "Hidden"
        {
            ("miss", None)
        } else if text.contains("偷") || text.contains("Steal") || text.contains("Stole") {
            ("steal", None)
        } else if text == "防御" || text == "防禦" || text == "Defend" {
            ("defend", None)
        } else {
            ("info", None)
        };
        let target = self
            .enemies
            .iter()
            .enumerate()
            .map(|(i, e)| ((e.x - x).abs() + (e.y - 30 - y).abs(), format!("enemy#{i}:{}", e.name)))
            .chain(self.heroes.iter().enumerate().map(|(i, h)| {
                ((h.x - x).abs() + (h.y - 30 - y).abs(), format!("hero#{i}"))
            }))
            .min_by_key(|(d, _)| *d)
            .filter(|(d, _)| *d <= 48)
            .map(|(_, label)| label)
            .unwrap_or_default();
        let actor = self.cur_actor.clone();
        let action = if self.cur_action.is_empty() {
            kind.to_string()
        } else {
            self.cur_action.clone()
        };
        let skill = self.cur_skill.clone();
        self.push_feed(actor, action, skill, target, value, kind, text.clone());
        self.float_texts.push(FloatText { x, y, text, color, age: 0 });
    }

    /// Whether auto-battle (围攻) is currently enabled.
    pub fn is_auto_repeat(&self) -> bool {
        self.auto_repeat
    }
}

const RED: [u8; 3] = [0xff, 0x6e, 0x6e];
const HERO_HIT: [u8; 3] = [0xff, 0x52, 0x52];
const GOLD: [u8; 3] = [0xff, 0xd1, 0x66];
const GREEN: [u8; 3] = [0x69, 0xf0, 0xae];
const CYAN: [u8; 3] = [0x8b, 0xe9, 0xfd];
const BLUE: [u8; 3] = [0x70, 0xd6, 0xff];

impl Game {
    pub(crate) fn battle_enemy_template(&mut self, enemy_id: u16, slot: usize) -> BattleEnemy {
        self.battle_enemy_template_with_obj(enemy_id, slot, 0)
    }

    pub(crate) fn battle_enemy_template_with_obj(
        &mut self,
        enemy_id: u16,
        slot: usize,
        obj_id: i32,
    ) -> BattleEnemy {
        let (x, y) = ENEMY_SLOTS[slot % ENEMY_SLOTS.len()];
        let d = self.data.enemies.get(&enemy_id).cloned();
        let mut e = BattleEnemy {
            enemy_id,
            obj_id,
            sprite_id: enemy_id,
            x,
            y,
            resist_elem: [5; 5],
            death_fade: 255,
            ..Default::default()
        };
        match d {
            Some(d) => {
                let hp = if d.hp != 0 { d.hp } else { 50 };
                e.sprite_id = d.abc_sprite_id;
                e.idle_frames = d.idle_frames;
                e.magic_frames = d.magic_frames;
                e.attack_frames = d.attack_frames;
                e.idle_anim_speed = d.idle_anim_speed;
                e.name = d.name.clone();
                e.name_hant = d.name_hant.clone();
                e.max_hp = hp.max(12);
                e.hp = e.max_hp;
                e.level = d.level;
                e.raw_dexterity = d.dexterity;
                e.attack = d.effective(d.attack, 1);
                e.magic_power = d.effective(d.magic_power, 0);
                e.defense = d.effective_defense();
                e.resist_phys = d.resist_phys;
                e.resist_poison = d.resist_poison;
                e.magic_id = d.magic_id;
                e.magic_rate = d.magic_rate;
                e.dual_move = d.dual_move;
                e.attack_equiv_item = d.attack_equiv_item;
                e.attack_equiv_rate = d.attack_equiv_rate;
                for (dst, &v) in e.resist_elem.iter_mut().zip(&d.resist_elem) {
                    *dst = v;
                }
                e.exp = d.exp;
                e.cash = d.cash;
                e.is_boss = d.is_boss();
                e.steal_item = d.steal_item;
                e.steal_count = d.steal_count;
                e.sfx_attack = d.sfx_attack;
                e.sfx_magic = d.sfx_magic;
                e.sfx_death = d.sfx_death;
                // Respect runtime `object_set_script` overrides for this enemy object (#185).
                let pre_lbl = if obj_id > 0 {
                    if let Some(&ov) = self.world.object_scripts.get(&obj_id) {
                        ov
                    } else {
                        self.prog.intern(&d.script_pre_turn)
                    }
                } else {
                    self.prog.intern(&d.script_pre_turn)
                };
                e.script_pre_turn = if pre_lbl != NO_LABEL { ScriptPos::new(pre_lbl, 0) } else { ScriptPos::NONE };
                let in_lbl = self.prog.intern(&d.script_in_turn);
                e.script_in_turn = if in_lbl != NO_LABEL { ScriptPos::new(in_lbl, 0) } else { ScriptPos::NONE };
                e.script_post_battle = self.prog.intern(&d.script_post_battle);
            }
            None => {
                e.name = "妖怪".into();
                e.max_hp = 60;
                e.hp = 60;
                e.attack = 24;
                e.defense = 14;
                e.exp = 18;
                e.cash = 35;
                e.resist_phys = 2;
            }
        }
        e
    }

    /// Set up a battle against enemy team `team` (legacy `BattleEngine.runBattle` prologue).
    pub(crate) fn battle_begin(&mut self, team: i32, can_flee: bool) {
        // Hidden pre-battle autosave slot (never exposed in the player's Save/Load menu).
        // Preserve the clean pre-event snapshot if already captured before a scripted boss event.
        if self.pre_battle_save.is_none()
            || matches!(
                self.fg.as_ref().map(|t| t.kind),
                Some(super::vm::FgKind::Battle(_))
            )
        {
            self.pre_battle_save = Some(self.save_json("pre_battle_autosave", "auto"));
        }
        self.record_auto_checkpoint("battle", false);
        let members: Vec<(i32, u16)> = self
            .data
            .teams
            .get(&(team.max(0) as u16))
            .and_then(|t| t.get("members"))
            .and_then(|m| m.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        let eid = m.get("enemy_id").and_then(|v| v.as_u64())? as u16;
                        let oid = m.get("obj_id").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                        Some((oid, eid))
                    })
                    .collect()
            })
            .filter(|v: &Vec<(i32, u16)>| !v.is_empty())
            .unwrap_or_else(|| vec![(0, 1)]);
        let bf = self.data.battlefields.get(&(self.world.battlefield_id.max(0) as u16)).cloned();
        let mut ctx = BattleCtx {
            fbp_id: bf.as_ref().and_then(|b| b.get("fbp_id")).and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            active: true,
            can_flee,
            ..Default::default()
        };
        if let Some(fe) = bf.as_ref().and_then(|b| b.get("elem_effect")).and_then(|v| v.as_array()) {
            for (dst, v) in ctx.field_elem.iter_mut().zip(fe) {
                *dst = v.as_i64().unwrap_or(0) as i32;
            }
        }
        for (slot, (oid, id)) in members.into_iter().take(5).enumerate() {
            let e = self.battle_enemy_template_with_obj(id, slot, oid);
            ctx.enemies.push(e);
        }
        for (idx, m) in self.world.party.iter().take(3).enumerate() {
            let (x, y) = HERO_SLOTS[idx % HERO_SLOTS.len()];
            ctx.heroes.push(BattleHero { role: m.role_id as usize, x, y, ..Default::default() });
        }
        let track = if self.world.battle_music_track != 0 { self.world.battle_music_track } else { 4 };
        self.emit(HostCmd::PlayMusic { track, looped: true });
        self.emit(HostCmd::BattleStarted);
        self.battle = Some(ctx);
    }

    /// Feed the player's command for the hero currently being prompted.
    pub fn battle_command(&mut self, cmd: BattleCmd) -> bool {
        match &mut self.battle {
            Some(b) if b.prompting.is_some() => {
                b.pending_cmd = Some(cmd);
                b.prompting = None;
                self.recorder
                    .push(self.tick_count, super::ReplayEvent::from_battle_cmd(cmd));
                true
            }
            _ => false,
        }
    }

    pub(crate) fn battle_victory_ack(&mut self) {
        if let Some(b) = &mut self.battle {
            b.victory_ack = true;
        }
    }

    /// Cancel auto-battle (围攻) so the player regains manual control on the next prompt.
    pub fn battle_cancel_auto(&mut self) -> bool {
        if let Some(b) = &mut self.battle {
            if b.auto_repeat {
                b.auto_repeat = false;
                self.recorder
                    .push(self.tick_count, super::ReplayEvent::BattleCancelAuto);
                return true;
            }
        }
        false
    }

    /// Spawn an enemy mid-battle (`enemy_summon`). Returns the slot index.
    pub(crate) fn battle_summon(&mut self, enemy_id: u16) -> Option<usize> {
        let b = self.battle.as_ref()?;
        let slot = if b.enemies.len() < 5 { b.enemies.len() } else { b.enemies.iter().position(|e| e.hp <= 0)? };
        let e = self.battle_enemy_template(enemy_id, slot);
        let b = self.battle.as_mut().unwrap();
        if slot == b.enemies.len() {
            b.enemies.push(e);
        } else {
            b.enemies[slot] = e;
        }
        Some(slot)
    }

    fn role_alive(&self, role: usize) -> bool {
        self.world.roles.get(role).map(|r| r.hp > 0).unwrap_or(false)
    }


    pub fn battle_awaiting_ack(&self) -> bool {
        self.battle.as_ref().map(|b| b.phase == Phase::AwaitVictoryAck).unwrap_or(false)
    }

}
