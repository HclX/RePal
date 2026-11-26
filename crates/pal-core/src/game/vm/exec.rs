//! VM execution engine: frame runner, party/NPC walking, target resolution, and auto-script ticking.

use super::*;

impl Game {
    /// One iteration of legacy `walkPartyDirectly`. Returns `true` when the walk is over.
    pub(super) fn walk_party_iteration(&mut self, gx: i32, gy: i32, ride: u16, guard: &mut u32) -> bool {
        *guard += 1;
        if *guard > 256 {
            self.world.leader_mut().frame = 0;
            return true;
        }
        let lead = self.world.leader().clone();
        let dx = gx - lead.x;
        let dy = gy - lead.y;
        let (snap_dx, snap_dy) = if ride > 0 { (4, 2) } else { (12, 6) };
        if dx.abs() <= snap_dx && dy.abs() <= snap_dy {
            if ride > 0 {
                let step_dx = gx - lead.x;
                let step_dy = gy - lead.y;
                for m in &mut self.world.party {
                    m.x += step_dx;
                    m.y += step_dy;
                    m.dir = lead.dir;
                    m.frame = 0;
                }
                if let Some(e) = self.world.event_mut(ride) {
                    e.x += step_dx;
                    e.y += step_dy;
                }
            } else {
                self.world.step_party_to(gx, gy, lead.dir);
            }
            self.world.leader_mut().frame = 0;
            return true;
        }
        let dir = direction_between(lead.x, lead.y, gx, gy);
        let (max_dx, max_dy) = if ride > 0 { (4, 2) } else { (16, 8) };
        let step_dx = dx.signum() * dx.abs().min(max_dx);
        let step_dy = dy.signum() * dy.abs().min(max_dy);
        let sx = lead.x + step_dx;
        let sy = lead.y + step_dy;
        if ride > 0 {
            for m in &mut self.world.party {
                m.x += step_dx;
                m.y += step_dy;
                m.dir = dir;
                m.frame = 0;
            }
            if let Some(e) = self.world.event_mut(ride) {
                e.x += step_dx;
                e.y += step_dy;
            }
        } else {
            self.world.step_party_to(sx, sy, dir);
        }
        false
    }

    /// One `npcAdvanceWalk` step. Returns legacy status (`0 moved, 1 done, 2 stalled`).
    pub(super) fn npc_walk_step(&mut self, ev_id: u16, gx: i32, gy: i32, spd: i32, leg_key: u64) -> i32 {
        let Some(ev) = self.world.event(ev_id) else { return 1 };
        let (x, y, solid) = (ev.x, ev.y, ev.state >= 2);
        let tracker = self.npc_walks.entry(ev_id).or_default();
        let out = npc_advance_walk(&self.grid, tracker, x, y, gx, gy, spd, solid, leg_key);
        let ev = self.world.event_mut(ev_id).unwrap();
        match out {
            NpcWalkOutcome::Done { x, y } => {
                ev.x = x;
                ev.y = y;
                ev.current_frame = 0;
                1
            }
            NpcWalkOutcome::Moved { x, y, dir } => {
                ev.direction = dir as i32;
                ev.x = x;
                ev.y = y;
                ev.advance_frame(3);
                0
            }
            NpcWalkOutcome::Stalled { dir } => {
                ev.direction = dir as i32;
                ev.advance_frame(3);
                2
            }
        }
    }

    // =====================================================================================
    // Chain execution
    // =====================================================================================

    pub(super) fn update_caller(&mut self, caller: Caller, pos: ScriptPos) {
        match caller {
            Caller::None => {}
            Caller::Trigger(id) => {
                if let Some(e) = self.world.event_mut(id) {
                    e.trigger = pos;
                }
            }
            Caller::Auto(id) => {
                if let Some(e) = self.world.event_mut(id) {
                    e.auto = pos;
                }
            }
            Caller::SceneEnter(sid) => {
                if let Some(s) = self.world.scenes.get_mut(&sid) {
                    s.enter = pos;
                }
            }
            Caller::EnemyPreTurn(idx) => {
                if let Some(e) = self.battle.as_mut().and_then(|b| b.enemies.get_mut(idx)) {
                    e.script_pre_turn = pos;
                }
            }
            Caller::EnemyInTurn(idx) => {
                if let Some(e) = self.battle.as_mut().and_then(|b| b.enemies.get_mut(idx)) {
                    e.script_in_turn = pos;
                }
            }
            Caller::MagicSuccess(magic_id) => {
                self.world.magic_script_pos.insert(magic_id, pos);
            }
        }
    }

    pub(super) fn persist_auto_ip(&mut self, fr: &Frame) {
        if fr.auto && fr.pos != fr.start {
            self.update_caller(fr.caller, fr.pos);
        }
    }

    /// Run a background chain to completion (legacy `execChain(..., isAutoTick=true)`).
    pub(super) const MAX_CALL_DEPTH: usize = 32;

    pub(crate) fn exec_chain_sync(&mut self, start: ScriptPos, event_id: u16, caller: Caller) -> bool {
        let mut fr = Frame::new(start, event_id, caller, true);
        loop {
            match self.run_frame(&mut fr) {
                FrameExit::End(r) => return r,
                FrameExit::Call(mut sub) => {
                    sub.auto = true;
                    sub.max_ops = 32;
                    let r = self.exec_sub_sync(sub, 1);
                    let _ = r; // background sub-chain results never abort the parent
                    fr.pos.idx += 1;
                }
                FrameExit::Wait(_) => return true,
            }
        }
    }

    fn exec_sub_sync(&mut self, mut fr: Frame, depth: usize) -> bool {
        if depth >= Self::MAX_CALL_DEPTH {
            return true;
        }
        loop {
            match self.run_frame(&mut fr) {
                FrameExit::End(r) => return r,
                FrameExit::Call(mut sub) => {
                    sub.auto = true;
                    sub.max_ops = 32;
                    self.exec_sub_sync(sub, depth + 1);
                    fr.pos.idx += 1;
                }
                FrameExit::Wait(_) => return true,
            }
        }
    }

    /// Execute instructions of one frame until it ends, calls a sub-chain or must wait.
    pub(super) fn run_frame(&mut self, fr: &mut Frame) -> FrameExit {
        loop {
            if fr.pos.label == NO_LABEL {
                break;
            }
            if fr.guard >= fr.max_ops {
                if let Some(cov) = self.coverage.as_mut() {
                    cov.guard_trips += 1;
                }
                break;
            }
            fr.guard += 1;
            if !self.prog.has_block(fr.pos.label) {
                let name = self.prog.label_name(fr.pos.label).to_string();
                match Program::chapter_for_label(&name) {
                    Some(ch) if !self.prog.is_chapter_loaded(ch) => {
                        self.emit(HostCmd::LoadChapter(ch));
                        if fr.auto {
                            return FrameExit::End(true);
                        }
                        fr.guard -= 1;
                        return FrameExit::Wait(Wait::Chapter(ch));
                    }
                    _ => {}
                }
                match self.prog.resolve_entry(&name) {
                    Some(p) => fr.pos = p,
                    None => break,
                }
            }
            let Some(ins) = self.prog.instr(fr.pos).cloned() else { break };
            if let Some(cov) = self.coverage.as_mut() {
                *cov.steps.entry((fr.pos.label, fr.pos.idx)).or_insert(0) += 1;
                *cov.opcodes.entry(ins.op).or_insert(0) += 1;
            }
            if !fr.auto {
                self.vmc.last_label = fr.pos.label;
                if self.trace.len() >= 35 {
                    self.trace.pop_front();
                }
                self.trace.push_back(format!("{}[{}]:{}", self.prog.label_name(fr.pos.label), fr.pos.idx, opcode_name(ins.op)));
            }
            match self.step(fr, &ins) {
                Flow::Next => fr.pos.idx += 1,
                Flow::Goto(p) => fr.pos = p,
                Flow::End(r) => return FrameExit::End(r),
                Flow::Call(sub) => return FrameExit::Call(sub),
                Flow::Wait(w) => return FrameExit::Wait(w),
            }
        }
        self.persist_auto_ip(fr);
        FrameExit::End(true)
    }

    // =====================================================================================
    // Helpers mirroring legacy ScriptVM methods
    // =====================================================================================

    /// Legacy `resolveEvent`: `0`, `-1` and `0xFFFF` mean "the current event".
    pub(super) fn resolve_event(&self, raw: i32, cur: u16) -> Option<u16> {
        let id = if raw == 0 || raw == -1 || raw == 0xFFFF { cur as i32 } else { raw };
        if id <= 0 || id > u16::MAX as i32 {
            return None;
        }
        self.world.event(id as u16).map(|e| e.id)
    }

    pub(super) fn battle_active(&self) -> bool {
        self.battle.as_ref().map(|b| b.active).unwrap_or(false)
    }

    /// Legacy `getTargetRoles(applyAll, rawRole)`.
    pub(super) fn target_roles(&self, apply_all: bool, raw_role: i32) -> Vec<usize> {
        if apply_all {
            return self.world.party_role_ids().into_iter().filter(|&r| r < self.world.roles.len()).collect();
        }
        let idx = if raw_role == 0 {
            if self.vmc.using_item || self.battle_active() {
                self.vmc.current_target_role as i64
            } else {
                self.world.leader_role() as i64
            }
        } else {
            raw_role as i64 - 1
        };
        if idx >= 0 && (idx as usize) < self.world.roles.len() {
            vec![idx as usize]
        } else {
            Vec::new()
        }
    }

    /// Role targeted by single-target item/field/battle-magic ops (`usingItem || battle ? target : leader`).
    pub(super) fn item_or_leader_role(&self) -> usize {
        if self.vmc.using_item || self.battle_active() {
            self.vmc.current_target_role
        } else {
            self.world.leader_role() as usize
        }
    }

    pub(super) fn role_from_raw(&self, raw_role: i32) -> usize {
        if raw_role == 0 {
            self.item_or_leader_role()
        } else {
            (raw_role - 1).max(0) as usize
        }
    }

    pub(super) fn target_enemies(&self, apply_all: bool) -> Vec<usize> {
        let Some(b) = &self.battle else { return Vec::new() };
        if apply_all {
            return (0..b.enemies.len()).filter(|&i| b.enemies[i].hp > 0).collect();
        }
        let idx = b.current_target_enemy;
        if b.enemies.get(idx).map(|e| e.hp > 0).unwrap_or(false) {
            vec![idx]
        } else {
            b.enemies.iter().position(|e| e.hp > 0).into_iter().collect()
        }
    }

    pub(crate) fn toast(&mut self, hans: &str, hant: &str, en: &str) {
        let s = self.world.t3(hans, hant, en).to_string();
        self.emit(HostCmd::Toast(s));
    }

    pub(super) fn apply_role_heal(&mut self, apply_all: bool, hp: i32, mp: i32) {
        let targets: Vec<usize> = if apply_all {
            self.world.party_role_ids()
        } else {
            vec![self.item_or_leader_role()]
        };
        let (mut changed, mut hp_gain, mut mp_gain) = (false, 0, 0);
        for rid in targets {
            let Some(r) = self.world.roles.get_mut(rid) else { continue };
            if hp != 0 {
                if hp > 0 && r.hp <= 0 {
                    // Non-revive HP heal (`0x001B`) does not resurrect KO'd party members.
                } else {
                    let prev = r.hp;
                    r.hp = (r.hp + hp).max(0).min(r.max_hp);
                    if r.hp != prev {
                        changed = true;
                        let delta = r.hp - prev;
                        hp_gain += delta;
                        if delta > 0 {
                            if let Some(b) = self.battle.as_mut() {
                                if let Some((hx, hy)) = b.heroes.iter().find(|x| x.role == rid).map(|x| (x.x, x.y)) {
                                    b.float(hx, hy - 30, format!("+{delta}"), [110, 240, 120]);
                                }
                            }
                        }
                    }
                }
            }
            if mp != 0 {
                let prev = r.mp;
                r.mp = (r.mp + mp).max(0).min(r.max_mp);
                if r.mp != prev {
                    changed = true;
                    mp_gain += r.mp - prev;
                }
            }
        }
        if self.vmc.using_item && (hp > 0 || mp > 0) {
            if !changed {
                self.vmc.item_script_failed = true;
                self.toast("体力真气已满，无需使用", "體力真氣已滿，無需使用", "HP and MP are already full");
            } else {
                self.emit(HostCmd::PlaySfx(138));
                let mut parts = Vec::new();
                if hp_gain > 0 {
                    parts.push(format!("{} +{hp_gain}", self.world.t3("体力", "體力", "HP")));
                }
                if mp_gain > 0 {
                    parts.push(format!("{} +{mp_gain}", self.world.t3("真气", "真氣", "MP")));
                }
                if !parts.is_empty() {
                    self.emit(HostCmd::Toast(parts.join("  ")));
                }
            }
        }
    }

    pub(super) fn apply_magic_effect(&mut self, magic_id: u16, is_throw: bool, dmg_override: Option<i32>) {
        let magic = self.data.magics.get(&magic_id).cloned();
        let base = dmg_override.unwrap_or_else(|| {
            let b = magic.as_ref().map(|m| m.base_damage).unwrap_or(0);
            if b != 0 { b.abs() } else { 35 }
        });
        if let Some(m) = &magic {
            if !m.target_enemy && !is_throw {
                self.apply_role_heal(m.target_all, base, 0);
                return;
            }
        }
        let targets = self.target_enemies(magic.as_ref().map(|m| m.target_all).unwrap_or(false));
        let caster = self.target_roles(false, 0).first().copied();
        let mag_pow = caster
            .and_then(|c| self.world.roles.get(c))
            .map(|r| r.stats[1])
            .filter(|&p| p != 0)
            .unwrap_or(20);
        let mut death_sfx = 0;
        if let Some(b) = &mut self.battle {
            for i in targets {
                let en = &mut b.enemies[i];
                let was_alive = en.hp > 0;
                let def = if en.defense != 0 { en.defense } else { 10 };
                let dmg = (base + (mag_pow as f64 * 1.2).floor() as i32 - (def as f64 * 0.4).floor() as i32).max(1);
                en.hp = (en.hp - dmg).max(0);
                en.flash = 6;
                en.pose = super::super::battle::EnemyPose::Hurt;
                en.pose_ticks = 8;
                if was_alive && en.hp == 0 && en.sfx_death > 0 {
                    death_sfx = en.sfx_death;
                }
                let (ex, ey) = (en.x, en.y);
                b.float(ex, ey - 30, format!("-{dmg}"), [0xff, 0xd1, 0x66]);
            }
        }
        if death_sfx > 0 {
            self.emit(HostCmd::PlaySfx(death_sfx));
        }
    }

    pub(super) fn counter_bump(&mut self, key: CounterKey) -> i32 {
        let c = self.world.script_counters.entry(key).or_insert(0);
        *c += 1;
        *c
    }

    // =====================================================================================

    /// SDLPAL `0x004C` (`PAL_MonsterChasePlayer`): a[0]=chase range (default 8), a[1]=speed (default 4), a[2]=floating.
    pub(super) fn monster_chase_step(&mut self, id: u16, ins: &Instr) {
        self.sync_obstacles(id);
        let a = ins.a;
        let lead = self.world.leader().clone();
        let range = if a[0] > 0 { a[0] } else { 8 };
        let floating = a[2] != 0;
        let non_chasing = ins.argc >= 1 && a[0] == 0;
        let mult = if self.world.chase_alert_cycles > 0 {
            self.world.chase_alert_range.max(1)
        } else {
            1
        };
        let eff = range.saturating_mul(mult).min(128);
        let disabled = self.world.chase_disabled_ticks > 0;
        // Same derivation as legacy JS: `((Math.random() * 0x7fffffff) | 0) ^ (id * 2654435761)`.
        let seed = ((self.world.rng.next_f64() * 2_147_483_647.0) as i32 as u32) ^ (id as u32).wrapping_mul(2_654_435_761);
        let Some(e) = self.world.event_mut(id) else { return };
        let total = e.anim_frames(2);
        let (sx, sy) = *e.spawn.get_or_insert((e.x, e.y));
        let (ex, ey, ed) = (e.x, e.y, e.direction);
        let res = npc_monster_step(&self.grid, ex, ey, ed as u8, sx, sy, true, lead.x, lead.y, eff * 32, non_chasing, disabled, floating, seed);
        if let Some(e) = self.world.event_mut(id) {
            e.direction = res.dir as i32;
            e.x = res.x;
            e.y = res.y;
            if res.animate {
                e.current_frame = (e.current_frame + 1) % total;
            }
        }
    }

    /// Finalize any pure exit-walk-and-hide auto scripts (`npc_walk_to*` -> `event_set_state(-1, 0)`
    /// -> `end`) in the current scene right before a foreground `scene_change` leaves it (#179).
    /// This guarantees cutscene NPCs (e.g. Scene #3 `s_1cb2` Auntie Li `ev#57` & Ling'er `ev#69`)
    /// never remain visible mid-corridor with active proximity triggers on return.
    pub(super) fn flush_departing_exit_walks(&mut self) {
        let ids = self.world.current_event_ids();
        let mut to_hide = Vec::new();
        for id in ids {
            let Some(ev) = self.world.event(id) else { continue };
            if ev.state <= 0 || ev.auto.is_none() {
                continue;
            }
            let Some(blk) = self.prog.block(ev.auto.label) else { continue };
            let start = ev.auto.idx as usize;
            if start >= blk.len() {
                continue;
            }
            let mut saw_walk = false;
            let mut hides_self = false;
            let mut pure_exit = true;
            for ins in &blk[start..] {
                match opcode_name(ins.op) {
                    "npc_walk_to" | "npc_walk_to_slow" | "npc_walk_to_2" | "npc_walk_to_fast_2" => {
                        saw_walk = true;
                    }
                    "npc_set_dir_frame" | "event_set_dir_frame" | "wait_ticks" | "video_redraw" => {}
                    "event_set_state" if (ins.a[0] == -1 || ins.a[0] == 65535 || ins.a[0] == id as i32) && ins.a[1] == 0 => {
                        hides_self = true;
                    }
                    "end" => break,
                    _ => {
                        pure_exit = false;
                        break;
                    }
                }
            }
            if pure_exit && saw_walk && hides_self {
                to_hide.push(id);
            }
        }
        for id in to_hide {
            if let Some(ev) = self.world.event_mut(id) {
                ev.state = 0;
                ev.auto = ScriptPos::NONE;
            }
            self.clear_walk_state(id);
        }
    }

    /// Run every current-scene auto script once (legacy `_tickAutoScriptsInner`).
    pub(crate) fn tick_auto_scripts(&mut self) {
        if self.world.chase_disabled_ticks > 0 {
            self.world.chase_disabled_ticks -= 1;
        }
        if self.world.chase_alert_cycles > 0 {
            self.world.chase_alert_cycles -= 1;
            if self.world.chase_alert_cycles == 0 {
                self.world.chase_alert_range = 1;
            }
        }
        let lead = self.world.leader().clone();
        for id in self.world.current_event_ids() {
            let Some(ev) = self.world.event(id) else { continue };
            if ev.vanish_time > 0 {
                let on_screen = (ev.x - lead.x).abs() < 160 && (ev.y - lead.y).abs() < 110;
                if let Some(ev) = self.world.event_mut(id) {
                    if ev.vanish_time > 1 || !on_screen {
                        ev.vanish_time -= 1;
                    }
                }
                continue;
            }
            if ev.state > 0 && !ev.auto.is_none() {
                let (px, py) = (ev.x, ev.y);
                let start = ev.auto;
                self.exec_chain_sync(start, id, Caller::Auto(id));
                if self.loading_scene {
                    return;
                }
                let Some(ev) = self.world.event(id) else { continue };
                if self.fg.is_none()
                    && !self.battle_active()
                    && (ev.x != px || ev.y != py)
                    && ev.state > 0
                    && ev.vanish_time <= 0
                    && !ev.trigger.is_none()
                    && ev.trigger_mode >= 4
                {
                    let iso = (ev.x - lead.x).abs() + (ev.y - lead.y).abs() * 2;
                    if iso < (ev.trigger_mode - 4) * 32 + 16 {
                        self.sync_obstacles(0);
                        self.trigger_event(id);
                        return;
                    }
                }
            }
        }
        self.sync_obstacles(0);
    }
}
