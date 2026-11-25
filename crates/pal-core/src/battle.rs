//! Battle damage formulas matching verbatim SDLPAL `fight.c` (#255).

use crate::rng::GameRng;

/// Compute base damage matching verbatim SDLPAL `PAL_CalcBaseDamage` (`fight.c:131-171`).
///
/// Returns `0` when `attack <= defense * 0.6`; callers apply bonuses/multipliers and clamp
/// final hit damage to `>= 1` where appropriate.
pub fn calc_base_damage(attack: i32, defense: i32) -> i32 {
    let atk = attack.max(0) as f64;
    let def = defense.max(0) as f64;
    if atk > def {
        (atk * 2.0 - def * 1.6 + 0.5) as i32
    } else if atk > def * 0.6 {
        (atk - def * 0.6 + 0.5) as i32
    } else {
        0
    }
}

/// Verbatim SDLPAL `PAL_CalcPhysicalAttackDamage` (`fight.c:253-286`).
///
/// Divides `PAL_CalcBaseDamage` by `phys_resist` when `phys_resist != 0` (e.g. `2` for
/// enemy-vs-hero halving, `99` for spirits); does NOT apply symmetric jitter (callers add
/// SDLPAL's exact `RandomLong`/`RandomFloat` terms).
pub fn sdlpal_calc_physical_damage(attack: i32, defense: i32, phys_resist: i32) -> i32 {
    let mut dmg = calc_base_damage(attack, defense);
    if phys_resist != 0 {
        dmg /= phys_resist;
    }
    dmg
}

/// Verbatim SDLPAL `PAL_CalcMagicDamage` (`fight.c:174-250`).
///
/// - `elemental == 0`: non-elemental (御剑术/万剑诀/剑神/酒神/乾坤一掷) skips resistance & field bonus.
/// - `elemental in 1..=5`: uses `elem_resist` (enemy: `0..=10` with `resist_mult = 1`;
///   hero: `100 + hero_elem_resist_pct` with `resist_mult = 20`) and `field_effect` (`-10..=10`).
/// - `elemental > 5`: uses `poison_resist` with `resist_mult` (no field bonus).
#[allow(clippy::too_many_arguments)]
pub fn sdlpal_calc_magic_damage(
    base_damage: i32,
    caster_magic: i32,
    target_defense: i32,
    elemental: i32,
    elem_resist: i32,
    poison_resist: i32,
    resist_mult: i32,
    field_effect: i32,
    rng: &mut GameRng,
) -> i32 {
    let mag_roll = rng.random_float(10.0, 11.0);
    let eff_magic = ((caster_magic.max(0) as f64) * mag_roll) as i32 / 10;

    let mut dmg = calc_base_damage(eff_magic, target_defense) / 4;
    dmg += base_damage;

    if elemental != 0 {
        let rm = resist_mult.max(1) as f64;
        let res_val = if elemental > 5 {
            poison_resist as f64
        } else {
            elem_resist as f64
        };
        dmg = ((dmg as f64) * (10.0 - (res_val / rm))) as i32;
        dmg /= 5;

        if elemental <= 5 {
            dmg = dmg * (10 + field_effect) / 10;
        }
    }

    dmg
}

/// Deterministic wrapper for callers passing an explicit seed instead of `&mut GameRng`.
pub fn calc_physical_damage(attack: i32, defense: i32, phys_resist: i32, _seed: u32) -> i32 {
    sdlpal_calc_physical_damage(attack, defense, phys_resist).max(1)
}

/// Deterministic wrapper for callers passing an explicit seed instead of `&mut GameRng`.
#[allow(clippy::too_many_arguments)]
pub fn calc_magic_damage_ex(
    base_damage: i32,
    caster_magic: i32,
    target_defense: i32,
    elemental: i32,
    resist: i32,
    vs_enemy: bool,
    field_bonus_pct: i32,
    seed: u32,
) -> i32 {
    let mut rng = GameRng::new(seed | 1);
    let (res, rm) = if vs_enemy {
        (resist, 1)
    } else {
        (100 + resist, 20)
    };
    let field_effect = field_bonus_pct / 10;
    sdlpal_calc_magic_damage(
        base_damage,
        caster_magic,
        target_defense,
        elemental,
        res,
        res,
        rm,
        field_effect,
        &mut rng,
    )
    .max(1)
}

/// Legacy wrapper preserved for unit tests without explicit `elemental`/`vs_enemy`.
pub fn calc_magic_damage(
    base_damage: i32,
    caster_magic: i32,
    target_defense: i32,
    elem_resist: i32,
    field_bonus_pct: i32,
    seed: u32,
) -> i32 {
    calc_magic_damage_ex(
        base_damage,
        caster_magic,
        target_defense,
        1,
        elem_resist * 10,
        false,
        field_bonus_pct,
        seed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdlpal_calc_base_damage_verbatim_rounding_and_zero_floor() {
        // fight.c:157-168:
        // atk=100, def=20 -> (200 - 32.0 + 0.5) as i32 = 168
        assert_eq!(calc_base_damage(100, 20), 168);
        // atk=50, def=31 -> (100 - 49.6 + 0.5) as i32 = 50 (vs truncated 51 in old integer math!)
        assert_eq!(calc_base_damage(50, 31), 50);
        // atk=40, def=50 -> atk > def*0.6 (30) -> (40 - 30.0 + 0.5) = 10
        assert_eq!(calc_base_damage(40, 50), 10);
        // atk=30, def=50 -> atk == def*0.6 -> 0 (not forced to 1!)
        assert_eq!(calc_base_damage(30, 50), 0);
        assert_eq!(calc_base_damage(5, 100), 0);
    }

    #[test]
    fn damage_formulas_scale_with_stats_and_resistance() {
        assert!(calc_base_damage(100, 20) > calc_base_damage(50, 20));
        assert!(calc_physical_damage(100, 20, 0, 6) > calc_physical_damage(100, 20, 8, 6));
        assert!(calc_physical_damage(1, 200, 10, 6) >= 1, "damage never drops below 1");
        assert!(calc_magic_damage(100, 80, 20, 0, 50, 1) > calc_magic_damage(100, 80, 20, 9, -30, 1) * 3);
    }

    #[test]
    fn issue126_early_enemy_wind_spell_does_not_deal_290_damage() {
        // Scene 16 绿叶妖精 (enemy_id 132) casts 风咒 (base_damage=45, magic_power=0) on Lv.1-3 hero (def=20).
        for seed in 0..16 {
            let dmg = calc_magic_damage(45, 0, 20, 0, 0, seed);
            assert!(
                (38..=55).contains(&dmg),
                "Expected authentic ~45 HP for 风咒, got {dmg} (seed={seed})"
            );
        }
    }

    #[test]
    fn sdlpal_batch2_physical_divisor_and_magic_elemental_parity() {
        // Physical resistance is a divisor: resist=2 halves damage (enemy vs hero); resist=99 spirit takes ~1/99.
        assert_eq!(sdlpal_calc_physical_damage(100, 20, 0), 168);
        assert_eq!(sdlpal_calc_physical_damage(100, 20, 2), 84);
        assert_eq!(sdlpal_calc_physical_damage(100, 20, 99), 1);

        // Non-elemental spells (elemental=0: 御剑术/万剑诀/剑神) skip elemental resist completely.
        let non_elem = calc_magic_damage_ex(100, 80, 20, 0, 10, true, -50, 5);
        assert!(non_elem >= 130, "non-elemental spell must ignore resist & field penalty, got {non_elem}");

        // Player elemental spell vs 0-resist enemy deals 2x base (SDLPAL ÷5); vs 5-resist enemy deals 1x; vs 10-resist deals 0.
        let mut r0 = GameRng::new(7);
        let weak_e = sdlpal_calc_magic_damage(100, 0, 20, 1, 0, 0, 1, 0, &mut r0);
        let mid_e = sdlpal_calc_magic_damage(100, 0, 20, 1, 5, 0, 1, 0, &mut r0);
        let immune_e = sdlpal_calc_magic_damage(100, 0, 20, 1, 10, 0, 1, 0, &mut r0);
        assert_eq!(weak_e, 200);
        assert_eq!(mid_e, 100);
        assert_eq!(immune_e, 0);
    }
}
