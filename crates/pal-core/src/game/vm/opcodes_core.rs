//! Core VM opcodes: control flow, dialogue, audio, party/scene/event/NPC movement.

use super::*;

impl Game {
    // Instruction semantics
    // =====================================================================================

    #[allow(clippy::cognitive_complexity)]
    pub(super) fn step(&mut self, fr: &mut Frame, ins: &Instr) -> Flow {
        let auto = fr.auto;
        let cur = fr.pos;
        let ev_id = fr.event_id;
        let caller = fr.caller;
        let goto = |l: LabelId| Flow::Goto(ScriptPos::new(l, 0));
        let a = ins.a;
        let name = opcode_name(ins.op);

        match name {
            "end" => {
                self.persist_auto_ip(fr);
                Flow::End(true)
            }
            "script_fail" => {
                self.vmc.item_script_failed = true;
                Flow::End(false)
            }
            "pause_yield" => {
                let len = self.prog.block(cur.label).map(|b| b.len()).unwrap_or(0) as u32;
                let next = if cur.idx + 1 < len { ScriptPos::new(cur.label, cur.idx + 1) } else { ScriptPos::NONE };
                self.update_caller(caller, next);
                Flow::End(true)
            }
            "replace_and_yield" => {
                let target = ScriptPos::new(ins.lbl[0], 0);
                let max = a[1];
                if max <= 0 {
                    self.update_caller(caller, target);
                } else {
                    let key = CounterKey { wait: false, event_id: ev_id, pos: cur };
                    let c = self.world.script_counters.get(&key).copied().unwrap_or(0) + 1;
                    if c < max {
                        self.world.script_counters.insert(key, c);
                        self.update_caller(caller, target);
                    } else {
                        self.world.script_counters.remove(&key);
                        self.update_caller(caller, ScriptPos::new(cur.label, cur.idx + 1));
                    }
                }
                Flow::End(true)
            }
            "replace_next_tick" => {
                self.update_caller(caller, ScriptPos::new(cur.label, cur.idx + 1));
                Flow::Next
            }
            "jump" => {
                if ins.lbl[0] == NO_LABEL {
                    self.persist_auto_ip(fr);
                    Flow::End(true)
                } else if a[1] > 0 {
                    let key = CounterKey { wait: false, event_id: ev_id, pos: cur };
                    let c = self.world.script_counters.get(&key).copied().unwrap_or(0) + 1;
                    if c < a[1] {
                        self.world.script_counters.insert(key, c);
                        goto(ins.lbl[0])
                    } else {
                        self.world.script_counters.remove(&key);
                        if ins.lbl[2] != NO_LABEL {
                            goto(ins.lbl[2])
                        } else {
                            Flow::Next
                        }
                    }
                } else {
                    goto(ins.lbl[0])
                }
            }
            "call_script" => {
                if ins.lbl[0] == NO_LABEL {
                    return Flow::Next;
                }
                let sub_ev = if a[1] != 0 { a[1] as u16 } else { ev_id };
                Flow::Call(Frame::new(ScriptPos::new(ins.lbl[0], 0), sub_ev, Caller::None, auto))
            }
            "branch_random" => {
                let pct = ins.arg_or(0, 50) as f64;
                let rnd = self.world.rng.next_f64();
                if rnd * 100.0 > pct {
                    if ins.lbl[1] != NO_LABEL {
                        goto(ins.lbl[1])
                    } else {
                        Flow::End(true)
                    }
                } else {
                    Flow::Next
                }
            }
            "dialogue_pos_center" | "dialogue_pos_box" => {
                self.vmc.dialogue = DialogueStyle { side: 0, portrait_id: 0, font_color: a[0], last_speaker: Default::default() };
                Flow::Next
            }
            "dialogue_pos_upper" => {
                self.vmc.dialogue = DialogueStyle { side: 1, portrait_id: a[0], font_color: a[1], last_speaker: Default::default() };
                Flow::Next
            }
            "dialogue_pos_lower" => {
                self.vmc.dialogue = DialogueStyle { side: 2, portrait_id: a[0], font_color: a[1], last_speaker: Default::default() };
                Flow::Next
            }
            "say" => {
                if auto {
                    return Flow::Next;
                }
                let s = self.prog.says[ins.text as usize].clone();
                let mut speaker = if self.world.lang.is_en() {
                    self.data
                        .i18n_en
                        .speaker(&s.speaker, &s.speaker_hant)
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| self.world.t(&s.speaker, &s.speaker_hant).to_string())
                } else {
                    self.world.t(&s.speaker, &s.speaker_hant).to_string()
                };
                let text = if self.world.lang.is_en() {
                    self.data
                        .i18n_en
                        .dialogue(s.msg_id, &s.text)
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| self.world.t(&s.text, &s.text_hant).to_string())
                } else {
                    self.world.t(&s.text, &s.text_hant).to_string()
                };
                if text.is_empty() {
                    return Flow::Next;
                }
                if self.vmc.using_item && Program::says_item_failed(&s) {
                    // An executed "无法使用 / 没有效果" line means the item was not consumed (review B9).
                    self.vmc.item_script_failed = true;
                }
                let is_bracket_notice = {
                    let tr = text.trim();
                    (tr.starts_with('『') && tr.ends_with('』'))
                        || (tr.starts_with('[') && tr.ends_with(']'))
                };
                if !s.speaker.is_empty() || !s.speaker_hant.is_empty() {
                    self.vmc.dialogue.last_speaker = (s.speaker.clone(), s.speaker_hant.clone());
                    // If an explicit speaker does not match the currently active portrait's owner
                    // (e.g. an unportraited NPC replying without a preceding `dialogue_pos_*` reset),
                    // clear the stale portrait so it neither lingers nor misattributes continuation
                    // lines (#216).
                    if self.vmc.dialogue.portrait_id > 0 {
                        if let Some((hans, hant, _)) = portrait_default_speaker(self.vmc.dialogue.portrait_id) {
                            if s.speaker != hans && s.speaker_hant != hant {
                                self.vmc.dialogue.portrait_id = 0;
                            }
                        }
                    }
                }
                let d = self.vmc.dialogue.clone();
                // In DOS PAL, continuation lines after `pause_yield`/`screen_restore` often omit
                // the speaker prefix in M.MSG; infer from `portrait_id` (#120) or the preceding
                // speaker in the same dialogue block (#123), except for `『...』` system notices.
                if speaker.is_empty() && !is_bracket_notice {
                    if d.portrait_id > 0 {
                        if let Some((hans, hant, en)) = portrait_default_speaker(d.portrait_id) {
                            speaker = self.world.t3(hans, hant, en).to_string();
                        }
                    }
                    if speaker.is_empty() && (!d.last_speaker.0.is_empty() || !d.last_speaker.1.is_empty()) {
                        speaker = if self.world.lang.is_en() {
                            self.data
                                .i18n_en
                                .speaker(&d.last_speaker.0, &d.last_speaker.1)
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| self.world.t(&d.last_speaker.0, &d.last_speaker.1).to_string())
                        } else {
                            self.world.t(&d.last_speaker.0, &d.last_speaker.1).to_string()
                        };
                    }
                }
                if self.vmc.need_fade_in && !self.vmc.pending_scene_enter {
                    self.vmc.need_fade_in = false;
                    self.world.view.fade_alpha = 0.0;
                }
                self.emit(HostCmd::ShowDialogue { speaker, text, side: d.side, portrait: d.portrait_id });
                Flow::Wait(Wait::Dialogue)
            }
            "dialogue_item_desc" => Flow::Next,
            "op_0054" => {
                // SDLPAL 0x0054: switch to night palette (`fNightPalette = TRUE`).
                self.world.night_palette = true;
                Flow::Next
            }
            "confirm_choice" => {
                if auto {
                    self.persist_auto_ip(fr);
                    return Flow::End(true);
                }
                self.emit(HostCmd::AskYesNo);
                Flow::Wait(Wait::Choice { on_no: ins.lbl[0] })
            }
            "music_play" => {
                self.world.music_track = a[0];
                self.emit(HostCmd::PlayMusic { track: a[0], looped: a[1] == 0 });
                Flow::Next
            }
            "cd_or_midi_play" => {
                // Operand 0 = CD track, Operand 1 = fallback MIDI track (used in web/desktop).
                let track = if a[1] > 0 { a[1] } else { a[0] };
                self.world.music_track = track;
                self.emit(HostCmd::PlayMusic { track, looped: true });
                Flow::Next
            }
            "music_stop" => {
                self.world.music_track = 0;
                self.emit(HostCmd::StopMusic);
                Flow::Next
            }
            "battle_music_set" => {
                self.world.battle_music_track = ins.arg_nz(0, 4);
                Flow::Next
            }
            "battlefield_set" => {
                self.world.battlefield_id = a[0];
                Flow::Next
            }
            "sfx_play" => {
                self.emit(HostCmd::PlaySfx(a[0]));
                Flow::Next
            }
            "video_redraw" => {
                if auto {
                    return Flow::Next;
                }
                Flow::Wait(Wait::Timeline(VecDeque::from([Tl::Sleep(1), Tl::FadeInIfNeeded])))
            }
            "delay" => {
                if auto {
                    return Flow::Next;
                }
                let ms = (ins.arg_nz(0, 1) * 75).clamp(40, 2000) as u32;
                Flow::Wait(Wait::Timeline(VecDeque::from([Tl::Sleep(ms_to_ticks(ms))])))
            }
            "wait_ticks" => {
                if auto {
                    if a[2] != 0 {
                        if let Some(e) = self.resolve_event(-1, ev_id).and_then(|id| self.world.event_mut(id)) {
                            e.advance_frame(3);
                        }
                    }
                    let max = ins.arg_nz(0, 1).max(1);
                    let next = ScriptPos::new(cur.label, cur.idx + 1);
                    if max <= 1 {
                        self.update_caller(caller, next);
                    } else {
                        let key = CounterKey { wait: true, event_id: ev_id, pos: cur };
                        let c = self.counter_bump(key);
                        if c >= max {
                            self.world.script_counters.remove(&key);
                            self.update_caller(caller, next);
                        } else {
                            self.update_caller(caller, cur);
                        }
                    }
                    return Flow::End(true);
                }
                let ticks = ins.arg_nz(0, 1).clamp(1, 60);
                let mut q = VecDeque::from([Tl::FadeInIfNeeded]);
                for _ in 0..ticks {
                    q.push_back(Tl::AutoTick);
                    q.push_back(Tl::Sleep(ms_to_ticks(65)));
                }
                Flow::Wait(Wait::Timeline(q))
            }
            "party_set_pos" => {
                self.world.set_party_tile_pos(a[0], a[1], a[2]);
                Flow::Next
            }
            "party_set_dir_frame" => {
                for m in &mut self.world.party {
                    m.dir = (a[0] & 3) as u8;
                    m.frame = a[1] as u8;
                }
                Flow::Next
            }
            "party_walk_step" => {
                // SDLPAL 0x007E: (dx, dy, layer) — 3rd operand sets `gpGlobals->wLayer` (e.g. Blood Pool #186).
                if ins.has & 0b100 != 0 || a[2] != 0 {
                    self.world.party_layer = ins.s16(2);
                }
                let (dx, dy) = (ins.s16(0), ins.s16(1));
                if dx != 0 || dy != 0 {
                    let lead = self.world.leader().clone();
                    let (nx, ny) = (lead.x + dx, lead.y + dy);
                    let dir = direction_between(lead.x, lead.y, nx, ny);
                    self.world.step_party_to(nx, ny, dir);
                }
                Flow::Next
            }
            "party_walk_to" | "party_walk_to_fast" | "party_walk_to_fastest" | "ride_npc_to" | "ride_npc_to_normal"
            | "ride_npc_to_fast" => {
                if ins.has & 0b11 != 0b11 {
                    return Flow::Next;
                }
                let gx = a[0] * 32 + a[2] * 16;
                let gy = a[1] * 16 + a[2] * 8;
                let ms = if name.contains("fastest") { 28 } else if name.contains("fast") { 45 } else { 65 };
                let ride = if name.starts_with("ride_npc") { self.resolve_event(0, ev_id).unwrap_or(0) } else { 0 };
                let interval = if ride > 0 { 1 } else { ms_to_ticks(ms) };
                if auto {
                    // Background chains cannot suspend: finish the walk immediately.
                    let mut guard = 0;
                    while !self.walk_party_iteration(gx, gy, ride, &mut guard) {}
                    return Flow::Next;
                }
                Flow::Wait(Wait::WalkParty { gx, gy, interval, cd: 0, guard: 0, ride })
            }
            "party_set_roles" => {
                let prev = self.world.party.first().cloned().unwrap_or_default();
                let mut np = Vec::new();
                for &r in a.iter().take(ins.argc as usize) {
                    if (1..=6).contains(&r) {
                        // Place newly-formed followers on the guaranteed-walkable leader tile (#170, #249)
                        // rather than blindly offsetting into walls/cliffs; they fan out naturally via trail.
                        np.push(super::super::world::PartyMember {
                            role_id: (r - 1) as u16,
                            x: prev.x,
                            y: prev.y,
                            dir: prev.dir,
                            frame: 0,
                        });
                    }
                }
                if !np.is_empty() {
                    self.world.party = np;
                    if self.world.trail.is_empty() {
                        self.world.trail.push_front((prev.x, prev.y, prev.dir));
                    }
                    self.sync_sprite_frames_auto();
                }
                Flow::Next
            }
            "role_set_sprite" => {
                let ri = ins.arg_or(0, 0);
                let spr = ins.arg_or(1, 2);
                if let Some(r) = self.world.roles.get_mut(ri.max(0) as usize) {
                    r.walk_sprite_mgo = spr as u16;
                }
                Flow::Next
            }
            "scene_change" => {
                let next = a[0];
                if next > 0 && next as u16 != self.world.scene_id {
                    if !auto {
                        self.flush_departing_exit_walks();
                    }
                    self.world.scene_id = next as u16;
                    self.world.party_layer = 0;
                    self.world.view.active_fbp = -1;
                    self.vmc.pending_scene_enter = true;
                    self.suppressed.clear();
                    self.just_changed_scene = true;
                    let map_id = self.world.current_scene().map(|s| s.map_id).unwrap_or(0);
                    self.emit(HostCmd::LoadScene { scene_id: next as u16, map_id });
                    self.emit(HostCmd::SceneChanged(next as u16));
                    if auto {
                        self.loading_scene = true;
                        return Flow::Next;
                    }
                    // Resume at the following instruction once assets arrive.
                    fr.pos.idx += 1;
                    return Flow::Wait(Wait::SceneAssets);
                }
                Flow::Next
            }
            "scene_set_scripts" => {
                // Exact SDLPAL 0x006D semantics:
                // - If a[1] != 0: update enter script.
                // - If a[2] != 0: update teleport script.
                // - If both == 0: clear both enter and teleport scripts.
                let sid = ins.arg_nz(0, self.world.scene_id as i32) as u16;
                if let Some(sc) = self.world.scenes.get_mut(&sid) {
                    let has_enter = ins.lbl[1] != NO_LABEL;
                    let has_tele = ins.lbl[2] != NO_LABEL;
                    if !has_enter && !has_tele {
                        sc.enter = ScriptPos::new(NO_LABEL, 0);
                        sc.teleport = NO_LABEL;
                    } else {
                        if has_enter {
                            sc.enter = ScriptPos::new(ins.lbl[1], 0);
                        }
                        if has_tele {
                            sc.teleport = ins.lbl[2];
                        }
                    }
                }
                Flow::Next
            }
            "scene_set_map" => {
                let sid = if a[0] == 0xFFFF || a[0] == 0 { self.world.scene_id } else { a[0] as u16 };
                if a[1] != 0 {
                    if let Some(sc) = self.world.scenes.get_mut(&sid) {
                        let map_id = a[1] as u16;
                        sc.map_id = map_id;
                        if sid == self.world.scene_id {
                            self.emit(HostCmd::LoadScene { scene_id: sid, map_id });
                            if auto {
                                self.loading_scene = true;
                                return Flow::Next;
                            }
                            fr.pos.idx += 1;
                            return Flow::Wait(Wait::SceneAssets);
                        }
                    }
                }
                Flow::Next
            }
            "scene_teleport_out" => {
                let tele = self.world.current_scene().map(|s| s.teleport).unwrap_or(NO_LABEL);
                if tele != NO_LABEL && !auto {
                    let mut sub = Frame::new(ScriptPos::new(tele, 0), 0, Caller::None, false);
                    sub.ignore_result = true;
                    Flow::Call(sub)
                } else if ins.lbl[0] != NO_LABEL {
                    goto(ins.lbl[0])
                } else {
                    self.vmc.item_script_failed = true;
                    Flow::Next
                }
            }
            "party_gather_at_leader" => {
                let l = self.world.leader().clone();
                self.world.set_party_world_pos(l.x, l.y);
                Flow::Next
            }
            "event_set_state" => {
                // SDLPAL 0x0049: event_id == 0 is a no-op (only 0xFFFF/-1 targets current event).
                if a[0] != 0 {
                    if let Some(e) = self.resolve_event(a[0], ev_id).and_then(|id| self.world.event_mut(id)) {
                        e.state = a[1];
                    }
                    self.sync_obstacles(0);
                }
                Flow::Next
            }
            "event_set_state_range" => {
                for id in a[0]..=a[1] {
                    if let Some(e) = u16::try_from(id).ok().and_then(|id| self.world.event_mut(id)) {
                        e.state = a[2];
                    }
                }
                self.sync_obstacles(0);
                Flow::Next
            }
            "event_sync_state" => {
                let other = self.resolve_event(a[0], ev_id);
                let me = self.resolve_event(-1, ev_id);
                let want = ins.s16(1);
                if let (Some(o), Some(m)) = (other, me) {
                    if self.world.event(o).map(|e| e.state == want).unwrap_or(false) {
                        if let Some(me_ev) = self.world.event_mut(m) {
                            me_ev.state = want;
                        }
                        if !auto {
                            self.sync_obstacles(0);
                        }
                    }
                }
                Flow::Next
            }
            "event_set_trigger_mode" => {
                if a[0] != 0 {
                    if let Some(e) = self.resolve_event(a[0], ev_id).and_then(|id| self.world.event_mut(id)) {
                        e.trigger_mode = a[1];
                    }
                }
                Flow::Next
            }
            "event_set_auto_script" => {
                if a[0] != 0 {
                    if let Some(id) = self.resolve_event(a[0], ev_id) {
                        if let Some(ev) = self.world.event_mut(id) {
                            ev.auto = ScriptPos::new(ins.lbl[1], 0);
                        }
                        self.clear_walk_state(id);
                        self.world.script_counters.retain(|k, _| k.event_id != id);
                    }
                }
                Flow::Next
            }
            "event_set_trigger_script" => {
                if a[0] != 0 {
                    if let Some(e) = self.resolve_event(a[0], ev_id).and_then(|id| self.world.event_mut(id)) {
                        e.trigger = ScriptPos::new(ins.lbl[1], 0);
                    }
                }
                Flow::Next
            }
            "event_set_dir_frame" => {
                if a[0] != 0 {
                    if let Some(e) = self.resolve_event(a[0], ev_id).and_then(|id| self.world.event_mut(id)) {
                        e.direction = a[1] & 3;
                        e.current_frame = a[2];
                    }
                }
                Flow::Next
            }
            "npc_set_dir_frame" => {
                if let Some(e) = self.resolve_event(-1, ev_id).and_then(|id| self.world.event_mut(id)) {
                    let d = ins.arg_or(0, 0);
                    let f = ins.arg_or(1, 0);
                    if d != 0xFFFF && d != -1 {
                        e.direction = d & 3;
                    }
                    if f != 0xFFFF && f != -1 {
                        e.current_frame = f;
                    }
                }
                Flow::Next
            }
            "npc_set_frame" => {
                if let Some(e) = self.resolve_event(-1, ev_id).and_then(|id| self.world.event_mut(id)) {
                    e.current_frame = a[0];
                }
                if auto {
                    self.update_caller(caller, ScriptPos::new(cur.label, cur.idx + 1));
                    return Flow::End(true);
                }
                Flow::Next
            }
            "event_set_pos" | "event_set_pos_rel" | "event_move_pos" => {
                if let Some(id) = self.resolve_event(a[0], ev_id) {
                    let lead = self.world.leader().clone();
                    let (dx, dy) = (ins.s16(1), ins.s16(2));
                    let e = self.world.event_mut(id).unwrap();
                    match name {
                        "event_set_pos" => {
                            e.x = dx;
                            e.y = dy;
                        }
                        "event_set_pos_rel" => {
                            e.x = lead.x + dx;
                            e.y = lead.y + dy;
                        }
                        _ => {
                            e.x += dx;
                            e.y += dy;
                        }
                    }
                    self.clear_walk_state(id);
                }
                Flow::Next
            }
            "event_set_layer" => {
                if ins.has & 0b10 != 0 {
                    if let Some(e) = self.resolve_event(a[0], ev_id).and_then(|id| self.world.event_mut(id)) {
                        e.layer = ins.s16(1);
                    }
                }
                Flow::Next
            }
            "npc_walk_dir_0" | "npc_walk_dir_1" | "npc_walk_dir_2" | "npc_walk_dir_3" => {
                let dir = (ins.op - 0x000B) as u8;
                if let Some(id) = self.resolve_event(-1, ev_id) {
                    let solid = self.world.event(id).unwrap().state >= 2;
                    if solid {
                        self.sync_obstacles(id);
                    }
                    let e = self.world.event(id).unwrap();
                    let (nx, ny) = npc_walk_dir(&self.grid, e.x, e.y, dir, solid);
                    let e = self.world.event_mut(id).unwrap();
                    e.direction = dir as i32;
                    e.x = nx;
                    e.y = ny;
                    e.advance_frame(3);
                }
                if auto {
                    self.update_caller(caller, ScriptPos::new(cur.label, cur.idx + 1));
                    return Flow::End(true);
                }
                Flow::Next
            }
            "npc_walk_step" => {
                if let Some(e) = self.resolve_event(a[0], ev_id).and_then(|id| self.world.event_mut(id)) {
                    e.x += ins.s16(1);
                    e.y += ins.s16(2);
                    e.advance_frame(3);
                }
                Flow::Next
            }
            "npc_walk_to" | "npc_walk_to_slow" | "npc_walk_to_2" | "npc_walk_to_fast_2" => {
                let explicit = name.ends_with("_2") && ins.argc >= 4;
                let off = usize::from(explicit);
                let Some(id) = self.resolve_event(if explicit { a[0] } else { -1 }, ev_id) else { return Flow::Next };
                if ins.has & (0b11 << off) != (0b11 << off) {
                    return Flow::Next;
                }
                let (tx, ty, th) = (a[off], a[off + 1], a[off + 2]);
                self.sync_obstacles(id);
                let gx = tx * 32 + th * 16;
                let gy = ty * 16 + th * 8;
                let spd = if name.contains("slow") { 6 } else if name.contains("fast") { 18 } else { 11 };
                let key_str = format!("{}:{}:{gx},{gy}", self.prog.label_name(cur.label), cur.idx);
                let leg_key = fnv_leg_key(&key_str);
                let e = self.world.event(id).unwrap();
                let initial = ((gx - e.x) as f64).hypot((gy - e.y) as f64);
                if auto {
                    let r = self.npc_walk_step(id, gx, gy, spd, leg_key);
                    if r == 1 {
                        self.clear_walk_state(id);
                        if initial > 0.0 {
                            self.update_caller(caller, ScriptPos::new(cur.label, cur.idx + 1));
                            return Flow::End(true);
                        }
                        return Flow::Next;
                    }
                    self.update_caller(caller, cur);
                    return Flow::End(true);
                }
                Flow::Wait(Wait::WalkNpc { ev: id, gx, gy, spd, leg_key, iter: 0, stall_iters: 0, cd: 0 })
            }
            "npc_chase_stop_ticks" => {
                self.world.chase_disabled_ticks = a[0].max(0);
                Flow::Next
            }
            "npc_chase_alert_range" => {
                // SDLPAL 0x0063: a[0] is duration in cycles (e.g. 600 for 十里香); chase range becomes 3 temporarily.
                self.world.chase_alert_cycles = ins.arg_nz(0, 600).max(1);
                self.world.chase_alert_range = 3;
                self.world.chase_disabled_ticks = 0;
                Flow::Next
            }
            "npc_chase_player" => {
                if let Some(id) = self.resolve_event(-1, ev_id) {
                    self.monster_chase_step(id, ins);
                }
                if auto {
                    self.update_caller(caller, ScriptPos::new(cur.label, cur.idx + 1));
                    return Flow::End(true);
                }
                Flow::Next
            }
            "npc_animate_in_place" => {
                if let Some(e) = self.resolve_event(-1, ev_id).and_then(|id| self.world.event_mut(id)) {
                    e.advance_frame(3);
                }
                if auto {
                    self.update_caller(caller, ScriptPos::new(cur.label, cur.idx + 1));
                    return Flow::End(true);
                }
                Flow::Next
            }
            "npc_vanish_short" | "npc_vanish_ticks" => {
                // SDLPAL 0x0052: default 800 frames (and only respawns once off-screen in tick_auto_scripts).
                if let Some(e) = self.resolve_event(-1, ev_id).and_then(|id| self.world.event_mut(id)) {
                    e.vanish_time = if name == "npc_vanish_short" { 15 } else { ins.arg_nz(0, 800) };
                }
                Flow::Next
            }
            _ => self.step_rpg_opcode(fr, ins, name),
        }
    }
}
