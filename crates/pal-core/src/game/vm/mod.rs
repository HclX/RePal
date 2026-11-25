//! Script VM: executes assembled [`Instr`]s against the [`World`](super::world::World).
//!
//! Semantics are a faithful port of the legacy `site/js/vm/script-vm.js` (`execChain`), with every
//! `await`/`setTimeout` replaced by an explicit [`Wait`] state that is advanced by fixed logic
//! ticks or resolved by a [`HostReply`]. Background ("auto") chains never wait; they run
//! synchronously inside [`Game::tick_auto_scripts`].

mod exec;
mod opcodes_core;
mod opcodes_rpg;

use super::world::{CounterKey, DialogueStyle};
use super::{ms_to_ticks, BattleOutcome, Game, HostCmd, HostReply};
use crate::isometric::direction_between;
use crate::npc_ai::{is_world_blocked, npc_advance_walk, npc_monster_step, npc_walk_dir, NpcWalkOutcome};
use crate::script::{opcode_name, Instr, LabelId, Program, ScriptPos, NO_LABEL};
use std::collections::VecDeque;

/// Where a chain persists its resume position (legacy `updateCaller`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller {
    None,
    Trigger(u16),
    Auto(u16),
    SceneEnter(u16),
    EnemyPreTurn(usize),
    EnemyInTurn(usize),
    MagicSuccess(u16),
}

/// What started a foreground script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FgKind {
    SceneEnter(u16),
    Event(u16),
    UseItem(u16),
    Battle(i32),
    /// Run an arbitrary block as event `.1` (tools, tests, debug consoles).
    Script(LabelId, u16),
}

/// One step of a timed presentation sequence (fades, delays, wait ticks).
#[derive(Clone, Debug, PartialEq)]
pub enum Tl {
    Alpha(f32),
    Color([u8; 3]),
    Sleep(u32),
    AutoTick,
    FadeInIfNeeded,
    NeedFadeIn(bool),
    CamPanStep(i32, i32),
}

/// Why the foreground script is suspended.
#[derive(Clone, Debug, PartialEq)]
pub enum Wait {
    None,
    /// Host is loading scene assets (`HostCmd::LoadScene` → `HostReply::SceneLoaded`).
    SceneAssets,
    /// Host must load a script chapter (`HostCmd::LoadChapter` → `HostReply::Done`).
    Chapter(u32),
    /// Generic host acknowledgement (RNG cutscene, return to title).
    Host,
    Dialogue,
    Choice { on_no: LabelId },
    DefeatNotice,
    DefeatRetryChoice,
    Modal,
    Battle { on_defeat: LabelId, on_flee: LabelId, prev_music: i32 },
    Timeline(VecDeque<Tl>),
    WalkParty { gx: i32, gy: i32, interval: u32, cd: u32, guard: u32, ride: u16 },
    WalkNpc { ev: u16, gx: i32, gy: i32, spd: i32, leg_key: u64, iter: u32, stall_iters: u32, cd: u32 },
}

#[derive(Clone, Debug)]
pub(crate) struct Frame {
    pos: ScriptPos,
    start: ScriptPos,
    event_id: u16,
    caller: Caller,
    guard: u32,
    max_ops: u32,
    auto: bool,
    /// `call_script` frames whose `false` result must not abort the parent (teleport scripts).
    ignore_result: bool,
    /// Frame runs a battle script requested by the battle flow.
    battle_script: bool,
}

impl Frame {
    fn new(pos: ScriptPos, event_id: u16, caller: Caller, auto: bool) -> Self {
        Self { pos, start: pos, event_id, caller, guard: 0, max_ops: if auto { 32 } else { 4000 }, auto, ignore_result: false, battle_script: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Main,
    ItemTrig { prev_using: bool },
    SceneEnter,
}

pub(crate) struct FgThread {
    pub kind: FgKind,
    stack: Vec<Frame>,
    pub wait: Wait,
    phase: Phase,
    /// Result of the main chain (legacy `runForegroundScript` return value).
    result: bool,
    /// Result of the most recently finished chain in the current phase.
    chain_result: bool,
    /// Battle wait stashed while a battle script frame runs.
    battle_wait: Option<Wait>,
}

/// Control flow produced by one instruction.
enum Flow {
    Next,
    Goto(ScriptPos),
    End(bool),
    Call(Frame),
    Wait(Wait),
}

enum FrameExit {
    End(bool),
    Call(Frame),
    Wait(Wait),
}

const STEP_DELTAS: [(i32, i32); 4] = [(-16, 8), (-16, -8), (16, -8), (16, 8)];

fn fnv_leg_key(s: &str) -> u64 {
    let mut lo: u32 = 0x811c_9dc5;
    let mut hi: u32 = 0x9e37_79b9;
    for (i, c) in s.encode_utf16().enumerate() {
        let c = c as u32;
        lo = (lo ^ c).wrapping_mul(0x0100_0193);
        hi = (hi ^ c.wrapping_add(i as u32)).wrapping_mul(0x85eb_ca6b);
    }
    ((hi as u64) << 32) | lo as u64
}

impl Game {
    // =====================================================================================
    // Foreground thread management
    // =====================================================================================

    pub(crate) fn start_foreground(&mut self, kind: FgKind) {
        if self.fg.is_none()
            && !self.in_title
            && matches!(kind, FgKind::Event(_) | FgKind::Script(_, _))
        {
            // Snapshot clean idle-overworld state BEFORE an event script can mutate events/party
            // and trigger a scripted boss fight (prevents post-defeat soft-locks if retried).
            self.pre_battle_save = Some(self.save_json("pre_event_autosave", "auto"));
        }
        self.vmc.dialogue = DialogueStyle::default();
        let mut t = FgThread { kind, stack: Vec::new(), wait: Wait::None, phase: Phase::Main, result: true, chain_result: true, battle_wait: None };
        match kind {
            FgKind::SceneEnter(sid) => {
                if let Some(sc) = self.world.scenes.get(&sid) {
                    if !sc.enter.is_none() {
                        t.stack.push(Frame::new(sc.enter, 0, Caller::SceneEnter(sid), false));
                    }
                }
            }
            FgKind::Event(id) => {
                if let Some(ev) = self.world.event(id) {
                    if ev.interact_anchor.len() >= 2 {
                        let a = ev.interact_anchor.clone();
                        self.world.set_party_tile_pos(a[0], a[1], a.get(2).copied().unwrap_or(0));
                    }
                    let pos = self.world.event(id).map(|e| e.trigger).unwrap_or_default();
                    t.stack.push(Frame::new(pos, id, Caller::Trigger(id), false));
                }
            }
            FgKind::Script(l, ev) => {
                t.stack.push(Frame::new(ScriptPos::new(l, 0), ev, Caller::None, false));
            }
            FgKind::Battle(team) => {
                self.battle_begin(team, true);
                t.wait = Wait::Battle { on_defeat: NO_LABEL, on_flee: NO_LABEL, prev_music: self.world.music_track };
                t.result = true;
                self.fg = Some(t);
                return;
            }
            FgKind::UseItem(item) => {
                let lbl = self.data.items.get(&item).map(|i| i.script_use.clone()).unwrap_or_default();
                let id = self.prog.intern(&lbl);
                if id != NO_LABEL {
                    t.stack.push(Frame::new(ScriptPos::new(id, 0), 0, Caller::None, false));
                }
            }
        }
        self.fg = Some(t);
    }

    /// Start an event's trigger script if nothing else is running (legacy `triggerEvent`).
    pub fn trigger_event(&mut self, id: u16) -> bool {
        if self.fg.is_some() {
            return false;
        }
        match self.world.event(id) {
            Some(ev) if !ev.trigger.is_none() => {}
            _ => return false,
        }
        self.start_foreground(FgKind::Event(id));
        self.run_foreground_tick();
        true
    }

    pub(crate) fn fg_reply(&mut self, r: HostReply) {
        let Some(t) = self.fg.as_mut() else { return };
        let wait = std::mem::replace(&mut t.wait, Wait::None);
        let mut goto: Option<LabelId> = None;
        match (wait, r) {
            (Wait::SceneAssets, HostReply::SceneLoaded) => {
                self.after_scene_loaded();
                // A scene-enter thread that was waiting for its first load has not executed anything yet.
                return self.resume_after_wait(false, None);
            }
            (Wait::Chapter(_), HostReply::Done) => return self.resume_after_wait(false, None),
            (w @ Wait::Battle { .. }, HostReply::ModalDone) => {
                self.battle_victory_ack();
                if let Some(t) = self.fg.as_mut() {
                    t.wait = w;
                }
                return self.run_foreground_tick();
            }
            (Wait::Host, HostReply::Done) | (Wait::Dialogue, HostReply::DialogueDone) | (Wait::Modal, HostReply::ModalDone) => {}
            (Wait::Choice { on_no }, HostReply::Choice(yes)) => {
                if !yes && on_no != NO_LABEL {
                    goto = Some(on_no);
                }
            }
            (Wait::DefeatNotice, HostReply::DialogueDone) => {
                let prompt = self
                    .world
                    .t3(
                        "是否要回到战前重新挑战？",
                        "是否要回到戰前重新挑戰？",
                        "Retry from just before this battle?",
                    )
                    .to_string();
                if let Some(t) = self.fg.as_mut() {
                    t.wait = Wait::DefeatRetryChoice;
                }
                self.emit(HostCmd::ShowDialogue {
                    speaker: String::new(),
                    text: prompt,
                    side: 2,
                    portrait: 0,
                });
                self.emit(HostCmd::AskYesNo);
                return;
            }
            (Wait::DefeatRetryChoice, HostReply::Choice(yes)) => {
                self.emit(HostCmd::HideDialogue);
                if yes {
                    if let Some(snap) = self.pre_battle_save.clone() {
                        if self.load_save_json_keeping_recorder(&snap).is_ok() {
                            let msg = self
                                .world
                                .t3(
                                    "已读取战前自动存档",
                                    "已讀取戰前自動存檔",
                                    "Reloaded pre-battle autosave",
                                )
                                .to_string();
                            self.emit(HostCmd::Toast(msg));
                            return;
                        }
                    }
                }
                self.abort_foreground();
                self.emit(HostCmd::GameOver);
                return;
            }
            (w, _) => {
                // Unexpected reply: keep waiting.
                if let Some(t) = self.fg.as_mut() {
                    t.wait = w;
                }
                return;
            }
        }
        self.resume_after_wait(true, goto);
    }

    /// Continue after a resolved wait: `advance` moves past the waiting instruction.
    fn resume_after_wait(&mut self, advance: bool, goto: Option<LabelId>) {
        if let Some(t) = self.fg.as_mut() {
            if let Some(fr) = t.stack.last_mut() {
                if let Some(l) = goto {
                    fr.pos = ScriptPos::new(l, 0);
                } else if advance {
                    fr.pos.idx += 1;
                }
            }
        }
        self.run_foreground_tick();
    }

    /// Advance timed waits by one tick and run the foreground chain until it waits or finishes.
    pub(crate) fn run_foreground_tick(&mut self) {
        // 1. Timed waits consume this tick.
        let ready = match self.fg.as_ref().map(|t| &t.wait) {
            None => return,
            Some(Wait::None) => true,
            Some(Wait::Timeline(_)) | Some(Wait::WalkParty { .. }) | Some(Wait::WalkNpc { .. }) => self.advance_timed_wait(true),
            Some(Wait::Battle { .. }) => self.advance_battle_wait(),
            Some(_) => false,
        };
        if !ready {
            return;
        }
        // 2. Run instructions.
        let mut budget = 64usize;
        while budget > 0 {
            budget -= 1;
            let Some(t) = self.fg.as_mut() else { return };
            if !matches!(t.wait, Wait::None) {
                return;
            }
            let Some(mut fr) = t.stack.pop() else {
                let res = t.chain_result;
                if !self.fg_chain_finished(res) {
                    return;
                }
                continue;
            };
            match self.run_frame(&mut fr) {
                FrameExit::End(_) if fr.battle_script => {
                    let t = self.fg.as_mut().unwrap();
                    t.wait = t.battle_wait.take().unwrap_or(Wait::None);
                    self.vmc.using_item = false;
                    if let Some(b) = self.battle.as_mut() {
                        b.script_running = false;
                    }
                    if !self.advance_battle_wait() {
                        return;
                    }
                }
                FrameExit::End(res) => {
                    let t = self.fg.as_mut().unwrap();
                    if t.stack.is_empty() {
                        t.chain_result = res;
                    } else if !res && !fr.ignore_result {
                        // Sub-chain failure aborts the whole foreground chain (legacy call_script).
                        t.stack.clear();
                        t.chain_result = false;
                    } else if let Some(parent) = t.stack.last_mut() {
                        parent.pos.idx += 1;
                    }
                }
                FrameExit::Call(sub) => {
                    let t = self.fg.as_mut().unwrap();
                    if t.stack.len() < Self::MAX_CALL_DEPTH {
                        t.stack.push(fr);
                        t.stack.push(sub);
                    } else {
                        fr.pos.idx += 1;
                        t.stack.push(fr);
                    }
                }
                FrameExit::Wait(w) => {
                    let t = self.fg.as_mut().unwrap();
                    t.stack.push(fr);
                    t.wait = w;
                    // Apply the instantaneous part of timed waits right away.
                    if matches!(self.fg.as_ref().unwrap().wait, Wait::Timeline(_) | Wait::WalkParty { .. } | Wait::WalkNpc { .. })
                        && self.advance_timed_wait(false)
                    {
                        continue;
                    }
                    return;
                }
            }
        }
    }

    /// Advance an in-progress battle (or start one of its scripts). Returns `true` once the battle
    /// is over and the waiting `battle_start` instruction may continue.
    fn advance_battle_wait(&mut self) -> bool {
        // A battle script is pending: run it as a nested foreground frame.
        if let Some((pos, caller)) = self.battle.as_mut().and_then(|b| b.script_request.take()) {
            let t = self.fg.as_mut().unwrap();
            t.battle_wait = Some(std::mem::replace(&mut t.wait, Wait::None));
            let mut fr = Frame::new(pos, 0, caller, false);
            fr.battle_script = true;
            t.stack.push(fr);
            if let Some(b) = self.battle.as_mut() {
                b.script_running = true;
            }
            return true; // let the runner execute the script frame now
        }
        let Some(outcome) = self.battle_tick() else {
            if self.battle.as_ref().map(|b| b.script_request.is_some()).unwrap_or(false) {
                return self.advance_battle_wait();
            }
            return false;
        };
        let t = self.fg.as_mut().unwrap();
        let Wait::Battle { on_defeat, on_flee, prev_music } = std::mem::replace(&mut t.wait, Wait::None) else { return true };
        if prev_music > 0 {
            self.emit(HostCmd::PlayMusic { track: prev_music, looped: true });
        }
        if outcome == BattleOutcome::Defeat {
            // In original DOS PAL (`0x0007`), normal loseable bosses specify `on_defeat: s_9c9b`
            // (`load_last_save`), whereas unwinnable scripted-loss story battles (e.g. 石长老 `s_42a6`,
            // 蝶精彩依 `s_6267`, 锁妖塔 `s_6d0d`) omit `on_defeat` and immediately follow `battle_start`
            // with post-battle cutscene ops (`battle_end_return`, `music_play`, `role_revive`, etc.).
            let has_post_battle_cutscene = self
                .fg
                .as_ref()
                .and_then(|t| t.stack.last())
                .and_then(|fr| self.prog.block(fr.pos.label).map(|blk| (blk, fr.pos.idx as usize)))
                .map(|(blk, idx)| {
                    blk.iter()
                        .skip(idx + 1)
                        .take(4)
                        .any(|ins| {
                            matches!(
                                opcode_name(ins.op),
                                "battle_end_return"
                                    | "role_revive"
                                    | "party_set_roles"
                                    | "music_play"
                                    | "music_stop"
                                    | "scene_set_scripts"
                                    | "event_set_state"
                            )
                        })
                })
                .unwrap_or(false);
            let scripted = matches!(self.fg.as_ref().map(|t| t.kind), Some(FgKind::Battle(_)))
                || on_defeat != NO_LABEL
                || has_post_battle_cutscene;
            if !scripted {
                if self.pre_battle_save.is_some() {
                    let text = self
                        .world
                        .t3(
                            "胜败乃兵家常事，大侠请重新来过。",
                            "勝敗乃兵家常事，大俠請重新來過。",
                            "Victory and defeat are common in battle. Rise again, hero.",
                        )
                        .to_string();
                    if let Some(t) = self.fg.as_mut() {
                        t.wait = Wait::DefeatNotice;
                    }
                    self.emit(HostCmd::ShowDialogue {
                        speaker: String::new(),
                        text,
                        side: 2,
                        portrait: 0,
                    });
                    return false;
                }
                // No pre-battle snapshot available: party wiped out -> game over (review B3).
                self.abort_foreground();
                self.emit(HostCmd::GameOver);
                return false;
            }
            // Scripted losses continue the story: revive fallen heroes at 25 % HP.
            for role in std::mem::take(&mut self.last_battle_heroes) {
                let r = &mut self.world.roles[role];
                if r.hp <= 0 {
                    r.hp = (r.max_hp / 4).max(1);
                }
            }
        }
        let goto = match outcome {
            BattleOutcome::Defeat if on_defeat != NO_LABEL => Some(on_defeat),
            BattleOutcome::Flee if on_flee != NO_LABEL => Some(on_flee),
            _ => None,
        };
        if let Some(fr) = self.fg.as_mut().unwrap().stack.last_mut() {
            match goto {
                Some(l) => fr.pos = ScriptPos::new(l, 0),
                None => fr.pos.idx += 1,
            }
        }
        true
    }

    /// Called when the active chain stack is empty. Returns `true` if another chain was queued.
    fn fg_chain_finished(&mut self, res: bool) -> bool {
        let t = self.fg.as_mut().unwrap();
        let phase = t.phase;
        if let Phase::Main = phase {
            t.result = res;
        }
        t.chain_result = true;
        if let Phase::ItemTrig { prev_using } = phase {
            self.vmc.using_item = prev_using;
        }
        // Item-triggered event chains (`branch_if_not_facing_event`).
        if !matches!(phase, Phase::SceneEnter) {
            while self.vmc.pending_item_triggered_event > 0 {
                let id = std::mem::take(&mut self.vmc.pending_item_triggered_event);
                if let Some(ev) = self.world.event(id) {
                    if ev.state > 0 && ev.vanish_time <= 0 && !ev.trigger.is_none() {
                        let pos = ev.trigger;
                        let prev_using = self.vmc.using_item;
                        self.vmc.using_item = false;
                        let t = self.fg.as_mut().unwrap();
                        t.phase = Phase::ItemTrig { prev_using };
                        t.stack.push(Frame::new(pos, id, Caller::Trigger(id), false));
                        return true;
                    }
                }
            }
        }
        if matches!(phase, Phase::SceneEnter) {
            self.world.view.fade_alpha = 0.0;
        }
        if self.vmc.pending_scene_enter {
            self.vmc.pending_scene_enter = false;
            let sid = self.world.scene_id;
            let t = self.fg.as_mut().unwrap();
            t.phase = Phase::SceneEnter;
            if let Some(sc) = self.world.scenes.get(&sid) {
                if !sc.enter.is_none() {
                    t.stack.push(Frame::new(sc.enter, 0, Caller::SceneEnter(sid), false));
                }
            }
            return true;
        }
        self.finish_foreground();
        false
    }

    fn finish_foreground(&mut self) {
        let Some(t) = self.fg.take() else { return };
        if self.vmc.need_fade_in || self.world.view.fade_alpha > 0.0 {
            self.vmc.need_fade_in = false;
            self.world.view.fade_alpha = 0.0;
        }
        self.emit(HostCmd::HideDialogue);
        self.sync_obstacles(0);
        match t.kind {
            FgKind::SceneEnter(_) => self.world.view.fade_alpha = 0.0,
            FgKind::UseItem(item) => {
                let ok = t.result && !self.vmc.item_script_failed;
                self.vmc.using_item = false;
                self.last_item_result = Some(ok);
                if ok && self.data.items.get(&item).map(|i| i.consumable).unwrap_or(false) {
                    self.world.remove_item(item, 1);
                }
            }
            FgKind::Event(_) | FgKind::Battle(_) | FgKind::Script(..) => {}
        }
        let finished_ev = match t.kind {
            FgKind::Event(id) => Some(id),
            _ => None,
        };
        self.pre_battle_save = None;
        self.on_foreground_end(finished_ev);
    }

    /// Advance a timed wait. `consume` = this call may spend the current tick.
    /// Returns `true` once the wait is finished (and clears it).
    fn advance_timed_wait(&mut self, consume: bool) -> bool {
        let mut wait = match self.fg.as_mut() {
            Some(t) => std::mem::replace(&mut t.wait, Wait::None),
            None => return false,
        };
        let done = match &mut wait {
            Wait::Timeline(q) => self.advance_timeline(q, consume),
            Wait::WalkParty { gx, gy, interval, cd, guard, ride } => {
                if *cd > 0 && (!consume || { *cd -= 1; *cd > 0 }) {
                    false
                } else {
                    let finished = self.walk_party_iteration(*gx, *gy, *ride, guard);
                    if !finished {
                        *cd = *interval;
                    }
                    finished
                }
            }
            Wait::WalkNpc { ev, gx, gy, spd, leg_key, iter, stall_iters, cd } => {
                if *cd > 0 && (!consume || { *cd -= 1; *cd > 0 }) {
                    false
                } else {
                    let ev_id = *ev;
                    self.sync_obstacles(ev_id);
                    let r = self.npc_walk_step(ev_id, *gx, *gy, *spd, *leg_key);
                    *iter += 1;
                    if r == 2 {
                        *stall_iters += 1;
                    } else {
                        *stall_iters = 0;
                    }
                    // Cap scripted stall window to ~2.7 s (60 × 45 ms) and unconditionally snap
                    // foreground scripted `npc_walk_to` (0x007C/7D/81/82) to `(gx, gy)` on arrival,
                    // stall, or budget exhaustion so cutscenes match SDLPAL `PAL_NPCWalkTo` (#200).
                    let finished = r == 1 || *stall_iters >= 60 || *iter >= 4096;
                    if finished {
                        if let Some(e) = self.world.event_mut(ev_id) {
                            e.x = *gx;
                            e.y = *gy;
                            e.current_frame = 0;
                        }
                        self.clear_walk_state(ev_id);
                    } else {
                        *cd = ms_to_ticks(45);
                    }
                    finished
                }
            }
            _ => true,
        };
        if done {
            if let Some(t) = self.fg.as_mut() {
                if let Some(fr) = t.stack.last_mut() {
                    fr.pos.idx += 1;
                }
            }
            true
        } else {
            if let Some(t) = self.fg.as_mut() {
                t.wait = wait;
            }
            false
        }
    }

    fn advance_timeline(&mut self, q: &mut VecDeque<Tl>, consume: bool) -> bool {
        let mut budget = consume;
        loop {
            let Some(front) = q.front_mut() else { return true };
            if let Tl::Sleep(n) = front {
                if *n == 0 {
                    q.pop_front();
                    continue;
                }
                if !budget {
                    return false;
                }
                budget = false;
                *n -= 1;
                if *n == 0 {
                    q.pop_front();
                    continue;
                }
                return false;
            }
            match q.pop_front().unwrap() {
                Tl::Alpha(a) => self.world.view.fade_alpha = a,
                Tl::Color(c) => self.world.view.fade_color = c,
                Tl::AutoTick => self.tick_auto_scripts(),
                Tl::NeedFadeIn(b) => self.vmc.need_fade_in = b,
                Tl::FadeInIfNeeded => {
                    if self.vmc.need_fade_in && !self.vmc.pending_scene_enter {
                        self.vmc.need_fade_in = false;
                        for s in fade_in_steps().into_iter().rev() {
                            q.push_front(s);
                        }
                    }
                }
                Tl::CamPanStep(dx, dy) => {
                    let lead = self.world.leader();
                    let (cx, cy) = self.world.view.cam_override.unwrap_or((lead.x - 160, lead.y - 100));
                    self.world.view.cam_override = Some((
                        (cx + dx).clamp(0, 2048 - 320),
                        (cy + dy).clamp(0, 2048 - 200),
                    ));
                }
                Tl::Sleep(_) => unreachable!(),
            }
        }
    }
}

fn fade_in_steps() -> Vec<Tl> {
    let s = ms_to_ticks(35);
    let mut v = Vec::new();
    for a in [0.75, 0.5, 0.25, 0.0] {
        v.push(Tl::Alpha(a));
        v.push(Tl::Sleep(s));
    }
    v.push(Tl::Alpha(0.0));
    v
}

/// Default speaker name (`(zh_cn, zh_tw, en)`) for portrait-bearing `say` steps whose DOS `M.MSG`
/// continuation line omitted an explicit `角色：` prefix (#120).
pub(crate) fn portrait_default_speaker(portrait_id: i32) -> Option<(&'static str, &'static str, &'static str)> {
    match portrait_id {
        1..=10 => Some(("李逍遥", "李逍遙", "Li Xiaoyao")),
        11..=20 | 88 | 91 => Some(("赵灵儿", "趙靈兒", "Zhao Ling'er")),
        21..=25 => Some(("林月如", "林月如", "Lin Yueru")),
        27..=32 => Some(("阿奴", "阿奴", "A-Nu")),
        33..=35 => Some(("秀兰", "秀蘭", "Xiulan")),
        36 => Some(("唐钰", "唐鈺", "Tang Yu")),
        37 => Some(("酒剑仙", "酒劍仙", "Drunken Sword Immortal")),
        39 => Some(("张四", "張四", "Uncle Zhang")),
        40 => Some(("刘晋元", "劉晉元", "Liu Jinyuan")),
        42 => Some(("姬三娘", "姬三娘", "Madam Ji")),
        43 => Some(("天鬼皇", "天鬼皇", "Ghost Emperor")),
        44 => Some(("盖罗娇", "蓋羅嬌", "Gai Luojiao")),
        48..=49 => Some(("彩依", "彩依", "Caiyi")),
        55 => Some(("李大娘", "李大娘", "Auntie Li")),
        60 => Some(("石长老", "石長老", "Elder Shi")),
        63 => Some(("剑圣", "劍聖", "Sword Saint")),
        64 => Some(("林天南", "林天南", "Lin Tiannan")),
        65 => Some(("韩医仙", "韓醫仙", "Doctor Han")),
        69 => Some(("书中仙", "書中仙", "Book Spirit")),
        73 => Some(("巫后", "巫后", "Empress Wu")),
        80..=81 => Some(("拜月教主", "拜月教主", "Moon Cult Leader")),
        82 => Some(("老婆婆", "老婆婆", "Old Granny")),
        87 => Some(("王小虎", "王小虎", "Wang Xiaohu")),
        _ => None,
    }
}

#[cfg(test)]
mod portrait_speaker_tests {
    use super::portrait_default_speaker;

    #[test]
    fn test_portrait_default_speaker_main_cast() {
        assert_eq!(portrait_default_speaker(7), Some(("李逍遥", "李逍遙", "Li Xiaoyao")));
        assert_eq!(portrait_default_speaker(14), Some(("赵灵儿", "趙靈兒", "Zhao Ling'er")));
        assert_eq!(portrait_default_speaker(22), Some(("林月如", "林月如", "Lin Yueru")));
        assert_eq!(portrait_default_speaker(55), Some(("李大娘", "李大娘", "Auntie Li")));
        assert_eq!(portrait_default_speaker(0), None);
    }
}
