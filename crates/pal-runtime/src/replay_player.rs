//! Unified platform-neutral `RePal_CoreReplay_v3` deterministic replay player (#235).
//!
//! Shared identically by:
//! - `pal-desktop` (`CoreReplayPlayer` adds cinema HUD, thumbnails & 1×/2× dialogue reading pause)
//! - `pal-wasm` (`core_replay_event` delegates directly to [`dispatch_replay_event`])
//! - `pal-headless` (`run_core_replay_rt` plays `.repal` / v3 bundles through [`ReplayPlayer`])
//! - `pal-runtime/tests/verify_repal.rs` (verifies exact segment boundary determinism in CI)

use crate::PalRuntime;
use pal_core::game::replay_rec::{ReplayAnchor, ReplayBundle, ReplayEvent};
use pal_core::game::{BattleCmd, HostReply, TickInput};

/// Result of advancing [`ReplayPlayer::step_one_tick`] by one deterministic 50 Hz tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayStepOutcome {
    /// Normal tick advanced within the current segment.
    Stepped,
    /// Current segment reached its end and drained all its events; host may inspect boundary
    /// state (e.g. drift check / transition banner) and then call [`ReplayPlayer::load_segment`].
    SegmentCompleted { next_idx: usize },
    /// Final segment completed.
    Finished,
}

/// Hard-load a [`ReplayAnchor`] into `rt` with recording disabled (`g.recorder.enabled = false`).
pub fn load_replay_anchor(rt: &mut PalRuntime, anchor: &ReplayAnchor) -> bool {
    if let Some(g) = rt.game.as_mut() {
        g.recorder.enabled = false;
    }
    let ok = match anchor {
        ReplayAnchor::NewGame { seed } => {
            rt.new_game(*seed);
            true
        }
        ReplayAnchor::SaveJson(save) => {
            if let Some(g) = rt.game.as_mut() {
                g.recorder.enabled = false;
                let res = g.load_save_json_keeping_recorder(save).is_ok();
                if res {
                    rt.reset_session();
                    rt.ui.set_view(pal_ui::UiView::None);
                    rt.route_cmds();
                    rt.project();
                }
                res
            } else {
                rt.load_save_json_str(&save.to_string())
            }
        }
    };
    if let Some(g) = rt.game.as_mut() {
        g.recorder.enabled = false;
    }
    ok
}

/// Dispatch a single non-movement [`ReplayEvent`] directly into `rt` (canonical implementation
/// shared by Desktop, Web WASM, Headless, and CI replay verification).
pub fn dispatch_replay_event(rt: &mut PalRuntime, ev: &ReplayEvent) {
    match *ev {
        ReplayEvent::DialogueDone => {
            if matches!(rt.ui.view, pal_ui::UiView::Dialog) {
                rt.ui.set_view(pal_ui::UiView::None);
            }
            rt.reply(HostReply::DialogueDone);
        }
        ReplayEvent::Choice(yes) => {
            if matches!(rt.ui.view, pal_ui::UiView::Dialog) {
                rt.ui.set_view(pal_ui::UiView::None);
            }
            rt.reply(HostReply::Choice(yes));
        }
        ReplayEvent::ModalDone => {
            rt.victory_open = false;
            if matches!(rt.ui.view, pal_ui::UiView::BattleVictory) {
                rt.ui.set_view(pal_ui::UiView::None);
            }
            rt.reply(HostReply::ModalDone);
        }
        ReplayEvent::BattleAttack(target) => {
            if let Some(g) = rt.game.as_mut() {
                g.battle_command(BattleCmd::Attack { target });
            }
        }
        ReplayEvent::BattleMagic { magic, target } => {
            if let Some(g) = rt.game.as_mut() {
                g.battle_command(BattleCmd::Magic { magic, target });
            }
        }
        ReplayEvent::BattleCoop(target) => {
            if let Some(g) = rt.game.as_mut() {
                g.battle_command(BattleCmd::Coop { target });
            }
        }
        ReplayEvent::BattleItem(item) => {
            if let Some(g) = rt.game.as_mut() {
                g.battle_command(BattleCmd::Item { item });
            }
        }
        ReplayEvent::BattleItemTarget { item, target } => {
            if let Some(g) = rt.game.as_mut() {
                g.battle_command(BattleCmd::ItemTarget { item, target });
            }
        }
        ReplayEvent::BattleDefend => {
            if let Some(g) = rt.game.as_mut() {
                g.battle_command(BattleCmd::Defend);
            }
        }
        ReplayEvent::BattleFlee => {
            if let Some(g) = rt.game.as_mut() {
                g.battle_command(BattleCmd::Flee);
            }
        }
        ReplayEvent::BattleAuto => {
            if let Some(g) = rt.game.as_mut() {
                g.battle_command(BattleCmd::Auto);
            }
        }
        ReplayEvent::BattleCancelAuto => {
            if let Some(g) = rt.game.as_mut() {
                g.battle_cancel_auto();
            }
        }
        ReplayEvent::UseItem { item_id, role } => {
            if let Some(g) = rt.game.as_mut() {
                g.use_item(item_id, role);
            }
        }
        ReplayEvent::EquipItem { role, item_id } => {
            if let Some(g) = rt.game.as_mut() {
                g.equip_from_menu(role, item_id);
            }
        }
        ReplayEvent::CastFieldMagic {
            caster,
            magic_id,
            target,
        } => {
            if let Some(g) = rt.game.as_mut() {
                g.cast_field_magic(caster, magic_id, target);
            }
        }
        ReplayEvent::BuyItem { item_id, price } => {
            if let Some(g) = rt.game.as_mut() {
                g.buy_item(item_id, price);
            }
        }
        ReplayEvent::SellItem { item_id, price } => {
            if let Some(g) = rt.game.as_mut() {
                g.sell_item(item_id, price);
            }
        }
        ReplayEvent::Debug(ref act) => {
            if let Some(g) = rt.game.as_mut() {
                // Strict #235 gate: only executes when `g.debug_mode == true` (set from bundle.debug_mode).
                let _ = g.apply_debug_action(act);
                if matches!(act, pal_core::game::replay_rec::DebugAction::Tp { .. }) {
                    rt.ui.set_view(pal_ui::UiView::None);
                    rt.world.fbp_override = None;
                }
                rt.route_cmds();
                rt.project();
            }
        }
        ReplayEvent::HeldDir(_)
        | ReplayEvent::Interact
        | ReplayEvent::Click(_, _)
        | ReplayEvent::Steer(_) => {}
    }
}

/// Platform-neutral deterministic segmented replay state machine.
#[derive(Clone, Debug)]
pub struct ReplayPlayer {
    pub bundle: ReplayBundle,
    pub seg_idx: usize,
    pub seg_tick: u64,
    pub event_idx: usize,
    pub annotation_idx: usize,
    pub held_dir: i8,
    pub steer: Option<(i32, i32)>,
    pub finished: bool,
}

impl ReplayPlayer {
    pub fn new(bundle: ReplayBundle) -> Result<Self, String> {
        if bundle.segments.is_empty() {
            return Err("replay contains no segments".into());
        }
        Ok(Self {
            bundle,
            seg_idx: 0,
            seg_tick: 0,
            event_idx: 0,
            annotation_idx: 0,
            held_dir: -1,
            steer: None,
            finished: false,
        })
    }

    pub fn current_abs_tick(&self) -> u64 {
        self.bundle
            .segments
            .get(self.seg_idx)
            .map(|s| s.start_tick + self.seg_tick)
            .unwrap_or(self.bundle.total_ticks)
            .min(self.bundle.total_ticks)
    }

    /// Peek the next event if it is due at or before `self.seg_tick` (used by GUI hosts for
    /// presentation-only dialogue reading pause without inspecting `annotations`).
    pub fn peek_due_event(&self) -> Option<&ReplayEvent> {
        let seg = self.bundle.segments.get(self.seg_idx)?;
        let entry = seg.events.get(self.event_idx)?;
        (entry.tick <= self.seg_tick).then_some(&entry.ev)
    }

    /// Restore the active AI thought subtitle at `(seg_idx, target_seg_tick)` via binary search
    /// over `annotations`, falling back across earlier segments in the same scene.
    pub fn restore_annotation_at(
        &mut self,
        seg_idx: usize,
        target_seg_tick: u64,
        rt: &mut PalRuntime,
    ) {
        let Some(seg) = self.bundle.segments.get(seg_idx) else {
            self.annotation_idx = 0;
            rt.set_ai_thought(None);
            return;
        };
        let next_idx = seg.annotations.partition_point(|a| a.tick <= target_seg_tick);
        self.annotation_idx = next_idx;
        let mut active: Option<String> = None;
        if next_idx > 0 {
            let txt = &seg.annotations[next_idx - 1].text;
            if !txt.is_empty() {
                active = Some(txt.clone());
            }
        } else {
            let cur_scene = seg.scene_id;
            for prev in self.bundle.segments[..seg_idx].iter().rev() {
                if prev.scene_id != cur_scene {
                    break;
                }
                if let Some(last) = prev.annotations.last() {
                    if !last.text.is_empty() {
                        active = Some(last.text.clone());
                    }
                    break;
                }
            }
        }
        rt.set_ai_thought(active);
    }

    /// Hard-load segment `idx` into `rt`, resetting segment cursors and restoring `t = 0` annotations.
    pub fn load_segment(&mut self, idx: usize, rt: &mut PalRuntime) -> bool {
        let Some(anchor) = self.bundle.segments.get(idx).map(|s| s.anchor.clone()) else {
            return false;
        };
        let ok = load_replay_anchor(rt, &anchor);
        if ok {
            if let Some(g) = rt.game.as_mut() {
                g.debug_mode = self.bundle.debug_mode;
                g.god_mode = false;
                g.recorder.tainted_by_debug = self.bundle.tainted_by_debug;
                g.recorder.debug_mode = self.bundle.debug_mode;
            }
            rt.ui.debug_mode = self.bundle.debug_mode || self.bundle.tainted_by_debug;
            self.seg_idx = idx;
            self.seg_tick = 0;
            self.event_idx = 0;
            self.held_dir = -1;
            self.steer = None;
            self.finished = false;
            self.restore_annotation_at(idx, 0, rt);
        }
        ok
    }

    /// Advance the replay by one deterministic 50 Hz tick.
    /// - `on_before_event(rt, &ev)` runs immediately before each due [`ReplayEvent`] is dispatched
    ///   (for host presentation toasts that inspect pre-command state, e.g. battle enemy names).
    /// - `on_after_event(rt, &ev)` runs immediately after each due [`ReplayEvent`] is dispatched
    ///   (for servicing `HostCmd::LoadScene` / `PlayRng` emitted by the event before subsequent
    ///   same-tick events execute).
    pub fn step_one_tick<F, G>(
        &mut self,
        rt: &mut PalRuntime,
        mut on_before_event: F,
        mut on_after_event: G,
    ) -> ReplayStepOutcome
    where
        F: FnMut(&mut PalRuntime, &ReplayEvent),
        G: FnMut(&mut PalRuntime, &ReplayEvent),
    {
        if self.finished {
            return ReplayStepOutcome::Finished;
        }
        // Do not advance `seg_tick` or drain events while the core is blocked waiting on an async
        // host operation (`LoadScene` / `PlayRng` / `LoadChapter`), matching Web playback (#242).
        if rt.game.as_ref().is_some_and(|g| g.is_blocked_on_host()) {
            rt.tick(TickInput::IDLE);
            return ReplayStepOutcome::Stepped;
        }
        let Some(seg) = self.bundle.segments.get(self.seg_idx) else {
            self.finished = true;
            return ReplayStepOutcome::Finished;
        };

        let mut latest_ann: Option<String> = None;
        while self.annotation_idx < seg.annotations.len()
            && seg.annotations[self.annotation_idx].tick <= self.seg_tick
        {
            latest_ann = Some(seg.annotations[self.annotation_idx].text.clone());
            self.annotation_idx += 1;
        }

        let mut pending_evs = Vec::new();
        while self.event_idx < seg.events.len() && seg.events[self.event_idx].tick <= self.seg_tick
        {
            pending_evs.push(seg.events[self.event_idx].ev.clone());
            self.event_idx += 1;
        }
        let seg_ev_len = seg.events.len();
        let seg_dur = seg.duration_ticks;

        if let Some(text) = latest_ann {
            rt.set_ai_thought((!text.is_empty()).then_some(text));
        }

        let mut interact = false;
        let mut click = None;
        for ev in pending_evs {
            on_before_event(rt, &ev);
            match ev {
                ReplayEvent::HeldDir(d) => self.held_dir = d,
                ReplayEvent::Interact => interact = true,
                ReplayEvent::Click(x, y) => click = Some((x, y)),
                ReplayEvent::Steer(s) => self.steer = s,
                ref other => dispatch_replay_event(rt, other),
            }
            on_after_event(rt, &ev);
        }

        let input = TickInput {
            held_dir: self.held_dir,
            interact,
            click,
            steer: self.steer,
        };
        rt.tick(input);
        self.seg_tick += 1;

        if self.event_idx >= seg_ev_len && self.seg_tick >= seg_dur {
            if self.seg_idx + 1 < self.bundle.segments.len() {
                ReplayStepOutcome::SegmentCompleted {
                    next_idx: self.seg_idx + 1,
                }
            } else {
                self.finished = true;
                ReplayStepOutcome::Finished
            }
        } else {
            ReplayStepOutcome::Stepped
        }
    }
}
