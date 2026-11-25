//! Centralized verbatim SDLPAL `PAL_CLASSIC` (`fight.c`) battle rules, turn-order
//! sorting, and damage formulas (#255, #270).
//!
//! Every formula below ports SDLPAL `fight.c` & `global.c` verbatim, preserving exact
//! integer truncation, RNG draw ordering, and `PAL_CLASSIC` dexterity-sorted turn order.

pub use crate::battle::{sdlpal_calc_magic_damage, sdlpal_calc_physical_damage};
use super::BattleCmd;
use crate::rng::GameRng;

/// Combatant entry in SDLPAL's per-round dexterity-sorted `ActionQueue` (`fight.c:1451-1586`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ActionActor {
    Hero { idx: usize, cmd: BattleCmd },
    Enemy { idx: usize, is_second: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ActionSlot {
    pub actor: ActionActor,
    pub dex: i32,
}

/// Action-type speed modifier category for SDLPAL `PAL_CLASSIC` turn order (`fight.c:1529-1556`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HeroSpeedKind {
    Normal,
    Coop,
    Defend,
    DefensiveMagic,
    UseItem,
    Flee,
}

/// SDLPAL `PAL_IsPlayerDying` (`fight.c:29-49`): `hp < min(100, max_hp / 5)`.
#[inline]
pub(super) fn sdlpal_is_player_dying(hp: i32, max_hp: i32) -> bool {
    hp < (max_hp / 5).min(100)
}

/// SDLPAL `PAL_GetEnemyDexterity` (`fight.c:289-333`, `PAL_CLASSIC`):
/// `(level + 6) * 3 + s16(raw_dexterity)`.
#[inline]
pub(super) fn sdlpal_enemy_dexterity(level: i32, raw_dexterity: i32) -> i32 {
    (level + 6) * 3 + (raw_dexterity as u16 as i16 as i32)
}

/// SDLPAL flee-defense contribution of a single alive enemy (`fight.c:4134-4135`):
/// `(level + 6) * 4 + s16(raw_dexterity)`.
#[inline]
pub(super) fn sdlpal_enemy_flee_def(level: i32, raw_dexterity: i32) -> i32 {
    (level + 6) * 4 + (raw_dexterity as u16 as i16 as i32)
}

/// SDLPAL `PAL_GetPlayerActualDexterity` (`fight.c:336-390`, `PAL_CLASSIC`):
/// `haste` triples dexterity, capped at `999`.
#[inline]
pub(super) fn sdlpal_player_actual_dexterity(dexterity: i32, haste: bool) -> i32 {
    let mut d = dexterity.max(0);
    if haste {
        d *= 3;
    }
    d.min(999)
}

/// Compute randomized enemy action-queue dexterity (`fight.c:1473-1474`).
#[inline]
pub(super) fn sdlpal_enemy_action_dexterity(enemy_dex: i32, rng: &mut GameRng) -> i32 {
    ((enemy_dex as f64) * rng.random_float(0.9, 1.1)) as i32
}

/// Compute randomized hero action-queue dexterity (`fight.c:1520-1565`).
pub(super) fn sdlpal_hero_action_dexterity(
    dexterity: i32,
    haste: bool,
    kind: HeroSpeedKind,
    is_dying: bool,
    rng: &mut GameRng,
) -> i32 {
    let mut d = sdlpal_player_actual_dexterity(dexterity, haste);
    match kind {
        HeroSpeedKind::Coop => d *= 10,
        HeroSpeedKind::Defend => d *= 5,
        HeroSpeedKind::DefensiveMagic | HeroSpeedKind::UseItem => d *= 3,
        HeroSpeedKind::Flee => d /= 2,
        HeroSpeedKind::Normal => {}
    }
    if is_dying {
        d /= 2;
    }
    ((d as f64) * rng.random_float(0.9, 1.1)) as i32
}

/// Exact SDLPAL `PAL_CLASSIC` action-queue descending sort (`fight.c:1574-1585`).
pub(super) fn sdlpal_sort_action_queue(queue: &mut [ActionSlot]) {
    let n = queue.len();
    for i in 0..n {
        for j in i..n {
            if queue[i].dex < queue[j].dex {
                queue.swap(i, j);
            }
        }
    }
}

/// Number of physical strikes for a hero attack (`STATUS_DUAL_ATTACK` doubles strikes).
#[inline]
pub(super) fn calc_hero_attack_hits(dual_attack: bool) -> i32 {
    if dual_attack {
        2
    } else {
        1
    }
}

/// Verbatim SDLPAL hero single-target physical hit (`fight.c:3636-3663`).
///
/// Exact RNG draw order:
/// 1. `RandomLong(1, 2)` added to `PAL_CalcPhysicalAttackDamage`
/// 2. `RandomLong(0, 5) == 0 || bravery` -> `*= 3` (critical)
/// 3. `role == 0 && RandomLong(0, 11) == 0` -> `*= 2` (Li Xiaoyao stacking bonus crit)
/// 4. `*= RandomFloat(1.0, 1.125)`, clamped to `>= 1`.
pub(super) fn sdlpal_hero_physical_hit(
    atk: i32,
    enemy_def: i32,
    enemy_resist_phys: i32,
    role: usize,
    bravery: bool,
    rng: &mut GameRng,
) -> (i32, bool) {
    let mut dmg = sdlpal_calc_physical_damage(atk, enemy_def, enemy_resist_phys);
    dmg += rng.random_long(1, 2);

    let mut critical = false;
    if rng.random_long(0, 5) == 0 || bravery {
        dmg *= 3;
        critical = true;
    }
    if role == 0 && rng.random_long(0, 11) == 0 {
        dmg *= 2;
        critical = true;
    }
    dmg = ((dmg as f64) * rng.random_float(1.0, 1.125)) as i32;
    (dmg.max(1), critical)
}

/// Verbatim SDLPAL confused hero attacking an ally (`fight.c:3812-3828`):
/// `defending` doubles DEF before calc; `protect` halves damage AFTER calc; min 1.
pub(super) fn sdlpal_hero_attack_mate(
    atk: i32,
    target_def: i32,
    defending: bool,
    protect: bool,
) -> i32 {
    let def = if defending { target_def * 2 } else { target_def };
    let mut dmg = sdlpal_calc_physical_damage(atk, def, 2);
    if protect {
        dmg /= 2;
    }
    dmg.max(1)
}

/// Verbatim SDLPAL flee check (`fight.c:4124-4143`):
/// `flee_rate >= RandomLong(0, sum(enemy_flee_def).max(0)) && can_flee && !has_boss`.
pub(super) fn sdlpal_flee_check(
    player_flee_rate: i32,
    enemy_flee_defs: &[i32],
    can_flee: bool,
    has_boss: bool,
    rng: &mut GameRng,
) -> bool {
    let def: i32 = enemy_flee_defs.iter().copied().sum::<i32>().max(0);
    let roll = rng.random_long(0, def);
    player_flee_rate >= roll && can_flee && !has_boss
}

/// Verbatim SDLPAL enemy physical attack (`fight.c:4924-5072`).
///
/// - `defending` doubles hero DEF before calc; `protect` does NOT double DEF.
/// - `RandomLong(0, 16) >= 10` (when hero is not Confused/Sleep/Paralyzed) triggers
///   auto-defend (complete physical evade -> `None`).
/// - Otherwise draws `RandomLong(0, 2)` added to `enemy_atk`, divides base damage by `2`,
///   adds `RandomLong(0, 1)`, halves after calc if `protect`, and clamps to `>= 1`.
pub(super) fn sdlpal_enemy_physical_hit(
    enemy_atk: i32,
    hero_def: i32,
    defending: bool,
    protect: bool,
    hero_incapacitated: bool,
    rng: &mut GameRng,
) -> Option<i32> {
    let def = if defending { hero_def * 2 } else { hero_def };
    let mut auto_defend = rng.random_long(0, 16) >= 10;
    if hero_incapacitated {
        auto_defend = false;
    }
    if auto_defend {
        return None;
    }
    let atk_roll = enemy_atk + rng.random_long(0, 2);
    let mut dmg = sdlpal_calc_physical_damage(atk_roll, def, 2);
    dmg += rng.random_long(0, 1);
    if protect {
        dmg /= 2;
    }
    Some(dmg.max(1))
}

/// Verbatim SDLPAL enemy magic attack against one hero (`fight.c:4790-4844`):
/// `PAL_CalcMagicDamage(..., 100 + elem_res, 100 + poison_res, 20)` then divided by
/// `((defending?2:1) * (protect?2:1)) + (auto_defend?1:0)`.
#[allow(clippy::too_many_arguments)]
pub(super) fn sdlpal_enemy_magic_hit(
    enemy_magic_power: i32,
    base_damage: i32,
    elemental: i32,
    hero_def: i32,
    hero_elem_resist: i32,
    hero_poison_resist: i32,
    field_effect: i32,
    defending: bool,
    protect: bool,
    auto_defend: bool,
    rng: &mut GameRng,
) -> i32 {
    let mut dmg = sdlpal_calc_magic_damage(
        base_damage,
        enemy_magic_power,
        hero_def,
        elemental,
        100 + hero_elem_resist,
        100 + hero_poison_resist,
        20,
        field_effect,
        rng,
    );
    let div = (if defending { 2 } else { 1 }) * (if protect { 2 } else { 1 })
        + (if auto_defend { 1 } else { 0 });
    dmg /= div.max(1);
    dmg.max(0)
}

/// Verbatim SDLPAL hero coop magic damage (`fight.c:3995-4041`):
/// `str = sum(atk + mag) / 4`, then `PAL_CalcMagicDamage(..., resist_mult = 1)`, min 1.
#[allow(clippy::too_many_arguments)]
pub(super) fn sdlpal_coop_magic_damage(
    combined_atk_and_mag: i32,
    base_damage: i32,
    elemental: i32,
    enemy_def: i32,
    enemy_elem_resist: i32,
    enemy_poison_resist: i32,
    field_effect: i32,
    rng: &mut GameRng,
) -> i32 {
    let str_val = combined_atk_and_mag / 4;
    sdlpal_calc_magic_damage(
        base_damage,
        str_val,
        enemy_def,
        elemental,
        enemy_elem_resist,
        enemy_poison_resist,
        1,
        field_effect,
        rng,
    )
    .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #104 Layer 3 differential verification against verbatim SDLPAL `fight.c` formulas
    /// with fixed deterministic seeds.
    #[test]
    fn layer3_sdlpal_turn_order_dexterity_and_modifiers() {
        let mut r1 = GameRng::new(0x1234_5678);
        let mut r2 = GameRng::new(0x1234_5678);

        // Enemy dexterity: (lv=5 + 6)*3 + raw(10) = 43
        assert_eq!(sdlpal_enemy_dexterity(5, 10), 43);
        // Player Haste triples dexterity up to 999
        assert_eq!(sdlpal_player_actual_dexterity(40, false), 40);
        assert_eq!(sdlpal_player_actual_dexterity(40, true), 120);
        assert_eq!(sdlpal_player_actual_dexterity(500, true), 999);
        // Player dying threshold: hp < min(100, max_hp / 5)
        assert!(sdlpal_is_player_dying(19, 100));
        assert!(!sdlpal_is_player_dying(20, 100));
        assert!(!sdlpal_is_player_dying(100, 999));
        assert!(sdlpal_is_player_dying(99, 999));

        // Verify action-speed priority ordering: Coop (10x) > Defend (5x) > UseItem/DefMagic (3x) > Normal (1x) > Flee (0.5x)
        let d_coop = sdlpal_hero_action_dexterity(30, false, HeroSpeedKind::Coop, false, &mut r1);
        let d_def = sdlpal_hero_action_dexterity(30, false, HeroSpeedKind::Defend, false, &mut r1);
        let d_item = sdlpal_hero_action_dexterity(30, false, HeroSpeedKind::UseItem, false, &mut r1);
        let d_norm = sdlpal_hero_action_dexterity(30, false, HeroSpeedKind::Normal, false, &mut r1);
        let d_flee = sdlpal_hero_action_dexterity(30, false, HeroSpeedKind::Flee, false, &mut r1);
        assert!(d_coop > d_def && d_def > d_item && d_item > d_norm && d_norm > d_flee);

        // Verify exact interleaved sort order with a fast enemy beating a slow fleeing hero
        let mut q = vec![
            ActionSlot {
                actor: ActionActor::Hero { idx: 0, cmd: BattleCmd::Flee },
                dex: sdlpal_hero_action_dexterity(30, false, HeroSpeedKind::Flee, false, &mut r2),
            },
            ActionSlot {
                actor: ActionActor::Enemy { idx: 0, is_second: false },
                dex: sdlpal_enemy_action_dexterity(60, &mut r2),
            },
            ActionSlot {
                actor: ActionActor::Hero { idx: 1, cmd: BattleCmd::Defend },
                dex: sdlpal_hero_action_dexterity(30, false, HeroSpeedKind::Defend, false, &mut r2),
            },
        ];
        sdlpal_sort_action_queue(&mut q);
        assert_eq!(q[0].actor, ActionActor::Hero { idx: 1, cmd: BattleCmd::Defend });
        assert_eq!(q[1].actor, ActionActor::Enemy { idx: 0, is_second: false });
        assert_eq!(q[2].actor, ActionActor::Hero { idx: 0, cmd: BattleCmd::Flee });
    }

    #[test]
    fn layer3_sdlpal_flee_uses_player_flee_rate_vs_enemy_dex_sum() {
        // High 吉运 (flee_rate=500) vs weak enemy (flee_def=30) always escapes non-boss
        let mut rng = GameRng::new(42);
        for _ in 0..20 {
            assert!(sdlpal_flee_check(500, &[30], true, false, &mut rng));
            assert!(!sdlpal_flee_check(500, &[30], true, true, &mut rng), "boss blocks flee");
        }
        // Low 吉运 (flee_rate=10) vs high-dex enemy pack (sum=400) fails most attempts
        let mut successes = 0;
        for _ in 0..100 {
            if sdlpal_flee_check(10, &[200, 200], true, false, &mut rng) {
                successes += 1;
            }
        }
        assert!(successes < 20, "low flee_rate must rarely escape fast enemies, got {successes}/100");
    }

    #[test]
    fn layer3_sdlpal_protect_halves_post_damage_and_auto_defend_evades() {
        // Find a seed where RandomLong(0, 16) < 10 (no auto-defend)
        let seed_hit = (1u32..500)
            .find(|&s| GameRng::new(s).random_long(0, 16) < 10)
            .expect("hit seed");
        let seed_evade = (1u32..500)
            .find(|&s| GameRng::new(s).random_long(0, 16) >= 10)
            .expect("evade seed");

        // Auto-defend evades physical hit when healthy, but NOT when incapacitated
        assert_eq!(
            sdlpal_enemy_physical_hit(120, 40, false, false, false, &mut GameRng::new(seed_evade)),
            None
        );
        assert!(
            sdlpal_enemy_physical_hit(120, 40, false, false, true, &mut GameRng::new(seed_evade)).is_some()
        );

        // Protect halves post-calculation damage (NOT doubling DEF before calc!)
        let normal = sdlpal_enemy_physical_hit(120, 40, false, false, false, &mut GameRng::new(seed_hit)).unwrap();
        let protected = sdlpal_enemy_physical_hit(120, 40, false, true, false, &mut GameRng::new(seed_hit)).unwrap();
        let defended = sdlpal_enemy_physical_hit(120, 40, true, false, false, &mut GameRng::new(seed_hit)).unwrap();
        assert_eq!(protected, (normal / 2).max(1), "Protect must halve post-calc damage");
        // For atk=120, def=40: normal base = 240 - 64 = 176 -> /2 = 88 -> protect = 44.
        // Defend doubles DEF to 80: base = 240 - 128 = 112 -> /2 = 56 (distinct from Protect's 44!).
        assert!(defended > protected, "Defend (2x DEF={defended}) and Protect (dmg/2={protected}) must follow distinct SDLPAL rules");

        // Enemy magic: Defend (2x) * Protect (2x) + AutoDefend (+1) = /5
        let m_norm = sdlpal_enemy_magic_hit(0, 100, 1, 20, 0, 0, 0, false, false, false, &mut GameRng::new(7));
        let m_prot = sdlpal_enemy_magic_hit(0, 100, 1, 20, 0, 0, 0, false, true, false, &mut GameRng::new(7));
        let m_all = sdlpal_enemy_magic_hit(0, 100, 1, 20, 0, 0, 0, true, true, true, &mut GameRng::new(7));
        assert_eq!(m_norm, 100);
        assert_eq!(m_prot, 50);
        assert_eq!(m_all, 20);
    }

    #[test]
    fn layer3_sdlpal_hero_attack_and_li_xiaoyao_stacking_crit() {
        // Find a seed where RandomLong(0, 5) == 0 (after RandomLong(1, 2)) AND RandomLong(0, 11) == 0
        let stack_seed = (1u32..50_000)
            .find(|&s| {
                let mut r = GameRng::new(s);
                let _ = r.random_long(1, 2);
                r.random_long(0, 5) == 0 && r.random_long(0, 11) == 0
            })
            .expect("stacking crit seed");
        let (dmg_li, crit_li) = sdlpal_hero_physical_hit(100, 20, 1, 0, false, &mut GameRng::new(stack_seed));
        let (dmg_other, crit_other) = sdlpal_hero_physical_hit(100, 20, 1, 1, false, &mut GameRng::new(stack_seed));
        assert!(crit_li && crit_other);
        // Li Xiaoyao stacks 3x * 2x = 6x vs other role's 3x!
        assert!(dmg_li >= dmg_other * 2 - 40, "Li Xiaoyao 2x bonus must stack with 3x crit ({dmg_li} vs {dmg_other})");
    }
}
