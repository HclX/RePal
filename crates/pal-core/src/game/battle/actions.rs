//! Hero combat actions: attack, defend, items, cooperative skills, and magic casting.

use super::*;

impl Game {
    /// Execute one stage of a hero action; returns the next stage or `None` when finished.
    pub(super) fn battle_hero_action(&mut self, h: usize, cmd: BattleCmd, stage: u8) -> Option<u8> {
        let role = self.battle.as_ref().unwrap().heroes[h].role;
        let (hx, hy) = {
            let hero = &self.battle.as_ref().unwrap().heroes[h];
            (hero.x, hero.y)
        };
        match cmd {
            BattleCmd::Defend => {
                let b = self.battle.as_mut().unwrap();
                b.heroes[h].defending = true;
                b.heroes[h].pose = HeroPose::Defend;
                let t = self.world.t("防御", "防禦").to_string();
                self.battle.as_mut().unwrap().float(hx, hy - 28, t, CYAN);
                self.sleep_ms(250);
                None
            }
            BattleCmd::Attack { target } if stage == 0 => {
                {
                    let b = self.battle.as_mut().unwrap();
                    let t_idx = if b.enemies.get(target).map(|e| e.hp > 0).unwrap_or(false) {
                        Some(target)
                    } else {
                        b.first_alive_enemy()
                    };
                    let lunge = if let Some(ti) = t_idx {
                        let (ex, ey) = (b.enemies[ti].x, b.enemies[ti].y);
                        ((ex - hx + 30).clamp(-140, -24), (ey - hy + 14).clamp(-76, -14))
                    } else {
                        (-24, -14)
                    };
                    b.heroes[h].lunge = lunge;
                    b.heroes[h].pose = HeroPose::AttackLunge;
                    b.heroes[h].pose_ticks = 9;
                }
                let sfx = self.world.roles[role].sfx.weapon;
                self.emit(HostCmd::PlaySfx(if sfx != 0 { sfx as i32 } else { 9 }));
                self.sleep_ms(160);
                Some(1)
            }
            BattleCmd::Attack { target } => {
                let confused = self.world.roles[role].statuses.get(&STATUS_CONFUSED).copied().unwrap_or(0) > 0;
                let bravery = self.world.roles[role].statuses.get(&STATUS_BRAVERY).copied().unwrap_or(0) > 0;
                let dual = self.world.roles[role].statuses.get(&STATUS_DUAL_ATTACK).copied().unwrap_or(0) > 0;
                let atk = self.world.roles[role].stats[0];

                if confused {
                    // SDLPAL: Confused hero attacks a random alive party member!
                    {
                        let b = self.battle.as_mut().unwrap();
                        b.heroes[h].lunge = (0, 0);
                        b.heroes[h].pose = HeroPose::AttackSlash;
                        b.heroes[h].pose_ticks = 6;
                    }
                    let hero_roles: Vec<usize> = self.battle.as_ref().unwrap().heroes.iter().map(|hh| hh.role).collect();
                    let allies: Vec<usize> = (0..hero_roles.len()).filter(|&i| self.role_alive(hero_roles[i])).collect();
                    if !allies.is_empty() {
                        let pick = allies[self.world.rng.below(allies.len() as u32) as usize];
                        let (t_role, tx, ty, defending) = {
                            let hh = &self.battle.as_ref().unwrap().heroes[pick];
                            (hh.role, hh.x, hh.y, hh.defending)
                        };
                        let protect = self.world.roles[t_role].statuses.get(&STATUS_PROTECT).copied().unwrap_or(0) > 0;
                        let def = self.world.roles[t_role].stats[2];
                        let dmg = sdlpal_hero_attack_mate(atk, def, defending, protect);
                        let rr = &mut self.world.roles[t_role];
                        rr.hp = (rr.hp - dmg).max(0);
                        rr.statuses.remove(&STATUS_SLEEP);
                        let b = self.battle.as_mut().unwrap();
                        b.heroes[pick].flash = 6;
                        b.heroes[pick].pose = HeroPose::Hurt;
                        b.heroes[pick].pose_ticks = 10;
                        b.float(tx, ty - 30, format!("-{dmg}"), HERO_HIT);
                    }
                    self.sleep_ms(280);
                    return None;
                }

                let hits = calc_hero_attack_hits(dual);
                let mut death_sfx = 0;
                for hit_i in 0..hits {
                    let idx = {
                        let b = self.battle.as_mut().unwrap();
                        b.heroes[h].lunge = (0, 0);
                        b.heroes[h].pose = HeroPose::AttackSlash;
                        b.heroes[h].pose_ticks = 7;
                        if b.enemies.get(target).map(|e| e.hp > 0).unwrap_or(false) {
                            Some(target)
                        } else {
                            b.first_alive_enemy()
                        }
                    };
                    if let Some(t) = idx {
                        let (e_def, e_res_phys) = {
                            let e = &self.battle.as_ref().unwrap().enemies[t];
                            (e.defense, e.resist_phys)
                        };
                        let (dmg, is_crit) = sdlpal_hero_physical_hit(
                            atk,
                            e_def,
                            e_res_phys,
                            role,
                            bravery,
                            &mut self.world.rng,
                        );
                        let b = self.battle.as_mut().unwrap();
                        b.current_target_enemy = t;
                        let e = &mut b.enemies[t];
                        let was_alive = e.hp > 0;
                        e.hp = (e.hp - dmg).max(0);
                        e.statuses.remove(&STATUS_SLEEP);
                        e.flash = 6;
                        e.pose = EnemyPose::Hurt;
                        e.pose_ticks = 8;
                        if was_alive && e.hp == 0 && e.sfx_death > 0 {
                            death_sfx = e.sfx_death;
                        }
                        if is_crit {
                            b.shake_ticks = 6;
                            b.shake_level = 2;
                        }
                        let (ex, ey) = (e.x, e.y);
                        let color = if is_crit { GOLD } else { RED };
                        b.cur_actor = format!("hero#{h}");
                        b.cur_action = "attack".to_string();
                        b.cur_skill.clear();
                        b.float(ex, ey - 30 - hit_i * 12, format!("-{dmg}"), color);
                    }
                }
                if death_sfx > 0 {
                    self.emit(HostCmd::PlaySfx(death_sfx));
                }
                self.sleep_ms(280);
                None
            }
            BattleCmd::Magic { magic, target } => self.battle_magic(h, role, magic, target, stage),
            BattleCmd::Coop { target } => self.battle_coop(h, role, target, stage),
            BattleCmd::Item { item } => self.battle_use_item(h, role, item, None),
            BattleCmd::ItemTarget { item, target } => self.battle_use_item(h, role, item, Some(target)),
            BattleCmd::Flee | BattleCmd::Auto => None,
        }
    }

    /// Execute an item (consumable heal/revive on chosen ally, or throwable weapon on chosen enemy).
    fn battle_use_item(&mut self, h: usize, role: usize, item: u16, target: Option<usize>) -> Option<u8> {
        let it = self.data.items.get(&item).cloned()?;
        // Reject non-usable / non-throwable items (e.g. weapons/armor) in battle.
        if !it.usable && !it.throwable {
            return None;
        }
        if self.world.item_count(item) <= 0 {
            return None;
        }
        // Only consume stackable consumables; protect reusable key items (e.g. 紫金葫芦 #270).
        if it.consumable && !self.world.remove_item(item, 1) {
            return None;
        }
        let is_throw = it.throwable && !it.usable;
        self.emit(HostCmd::PlaySfx(if is_throw { 10 } else { 19 }));
        let item_name = self.item_display_name(item);
        let actor_name = self.role_display_name(role);
        let (hx, hy) = {
            let b = self.battle.as_mut().unwrap();
            b.set_banner(format!("hero#{h}:{actor_name}"), "item", item_name, 26);
            if is_throw {
                b.heroes[h].lunge = (-16, -8);
                b.heroes[h].pose = HeroPose::AttackSlash;
                b.heroes[h].pose_ticks = 10;
            } else {
                b.heroes[h].pose = HeroPose::CastRelease;
                b.heroes[h].pose_ticks = 12;
            }
            (b.heroes[h].x, b.heroes[h].y)
        };
        let use_lbl = if !it.script_use.is_empty() {
            self.prog.intern(&it.script_use)
        } else {
            NO_LABEL
        };
        let throw_lbl = if !it.script_throw.is_empty() {
            self.prog.intern(&it.script_throw)
        } else {
            NO_LABEL
        };
        let script_lbl = if is_throw && throw_lbl != NO_LABEL {
            throw_lbl
        } else if use_lbl != NO_LABEL {
            use_lbl
        } else {
            throw_lbl
        };

        let target_hero_idx = if !is_throw {
            target.filter(|&ti| ti < self.battle.as_ref().unwrap().heroes.len()).unwrap_or(h)
        } else {
            h
        };
        let target_role = self
            .battle
            .as_ref()
            .and_then(|b| b.heroes.get(target_hero_idx))
            .map(|hh| hh.role)
            .unwrap_or(role);

        if script_lbl != NO_LABEL {
            self.vmc.using_item = true;
            self.vmc.current_target_role = target_role;
            self.vmc.target_item_all = it.target_all;
            if let Some(b) = self.battle.as_mut() {
                b.heroes[h].lunge = (0, 0);
                let enemy_t = if is_throw {
                    target
                        .filter(|&ti| b.enemies.get(ti).map(|e| e.hp > 0).unwrap_or(false))
                        .or_else(|| b.first_alive_enemy())
                        .unwrap_or(0)
                } else {
                    b.first_alive_enemy().unwrap_or(0)
                };
                b.current_target_enemy = enemy_t;
            }
            self.sleep_ms(220);
            self.request_script(script_lbl);
            return None;
        }

        if is_throw {
            let mut death_sfx = 0;
            {
                let b = self.battle.as_mut().unwrap();
                b.heroes[h].lunge = (0, 0);
                let targets: Vec<usize> = if it.target_all {
                    (0..b.enemies.len()).filter(|&i| b.enemies[i].hp > 0).collect()
                } else {
                    target
                        .filter(|&ti| b.enemies.get(ti).map(|e| e.hp > 0).unwrap_or(false))
                        .or_else(|| b.first_alive_enemy())
                        .into_iter()
                        .collect()
                };
                let dmg = ((it.price / 4).clamp(45, 350)).max(45);
                for ti in targets {
                    let e = &mut b.enemies[ti];
                    let was_alive = e.hp > 0;
                    e.hp = (e.hp - dmg).max(0);
                    e.flash = 6;
                    e.pose = EnemyPose::Hurt;
                    e.pose_ticks = 8;
                    if was_alive && e.hp == 0 && e.sfx_death > 0 {
                        death_sfx = e.sfx_death;
                    }
                    let (ex, ey) = (e.x, e.y);
                    b.float(ex, ey - 30, format!("-{dmg}"), RED);
                }
            }
            if death_sfx > 0 {
                self.emit(HostCmd::PlaySfx(death_sfx));
            }
            self.sleep_ms(280);
            return None;
        }

        let mp_item = ["鼠儿果", "还神丹", "灵山仙芝", "雪莲", "茶", "酒", "露"]
            .iter()
            .any(|k| it.name.contains(k))
            && !["止血", "金创", "行军"].iter().any(|k| it.name.contains(k));
        let (tx, ty) = self
            .battle
            .as_ref()
            .and_then(|b| b.heroes.get(target_hero_idx))
            .map(|hh| (hh.x, hh.y))
            .unwrap_or((hx, hy));
        let r = &mut self.world.roles[target_role];
        let (text, color) = if mp_item {
            r.mp = (r.mp + 80).min(r.max_mp);
            ("+80 MP".to_string(), BLUE)
        } else if r.hp > 0 {
            r.hp = (r.hp + 150).min(r.max_hp);
            ("+150 HP".to_string(), GREEN)
        } else {
            return None;
        };
        self.battle.as_mut().unwrap().float(tx, ty - 30, text, color);
        self.sleep_ms(300);
        None
    }

    /// Authentic SDLPAL Coop Magic (`kBattleActionCoopMagic`): all alive party members strike in unison!
    fn battle_coop(&mut self, h: usize, role: usize, target: usize, stage: u8) -> Option<u8> {
        let coop_id = self.world.roles.get(role).map(|r| r.coop_magic).unwrap_or(0);
        let m = self.data.magics.get(&coop_id).cloned();
        let alive_heroes: Vec<usize> = {
            let b = self.battle.as_ref().unwrap();
            (0..b.heroes.len())
                .filter(|&i| self.role_alive(b.heroes[i].role))
                .collect()
        };
        if alive_heroes.len() < 2 || coop_id == 0 || m.is_none() {
            // Fallback to normal physical attack if party dropped to 1 hero before execution.
            return self.battle_hero_action(h, BattleCmd::Attack { target }, stage);
        }
        let m = m.unwrap();
        let cost_mp = m.cost_mp.max(15);
        match stage {
            0 => {
                if self.world.roles[role].mp < cost_mp {
                    return self.battle_hero_action(h, BattleCmd::Attack { target }, stage);
                }
                self.world.roles[role].mp = (self.world.roles[role].mp - cost_mp).max(0);
                let spell_name = self.magic_display_name(coop_id);
                let actor_name = self.role_display_name(role);
                {
                    let label = if spell_name.is_empty() {
                        self.world.t3("合体技", "合體技", "Co-op").to_string()
                    } else {
                        spell_name
                    };
                    let b = self.battle.as_mut().unwrap();
                    b.set_banner(format!("hero#{h}:{actor_name}"), "coop", label, 40);
                    b.dim_bg = true;
                    for &hi in &alive_heroes {
                        b.heroes[hi].lunge = (-14, -8);
                        b.heroes[hi].pose = HeroPose::CastPrep;
                        b.heroes[hi].pose_ticks = 18;
                    }
                }
                let sfx = if m.sfx_id != 0 {
                    m.sfx_id as i32
                } else if self.world.roles[role].sfx.magic != 0 {
                    self.world.roles[role].sfx.magic as i32
                } else {
                    19
                };
                self.emit(HostCmd::PlaySfx(sfx));
                Some(if m.fire_effect_id >= 0 && m.fire_effect_id < 0xFFFF { 1 } else { 15 })
            }
            1..=14 => {
                let frame = (stage - 1) as u16;
                let b = self.battle.as_mut().unwrap();
                for &hi in &alive_heroes {
                    b.heroes[hi].pose = if stage >= 6 {
                        HeroPose::CastRelease
                    } else {
                        HeroPose::CastPrep
                    };
                    b.heroes[hi].pose_ticks = 14;
                }
                let t = b
                    .enemies
                    .get(target)
                    .filter(|e| e.hp > 0)
                    .map(|_| target)
                    .or_else(|| b.first_alive_enemy());
                let (x, y) = t.map(|i| (b.enemies[i].x, b.enemies[i].y)).unwrap_or((110, 95));
                b.fire = Some(FireAnim {
                    fire_id: m.fire_effect_id as u16,
                    frame,
                    x,
                    y,
                    target_all: m.target_all,
                    target_enemy: true,
                    is_summon: m.magic_type == 9,
                });
                self.sleep_ms(45);
                Some(stage + 1)
            }
            15 => {
                // Combined party power: sum of (attack + magic_power) across all participating allies
                let combined_power: i32 = alive_heroes
                    .iter()
                    .filter_map(|&hi| {
                        let rid = self.battle.as_ref().unwrap().heroes[hi].role;
                        self.world.roles.get(rid).map(|r| r.stats[0] + r.stats[1])
                    })
                    .sum();
                let elem = if (1..=5).contains(&m.elemental) {
                    Some((m.elemental - 1) as usize)
                } else {
                    None
                };
                let base = if m.base_damage != 0 { m.base_damage } else { 90 };
                let mut death_sfx = 0;
                let (field_eff, targets): (i32, Vec<usize>) = {
                    let b = self.battle.as_mut().unwrap();
                    b.fire = None;
                    b.dim_bg = false;
                    b.shake_ticks = 12;
                    b.shake_level = 3;
                    for &hi in &alive_heroes {
                        b.heroes[hi].lunge = (0, 0);
                        b.heroes[hi].pose = HeroPose::AttackSlash;
                        b.heroes[hi].pose_ticks = 8;
                    }
                    let fe = elem.map(|e| b.field_elem[e]).unwrap_or(0);
                    let targets: Vec<usize> = if m.target_all {
                        (0..b.enemies.len()).filter(|&i| b.enemies[i].hp > 0).collect()
                    } else {
                        b.enemies
                            .get(target)
                            .filter(|e| e.hp > 0)
                            .map(|_| target)
                            .or_else(|| b.first_alive_enemy())
                            .into_iter()
                            .collect()
                    };
                    if let Some(&first) = targets.first() {
                        b.current_target_enemy = first;
                    }
                    (fe, targets)
                };
                for i in targets {
                    let (e_def, resist, poison_res) = {
                        let e = &self.battle.as_ref().unwrap().enemies[i];
                        (e.defense, elem.map(|x| e.resist_elem[x]).unwrap_or(0), e.resist_poison)
                    };
                    let dmg = sdlpal_coop_magic_damage(
                        combined_power,
                        base,
                        m.elemental,
                        e_def,
                        resist,
                        poison_res,
                        field_eff,
                        &mut self.world.rng,
                    );
                    let b = self.battle.as_mut().unwrap();
                    let e = &mut b.enemies[i];
                    let was_alive = e.hp > 0;
                    e.hp = (e.hp - dmg).max(0);
                    e.flash = 8;
                    e.pose = EnemyPose::Hurt;
                    e.pose_ticks = 10;
                    if was_alive && e.hp == 0 && e.sfx_death > 0 {
                        death_sfx = e.sfx_death;
                    }
                    let (x, y) = (e.x, e.y);
                    b.float(x, y - 30, format!("-{dmg}"), GOLD);
                }
                if death_sfx > 0 {
                    self.emit(HostCmd::PlaySfx(death_sfx));
                }
                self.sleep_ms(320);
                None
            }
            _ => None,
        }
    }

    /// Whether the battle is waiting for the victory screen to be dismissed.
    fn battle_magic(&mut self, h: usize, role: usize, magic_id: u16, target: usize, stage: u8) -> Option<u8> {
        let m = self.data.magics.get(&magic_id).cloned()?;
        let use_l = self.prog.intern(&m.script_use);
        let succ_l = self.prog.intern(&m.script_success);
        match stage {
            0 => {
                let silenced = self.world.roles[role].statuses.get(&STATUS_SILENCE).copied().unwrap_or(0) > 0;
                if silenced || self.world.roles[role].mp < m.cost_mp {
                    return None;
                }
                self.vmc.current_target_role = role;
                self.vmc.item_script_failed = false;
                if let Some(b) = self.battle.as_mut() {
                    b.active_hero = h;
                    b.base_magic_dmg_override = None;
                    if m.target_enemy {
                        let t = b
                            .enemies
                            .get(target)
                            .filter(|e| e.hp > 0)
                            .map(|_| target)
                            .or_else(|| b.first_alive_enemy())
                            .unwrap_or(0);
                        b.current_target_enemy = t;
                    }
                }
                // Run pre-cast `script_use` first (SDLPAL: item/cash checks, 酒神 MP-to-dmg, 乾坤一掷, 金蝉脱壳, #213).
                if use_l != NO_LABEL {
                    self.request_script(use_l);
                    return Some(16);
                }
                Some(16)
            }
            16 => {
                let b_active = self.battle.as_ref().map(|b| b.active).unwrap_or(false);
                if self.vmc.item_script_failed || !b_active {
                    self.vmc.item_script_failed = false;
                    if let Some(b) = self.battle.as_mut() {
                        b.base_magic_dmg_override = None;
                    }
                    return None;
                }
                let spell_name = self.magic_display_name(magic_id);
                let actor_name = self.role_display_name(role);
                {
                    let b = self.battle.as_mut().unwrap();
                    b.set_banner(format!("hero#{h}:{actor_name}"), "magic", spell_name, 38);
                    b.dim_bg = true;
                    b.heroes[h].pose = HeroPose::CastPrep;
                    b.heroes[h].pose_ticks = 14;
                }
                let sfx = if m.sfx_id != 0 {
                    m.sfx_id as i32
                } else if self.world.roles[role].sfx.magic != 0 {
                    self.world.roles[role].sfx.magic as i32
                } else {
                    19
                };
                self.emit(HostCmd::PlaySfx(sfx));
                Some(if m.fire_effect_id >= 0 && m.fire_effect_id < 0xFFFF { 1 } else { 15 })
            }
            1..=14 => {
                let frame = (stage - 1) as u16;
                let b = self.battle.as_mut().unwrap();
                if stage >= 6 {
                    b.heroes[h].pose = HeroPose::CastRelease;
                    b.heroes[h].pose_ticks = 14;
                } else {
                    b.heroes[h].pose = HeroPose::CastPrep;
                    b.heroes[h].pose_ticks = 10;
                }
                let t = b.enemies.get(target).filter(|e| e.hp > 0).map(|_| target).or_else(|| b.first_alive_enemy());
                let (x, y) = if m.target_enemy {
                    t.map(|i| (b.enemies[i].x, b.enemies[i].y)).unwrap_or((110, 95))
                } else {
                    let hh = b.heroes.get(target).unwrap_or(&b.heroes[h]);
                    (hh.x, hh.y)
                };
                let is_summon = m.magic_type == 9;
                b.fire = Some(FireAnim {
                    fire_id: m.fire_effect_id as u16,
                    frame,
                    x: if is_summon { 160 } else { x },
                    y: if is_summon { 100 } else { y },
                    target_all: m.target_all,
                    target_enemy: m.target_enemy,
                    is_summon,
                });
                self.sleep_ms(45);
                Some(stage + 1)
            }
            15 => {
                let b = self.battle.as_mut().unwrap();
                b.fire = None;
                b.dim_bg = false;
                let elem = if (1..=5).contains(&m.elemental) { Some((m.elemental - 1) as usize) } else { None };
                let dmg_override = b.base_magic_dmg_override.take();
                let base = dmg_override.unwrap_or_else(|| if m.base_damage != 0 { m.base_damage } else { 35 });
                let r = &self.world.roles[role];
                if r.hp <= 0 || (dmg_override.is_none() && r.mp < m.cost_mp) {
                    return None;
                }
                let mag_pow = r.stats[1];
                self.world.roles[role].mp = (self.world.roles[role].mp - m.cost_mp).max(0);
                let mut death_sfx = 0;
                if !m.target_enemy {
                    let has_revive_op = |lbl| {
                        self.prog
                            .block(lbl)
                            .map(|steps| steps.iter().any(|s| s.op == 0x0022))
                            .unwrap_or(false)
                    };
                    let is_revive = magic_id == 301
                        || magic_id == 302
                        || has_revive_op(use_l)
                        || has_revive_op(succ_l);
                    // Only heal when the spell actually has healing power or is a revive spell;
                    // utility/buff spells (金蝉脱壳, 梦蛇, 净衣咒, 金刚咒, etc.) have base_damage == 0
                    // and execute via `script_success` / `script_use` without healing the party.
                    if m.base_damage != 0 || is_revive {
                        let heal = (base.abs() + mag_pow * 2).max(35);
                        let b = self.battle.as_ref().unwrap();
                        let heroes: Vec<(usize, i32, i32)> = if m.target_all {
                            b.heroes.iter().map(|x| (x.role, x.x, x.y)).collect()
                        } else {
                            let hh = b.heroes.get(target).unwrap_or(&b.heroes[h]);
                            vec![(hh.role, hh.x, hh.y)]
                        };
                        for (r, x, y) in heroes {
                            let rr = &mut self.world.roles[r];
                            if !is_revive && rr.hp <= 0 {
                                continue;
                            }
                            rr.hp = (rr.hp + heal).min(rr.max_hp);
                            self.battle.as_mut().unwrap().float(x, y - 30, format!("+{heal}"), GREEN);
                        }
                    }
                } else {
                    let (field_eff, targets): (i32, Vec<usize>) = {
                        let b = self.battle.as_mut().unwrap();
                        let fe = elem.map(|e| b.field_elem[e]).unwrap_or(0);
                        let targets: Vec<usize> = if m.target_all {
                            (0..b.enemies.len()).filter(|&i| b.enemies[i].hp > 0).collect()
                        } else {
                            b.enemies
                                .get(target)
                                .filter(|e| e.hp > 0)
                                .map(|_| target)
                                .or_else(|| b.first_alive_enemy())
                                .into_iter()
                                .collect()
                        };
                        if let Some(&first) = targets.first() {
                            b.current_target_enemy = first;
                        }
                        if m.base_damage != 0 || dmg_override.is_some() || (use_l == NO_LABEL && succ_l == NO_LABEL) {
                            b.shake_ticks = if m.magic_type == 9 { 14 } else { 9 };
                            b.shake_level = if m.magic_type == 9 {
                                4
                            } else if m.target_all {
                                3
                            } else {
                                2
                            };
                        }
                        (fe, targets)
                    };
                    if m.base_damage != 0 || dmg_override.is_some() || (use_l == NO_LABEL && succ_l == NO_LABEL) {
                        for i in targets {
                            let (e_def, resist, poison_res) = {
                                let e = &self.battle.as_ref().unwrap().enemies[i];
                                (e.defense, elem.map(|x| e.resist_elem[x]).unwrap_or(0), e.resist_poison)
                            };
                            let dmg = sdlpal_calc_magic_damage(
                                base,
                                mag_pow,
                                e_def,
                                m.elemental,
                                resist,
                                poison_res,
                                1,
                                field_eff,
                                &mut self.world.rng,
                            )
                            .max(1);
                            let b = self.battle.as_mut().unwrap();
                            let e = &mut b.enemies[i];
                            let was_alive = e.hp > 0;
                            e.hp = (e.hp - dmg).max(0);
                            e.flash = 6;
                            e.pose = EnemyPose::Hurt;
                            e.pose_ticks = 8;
                            if was_alive && e.hp == 0 && e.sfx_death > 0 {
                                death_sfx = e.sfx_death;
                            }
                            let (x, y) = (e.x, e.y);
                            b.float(x, y - 30, format!("-{dmg}"), GOLD);
                        }
                    }
                }
                if death_sfx > 0 {
                    self.emit(HostCmd::PlaySfx(death_sfx));
                }
                if !m.target_enemy {
                    // Ensure ally-targeted heal/buff scripts (e.g. 观音咒/气疗术/金刚咒) target the chosen ally (#157).
                    let target_role = self
                        .battle
                        .as_ref()
                        .and_then(|b| b.heroes.get(target))
                        .map(|x| x.role)
                        .unwrap_or(role);
                    self.vmc.current_target_role = target_role;
                } else {
                    // Enemy-targeted spells run `script_success` with caster as target role (e.g. 酒神 `role_forget_magic`).
                    self.vmc.current_target_role = role;
                }
                self.sleep_ms(300);
                if succ_l != NO_LABEL {
                    let pos = self
                        .world
                        .magic_script_pos
                        .get(&magic_id)
                        .copied()
                        .unwrap_or_else(|| ScriptPos::new(succ_l, 0));
                    self.request_script_pos(pos, super::super::vm::Caller::MagicSuccess(magic_id));
                }
                None
            }
            _ => None,
        }
    }
}
