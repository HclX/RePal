//! RPG & presentation VM opcodes: items, cash, roles, equipment, screen/FBP/RNG, shops, and battle scripts.

use super::*;

impl Game {
    #[allow(clippy::cognitive_complexity)]
    pub(super) fn step_rpg_opcode(&mut self, fr: &mut Frame, ins: &Instr, name: &str) -> Flow {
        let auto = fr.auto;
        let cur = fr.pos;
        let ev_id = fr.event_id;
        let caller = fr.caller;
        let goto = |l: LabelId| Flow::Goto(ScriptPos::new(l, 0));
        let a = ins.a;

        match name {
            "item_add" => {
                if a[0] > 0 {
                    self.world.add_item(a[0] as u16, ins.arg_or(1, 1));
                }
                Flow::Next
            }
            "item_remove" => {
                // SDLPAL 0x0020: count defaults to 1 if 0; counts & removes from inventory first, then equipped slots.
                let item_id = a[0] as u16;
                let need = ins.arg_nz(1, 1).max(1);
                if self.world.item_count(item_id) < need {
                    if ins.lbl[2] != NO_LABEL {
                        return goto(ins.lbl[2]);
                    }
                    // When no else-jump is given, still remove whatever copies exist.
                    let avail = self.world.item_count(item_id);
                    if avail > 0 {
                        let unequipped = self.world.remove_item_including_equipped(item_id, avail);
                        for (rid, _eq_id) in unequipped {
                            self.recompute_equip_effects(rid);
                        }
                    }
                } else {
                    let unequipped = self.world.remove_item_including_equipped(item_id, need);
                    for (rid, _eq_id) in unequipped {
                        self.recompute_equip_effects(rid);
                    }
                }
                Flow::Next
            }
            "branch_if_item_less" => {
                if self.world.item_count(a[0] as u16) < ins.arg_or(1, 1) && ins.lbl[2] != NO_LABEL {
                    return goto(ins.lbl[2]);
                }
                Flow::Next
            }
            "cash_add" => {
                let amt = ins.s16(0);
                if amt < 0 && (self.world.cash as i64 + amt as i64) < 0 {
                    if ins.lbl[1] != NO_LABEL {
                        return goto(ins.lbl[1]);
                    }
                } else {
                    self.world.add_cash(amt);
                    if amt > 0 && !auto && self.battle.is_none() {
                        let s = if self.world.lang.is_en() {
                            format!("Obtained {amt} Wen")
                        } else {
                            format!("{}{amt} 文钱", self.world.t("获得金钱：", "獲得金錢："))
                        };
                        self.emit(HostCmd::Toast(s));
                    }
                }
                Flow::Next
            }
            "cash_halve" => {
                self.world.halve_cash();
                Flow::Next
            }
            "party_heal_hp" | "party_heal_mp" | "party_heal_hp_mp" => {
                let all = a[0] != 0 || (self.vmc.using_item && self.vmc.target_item_all);
                let hp = if ins.argc >= 2 { ins.s16(1) } else { (ins.arg_nz(0, 9999) as u16) as i16 as i32 };
                match name {
                    "party_heal_hp" => self.apply_role_heal(all, hp, 0),
                    "party_heal_mp" => self.apply_role_heal(all, 0, hp),
                    _ => {
                        let mp = if ins.argc >= 3 && a[2] != 0 { ins.s16(2) } else { hp };
                        self.apply_role_heal(all, hp, mp);
                    }
                }
                Flow::Next
            }
            "role_revive" => {
                let all = a[0] != 0 || (self.vmc.using_item && self.vmc.target_item_all);
                let tenths = ins.arg_nz(1, 10).clamp(1, 10);
                let ids = if all { self.world.party_role_ids() } else { vec![self.item_or_leader_role()] };
                let mut revived = false;
                for rid in ids {
                    if let Some(r) = self.world.roles.get_mut(rid) {
                        revived |= r.revive(tenths);
                    }
                }
                if self.vmc.using_item && !revived {
                    self.vmc.item_script_failed = true;
                }
                Flow::Next
            }
            "player_halve_hp" | "player_kill_instant" => {
                let rid = self.item_or_leader_role();
                if let Some(r) = self.world.roles.get_mut(rid) {
                    if name == "player_halve_hp" {
                        r.halve_hp();
                    } else {
                        r.hp = 0;
                    }
                }
                Flow::Next
            }
            "role_add_attr" | "role_set_attr" => {
                let rid = self.role_from_raw(a[2]);
                let val = ins.s16(1);
                if let Some(r) = self.world.roles.get_mut(rid) {
                    r.apply_attr(a[0], val, name == "role_set_attr");
                }
                Flow::Next
            }
            "role_learn_magic" => {
                let (mag, raw) = if a[0] >= 61 { (a[0], a[1]) } else { (a[1], a[0]) };
                let rid = self.role_from_raw(raw);
                let mag = mag as u16;
                let mut learned_name = None;
                if let Some(r) = self.world.roles.get_mut(rid) {
                    if mag > 0 && !r.magics.contains(&mag) {
                        r.magics.push(mag);
                        if let Some(m) = self.data.magics.get(&mag) {
                            if !auto && a[2] <= 0 {
                                learned_name = Some((r.name.clone(), r.name_hant.clone(), m.name.clone(), m.name_hant.clone()));
                            }
                        }
                    }
                }
                if let Some((rn, rnh, mn, mnh)) = learned_name {
                    let s = format!(
                        "{} {}「{}」！",
                        self.world.t(&rn, &rnh),
                        self.world.t3("领悟了仙术", "領悟了仙術", "learned"),
                        self.world.t(&mn, &mnh)
                    );
                    self.emit(HostCmd::Toast(s));
                }
                Flow::Next
            }
            "role_level_up" => {
                let rid = if ins.argc >= 2 {
                    if a[0] == 0 { self.world.leader_role() as usize } else { (a[0] - 1).max(0) as usize }
                } else {
                    self.item_or_leader_role()
                };
                let gain = if ins.argc >= 2 { ins.arg_nz(1, 1) } else { ins.arg_nz(0, 1) }.max(1);
                // Same rules as battle level-ups, including `levelup_magic` learning (review B4).
                self.world.apply_level_up(&self.data, rid, gain, true);
                Flow::Next
            }
            "screen_wave" => {
                let v = ins.s16(0);
                self.world.screen_wave = if v > 200 { 0 } else { v.max(0) };
                Flow::Next
            }
            "camera_shake" => {
                self.world.view.shake_ticks = ins.arg_nz(0, 16).clamp(8, 90);
                self.world.view.shake_level = ins.arg_nz(1, 2).max(1);
                Flow::Next
            }
            "camera_pan" => {
                // Exact 1995 DOS PAL `0x007F` (MoveViewport) semantics (#138):
                // - `(0, 0)` (no args): reset camera back onto the party.
                // - `a[2] == 0xFFFF`: snap directly to tile `(a[0], a[1])`.
                // - `a[2] == 0`: instant relative pixel offset `(s16(a0), s16(a1))`.
                // - `a[2] > 0`: smoothly pan by `(s16(a0), s16(a1))` per step for `a[2]` steps.
                let (dx, dy) = (ins.s16(0), ins.s16(1));
                let mode = a[2];
                if dx == 0 && dy == 0 {
                    self.world.view.cam_override = None;
                    Flow::Next
                } else if mode == 0xFFFF || mode < 0 {
                    let tx = (a[0] * 32 - 160).clamp(0, 2048 - 320);
                    let ty = (a[1] * 16 - 112).clamp(0, 2048 - 200);
                    self.world.view.cam_override = Some((tx, ty));
                    Flow::Next
                } else if mode == 0 || auto {
                    let steps = mode.max(1).min(256);
                    let lead = self.world.leader();
                    let (cx, cy) = self.world.view.cam_override.unwrap_or((lead.x - 160, lead.y - 112));
                    self.world.view.cam_override = Some((
                        (cx + dx * steps).clamp(0, 2048 - 320),
                        (cy + dy * steps).clamp(0, 2048 - 200),
                    ));
                    Flow::Next
                } else {
                    let steps = mode.clamp(1, 256);
                    let mut q = VecDeque::new();
                    for _ in 0..steps {
                        q.push_back(Tl::CamPanStep(dx, dy));
                        q.push_back(Tl::AutoTick);
                        q.push_back(Tl::Sleep(2));
                    }
                    Flow::Wait(Wait::Timeline(q))
                }
            }
            "palette_time_of_day" => {
                self.world.night_palette = a[0] != 0;
                Flow::Next
            }
            "palette_toggle_day_night" => {
                self.world.night_palette = !self.world.night_palette;
                Flow::Next
            }
            "screen_fade_out" => {
                if auto {
                    return Flow::Next;
                }
                let s = ms_to_ticks(35);
                let mut q = VecDeque::new();
                for a in [0.25, 0.5, 0.75, 1.0] {
                    q.push_back(Tl::Alpha(a));
                    q.push_back(Tl::Sleep(s));
                }
                q.push_back(Tl::NeedFadeIn(true));
                Flow::Wait(Wait::Timeline(q))
            }
            "screen_fade_in" => {
                if auto {
                    return Flow::Next;
                }
                let mut q = VecDeque::from([Tl::NeedFadeIn(false)]);
                q.extend(fade_in_steps());
                Flow::Wait(Wait::Timeline(q))
            }
            "screen_fade_to_scene" => {
                // Authentic SDLPAL 0x0073 (`VIDEO_BackupScreen` + `PAL_MakeScene` + `VIDEO_FadeScreen`):
                // when the screen is visible, cross-fade smoothly from the backed-up pre-change frame
                // to the updated scene (e.g. smashing Asura statues in Scene #17 with 破天锤, #6);
                // when currently faded to black, fade in from black.
                if auto {
                    return Flow::Next;
                }
                let mut q = VecDeque::from([Tl::NeedFadeIn(false)]);
                if self.world.view.fade_alpha < 0.1 {
                    let speed = ins.arg_or(0, 1).clamp(0, 8) as u32;
                    let ticks_per_step = (speed + 1).clamp(1, 5);
                    q.push_back(Tl::CrossfadeStart);
                    for i in 1..=12 {
                        let alpha = (12 - i) as f32 / 12.0;
                        q.push_back(Tl::Sleep(ticks_per_step));
                        q.push_back(Tl::CrossfadeAlpha(alpha));
                    }
                } else {
                    q.extend(fade_in_steps());
                }
                Flow::Wait(Wait::Timeline(q))
            }
            "screen_fade_step" => {
                // SDLPAL 0x0093: negative s16(a[0]) fades OUT to black & sets fNeedToFadeIn; positive fades IN.
                if auto {
                    return Flow::Next;
                }
                if ins.s16(0) < 0 {
                    let s = ms_to_ticks(35);
                    let mut q = VecDeque::new();
                    for a in [0.25, 0.5, 0.75, 1.0] {
                        q.push_back(Tl::Alpha(a));
                        q.push_back(Tl::Sleep(s));
                    }
                    q.push_back(Tl::NeedFadeIn(true));
                    Flow::Wait(Wait::Timeline(q))
                } else {
                    let mut q = VecDeque::from([Tl::NeedFadeIn(false)]);
                    q.extend(fade_in_steps());
                    Flow::Wait(Wait::Timeline(q))
                }
            }
            "screen_fade_red" => {
                if auto {
                    return Flow::Next;
                }
                Flow::Wait(Wait::Timeline(VecDeque::from([
                    Tl::Color([0x9b, 0x11, 0x1e]),
                    Tl::Alpha(0.65),
                    Tl::Sleep(ms_to_ticks(180)),
                    Tl::Alpha(0.0),
                    Tl::Color([0, 0, 0]),
                ])))
            }
            "fbp_fade_show" | "fbp_scroll" | "fbp_show_with_npc" => {
                let id = ins.arg_or(0, -1);
                if id == 0xFFFF || id < 0 {
                    self.world.view.active_fbp = -1;
                } else {
                    self.world.view.active_fbp = id;
                    self.world.view.fade_alpha = 0.0;
                    self.vmc.need_fade_in = false;
                }
                Flow::Next
            }
            "screen_restore" => {
                // Legacy renderer: restore the FBP picture captured by `screen_backup`.
                let v = &mut self.world.view;
                v.active_fbp = if v.screen_backed_up { v.backed_fbp } else { -1 };
                v.screen_backed_up = false;
                v.backed_fbp = -1;
                Flow::Next
            }
            "rng_set" => {
                self.world.view.rng_id = a[0];
                Flow::Next
            }
            "rng_play" => {
                if auto {
                    return Flow::Next;
                }
                self.world.view.fade_alpha = 0.0;
                self.vmc.need_fade_in = false;
                let id = self.world.view.rng_id;
                self.emit(HostCmd::PlayRng { id, start: a[0], end: a[1], speed: ins.arg_nz(2, 10) });
                Flow::Wait(Wait::Host)
            }
            "shop_open" => {
                if auto {
                    return Flow::Next;
                }
                self.emit(HostCmd::OpenShop(a[0]));
                Flow::Wait(Wait::Modal)
            }
            "pawnshop_open" => {
                if auto {
                    return Flow::Next;
                }
                self.emit(HostCmd::OpenPawnshop);
                Flow::Wait(Wait::Modal)
            }
            "battle_start" => {
                if auto {
                    return Flow::Next;
                }
                let prev_music = self.world.music_track;
                self.battle_begin(a[0], ins.lbl[2] != NO_LABEL);
                Flow::Wait(Wait::Battle { on_defeat: ins.lbl[1], on_flee: ins.lbl[2], prev_music })
            }
            "branch_if_event_state_eq" => {
                let hit = self.resolve_event(a[0], ev_id).map(|id| self.world.event(id).unwrap().state == a[1]).unwrap_or(false);
                if hit {
                    if ins.lbl[2] != NO_LABEL {
                        goto(ins.lbl[2])
                    } else {
                        Flow::End(true)
                    }
                } else {
                    Flow::Next
                }
            }
            "branch_if_scene_eq" => {
                if self.world.scene_id as i32 == a[0] && ins.lbl[1] != NO_LABEL { goto(ins.lbl[1]) } else { Flow::Next }
            }
            "branch_if_role_in_party" => {
                // SDLPAL 0x0079: compares against DOS rgwName word IDs (36=Xiaoyao..41=Gai Luojiao).
                // Keep 1..=6 fallback for custom mods.
                let raw = a[0];
                let want = if (36..=41).contains(&raw) {
                    raw - 36
                } else if (1..=6).contains(&raw) {
                    raw - 1
                } else {
                    -1
                };
                let present = want >= 0 && self.world.party.iter().any(|p| p.role_id as i32 == want);
                if present && ins.lbl[1] != NO_LABEL { goto(ins.lbl[1]) } else { Flow::Next }
            }
            "branch_if_not_full_hp" => {
                let ids = if self.vmc.using_item && !self.vmc.target_item_all {
                    vec![self.vmc.current_target_role]
                } else {
                    self.world.party_role_ids()
                };
                let not_full = ids.iter().filter_map(|&i| self.world.roles.get(i)).any(|r| r.hp < r.max_hp);
                if not_full && ins.lbl[0] != NO_LABEL { goto(ins.lbl[0]) } else { Flow::Next }
            }
            "place_item_at_player" => {
                let id = a[0].clamp(0, u16::MAX as i32) as u16;
                let mut placed = false;
                if self.world.event(id).is_some() && self.world.is_event_in_current_scene(id) {
                    let lead = self.world.leader().clone();
                    let (dx, dy) = STEP_DELTAS[(lead.dir & 3) as usize];
                    self.sync_obstacles(0);
                    if !is_world_blocked(&self.grid, lead.x + dx, lead.y + dy) {
                        let e = self.world.event_mut(id).unwrap();
                        e.x = lead.x + dx;
                        e.y = lead.y + dy;
                        e.state = a[1];
                        self.sync_obstacles(0);
                        placed = true;
                    }
                }
                if !placed {
                    if ins.lbl[2] != NO_LABEL {
                        return goto(ins.lbl[2]);
                    }
                    self.vmc.item_script_failed = true;
                }
                Flow::Next
            }
            "branch_if_not_facing_event" => {
                let id = a[0].clamp(0, u16::MAX as i32) as u16;
                let extra = a[1];
                let mut matched = false;
                let active = self
                    .world
                    .event(id)
                    .map(|e| e.state > 0 && e.vanish_time <= 0)
                    .unwrap_or(false)
                    && self.world.is_event_in_current_scene(id);
                if active {
                    let lead = self.world.leader().clone();
                    let e = self.world.event(id).unwrap().clone();
                    let (dx, dy) = STEP_DELTAS[(lead.dir & 3) as usize];
                    let probe = (e.x - (lead.x + dx)).abs() + (e.y - (lead.y + dy)).abs() * 2;
                    if probe < extra * 32 + 16 {
                        matched = true;
                    } else if (e.x - lead.x).abs() + (e.y - lead.y).abs() * 2 <= (extra + 1) * 32 + 16 {
                        self.world.leader_mut().dir = direction_between(lead.x, lead.y, e.x, e.y);
                        matched = true;
                    }
                }
                if matched {
                    if extra > 0 {
                        self.vmc.pending_item_triggered_event = id;
                    }
                    Flow::Next
                } else {
                    // SDLPAL 0x0081 (`script.c`): on miss, sets sticky `g_fScriptSuccess = FALSE`
                    // and never resets it to TRUE on a later chained match. Keep this SDLPAL-faithful
                    // so consumable item semantics match DOS PAL (#234).
                    self.vmc.item_script_failed = true;
                    if ins.lbl[2] != NO_LABEL {
                        goto(ins.lbl[2])
                    } else {
                        Flow::Next
                    }
                }
            }
            "role_set_attr_equip" => {
                let val = ins.s16(2);
                for rid in self.target_roles(false, 0) {
                    self.world.roles[rid].equip_effects.insert((a[0], a[1]), val);
                }
                Flow::Next
            }
            "equip_item" => {
                let s = a[0] - 11;
                let slot = if (0..6).contains(&s) { s as usize } else { 5 };
                let new_item = a[1] as u16;
                let targets = self.target_roles(false, 0);
                for rid in targets {
                    self.world.roles[rid].equipment[slot] = new_item;
                    self.recompute_equip_effects(rid);
                }
                Flow::Next
            }
            "role_unequip" => {
                // SDLPAL 0x0023: a[0] is 0-based player role ID (0=Xiaoyao, 1=Ling'er, 2=Yueru, ...).
                let code = a[1];
                let slots: Vec<usize> = if code == 0 {
                    (0..6).collect()
                } else {
                    let s = if code >= 11 { code - 11 } else { code - 1 };
                    if (0..6).contains(&s) { vec![s as usize] } else { Vec::new() }
                };
                let rid = a[0].max(0) as usize;
                if rid < self.world.roles.len() {
                    for &s in &slots {
                        let prev = self.world.roles[rid].equipment[s];
                        if prev > 0 {
                            self.world.roles[rid].equipment[s] = 0;
                            self.world.add_item(prev, 1);
                        }
                    }
                    self.recompute_equip_effects(rid);
                }
                Flow::Next
            }
            "branch_if_not_equipped" => {
                let item = a[0] as u16;
                let min = ins.arg_nz(1, 1).max(1);
                let count = self
                    .world
                    .party
                    .iter()
                    .filter_map(|p| self.world.roles.get(p.role_id as usize))
                    .filter(|r| r.equipment.contains(&item))
                    .count() as i32;
                if count < min && ins.lbl[2] != NO_LABEL { goto(ins.lbl[2]) } else { Flow::Next }
            }
            "role_boost_attr_pct" => {
                let pct = ins.s16(1);
                for rid in self.target_roles(false, 0) {
                    self.world.roles[rid].boost_attr_pct(a[0], pct);
                }
                Flow::Next
            }
            "role_forget_magic" => {
                for rid in self.target_roles(false, 0) {
                    self.world.roles[rid].magics.retain(|&m| m as i32 != a[0]);
                }
                Flow::Next
            }
            "role_apply_poison" | "role_cure_poison_id" => {
                for rid in self.target_roles(a[0] != 0, 0) {
                    let r = &mut self.world.roles[rid];
                    if name == "role_apply_poison" {
                        r.apply_poison(a[1] as u16);
                    } else {
                        r.cure_poison(a[1] as u16);
                    }
                }
                Flow::Next
            }
            "role_cure_poison_level" => {
                // Legacy runtime has no poison level table, so every poison counts as level 1.
                for rid in self.target_roles(a[0] != 0, 0) {
                    if a[1] >= 1 {
                        self.world.roles[rid].poisons.clear();
                    }
                }
                Flow::Next
            }
            "branch_if_not_poisoned" => {
                let has = self.target_roles(false, 0).iter().any(|&r| self.world.roles[r].poisons.contains(&(a[0] as u16)));
                if !has && ins.lbl[1] != NO_LABEL { goto(ins.lbl[1]) } else { Flow::Next }
            }
            "branch_if_no_poison_any" => {
                let has = self.target_roles(false, 0).iter().any(|&r| !self.world.roles[r].poisons.is_empty());
                if !has && ins.lbl[0] != NO_LABEL { goto(ins.lbl[0]) } else { Flow::Next }
            }
            "role_set_status" => {
                let turns = ins.arg_nz(1, 1);
                for rid in self.target_roles(false, 0) {
                    self.world.roles[rid].statuses.insert(a[0] as u16, turns);
                }
                Flow::Next
            }
            "role_clear_status" => {
                for rid in self.target_roles(false, 0) {
                    self.world.roles[rid].clear_status(a[0] as u16);
                }
                Flow::Next
            }
            "enemy_damage" => {
                let dmg = ins.s16(1).max(0);
                let t = self.target_enemies(a[0] != 0);
                if let Some(b) = &mut self.battle {
                    for i in t {
                        b.enemies[i].hp = (b.enemies[i].hp - dmg).max(0);
                    }
                }
                Flow::Next
            }
            "enemy_kill_instant" => {
                let t = self.target_enemies(false);
                if let Some(b) = &mut self.battle {
                    for i in t {
                        if !b.enemies[i].is_boss {
                            b.enemies[i].hp = 0;
                        }
                    }
                }
                Flow::Next
            }
            "enemy_halve_hp" => {
                // SDLPAL 0x005B: a[0] is max damage cap (`min(hp/2 + 1, cap)`), not a max_hp gate.
                let cap = if a[0] > 0 { a[0] } else { i32::MAX };
                let t = self.target_enemies(false);
                if let Some(b) = &mut self.battle {
                    for i in t {
                        let e = &mut b.enemies[i];
                        if e.hp > 0 && !e.is_boss {
                            let dmg = (e.hp / 2 + 1).min(cap);
                            e.hp = (e.hp - dmg).max(0);
                        }
                    }
                }
                Flow::Next
            }
            "drain_hp_from_enemy" => {
                let dmg = ins.s16(0).max(0);
                let t = self.target_enemies(false);
                let mut total = 0;
                if let Some(b) = &mut self.battle {
                    for i in t {
                        let take = b.enemies[i].hp.min(dmg);
                        b.enemies[i].hp -= take;
                        total += take;
                    }
                }
                if total > 0 {
                    self.apply_role_heal(false, total, 0);
                }
                Flow::Next
            }
            "enemy_apply_poison" | "enemy_cure_poison" => {
                let t = self.target_enemies(a[0] != 0);
                let pid = a[1] as u16;
                if let Some(b) = &mut self.battle {
                    for i in t {
                        let e = &mut b.enemies[i];
                        if name == "enemy_apply_poison" {
                            if pid > 0 && !e.poisons.contains(&pid) {
                                e.poisons.push(pid);
                            }
                        } else {
                            e.poisons.retain(|&p| p != pid);
                        }
                    }
                }
                Flow::Next
            }
            "enemy_set_status" => {
                let t = self.target_enemies(false);
                let turns = ins.arg_nz(1, 1);
                let mut applied = false;
                if let Some(b) = &mut self.battle {
                    for i in t {
                        if !b.enemies[i].is_boss {
                            b.enemies[i].statuses.insert(a[0] as u16, turns);
                            applied = true;
                        }
                    }
                }
                let fail_lbl = if ins.lbl[2] != NO_LABEL {
                    ins.lbl[2]
                } else if a[2] > 0 {
                    self.prog.label_id(&format!("s_{:04x}", a[2] as u16)).unwrap_or(NO_LABEL)
                } else {
                    NO_LABEL
                };
                if !applied && fail_lbl != NO_LABEL { goto(fail_lbl) } else { Flow::Next }
            }
            "branch_if_enemy_not_poisoned" => {
                let t = self.target_enemies(false);
                let has = self.battle.as_ref().map(|b| t.iter().any(|&i| b.enemies[i].poisons.contains(&(a[0] as u16)))).unwrap_or(false);
                if !has {
                    return if ins.lbl[1] != NO_LABEL { goto(ins.lbl[1]) } else { Flow::End(true) };
                }
                Flow::Next
            }
            "branch_if_enemy_hp_gt" => {
                let t = self.target_enemies(false);
                let hit = match (&self.battle, t.first()) {
                    (Some(b), Some(&i)) => b.enemies[i].hp as f64 > (b.enemies[i].max_hp as f64 * a[0] as f64) / 100.0,
                    _ => false,
                };
                if hit && ins.lbl[1] != NO_LABEL { goto(ins.lbl[1]) } else { Flow::Next }
            }
            "branch_if_enemy_not_alone" => {
                // SDLPAL 0x0091: jump only if acting enemy is NOT the first alive enemy of its enemy_id.
                let cur_idx = self.target_enemies(false).first().copied();
                let not_first = self
                    .battle
                    .as_ref()
                    .and_then(|b| {
                        let cur_idx = cur_idx?;
                        let cur_eid = b.enemies.get(cur_idx)?.enemy_id;
                        let first_same = b.enemies.iter().position(|e| e.hp > 0 && e.enemy_id == cur_eid)?;
                        Some(first_same != cur_idx)
                    })
                    .unwrap_or(false);
                if not_first {
                    return if ins.lbl[0] != NO_LABEL { goto(ins.lbl[0]) } else { Flow::End(true) };
                }
                Flow::Next
            }
            "branch_if_enemy_turn" => {
                let turn = self.battle.as_ref().map(|b| b.is_enemy_turn).unwrap_or(false);
                if turn {
                    if ins.lbl[0] != NO_LABEL {
                        goto(ins.lbl[0])
                    } else {
                        Flow::End(true)
                    }
                } else {
                    Flow::Next
                }
            }
            "enemy_set_magic" => {
                let t = self.target_enemies(false);
                if let (Some(b), Some(&i)) = (&mut self.battle, t.first()) {
                    b.enemies[i].selected_magic = if a[0] == 65535 { 0 } else { a[0] as u16 };
                }
                Flow::Next
            }
            "enemy_summon" => {
                // SDLPAL 0x009E: a[0] == 0 or 0xFFFF summons self; a[1] is count (default 1).
                let self_idx = self.target_enemies(false).first().copied();
                let eid = if a[0] == 0 || a[0] == 0xFFFF {
                    self.battle
                        .as_ref()
                        .and_then(|b| b.enemies.get(self_idx?))
                        .map(|e| e.enemy_id)
                        .unwrap_or(0)
                } else if a[0] >= 398 {
                    (a[0] - 397) as u16
                } else {
                    a[0].max(0) as u16
                };
                let count = ins.arg_nz(1, 1).clamp(1, 4);
                let mut summoned = 0;
                if eid > 0 {
                    for _ in 0..count {
                        if self.battle_summon(eid).is_some() {
                            summoned += 1;
                        }
                    }
                }
                if summoned == 0 && ins.lbl[2] != NO_LABEL {
                    goto(ins.lbl[2])
                } else {
                    Flow::Next
                }
            }
            "enemy_clone" => {
                // SDLPAL 0x009C: requires alive == 1 && hp > 1, and splits HP between original and clones.
                let (alive, src_opt) = self
                    .battle
                    .as_ref()
                    .map(|b| {
                        let alive = b.enemies.iter().filter(|e| e.hp > 0).count();
                        let src = self.target_enemies(false).first().copied();
                        (alive, src)
                    })
                    .unwrap_or((0, None));
                let can_clone = alive == 1
                    && src_opt
                        .and_then(|i| self.battle.as_ref()?.enemies.get(i))
                        .map(|e| e.hp > 1)
                        .unwrap_or(false);
                let cloned = if can_clone {
                    src_opt.and_then(|i| {
                        let id = self.battle.as_ref()?.enemies.get(i)?.enemy_id.max(1);
                        self.battle_summon(id).map(|slot| (i, slot))
                    })
                } else {
                    None
                };
                match cloned {
                    Some((i, slot)) => {
                        if let Some(b) = self.battle.as_mut() {
                            if i < b.enemies.len() && slot < b.enemies.len() {
                                let split_hp = ((b.enemies[i].hp + 1) / 2).max(1);
                                let max = b.enemies[i].max_hp;
                                b.enemies[i].hp = split_hp;
                                b.enemies[slot].hp = split_hp;
                                b.enemies[slot].max_hp = max;
                            }
                        }
                        Flow::Next
                    }
                    None if ins.lbl[1] != NO_LABEL => goto(ins.lbl[1]),
                    None if ins.lbl[0] != NO_LABEL => goto(ins.lbl[0]),
                    None => Flow::Next,
                }
            }
            "enemy_morph" => {
                let t = self.target_enemies(false).first().copied();
                let eid = if a[0] >= 398 { (a[0] - 397) as u16 } else { a[0].max(0) as u16 };
                if let (Some(i), true) = (t, eid > 0) {
                    if let Some(old) = self.battle.as_ref().and_then(|b| b.enemies.get(i)).cloned() {
                        let ratio = if old.max_hp > 0 { old.hp as f64 / old.max_hp as f64 } else { 1.0 };
                        let mut n = self.battle_enemy_template(eid, i);
                        n.x = old.x;
                        n.y = old.y;
                        n.hp = ((n.max_hp as f64 * ratio).round() as i32).max(1);
                        if let Some(slot) = self.battle.as_mut().and_then(|b| b.enemies.get_mut(i)) {
                            *slot = n;
                        }
                    }
                }
                Flow::Next
            }
            "simulate_magic_role" | "throw_weapon_magic" => {
                self.apply_magic_effect(a[0] as u16, name == "throw_weapon_magic", None);
                Flow::Next
            }
            "magic_base_dmg_by_mp" => {
                let rid = self.target_roles(false, 0).first().copied();
                let used = match rid {
                    Some(r) => {
                        let used = self.world.roles[r].mp.max(1);
                        self.world.roles[r].mp = 0;
                        used
                    }
                    None => 10,
                };
                if let Some(b) = &mut self.battle {
                    b.base_magic_dmg_override = Some(used * 8);
                } else {
                    self.apply_magic_effect(a[0] as u16, false, Some(used * 8));
                }
                Flow::Next
            }
            "magic_dmg_by_cash" => {
                let spend = self.world.cash.min(5000);
                self.world.cash -= spend;
                let dmg = ((spend as f64 * 0.4).floor() as i32).max(50);
                if let Some(b) = &mut self.battle {
                    b.base_magic_dmg_override = Some(dmg);
                } else {
                    self.apply_magic_effect(a[0] as u16, false, Some(dmg));
                }
                Flow::Next
            }
            "magic_cast_anim" => {
                if let Some(b) = &mut self.battle {
                    b.magic_cast_anim = a[0];
                }
                Flow::Next
            }
            "collect_enemy_gourd" => {
                let t = self.target_enemies(false);
                let mut ok = None;
                if let (Some(b), Some(&i)) = (&mut self.battle, t.first()) {
                    let e = &mut b.enemies[i];
                    if !e.is_boss && e.hp <= 20.max((e.max_hp as f64 * 0.35).floor() as i32) {
                        e.hp = 0;
                        ok = Some(e.enemy_id.max(1));
                    }
                }
                match ok {
                    Some(id) => {
                        self.world.gourd_essences.push(id);
                        Flow::Next
                    }
                    None if ins.lbl[0] != NO_LABEL => goto(ins.lbl[0]),
                    None => Flow::Next,
                }
            }
            "gourd_refine" => {
                if !self.world.gourd_essences.is_empty() {
                    self.world.gourd_essences.remove(0);
                    let reward = if a[1] != 0 { a[1] as u16 } else { 58 };
                    self.world.add_item(reward, 1);
                    Flow::Next
                } else if ins.lbl[0] != NO_LABEL {
                    goto(ins.lbl[0])
                } else {
                    Flow::Next
                }
            }
            "battle_steal" => {
                let div = ins.arg_nz(0, 1).max(1);
                let t = self.target_enemies(false);
                let mut gain = None;
                let mut pos = (112, 92);
                if let (Some(b), Some(&i)) = (&mut self.battle, t.first()) {
                    let e = &mut b.enemies[i];
                    pos = (e.x, e.y);
                    e.flash = 6;
                    if e.steal_item > 0 && e.steal_count > 0 {
                        e.steal_count -= 1;
                        gain = Some((e.steal_item, 0));
                    } else {
                        let c = if e.cash != 0 { e.cash } else { 30 };
                        gain = Some((0, (c / div).max(10)));
                    }
                }
                match gain {
                    Some((item, 0)) if item > 0 => {
                        self.world.add_item(item, 1);
                        let item_name = self.item_display_name(item);
                        let msg = if self.world.lang.is_en() {
                            format!("Stole {item_name}!")
                        } else {
                            if self.world.lang.is_en() {
                                format!("Stole [{item_name}]!")
                            } else {
                                format!("{}「{item_name}」", self.world.t("偷得", "偷得"))
                            }
                        };
                        if let Some(b) = &mut self.battle {
                            b.spell_banner = Some(msg.clone());
                            b.spell_banner_ticks = 30;
                            b.float(pos.0, pos.1 - 32, msg, [0xff, 0xd1, 0x66]);
                        }
                    }
                    Some((_, cash)) => {
                        self.world.add_cash(cash);
                        let msg = if self.world.lang.is_en() {
                            format!("Stole {cash} Wen!")
                        } else {
                            format!("偷得 {cash} 文")
                        };
                        if let Some(b) = &mut self.battle {
                            b.spell_banner = Some(msg.clone());
                            b.spell_banner_ticks = 30;
                            b.float(pos.0, pos.1 - 32, msg, [0xff, 0xd1, 0x66]);
                        }
                    }
                    None => {}
                }
                Flow::Next
            }
            "battle_blow_enemies" => {
                // SDLPAL 0x006B: sets visual wind knockback parameter `g_Battle.iBlow = (SHORT)a[0]` (never kills).
                Flow::Next
            }
            "battle_enemies_escape" => {
                if let Some(b) = &mut self.battle {
                    b.outcome = Some(BattleOutcome::Victory);
                    b.active = false;
                }
                Flow::Next
            }
            "battle_flee_nonboss" => {
                let boss = self.battle.as_ref().map(|b| b.enemies.iter().any(|e| e.hp > 0 && e.is_boss)).unwrap_or(false);
                if !boss {
                    if let Some(b) = &mut self.battle {
                        b.outcome = Some(BattleOutcome::Flee);
                        b.active = false;
                    }
                    Flow::Next
                } else if ins.lbl[0] != NO_LABEL {
                    goto(ins.lbl[0])
                } else {
                    Flow::Next
                }
            }
            "battle_end_return" => {
                // SDLPAL 0x0078: no-op (`break;`), returns next instruction so 35 post-battle cutscenes continue!
                if let Some(b) = &mut self.battle {
                    b.active = false;
                }
                Flow::Next
            }
            "battle_set_result" => {
                if let Some(b) = &mut self.battle {
                    b.outcome = Some(match a[0] {
                        1 => BattleOutcome::Defeat,
                        2 => BattleOutcome::Flee,
                        _ => BattleOutcome::Victory,
                    });
                }
                Flow::Next
            }
            "battle_set_auto_next" => {
                if let Some(b) = &mut self.battle {
                    b.auto_next = true;
                }
                Flow::Next
            }
            "battle_hide_role" => {
                // SDLPAL 0x008A: hides the party from enemy attacks for `a[0]` rounds (e.g. 隐蛊 = 3).
                if let Some(b) = &mut self.battle {
                    b.hidden_roles.push(a[0]);
                    b.party_hidden_turns = b.party_hidden_turns.max(a[0].max(1));
                }
                Flow::Next
            }
            "battle_change_sprite" => {
                if a[0] > 0 {
                    for rid in self.target_roles(false, 0) {
                        self.world.roles[rid].battle_sprite_f = a[0] as u16;
                    }
                }
                Flow::Next
            }
            "party_set_follower" => {
                self.world.follower_sprites = a.iter().take(ins.argc as usize).copied().filter(|&v| v > 0 && v != 65535).map(|v| v as u16).collect();
                Flow::Next
            }
            "object_set_script" => {
                // Operand 1 is the new script address (the decompiler's jump mask marks slot 2).
                let lbl = if a[1] > 0 { self.prog.intern(&format!("s_{:04x}", a[1])) } else { NO_LABEL };
                self.world.object_scripts.insert(a[0], lbl);
                // Also update any live battle enemy sharing this object ID (#185).
                if let Some(b) = &mut self.battle {
                    let next_pos = if lbl != NO_LABEL { ScriptPos::new(lbl, 0) } else { ScriptPos::NONE };
                    for e in &mut b.enemies {
                        if e.obj_id == a[0] {
                            e.script_pre_turn = next_pos;
                        }
                    }
                }
                Flow::Next
            }
            "branch_if_event_not_in_zone" => {
                // Exact SDLPAL 0x0083 semantics (`script.c:2448-2471`):
                // Measures diamond distance between calling event `ev_id` and target event `a[0]`,
                // requiring `a[0]` to be in the current scene, and jumps if `dist >= a[1] * 32 + 16`.
                let max = a[1].max(0) * 32 + 16;
                let target_id = self.resolve_event(a[0], ev_id);
                let in_zone = match (self.resolve_event(-1, ev_id), target_id) {
                    (Some(cur_id), Some(tid)) if self.world.is_event_in_current_scene(tid) => {
                        let c = self.world.event(cur_id).unwrap();
                        let t = self.world.event(tid).unwrap();
                        (c.x - t.x).abs() + (c.y - t.y).abs() * 2 < max
                    }
                    _ => false,
                };
                if !in_zone {
                    self.vmc.item_script_failed = true;
                    if ins.lbl[2] != NO_LABEL {
                        if auto {
                            self.update_caller(caller, ScriptPos::new(ins.lbl[2], 0));
                            return Flow::End(true);
                        }
                        return goto(ins.lbl[2]);
                    }
                }
                Flow::Next
            }
            "jump_random_next" => {
                let max = ins.arg_nz(0, 1).max(1) as u32;
                let delta = self.world.rng.below(max);
                let target_idx = cur.idx + 1 + delta;
                let len = self.prog.block(cur.label).map(|b| b.len()).unwrap_or(1) as u32;
                if target_idx < len {
                    Flow::Goto(ScriptPos::new(cur.label, target_idx))
                } else {
                    // Resolve sibling block `s_{base_addr + target_idx}` (e.g. s_a3c8, s_a3f1).
                    let name = self.prog.label_name(cur.label);
                    if let Some(base) = name
                        .strip_prefix("s_")
                        .and_then(|h| u32::from_str_radix(h, 16).ok())
                    {
                        let sibling = format!("s_{:04x}", base + target_idx);
                        if let Some(pos) = self.prog.resolve_entry(&sibling) {
                            return Flow::Goto(pos);
                        }
                    }
                    fr.pos.idx = len.saturating_sub(1);
                    Flow::Next
                }
            }
            "load_last_save" => {
                self.world.last_save_load_requested = true;
                if !auto {
                    if self.pre_battle_save.is_some() {
                        let prompt = self
                            .world
                            .t3(
                                "是否要回到战前重新挑战？",
                                "是否要回到戰前重新挑戰？",
                                "Retry from just before this battle?",
                            )
                            .to_string();
                        self.emit(HostCmd::ShowDialogue {
                            speaker: String::new(),
                            text: prompt,
                            side: 2,
                            portrait: 0,
                        });
                        self.emit(HostCmd::AskYesNo);
                        return Flow::Wait(Wait::DefeatRetryChoice);
                    }
                    self.emit(HostCmd::ReturnToTitle);
                    return Flow::End(true);
                }
                Flow::Next
            }
            "palette_set" => {
                self.world.palette_id = a[0];
                Flow::Next
            }
            "color_fade" => {
                self.world.palette_id = a[0];
                self.world.view.fade_alpha = 0.0;
                self.vmc.need_fade_in = false;
                Flow::Next
            }
            "fbp_fade_current" => {
                // SDLPAL 0x009B: redraws current scene and fades in (`VIDEO_FadeScreen`), never displays FBP #a[0].
                self.world.view.active_fbp = -1;
                if auto {
                    self.world.view.fade_alpha = 0.0;
                    self.vmc.need_fade_in = false;
                    return Flow::Next;
                }
                let mut q = VecDeque::from([Tl::NeedFadeIn(false)]);
                q.extend(fade_in_steps());
                Flow::Wait(Wait::Timeline(q))
            }
            "screen_backup" => {
                self.world.view.screen_backed_up = true;
                self.world.view.backed_fbp = self.world.view.active_fbp;
                Flow::Next
            }
            "ending_animation" => {
                self.world.ending_triggered = true;
                if !auto {
                    self.toast("全剧终", "全劇終", "The End");
                }
                Flow::Next
            }
            "wait_any_key" => {
                if auto {
                    return Flow::Next;
                }
                self.emit(HostCmd::ShowDialogue { speaker: String::new(), text: "...".into(), side: 0, portrait: -1 });
                Flow::Wait(Wait::Dialogue)
            }
            "quit_game" => {
                self.world.game_quit = true;
                self.persist_auto_ip(fr);
                Flow::End(false)
            }
            _ => Flow::Next,
        }
    }
}
