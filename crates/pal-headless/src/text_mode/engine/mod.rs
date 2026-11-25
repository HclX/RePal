//! Unified `TextEngine` command executor and multi-tick state machine.
//!
//! Runs identically when pumped synchronously in headless `--text` mode (zero rendering overhead)
//! or stepped one 20 ms tick per frame inside `repal`'s 50 Hz GUI loop (`--gui --text` / `--listen`).


mod helpers;
mod step;
mod submit_debug;
mod submit_nav;
mod submit_rpg_io;

#[cfg(feature = "disk")]
pub use helpers::{
    confine_socket_path, confine_socket_write_path, default_magic_target, load_save_into_rt,
    resolve_save_path,
};
#[cfg(not(feature = "disk"))]
pub use helpers::default_magic_target;
use helpers::*;
#[cfg(feature = "disk")]
use crate::replay::{apply_named_view_rt, compare_with_golden_png_fb, run_core_replay_rt};
use crate::text_mode::command::{parse_command, Command};
use crate::text_mode::response::{
    build_response_json_ex, current_mode, dir_code, event_visible_in_viewport,
    is_fair_visible_exit, item_target_kind, magic_target_kind, runtime_camera, ResponseOpts,
};
use crate::text_mode::{ClientOrigin, MinimapMode, ObsMode, TextHost};
#[cfg(feature = "disk")]
use crate::{export_fb_to_png, non_black_pixel_ratio_fb};
use pal_core::game::{BattleCmd, TickInput, MOVE_TICKS};
use pal_ui::{PointerEvent, PointerKind, UiAction, UiKey, UiView};
use std::collections::HashMap;
#[cfg(feature = "disk")]
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Single command reply produced by [`TextEngine`].
#[derive(Clone, Debug)]
pub struct TextReply {
    pub response: serde_json::Value,
    pub show_events: bool,
    pub golden_failed: bool,
    pub bye: bool,
}

/// Result of submitting a command line to [`TextEngine`].
pub enum ExecOutcome {
    Done(TextReply),
    Pending,
    Quit(TextReply),
}

/// Pre-use observable state snapshot so Text Mode `use` distinguishes real world effects
/// (e.g. `破天锤 #279` smashing Asura statues despite SDLPAL sticky `0x0081` false flag) from
/// true no-op refusals (e.g. full-HP heal item, #234).
#[derive(Clone, Debug)]
struct UseItemSnapshot {
    item_id: u16,
    scene_id: u16,
    inventory: Vec<(u16, i32)>,
    party_vitals: Vec<(i32, i32, usize)>,
    event_states: Vec<(u16, i32, i32)>,
}

impl UseItemSnapshot {
    fn capture(rt: &pal_runtime::PalRuntime, item_id: u16) -> Self {
        let Some(g) = rt.game.as_ref() else {
            return Self {
                item_id,
                scene_id: 0,
                inventory: Vec::new(),
                party_vitals: Vec::new(),
                event_states: Vec::new(),
            };
        };
        let mut inventory: Vec<(u16, i32)> = g
            .world
            .inventory
            .iter()
            .filter(|(_, &q)| q > 0)
            .map(|(&id, &q)| (id, q))
            .collect();
        inventory.sort_by_key(|(id, _)| *id);
        let party_vitals = g
            .world
            .roles
            .iter()
            .map(|r| (r.hp, r.mp, r.poisons.len()))
            .collect();
        let event_states = g
            .world
            .current_event_ids()
            .into_iter()
            .filter_map(|id| g.world.event(id).map(|e| (id, e.state, e.vanish_time)))
            .collect();
        Self {
            item_id,
            scene_id: g.world.scene_id,
            inventory,
            party_vitals,
            event_states,
        }
    }

    fn diff_effects(&self, rt: &pal_runtime::PalRuntime, mode: &str) -> Vec<&'static str> {
        let mut effects = Vec::new();
        if matches!(mode, "dialog" | "choice") {
            effects.push("dialog_opened");
        }
        if matches!(mode, "battle" | "battle_anim") {
            effects.push("battle_started");
        }
        let Some(g) = rt.game.as_ref() else {
            return effects;
        };
        if g.world.scene_id != self.scene_id {
            effects.push("scene_changed");
        }
        let mut now_inv: Vec<(u16, i32)> = g
            .world
            .inventory
            .iter()
            .filter(|(_, &q)| q > 0)
            .map(|(&id, &q)| (id, q))
            .collect();
        now_inv.sort_by_key(|(id, _)| *id);
        if now_inv != self.inventory {
            effects.push("inventory_changed");
        }
        let now_vitals: Vec<(i32, i32, usize)> = g
            .world
            .roles
            .iter()
            .map(|r| (r.hp, r.mp, r.poisons.len()))
            .collect();
        if now_vitals != self.party_vitals {
            effects.push("party_changed");
        }
        if g.world.scene_id == self.scene_id {
            let now_events: Vec<(u16, i32, i32)> = g
                .world
                .current_event_ids()
                .into_iter()
                .filter_map(|id| g.world.event(id).map(|e| (id, e.state, e.vanish_time)))
                .collect();
            if now_events != self.event_states {
                effects.push("event_changed");
            }
        }
        effects
    }
}

#[derive(Clone, Debug)]
struct TalkCheck {
    target_xy: (i32, i32),
    start_scene: u16,
    start_notice_seq: u64,
    snapshot: UseItemSnapshot,
}

enum ActiveTask {
    WalkDir {
        cmd: String,
        dir: u8,
        target_steps: usize,
        moved_steps: usize,
        last_pos: (i32, i32),
        stall_ticks: u32,
        start_scene: u16,
    },
    Goto {
        cmd: String,
        target_xy: (i32, i32),
        click_xy: (i32, i32),
        target_ev_id: Option<u16>,
        target_is_touch_exit: bool,
        talk_after: bool,
        start_scene: u16,
        steps_taken: usize,
        last_pos: (i32, i32),
        stall_ticks: u32,
        max_ticks: u32,
    },
    TurnAndTalk {
        cmd: String,
        dir: u8,
        target_xy: (i32, i32),
        start_scene: u16,
        turned: bool,
        log: Vec<String>,
    },
    ContinueDialog {
        cmd: String,
        cooldown: u32,
        pace_ticks: u32,
        max_steps: u32,
        elapsed_ticks: u32,
        max_ticks: u32,
        waited_cutscene: bool,
        last_logged_dialog: Option<(String, String)>,
        log: Vec<String>,
    },
    Settle {
        cmd: String,
        min_ticks: u32,
        max_ticks: u32,
        elapsed: u32,
        show_events_on_explore: bool,
        use_item_snapshot: Option<UseItemSnapshot>,
        expect_leave_choice: bool,
        /// Pre-interaction snapshot when verifying `talk <ev>` achieved an interaction (#278).
        talk_check: Option<TalkCheck>,
        interrupted_by: Option<&'static str>,
        log: Vec<String>,
    },
    WaitTicks {
        cmd: String,
        remaining: usize,
        total: usize,
    },
}

impl ActiveTask {
    fn settle(
        cmd: impl Into<String>,
        min_ticks: u32,
        max_ticks: u32,
        show_events_on_explore: bool,
        log: Vec<String>,
    ) -> Self {
        Self::Settle {
            cmd: cmd.into(),
            min_ticks,
            max_ticks,
            elapsed: 0,
            show_events_on_explore,
            use_item_snapshot: None,
            expect_leave_choice: false,
            talk_check: None,
            interrupted_by: None,
            log,
        }
    }
}

/// Per-client write quotas enforced on non-loopback remote TCP clients (#262).
const MAX_REMOTE_WRITES_PER_CLIENT: usize = 64;
const MAX_REMOTE_WRITE_BYTES_PER_CLIENT: u64 = 64 * 1024 * 1024;

pub struct TextEngine {
    pub fog: bool,
    pub obs_mode: ObsMode,
    pub minimap_mode: MinimapMode,
    pub pace_ticks: u32,
    pub god_mode: bool,
    /// When `false` (default), non-loopback TCP clients cannot execute debug/load/core-replay/view (#193).
    pub allow_unsafe_remote: bool,
    pub shot_counter: usize,
    pub golden_failed_any: bool,
    last_notice_seq: u64,
    /// Per-TCP-client notice cursors keyed by `ClientOrigin::client_id()` (#233).
    client_notice_cursors: HashMap<u64, u64>,
    /// Per-remote-client disk write counters `(file_count, total_bytes)` (#262).
    remote_write_usage: HashMap<u64, (usize, u64)>,
    active_origin: ClientOrigin,
    last_reported_scene: Option<u16>,
    last_battle_feed_total: u64,
    last_cmd_was_think: bool,
    active_task: Option<ActiveTask>,
    #[cfg(feature = "disk")]
    capture: Option<crate::capture::FrameRecorder>,
}

impl TextEngine {
    pub fn new(fog: bool, pace_ticks: u32) -> Self {
        Self::with_obs(if fog { ObsMode::Fair } else { ObsMode::Debug }, pace_ticks)
    }

    pub fn with_obs(obs_mode: ObsMode, pace_ticks: u32) -> Self {
        Self {
            fog: obs_mode.is_fair(),
            obs_mode,
            minimap_mode: MinimapMode::Auto,
            pace_ticks,
            god_mode: false,
            allow_unsafe_remote: false,
            shot_counter: 0,
            golden_failed_any: false,
            last_notice_seq: 0,
            client_notice_cursors: HashMap::new(),
            remote_write_usage: HashMap::new(),
            active_origin: ClientOrigin::Local,
            last_reported_scene: None,
            last_battle_feed_total: 0,
            last_cmd_was_think: false,
            active_task: None,
            #[cfg(feature = "disk")]
            capture: None,
        }
    }

    /// Capture one rendered frame into the active [`FrameRecorder`] (if `capture start` is running).
    #[cfg(feature = "disk")]
    pub fn capture_tick_if_active<H: TextHost>(&mut self, host: &mut H) {
        if self.capture.is_none() {
            return;
        }
        host.render_settled_frame();
        let with_thought = self.capture.as_ref().is_some_and(|c| c.with_thought());
        let res = if with_thought && host.runtime().has_visible_ai_thought_overlay() {
            let mut fb_copy = host.runtime().compositor.fb.clone();
            host.runtime_mut().draw_ai_thought_overlay_onto(&mut fb_copy);
            self.capture.as_mut().unwrap().push_tick(&fb_copy)
        } else {
            let fb = &host.runtime().compositor.fb;
            self.capture.as_mut().unwrap().push_tick(fb)
        };
        if let Err(e) = res {
            eprintln!("[repal] capture error (stopping capture): {e}");
            let _ = self.capture.take().map(|c| c.finish());
        }
    }

    #[cfg(not(feature = "disk"))]
    pub fn capture_tick_if_active<H: TextHost>(&mut self, _host: &mut H) {}

    fn check_remote_write_quota(&mut self, origin: ClientOrigin, est_bytes: u64) -> Result<(), String> {
        if !origin.is_remote() || self.allow_unsafe_remote {
            return Ok(());
        }
        let cid = origin.client_id();
        let entry = self.remote_write_usage.entry(cid).or_insert((0, 0));
        if entry.0 >= MAX_REMOTE_WRITES_PER_CLIENT {
            return Err(format!(
                "remote write quota exceeded (max {MAX_REMOTE_WRITES_PER_CLIENT} files per session)"
            ));
        }
        if entry.1.saturating_add(est_bytes) > MAX_REMOTE_WRITE_BYTES_PER_CLIENT {
            return Err(format!(
                "remote write byte quota exceeded (max {} MiB per session)",
                MAX_REMOTE_WRITE_BYTES_PER_CLIENT / (1024 * 1024)
            ));
        }
        Ok(())
    }

    fn record_remote_write(&mut self, origin: ClientOrigin, bytes: u64) {
        if !origin.is_remote() || self.allow_unsafe_remote {
            return;
        }
        let cid = origin.client_id();
        let entry = self.remote_write_usage.entry(cid).or_insert((0, 0));
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(bytes);
    }

    pub fn effective_obs_mode(&self) -> ObsMode {
        if self.fog || self.obs_mode.is_fair() {
            ObsMode::Fair
        } else {
            ObsMode::Debug
        }
    }

    pub fn has_active_task(&self) -> bool {
        self.active_task.is_some()
    }

    pub fn cancel_active_task(&mut self) {
        self.active_task = None;
    }

    /// Build a structured error reply when an in-flight multi-tick task is cancelled by the host
    /// (e.g. user drops a save/replay onto the GUI or switches Agent Mode), so TCP/stdio clients
    /// receive a clean JSON/plain response instead of a dropped connection.
    pub fn cancelled_reply<H: TextHost>(&self, host: &H) -> TextReply {
        self.stateless_err_reply(host, "", "cancelled", "task cancelled by host")
    }

    /// Build a structured error reply when a human player cancels an in-flight agent task
    /// (e.g. pressing Esc in GUI+Text mode, #228).
    pub fn cancelled_reply_with_reason<H: TextHost>(
        &self,
        host: &H,
        interrupted_by: &str,
        msg: &str,
    ) -> TextReply {
        let rt = host.runtime();
        let resp = build_response_json_ex(
            rt,
            false,
            "",
            serde_json::json!({}),
            &[],
            ResponseOpts {
                obs_mode: self.effective_obs_mode(),
                error: Some(msg),
                error_code: Some("cancelled"),
                interrupted_by: Some(interrupted_by),
                rng: host.rng_status(),
                ..Default::default()
            },
        );
        TextReply {
            response: resp,
            show_events: false,
            golden_failed: false,
            bye: false,
        }
    }

    /// Public wrapper for hosts rejecting disallowed commands (e.g. mutating commands during replay, #196).
    pub fn err_reply_public<H: TextHost>(&self, host: &H, cmd: &str, msg: &str) -> TextReply {
        self.stateless_err_reply(host, cmd, "disallowed", msg)
    }

    /// Build a fresh `look` reply immediately (e.g. initial greeting on connect/startup).
    pub fn initial_look<H: TextHost>(&self, host: &H) -> TextReply {
        TextReply {
            response: build_response_json_ex(
                host.runtime(),
                true,
                "look",
                serde_json::json!({}),
                &[],
                ResponseOpts {
                    obs_mode: self.effective_obs_mode(),
                    rng: host.rng_status(),
                    ..Default::default()
                },
            ),
            show_events: true,
            golden_failed: false,
            bye: false,
        }
    }

    /// Submit one command line from a local caller (stdio/batch/tests).
    pub fn submit<H: TextHost>(
        &mut self,
        host: &mut H,
        raw: &str,
        interactive_blank: bool,
    ) -> ExecOutcome {
        self.submit_from(host, raw, interactive_blank, ClientOrigin::Local)
    }

    /// Submit one command line with explicit transport provenance (`ClientOrigin`) for
    /// per-client notice cursors (#233) and socket/remote security policy (#193).

    /// Performs zero rendering unless the command itself calls `render_settled_frame()` (`shot`/`golden`/`click`).
    pub fn execute_sync<H: TextHost>(
        &mut self,
        host: &mut H,
        raw: &str,
        interactive_blank: bool,
    ) -> TextReply {
        self.execute_sync_from(host, raw, interactive_blank, ClientOrigin::Local)
    }

    /// Execute one command synchronously with explicit [`ClientOrigin`] provenance (#193, #233).
    pub fn execute_sync_from<H: TextHost>(
        &mut self,
        host: &mut H,
        raw: &str,
        interactive_blank: bool,
        origin: ClientOrigin,
    ) -> TextReply {
        match self.submit_from(host, raw, interactive_blank, origin) {
            ExecOutcome::Done(r) | ExecOutcome::Quit(r) => {
                self.capture_tick_if_active(host);
                r
            }
            ExecOutcome::Pending => {
                // Capture the post-submit frame (e.g. menu closing on `ui ok` before `video_redraw` finishes).
                self.capture_tick_if_active(host);
                // Tight simulation pump — zero compositor calls unless `capture` is active
                let mut guard = 0usize;
                loop {
                    let (input, done) = self.step_tick(host);
                    if let Some(reply) = done {
                        return reply;
                    }
                    host.runtime_mut().tick(input);
                    host.service_host_cmds();
                    self.capture_tick_if_active(host);
                    guard += 1;
                    if guard > 4000 {
                        self.active_task = None;
                        return self.err_reply(host, raw.trim(), "command timed out waiting to settle");
                    }
                }
            }
        }
    }

    /// Settle an initial state (e.g. right after `--new` or `--save`) synchronously with zero rendering.
    pub fn settle_sync<H: TextHost>(&mut self, host: &mut H, max_ticks: u32) {
        self.active_task = Some(ActiveTask::settle("", 0, max_ticks, false, Vec::new()));
        loop {
            let (input, done) = self.step_tick(host);
            if done.is_some() {
                break;
            }
            host.runtime_mut().tick(input);
            host.service_host_cmds();
        }
    }

    // ---- Internal helpers ------------------------------------------------------------------

    fn apply_god_mode<H: TextHost>(&self, host: &mut H) {
        if !self.god_mode {
            return;
        }
        if let Some(g) = host.runtime_mut().game.as_mut() {
            // Re-assert deterministic god_mode across `new`/`load` session resets while /god is ON (#235, #236).
            if !g.debug_mode || !g.god_mode || !g.recorder.tainted_by_debug || !g.recorder.debug_mode {
                g.set_debug_mode(true);
                let _ = g.apply_debug_action(&pal_core::game::replay_rec::DebugAction::God(true));
            }
        }
    }

    fn dispatch_ui_key<H: TextHost>(
        &mut self,
        host: &mut H,
        raw: &str,
        uk: UiKey,
    ) -> ExecOutcome {
        if matches!(uk, UiKey::Confirm | UiKey::Cancel) && host.skip_rng_if_active() {
            self.active_task = Some(ActiveTask::settle(
                raw,
                self.pace_ticks.min(4),
                90,
                true,
                vec![format!("[key] {uk:?} (skipped RNG)")],
            ));
            return ExecOutcome::Pending;
        }
        let was_in_battle = host
            .runtime()
            .game
            .as_ref()
            .and_then(|g| g.battle.as_ref())
            .is_some_and(|b| b.active);
        let act = host.runtime_mut().on_key(uk);
        let is_flee = matches!(act, Some(UiAction::BattleFlee));
        if let Some(act) = act {
            host.handle_ui_action(act);
        }
        host.service_host_cmds();
        let in_battle = host
            .runtime()
            .game
            .as_ref()
            .and_then(|g| g.battle.as_ref())
            .is_some_and(|b| b.active);
        self.active_task = Some(ActiveTask::settle(
            raw,
            if is_flee { 1 } else { self.pace_ticks.min(4) },
            if in_battle || was_in_battle { 480 } else { 90 },
            false,
            vec![format!("[key] {uk:?}")],
        ));
        ExecOutcome::Pending
    }

    fn start_goto<H: TextHost>(
        &mut self,
        host: &mut H,
        raw: &str,
        target_xy: (i32, i32),
        label: String,
        target_ev: Option<(u16, bool)>,
        talk_after: bool,
    ) -> ExecOutcome {
        if current_mode(host.runtime()) != "explore" {
            return ExecOutcome::Done(self.err_reply(
                host,
                raw,
                &format!("cannot walk in mode '{}'", current_mode(host.runtime())),
            ));
        }
        let (start_scene, cur_pos) = leader_scene_pos(host.runtime());
        let start_seq = host.runtime().latest_notice_seq();
        let pre_snap = talk_after.then(|| UseItemSnapshot::capture(host.runtime(), 0));
        // Drive movement & NPC interaction purely through standard player `TickInput::click`
        // (matching how a human mouse player clicks the world/NPC so `.repal` stays 100% pure
        // player inputs with zero Text-Mode-only opcodes, #244):
        // - For `talk <ev>`: click exact `(ev.x, ev.y)` (already 10px < 32px from `ev.y - 10`, so
        //   `find_event_near_point` matches cleanly without shifting A* north into walls).
        // - For `goto <target>`: if `(tx, ty)` lies inside any interactive event's 32px click box,
        //   pick the nearest walkable tile just outside all interactive click boxes so `goto`
        //   walks near the NPC without unexpectedly triggering dialogue (#240, #244).
        let click_xy = if talk_after {
            target_xy
        } else {
            pick_non_talking_click_target(host.runtime(), target_xy.0, target_xy.1)
        };

        host.on_click_ripple(click_xy.0, click_xy.1);
        host.on_toast(&format!("前往 · {label}"));

        host.runtime_mut().tick(TickInput {
            click: Some(click_xy),
            ..TickInput::IDLE
        });
        host.service_host_cmds();

        let (post_scene, post_pos) = leader_scene_pos(host.runtime());
        let post_mode = current_mode(host.runtime());
        let walking = host
            .runtime()
            .game
            .as_ref()
            .map(|g| g.is_auto_walking())
            .unwrap_or(false);

        if post_scene != start_scene || post_mode != "explore" {
            self.active_task = Some(ActiveTask::Settle {
                cmd: raw.to_string(),
                min_ticks: self.pace_ticks.min(6),
                max_ticks: 180,
                elapsed: 0,
                show_events_on_explore: true,
                use_item_snapshot: None,
                expect_leave_choice: false,
                talk_check: pre_snap.map(|snapshot| TalkCheck {
                    target_xy,
                    start_scene,
                    start_notice_seq: start_seq,
                    snapshot,
                }),
                interrupted_by: None,
                log: Vec::new(),
            });
            return ExecOutcome::Pending;
        }

        if !walking && talk_after {
            return self.trigger_interact_and_settle(host, raw, Some(target_xy));
        }

        let steps_taken = usize::from(post_pos != cur_pos);
        self.active_task = Some(ActiveTask::Goto {
            cmd: raw.to_string(),
            target_xy,
            click_xy,
            target_ev_id: target_ev.map(|(id, _)| id),
            target_is_touch_exit: target_ev.is_some_and(|(_, is_exit)| is_exit),
            talk_after,
            start_scene,
            steps_taken,
            last_pos: post_pos,
            stall_ticks: 0,
            max_ticks: 800,
        });
        ExecOutcome::Pending
    }

    fn trigger_interact_and_settle<H: TextHost>(
        &mut self,
        host: &mut H,
        raw: &str,
        talk_target_xy: Option<(i32, i32)>,
    ) -> ExecOutcome {
        let (start_sc, _) = leader_scene_pos(host.runtime());
        let start_seq = host.runtime().latest_notice_seq();
        let pre_snap = talk_target_xy.map(|_| UseItemSnapshot::capture(host.runtime(), 0));
        host.runtime_mut().tick(TickInput {
            interact: true,
            ..TickInput::IDLE
        });
        host.service_host_cmds();
        self.active_task = Some(ActiveTask::Settle {
            cmd: raw.to_string(),
            min_ticks: self.pace_ticks.min(6),
            max_ticks: 180,
            elapsed: 0,
            show_events_on_explore: true,
            use_item_snapshot: None,
            expect_leave_choice: false,
            talk_check: talk_target_xy.zip(pre_snap).map(|(target_xy, snapshot)| TalkCheck {
                target_xy,
                start_scene: start_sc,
                start_notice_seq: start_seq,
                snapshot,
            }),
            interrupted_by: None,
            log: Vec::new(),
        });
        ExecOutcome::Pending
    }

    fn submit_battle_cmd<H: TextHost>(
        &mut self,
        host: &mut H,
        raw: &str,
        bc: BattleCmd,
    ) -> ExecOutcome {
        let in_battle = host
            .runtime()
            .game
            .as_ref()
            .and_then(|g| g.battle.as_ref())
            .is_some_and(|b| b.active);
        if !in_battle {
            return ExecOutcome::Done(self.err_reply_code(host, raw, "wrong_mode", "not in battle"));
        }
        let accepted = host
            .runtime_mut()
            .game
            .as_mut()
            .map(|g| g.battle_command(bc))
            .unwrap_or(false);
        if !accepted {
            return ExecOutcome::Done(self.err_reply_code(
                host,
                raw,
                "wrong_mode",
                "battle is not currently prompting for a command",
            ));
        }
        if matches!(host.runtime().ui.view, UiView::BattleHud(_)) {
            host.runtime_mut().ui.set_view(UiView::None);
            host.runtime_mut().compositor.mark_ui_dirty();
        }
        host.runtime_mut().route_cmds();
        host.service_host_cmds();
        self.active_task = Some(ActiveTask::settle(raw, 1, 600, true, Vec::new()));
        ExecOutcome::Pending
    }

    fn make_reply<H: TextHost>(
        &mut self,
        host: &H,
        ok: bool,
        cmd: &str,
        show_events: bool,
        artifacts: serde_json::Value,
        log: Vec<String>,
        interrupted_by: Option<&str>,
        error: Option<&str>,
        error_code: Option<&str>,
    ) -> TextReply {
        let rt = host.runtime();
        let cid = self.active_origin.client_id();
        let since_seq = if cid > 0 {
            *self.client_notice_cursors.get(&cid).unwrap_or(&rt.latest_notice_seq())
        } else {
            self.last_notice_seq
        };
        let (notices, next_seq) = rt.notices_since(since_seq);
        if cid > 0 {
            self.client_notice_cursors.insert(cid, next_seq);
        } else {
            self.last_notice_seq = next_seq;
        }
        let battle_feed = rt
            .game
            .as_ref()
            .and_then(|g| g.battle.as_ref())
            .filter(|b| b.active)
            .map(|b| {
                let new_count = if b.feed_total >= self.last_battle_feed_total {
                    (b.feed_total - self.last_battle_feed_total) as usize
                } else {
                    b.feed.len()
                };
                self.last_battle_feed_total = b.feed_total;
                let take = new_count.min(b.feed.len());
                b.feed[b.feed.len() - take..].to_vec()
            })
            .unwrap_or_else(|| {
                self.last_battle_feed_total = 0;
                Vec::new()
            });
        let mode_now = current_mode(rt);
        let cur_scene = rt.game.as_ref().map(|g| g.world.scene_id);
        let scene_changed = mode_now == "explore" && cur_scene != self.last_reported_scene;
        let cmd_lower = cmd.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        let explicit_map = matches!(cmd_lower.as_str(), "map" | "minimap" | "地图");
        let is_look = matches!(cmd_lower.as_str(), "look" | "l");
        let is_blocked_move = matches!(error_code, Some("blocked") | Some("unreachable"));
        let include_minimap = mode_now == "explore"
            && (explicit_map
                || match self.minimap_mode {
                    MinimapMode::Always => true,
                    MinimapMode::Off => false,
                    MinimapMode::Auto => is_look || scene_changed || is_blocked_move,
                });
        if mode_now == "explore" {
            self.last_reported_scene = cur_scene;
        }
        let resp = build_response_json_ex(
            rt,
            ok,
            cmd,
            artifacts,
            &log,
            ResponseOpts {
                obs_mode: self.effective_obs_mode(),
                include_minimap,
                error,
                error_code,
                interrupted_by,
                notices: &notices,
                battle_feed: &battle_feed,
                rng: host.rng_status(),
            },
        );
        TextReply {
            response: resp,
            show_events,
            golden_failed: false,
            bye: false,
        }
    }

    fn ok_reply<H: TextHost>(
        &mut self,
        host: &H,
        cmd: &str,
        show_events: bool,
        artifacts: serde_json::Value,
        log: Vec<String>,
    ) -> TextReply {
        self.make_reply(host, true, cmd, show_events, artifacts, log, None, None, None)
    }

    fn err_reply<H: TextHost>(&mut self, host: &H, cmd: &str, msg: &str) -> TextReply {
        self.make_reply(
            host,
            false,
            cmd,
            false,
            serde_json::json!({}),
            Vec::new(),
            None,
            Some(msg),
            None,
        )
    }

    fn err_reply_code<H: TextHost>(
        &mut self,
        host: &H,
        cmd: &str,
        code: &str,
        msg: &str,
    ) -> TextReply {
        self.make_reply(
            host,
            false,
            cmd,
            false,
            serde_json::json!({}),
            Vec::new(),
            None,
            Some(msg),
            Some(code),
        )
    }

    fn stateless_err_reply<H: TextHost>(
        &self,
        host: &H,
        cmd: &str,
        code: &str,
        msg: &str,
    ) -> TextReply {
        TextReply {
            response: build_response_json_ex(
                host.runtime(),
                false,
                cmd,
                serde_json::json!({}),
                &[],
                ResponseOpts {
                    obs_mode: self.effective_obs_mode(),
                    error: Some(msg),
                    error_code: Some(code),
                    rng: host.rng_status(),
                    ..Default::default()
                },
            ),
            show_events: false,
            golden_failed: false,
            bye: false,
        }
    }
}

