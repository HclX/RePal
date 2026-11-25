//! Battle state machine: tick loop, SDLPAL `PAL_CLASSIC` dexterity-sorted interleaved
//! `ActionQueue`, enemy AI, command acceptance, and round-end status/poison ticking (#255).

use super::*;

impl Game {
    /// Advance the battle by one tick. Returns `Some(outcome)` once the battle is over.
    pub(crate) fn battle_tick(&mut self) -> Option<BattleOutcome> {
        // Presentation timers.
        if let Some(b) = &mut self.battle {
            b.anim_tick = b.anim_tick.wrapping_add(1);
            b.float_texts.retain_mut(|t| {
                t.age += 1;
                t.age <= 34
            });
            if b.spell_banner_ticks > 0 {
                b.spell_banner_ticks -= 1;
                if b.spell_banner_ticks == 0 {
                    b.spell_banner = None;
                }
            }
            if b.shake_ticks > 0 {
                b.shake_ticks -= 1;
            }
            if b.enemy_fire_ticks > 0 {
                b.enemy_fire_ticks -= 1;
                if let Some(f) = b.fire.as_mut() {
                    f.frame = f.frame.saturating_add(1);
                }
                if b.enemy_fire_ticks == 0 {
                    b.fire = None;
                    b.dim_bg = false;
                }
            }
            for e in &mut b.enemies {
                e.flash = (e.flash - 1).max(0);
                if e.pose_ticks > 0 {
                    e.pose_ticks -= 1;
                    if e.pose_ticks == 0 {
                        e.pose = EnemyPose::Idle;
                    }
                }
                if e.hp <= 0 && e.death_fade > 0 {
                    e.death_fade = e.death_fade.saturating_sub(18);
                }
            }
            let phase = b.phase;
            for (hidx, h) in b.heroes.iter_mut().enumerate() {
                h.flash = (h.flash - 1).max(0);
                if h.pose_ticks > 0 {
                    h.pose_ticks -= 1;
                    if h.pose_ticks == 0 {
                        h.pose = if h.defending { HeroPose::Defend } else { HeroPose::Idle };
                    }
                }
                // Live ATB readiness bar (purely visual, derived from current battle phase).
                let target_atb: u8 = match phase {
                    Phase::Command(cur) => {
                        if hidx <= cur { 100 } else { 80 }
                    }
                    Phase::ActQueue(qi, _) => {
                        let pos = b
                            .action_queue
                            .iter()
                            .position(|slot| matches!(slot.actor, ActionActor::Hero { idx, .. } if idx == hidx));
                        match pos {
                            Some(p) if p < qi => 12,
                            Some(p) if p == qi => 100,
                            _ => 88,
                        }
                    }
                    Phase::RoundStart | Phase::PreTurn(_) => 95,
                    _ => 100,
                };
                if h.atb_pct < target_atb {
                    h.atb_pct = (h.atb_pct + 4).min(target_atb);
                } else if h.atb_pct > target_atb {
                    h.atb_pct = h.atb_pct.saturating_sub(12).max(target_atb);
                }
            }
            if b.sleep > 0 {
                b.sleep -= 1;
                return None;
            }
        }
        for _ in 0..64 {
            let Some(b) = self.battle.as_ref() else { return Some(BattleOutcome::Victory) };
            if b.script_request.is_some() || b.script_running || b.sleep > 0 || b.prompting.is_some() {
                return None;
            }
            if let Some(done) = self.battle_step() {
                return Some(done);
            }
        }
        None
    }

    pub(super) fn set_phase(&mut self, p: Phase) {
        self.battle.as_mut().unwrap().phase = p;
    }

    pub(super) fn sleep_ms(&mut self, ms: u32) {
        self.battle.as_mut().unwrap().sleep = ms_to_ticks(ms);
    }

    pub(super) fn request_script(&mut self, l: LabelId) -> bool {
        if l == NO_LABEL {
            return false;
        }
        self.request_script_pos(ScriptPos::new(l, 0), super::super::vm::Caller::None)
    }

    pub(super) fn request_script_pos(&mut self, pos: ScriptPos, caller: super::super::vm::Caller) -> bool {
        if pos.is_none() {
            return false;
        }
        self.battle.as_mut().unwrap().script_request = Some((pos, caller));
        true
    }

    fn hero_speed_kind(&self, cmd: BattleCmd) -> HeroSpeedKind {
        match cmd {
            BattleCmd::Coop { .. } => HeroSpeedKind::Coop,
            BattleCmd::Defend => HeroSpeedKind::Defend,
            BattleCmd::Flee => HeroSpeedKind::Flee,
            BattleCmd::Magic { magic, .. } => {
                let target_enemy = self
                    .data
                    .magics
                    .get(&magic)
                    .map(|m| m.target_enemy)
                    .unwrap_or(true);
                if !target_enemy {
                    HeroSpeedKind::DefensiveMagic
                } else {
                    HeroSpeedKind::Normal
                }
            }
            BattleCmd::Item { item } | BattleCmd::ItemTarget { item, .. } => {
                let is_throw = self
                    .data
                    .items
                    .get(&item)
                    .map(|it| it.throwable && !it.usable)
                    .unwrap_or(false);
                if is_throw {
                    HeroSpeedKind::Normal
                } else {
                    HeroSpeedKind::UseItem
                }
            }
            BattleCmd::Attack { .. } | BattleCmd::Auto => HeroSpeedKind::Normal,
        }
    }

    /// Build and sort the round's interleaved `PAL_CLASSIC` `ActionQueue` (`fight.c:1458-1590`).
    fn build_action_queue(&mut self) {
        let (enemy_info, hero_actions, hero_roles): (
            Vec<(usize, i32, i32, i32)>,
            Vec<(usize, BattleCmd)>,
            Vec<usize>,
        ) = {
            let b = self.battle.as_ref().unwrap();
            let ei = b
                .enemies
                .iter()
                .enumerate()
                .filter(|(_, e)| e.hp > 0)
                .map(|(i, e)| (i, e.level, e.raw_dexterity, e.dual_move))
                .collect();
            let hr = b.heroes.iter().map(|h| h.role).collect();
            (ei, b.actions.clone(), hr)
        };

        let mut queue: Vec<ActionSlot> = Vec::with_capacity(enemy_info.len() * 2 + hero_actions.len());

        // 1. Enemies (plus optional dual-move second slot, `fight.c:1463-1493`).
        for (i, level, raw_dex, dual_move) in enemy_info {
            let base_dex = sdlpal_enemy_dexterity(level, raw_dex);
            let dex1 = sdlpal_enemy_action_dexterity(base_dex, &mut self.world.rng);
            if dual_move > 0 {
                let dex2 = sdlpal_enemy_action_dexterity(base_dex, &mut self.world.rng);
                if dex2 <= dex1 {
                    queue.push(ActionSlot {
                        actor: ActionActor::Enemy { idx: i, is_second: false },
                        dex: dex1,
                    });
                    queue.push(ActionSlot {
                        actor: ActionActor::Enemy { idx: i, is_second: true },
                        dex: dex2,
                    });
                } else {
                    queue.push(ActionSlot {
                        actor: ActionActor::Enemy { idx: i, is_second: true },
                        dex: dex1,
                    });
                    queue.push(ActionSlot {
                        actor: ActionActor::Enemy { idx: i, is_second: false },
                        dex: dex2,
                    });
                }
            } else {
                queue.push(ActionSlot {
                    actor: ActionActor::Enemy { idx: i, is_second: false },
                    dex: dex1,
                });
            }
        }

        // 2. Heroes (`fight.c:1498-1569`).
        for (h, cmd) in hero_actions {
            let Some(&role) = hero_roles.get(h) else {
                continue;
            };
            let (hp, max_hp, dex_stat, haste, incap) = self
                .world
                .roles
                .get(role)
                .map(|r| {
                    (
                        r.hp,
                        r.max_hp,
                        r.stats[3],
                        r.statuses.get(&STATUS_HASTE).copied().unwrap_or(0) > 0,
                        r.statuses.get(&STATUS_SLEEP).copied().unwrap_or(0) > 0
                            || r.statuses.get(&STATUS_PARALYZED).copied().unwrap_or(0) > 0,
                    )
                })
                .unwrap_or((0, 1, 0, false, true));
            let q_dex = if hp <= 0 || incap {
                0
            } else {
                let kind = self.hero_speed_kind(cmd);
                let dying = sdlpal_is_player_dying(hp, max_hp);
                sdlpal_hero_action_dexterity(dex_stat, haste, kind, dying, &mut self.world.rng)
            };
            queue.push(ActionSlot {
                actor: ActionActor::Hero { idx: h, cmd },
                dex: q_dex,
            });
        }

        sdlpal_sort_action_queue(&mut queue);

        let b = self.battle.as_mut().unwrap();
        b.action_queue = queue;
        b.this_turn_coop = false;
    }

    /// End-of-round cleanup (`fight.c:1602-1708`): reset manual Defend, tick status timers
    /// & poison damage, decrement party hiding timer, and check victory/defeat.
    fn finish_round(&mut self) {
        let hero_roles: Vec<usize> = {
            let b = self.battle.as_mut().unwrap();
            b.is_enemy_turn = false;
            b.this_turn_coop = false;
            for h in &mut b.heroes {
                h.defending = false;
            }
            b.heroes.iter().map(|h| h.role).collect()
        };

        // Tick hero status timers at round end (SDLPAL `fight.c:1632-1638`).
        for rid in hero_roles {
            if let Some(r) = self.world.roles.get_mut(rid) {
                r.statuses.retain(|_, turns| {
                    *turns -= 1;
                    *turns > 0
                });
            }
        }

        let b = self.battle.as_mut().unwrap();
        // Tick enemy status timers and poison damage at round end.
        let mut poison_floats = Vec::new();
        for e in &mut b.enemies {
            if e.hp <= 0 {
                continue;
            }
            e.statuses.retain(|_, turns| {
                *turns -= 1;
                *turns > 0
            });
            if !e.poisons.is_empty() {
                let dmg = (e.max_hp / 20).max(5) * (e.poisons.len() as i32);
                e.hp = (e.hp - dmg).max(0);
                e.flash = 4;
                poison_floats.push((e.x, e.y - 30, format!("-{dmg}")));
            }
        }
        for (x, y, text) in poison_floats {
            b.float(x, y, text, GREEN);
        }
        if b.party_hidden_turns > 0 {
            b.party_hidden_turns -= 1;
        }

        if b.all_enemies_dead() {
            b.outcome = Some(BattleOutcome::Victory);
            self.set_phase(Phase::PostBattle(0));
        } else {
            self.set_phase(Phase::RoundStart);
        }
    }

    /// One state-machine transition. Returns the outcome when the battle has fully finished.
    fn battle_step(&mut self) -> Option<BattleOutcome> {
        let phase = self.battle.as_ref().unwrap().phase;
        match phase {
            Phase::RoundStart => {
                {
                    let b = self.battle.as_mut().unwrap();
                    b.is_enemy_turn = false;
                }
                let b = self.battle.as_ref().unwrap();
                if !b.active {
                    self.set_phase(Phase::Finish);
                } else if b.all_enemies_dead() {
                    self.battle.as_mut().unwrap().outcome = Some(BattleOutcome::Victory);
                    self.set_phase(Phase::PostBattle(0));
                } else if b.heroes.iter().all(|h| !self.role_alive(h.role)) {
                    self.battle.as_mut().unwrap().outcome = Some(BattleOutcome::Defeat);
                    self.set_phase(Phase::Finish);
                } else {
                    let b = self.battle.as_mut().unwrap();
                    b.actions.clear();
                    b.action_queue.clear();
                    b.this_turn_coop = false;
                    b.auto_next = false;
                    self.set_phase(Phase::PreTurn(0));
                }
            }
            Phase::PreTurn(i) => {
                let b = self.battle.as_ref().unwrap();
                if !b.active {
                    self.set_phase(Phase::Finish);
                } else if b.party_hidden_turns > 0 || i >= b.enemies.len() {
                    self.set_phase(Phase::Command(0));
                } else {
                    let (alive, pos) = (b.enemies[i].hp > 0, b.enemies[i].script_pre_turn);
                    self.battle.as_mut().unwrap().current_target_enemy = i;
                    self.set_phase(Phase::PreTurn(i + 1));
                    if alive && !pos.is_none() {
                        // Persist resume offset via `Caller::EnemyPreTurn(i)` so multi-turn boss
                        // scripts (`pause_yield` / `replace_and_yield`, #212) advance cleanly.
                        self.request_script_pos(pos, super::super::vm::Caller::EnemyPreTurn(i));
                    }
                }
            }
            Phase::Command(h) => {
                let b = self.battle.as_ref().unwrap();
                if h >= b.heroes.len() {
                    self.build_action_queue();
                    self.set_phase(Phase::ActQueue(0, 0));
                    return None;
                }
                let role = b.heroes[h].role;
                if let Some(cmd) = self.battle.as_mut().unwrap().pending_cmd.take() {
                    return self.battle_accept_cmd(h, cmd);
                }
                let b = self.battle.as_mut().unwrap();
                b.active_hero = h;
                b.heroes[h].defending = false;
                let (hp, has_puppet, incapacitated, confused) = self
                    .world
                    .roles
                    .get(role)
                    .map(|r| {
                        (
                            r.hp,
                            r.statuses.get(&STATUS_PUPPET).copied().unwrap_or(0) > 0,
                            r.statuses.get(&STATUS_SLEEP).copied().unwrap_or(0) > 0
                                || r.statuses.get(&STATUS_PARALYZED).copied().unwrap_or(0) > 0,
                            r.statuses.get(&STATUS_CONFUSED).copied().unwrap_or(0) > 0,
                        )
                    })
                    .unwrap_or((0, false, false, false));
                if hp <= 0 && !has_puppet {
                    self.set_phase(Phase::Command(h + 1));
                } else if incapacitated {
                    self.set_phase(Phase::Command(h + 1));
                } else if (hp <= 0 && has_puppet) || confused || b.auto_repeat || b.auto_next {
                    let t = b.first_alive_enemy().unwrap_or(0);
                    b.actions.push((h, BattleCmd::Attack { target: t }));
                    self.set_phase(Phase::Command(h + 1));
                } else {
                    b.prompting = Some(h);
                    self.emit(HostCmd::BattlePrompt { hero: h });
                }
            }
            Phase::ActQueue(qi, stage) => {
                let b = self.battle.as_ref().unwrap();
                if !b.active {
                    self.set_phase(Phase::Finish);
                    return None;
                }
                if b.all_enemies_dead() {
                    self.battle.as_mut().unwrap().outcome = Some(BattleOutcome::Victory);
                    self.set_phase(Phase::PostBattle(0));
                    return None;
                }
                if b.heroes.iter().all(|h| {
                    let r = self.world.roles.get(h.role);
                    let hp = r.map(|x| x.hp).unwrap_or(0);
                    let puppet = r
                        .and_then(|x| x.statuses.get(&STATUS_PUPPET).copied())
                        .unwrap_or(0)
                        > 0;
                    hp <= 0 && !puppet
                }) {
                    self.battle.as_mut().unwrap().outcome = Some(BattleOutcome::Defeat);
                    self.set_phase(Phase::Finish);
                    return None;
                }
                if qi >= b.action_queue.len() {
                    self.finish_round();
                    return None;
                }
                let actor = b.action_queue[qi].actor;
                match actor {
                    ActionActor::Hero { idx: h, cmd } => {
                        self.battle.as_mut().unwrap().is_enemy_turn = false;
                        let this_turn_coop = self.battle.as_ref().unwrap().this_turn_coop;
                        if this_turn_coop && !matches!(cmd, BattleCmd::Coop { .. }) {
                            self.set_phase(Phase::ActQueue(qi + 1, 0));
                            return None;
                        }
                        let role = self.battle.as_ref().unwrap().heroes[h].role;
                        if stage == 0 {
                            let (alive, puppet, incap) = self
                                .world
                                .roles
                                .get(role)
                                .map(|r| {
                                    (
                                        r.hp > 0,
                                        r.statuses.get(&STATUS_PUPPET).copied().unwrap_or(0) > 0,
                                        r.statuses.get(&STATUS_SLEEP).copied().unwrap_or(0) > 0
                                            || r.statuses.get(&STATUS_PARALYZED).copied().unwrap_or(0) > 0,
                                    )
                                })
                                .unwrap_or((false, false, true));
                            if (!alive && !puppet) || incap {
                                self.set_phase(Phase::ActQueue(qi + 1, 0));
                                return None;
                            }
                            if matches!(cmd, BattleCmd::Coop { .. }) {
                                self.battle.as_mut().unwrap().this_turn_coop = true;
                            }
                            if matches!(cmd, BattleCmd::Flee) {
                                self.emit(HostCmd::PlaySfx(45));
                                let flee_rate = self.world.roles.get(role).map(|r| r.stats[4]).unwrap_or(0);
                                let (can_flee, has_boss, enemy_flee_defs) = {
                                    let b = self.battle.as_ref().unwrap();
                                    let defs: Vec<i32> = b
                                        .enemies
                                        .iter()
                                        .filter(|e| e.hp > 0)
                                        .map(|e| sdlpal_enemy_flee_def(e.level, e.raw_dexterity))
                                        .collect();
                                    let boss = b.enemies.iter().any(|e| e.hp > 0 && e.is_boss);
                                    (b.can_flee, boss, defs)
                                };
                                if sdlpal_flee_check(
                                    flee_rate,
                                    &enemy_flee_defs,
                                    can_flee,
                                    has_boss,
                                    &mut self.world.rng,
                                ) {
                                    let b = self.battle.as_mut().unwrap();
                                    b.outcome = Some(BattleOutcome::Flee);
                                    b.active = false;
                                    self.toast("成功脱离战场！", "成功脫離戰場！");
                                    self.sleep_ms(450);
                                    self.set_phase(Phase::Finish);
                                    return None;
                                }
                                self.toast("逃跑失败！", "逃跑失敗！");
                                self.sleep_ms(400);
                                self.set_phase(Phase::ActQueue(qi + 1, 0));
                                return None;
                            }
                        }
                        let next = self.battle_hero_action(h, cmd, stage);
                        self.set_phase(match next {
                            Some(s) => Phase::ActQueue(qi, s),
                            None => Phase::ActQueue(qi + 1, 0),
                        });
                    }
                    ActionActor::Enemy { idx: i, is_second } => {
                        self.step_enemy_action(qi, i, is_second, stage);
                    }
                }
            }
            Phase::PostBattle(i) => {
                let b = self.battle.as_ref().unwrap();
                if i >= b.enemies.len() {
                    self.set_phase(Phase::Rewards);
                } else {
                    let l = b.enemies[i].script_post_battle;
                    self.set_phase(Phase::PostBattle(i + 1));
                    self.request_script(l);
                }
            }
            Phase::Rewards => {
                let b = self.battle.as_ref().unwrap();
                let total_exp: i32 = b.enemies.iter().map(|e| e.exp.max(0)).sum();
                let total_cash: i32 = b.enemies.iter().map(|e| e.cash.max(0)).sum();
                let heroes: Vec<usize> = b.heroes.iter().map(|h| h.role).collect();
                self.world.cash = self.world.cash.saturating_add(total_cash.max(0) as u32);
                let mut level_ups = Vec::new();
                for role in heroes {
                    if !self.role_alive(role) {
                        continue;
                    }
                    self.world.roles[role].exp += total_exp;
                    let mut gained = 0;
                    let mut learned = Vec::new();
                    loop {
                        let lv = self.world.roles[role].level;
                        let need = self
                            .data
                            .levelup_exp
                            .get(lv.max(0) as usize)
                            .copied()
                            .filter(|&v| v > 0)
                            .unwrap_or(lv * 45 + 25);
                        if lv >= 99 || self.world.roles[role].exp < need {
                            break;
                        }
                        self.world.roles[role].exp -= need;
                        learned.extend(self.world.apply_level_up(&self.data, role, 1, true));
                        gained += 1;
                    }
                    if gained > 0 {
                        let r = &self.world.roles[role];
                        let nm = self.world.t(&r.name, &r.name_hant).to_string();
                        let suffix = if learned.is_empty() {
                            String::new()
                        } else {
                            format!(" 習得{}", learned.join("、"))
                        };
                        level_ups.push(format!("{nm} Lv.{}{suffix}", r.level));
                    }
                }
                self.emit(HostCmd::BattleVictory { exp: total_exp, cash: total_cash, level_ups });
                self.set_phase(Phase::AwaitVictoryAck);
            }
            Phase::AwaitVictoryAck => {
                if std::mem::take(&mut self.battle.as_mut().unwrap().victory_ack) {
                    self.set_phase(Phase::Finish);
                } else {
                    self.battle.as_mut().unwrap().sleep = 1;
                }
            }
            Phase::Finish => {
                let b = self.battle.take().unwrap();
                let outcome = b.outcome.unwrap_or(BattleOutcome::Victory);
                for h in &b.heroes {
                    if let Some(r) = self.world.roles.get_mut(h.role) {
                        r.statuses.clear();
                        // SDLPAL: revive fallen party members to 1 HP on victory or flee (#191).
                        if matches!(outcome, BattleOutcome::Victory | BattleOutcome::Flee) && r.hp <= 0 {
                            r.hp = 1;
                        }
                    }
                }
                self.last_battle_heroes = b.heroes.iter().map(|h| h.role).collect();
                self.emit(HostCmd::BattleEnded);
                return Some(outcome);
            }
        }
        None
    }

    /// Execute one enemy slot from the interleaved `ActionQueue` (`fight.c:4551-5149`).
    fn step_enemy_action(&mut self, qi: usize, i: usize, is_second: bool, stage: u8) {
        self.battle.as_mut().unwrap().is_enemy_turn = true;
        let b = self.battle.as_ref().unwrap();
        if i >= b.enemies.len() || b.enemies[i].hp <= 0 {
            self.set_phase(Phase::ActQueue(qi + 1, 0));
            return;
        }
        let e_incap = b.enemies[i].statuses.get(&STATUS_SLEEP).copied().unwrap_or(0) > 0
            || b.enemies[i].statuses.get(&STATUS_PARALYZED).copied().unwrap_or(0) > 0;
        if e_incap {
            self.set_phase(Phase::ActQueue(qi + 1, 0));
            return;
        }

        match stage {
            0 => {
                if b.party_hidden_turns > 0 {
                    let (ex, ey) = (b.enemies[i].x, b.enemies[i].y);
                    let miss = self.world.t3("未发现目标", "未發現目標", "Hidden").to_string();
                    self.battle.as_mut().unwrap().float(ex, ey - 30, miss, CYAN);
                    self.sleep_ms(220);
                    self.set_phase(Phase::ActQueue(qi + 1, 0));
                    return;
                }
                let pos = b.enemies[i].script_in_turn;
                self.battle.as_mut().unwrap().current_target_enemy = i;
                self.set_phase(Phase::ActQueue(qi, 1));
                if !is_second && !pos.is_none() {
                    self.request_script_pos(pos, super::super::vm::Caller::EnemyInTurn(i));
                }
            }
            1 => {
                if b.party_hidden_turns > 0 {
                    let (ex, ey) = (b.enemies[i].x, b.enemies[i].y);
                    let miss = self.world.t3("未发现目标", "未發現目標", "Hidden").to_string();
                    self.battle.as_mut().unwrap().float(ex, ey - 30, miss, CYAN);
                    self.sleep_ms(220);
                    self.set_phase(Phase::ActQueue(qi + 1, 0));
                    return;
                }
                let alive: Vec<usize> = (0..b.heroes.len())
                    .filter(|&h| self.role_alive(b.heroes[h].role))
                    .collect();
                if alive.is_empty() {
                    self.set_phase(Phase::ActQueue(qi + 1, 0));
                    return;
                }
                let pick = alive[self.world.rng.below(alive.len() as u32) as usize];
                let b = self.battle.as_mut().unwrap();
                b.current_target_enemy = i;
                let (ex, ey) = (b.enemies[i].x, b.enemies[i].y);
                let (hx, hy) = (b.heroes[pick].x, b.heroes[pick].y);
                b.enemies[i].lunge = (((hx - ex) / 4).clamp(16, 42), ((hy - ey) / 4).clamp(8, 24));
                b.enemies[i].pose = EnemyPose::Attack;
                b.enemies[i].pose_ticks = 14;
                b.enemy_target = pick;
                let sfx = if b.enemies[i].sfx_attack > 0 { b.enemies[i].sfx_attack } else { 9 };
                self.emit(HostCmd::PlaySfx(sfx));
                self.sleep_ms(170);
                self.set_phase(Phase::ActQueue(qi, 2));
            }
            _ => {
                let silenced = self.battle.as_ref().unwrap().enemies[i]
                    .statuses
                    .get(&STATUS_SILENCE)
                    .copied()
                    .unwrap_or(0)
                    > 0;
                let (mut selected_magic, innate_magic, innate_rate, atk, mag_pow, equiv_item, equiv_rate) = {
                    let e = &mut self.battle.as_mut().unwrap().enemies[i];
                    (
                        std::mem::take(&mut e.selected_magic),
                        e.magic_id,
                        e.magic_rate,
                        e.attack,
                        e.magic_power,
                        e.attack_equiv_item,
                        e.attack_equiv_rate,
                    )
                };
                if selected_magic == 0
                    && !silenced
                    && innate_magic > 0
                    && innate_magic != 0xFFFF
                    && innate_rate > 0
                    && self.world.rng.random_long(0, 9) < innate_rate
                {
                    selected_magic = innate_magic;
                }
                let pick = {
                    let b = self.battle.as_mut().unwrap();
                    b.enemies[i].lunge = (0, 0);
                    b.enemy_target
                };
                let pick = if self
                    .battle
                    .as_ref()
                    .unwrap()
                    .heroes
                    .get(pick)
                    .map(|hh| self.role_alive(hh.role))
                    .unwrap_or(false)
                {
                    Some(pick)
                } else {
                    (0..self.battle.as_ref().unwrap().heroes.len())
                        .find(|&h| self.role_alive(self.battle.as_ref().unwrap().heroes[h].role))
                };

                if let Some(pick) = pick {
                    let field_elem = self.battle.as_ref().unwrap().field_elem;
                    if selected_magic != 0 && !silenced {
                        if let Some(m) = self.data.magics.get(&selected_magic).cloned() {
                            let sfx = if m.sfx_id != 0 {
                                m.sfx_id as i32
                            } else {
                                self.battle.as_ref().unwrap().enemies[i].sfx_magic.max(12)
                            };
                            self.emit(HostCmd::PlaySfx(sfx));
                            let (eid, ename_hans, ename_hant) = {
                                let e = &self.battle.as_ref().unwrap().enemies[i];
                                (e.enemy_id, e.name.clone(), e.name_hant.clone())
                            };
                            let ename = self.enemy_display_name(eid, &ename_hans, &ename_hant);
                            let mname = self.magic_display_name(selected_magic);
                            {
                                let b = self.battle.as_mut().unwrap();
                                b.enemies[i].pose = EnemyPose::Magic;
                                b.enemies[i].pose_ticks = 12;
                                b.set_banner(format!("enemy#{i}:{ename}"), "magic", mname, 48);
                                if m.fire_effect_id > 0 {
                                    let (fx, fy) = if m.target_all {
                                        (240, 146)
                                    } else {
                                        (b.heroes[pick].x, b.heroes[pick].y)
                                    };
                                    b.fire = Some(FireAnim {
                                        fire_id: m.fire_effect_id as u16,
                                        frame: 0,
                                        x: fx,
                                        y: fy,
                                        target_all: m.target_all,
                                        target_enemy: false,
                                        is_summon: m.magic_type == 9,
                                    });
                                    b.enemy_fire_ticks = 10;
                                    b.dim_bg = true;
                                }
                            }
                            let base = if m.base_damage != 0 { m.base_damage } else { 35 };
                            let field_eff = if (1..=5).contains(&m.elemental) {
                                field_elem[(m.elemental - 1) as usize]
                            } else {
                                0
                            };
                            let hero_targets: Vec<usize> = if m.target_all {
                                let b = self.battle.as_ref().unwrap();
                                (0..b.heroes.len())
                                    .filter(|&h| self.role_alive(b.heroes[h].role))
                                    .collect()
                            } else {
                                vec![pick]
                            };
                            for hidx in hero_targets {
                                let (role, hx, hy, defending) = {
                                    let hh = &self.battle.as_ref().unwrap().heroes[hidx];
                                    (hh.role, hh.x, hh.y, hh.defending)
                                };
                                let r = &self.world.roles[role];
                                let protect = r.statuses.get(&STATUS_PROTECT).copied().unwrap_or(0) > 0;
                                let incap = r.statuses.get(&STATUS_SLEEP).copied().unwrap_or(0) > 0
                                    || r.statuses.get(&STATUS_PARALYZED).copied().unwrap_or(0) > 0
                                    || r.statuses.get(&STATUS_CONFUSED).copied().unwrap_or(0) > 0;
                                let hero_def = r.stats[2];
                                let elem_resist = if (1..=5).contains(&m.elemental) {
                                    r.elem_resist((m.elemental - 1) as usize)
                                } else {
                                    0
                                };
                                let poison_resist = r.poison_resist();
                                // SDLPAL `fight.c:4734/4753`: 1/3 chance to auto-defend enemy magic.
                                let auto_defend = !incap && self.world.rng.random_long(0, 2) == 0;
                                let dmg = sdlpal_enemy_magic_hit(
                                    mag_pow,
                                    base,
                                    m.elemental,
                                    hero_def,
                                    elem_resist,
                                    poison_resist,
                                    field_eff,
                                    defending,
                                    protect,
                                    auto_defend,
                                    &mut self.world.rng,
                                );
                                let r = &mut self.world.roles[role];
                                r.hp = (r.hp - dmg).max(0);
                                let b = self.battle.as_mut().unwrap();
                                b.heroes[hidx].flash = 6;
                                b.heroes[hidx].pose = if auto_defend { HeroPose::Defend } else { HeroPose::Hurt };
                                b.heroes[hidx].pose_ticks = 10;
                                b.float(hx, hy - 30, format!("-{dmg}"), HERO_HIT);
                            }
                        }
                    } else {
                        // Verbatim SDLPAL enemy physical attack (`fight.c:4917-5147`):
                        // Defend doubles DEF; Protect halves post-calc damage; auto-defend evades.
                        let (role, hx, hy, defending) = {
                            let hh = &self.battle.as_ref().unwrap().heroes[pick];
                            (hh.role, hh.x, hh.y, hh.defending)
                        };
                        let (hero_def, protect, incap) = {
                            let r = &self.world.roles[role];
                            (
                                r.stats[2],
                                r.statuses.get(&STATUS_PROTECT).copied().unwrap_or(0) > 0,
                                r.statuses.get(&STATUS_SLEEP).copied().unwrap_or(0) > 0
                                    || r.statuses.get(&STATUS_PARALYZED).copied().unwrap_or(0) > 0
                                    || r.statuses.get(&STATUS_CONFUSED).copied().unwrap_or(0) > 0,
                            )
                        };
                        let hit = sdlpal_enemy_physical_hit(
                            atk,
                            hero_def,
                            defending,
                            protect,
                            incap,
                            &mut self.world.rng,
                        );
                        match hit {
                            None => {
                                let label = self.world.t3("格挡", "格擋", "Block").to_string();
                                let b = self.battle.as_mut().unwrap();
                                b.heroes[pick].pose = HeroPose::Defend;
                                b.heroes[pick].pose_ticks = 10;
                                b.cur_actor = format!("enemy#{i}:{}", b.enemies[i].name);
                                b.cur_action = "attack".to_string();
                                b.cur_skill.clear();
                                b.float(hx, hy - 30, label, CYAN);
                            }
                            Some(dmg) => {
                                let p_res = {
                                    let r = &mut self.world.roles[role];
                                    r.hp = (r.hp - dmg).max(0);
                                    r.statuses.remove(&STATUS_SLEEP);
                                    r.poison_resist()
                                };
                                // Verbatim SDLPAL `fight.c:5140-5141`:
                                // `wAttackEquivItemRate >= RandomLong(1, 10) && poison_resist < RandomLong(1, 100)`
                                if equiv_item > 0
                                    && equiv_rate > 0
                                    && equiv_rate >= self.world.rng.random_long(1, 10)
                                    && p_res < self.world.rng.random_long(1, 100)
                                {
                                    let r = &mut self.world.roles[role];
                                    if !r.poisons.contains(&equiv_item) {
                                        r.poisons.push(equiv_item);
                                    }
                                }
                                let b = self.battle.as_mut().unwrap();
                                b.heroes[pick].flash = 6;
                                b.heroes[pick].pose = HeroPose::Hurt;
                                b.heroes[pick].pose_ticks = 10;
                                b.cur_actor = format!("enemy#{i}:{}", b.enemies[i].name);
                                b.cur_action = "attack".to_string();
                                b.cur_skill.clear();
                                b.float(hx, hy - 30, format!("-{dmg}"), HERO_HIT);
                            }
                        }
                    }
                }
                self.sleep_ms(260);
                self.set_phase(Phase::ActQueue(qi + 1, 0));
            }
        }
    }

    fn battle_accept_cmd(&mut self, h: usize, cmd: BattleCmd) -> Option<BattleOutcome> {
        let b = self.battle.as_mut().unwrap();
        match cmd {
            BattleCmd::Auto => {
                b.auto_repeat = true;
                let t = b.first_alive_enemy().unwrap_or(0);
                b.actions.push((h, BattleCmd::Attack { target: t }));
            }
            BattleCmd::Flee => {
                // SDLPAL `PAL_CLASSIC` (`fight.c:1546-1548`): Flee queues with `wDexterity /= 2`
                // and resolves in dexterity turn order rather than instantaneously.
                b.actions.push((h, BattleCmd::Flee));
            }
            BattleCmd::Coop { target } => {
                // Coop consumes MP and commits the remaining party turns for this round (#188).
                let role = b.heroes[h].role;
                let coop_id = self.world.roles.get(role).map(|r| r.coop_magic).unwrap_or(0);
                let cost_mp = self
                    .data
                    .magics
                    .get(&coop_id)
                    .map(|m| m.cost_mp.max(15))
                    .unwrap_or(15);
                let alive_count = b
                    .heroes
                    .iter()
                    .filter(|hh| self.world.roles.get(hh.role).map(|r| r.hp > 0).unwrap_or(false))
                    .count();
                let cur_mp = self.world.roles.get(role).map(|r| r.mp).unwrap_or(0);
                if alive_count >= 2 && coop_id != 0 && self.data.magics.contains_key(&coop_id) && cur_mp >= cost_mp {
                    b.actions.push((h, BattleCmd::Coop { target }));
                    self.build_action_queue();
                    self.set_phase(Phase::ActQueue(0, 0));
                    return None;
                }
                if cur_mp < cost_mp {
                    self.toast("真气不足！", "真氣不足！");
                }
                let b = self.battle.as_mut().unwrap();
                b.actions.push((h, BattleCmd::Attack { target }));
            }
            c => b.actions.push((h, c)),
        }
        self.set_phase(Phase::Command(h + 1));
        None
    }
}
