//! Deterministic, host-agnostic game core.
//!
//! [`Game`] owns the authoritative [`World`] state, the assembled script [`Program`] and the
//! script VM. It advances only through [`Game::tick`] (one fixed logic step of [`TICK_MS`]) and
//! [`Game::reply`] (answers to [`HostCmd`]s such as "dialogue dismissed" or "scene assets
//! loaded"). It never reads clocks or platform randomness, so the same inputs always produce the
//! same state — this is what makes replays and native story tests possible.
//!
//! The host (browser JS, `pal-headless`, or a test) is responsible for loading assets, showing UI
//! and playing audio in response to drained [`HostCmd`]s.

pub mod battle;
pub mod data;
mod explore;
mod menu;
pub mod replay_rec;
mod save;
mod snapshot;
mod vm;
pub mod world;

use crate::isometric::{world_to_half_tile, CollisionGrid};
use crate::npc_ai::NpcWalkTracker;
use crate::script::{LabelId, Program};
use data::GameData;
use std::collections::{BTreeMap, HashMap, VecDeque};
use world::{DialogueStyle, World};

pub use battle::{BattleCmd, BattleCtx, BattleEnemy};
pub use replay_rec::{
    ReplayAnchor, ReplayBundle, ReplayEntry, ReplayEvent, ReplayRecorder, ReplaySegment,
};
pub use vm::{FgKind, Wait};

/// Duration of one logic tick in milliseconds (50 Hz).
pub const TICK_MS: u32 = 20;
/// Player walking cadence (legacy JS: one step per 85 ms).
pub const MOVE_TICKS: u32 = 4;
/// Background NPC auto-script cadence (legacy JS: every 120 ms).
pub const AUTO_TICKS: u32 = 6;

/// Convert a legacy millisecond delay into whole ticks (at least one).
pub const fn ms_to_ticks(ms: u32) -> u32 {
    let t = (ms + TICK_MS / 2) / TICK_MS;
    if t == 0 {
        1
    } else {
        t
    }
}

/// Requests from the core to the host. Drain them with [`Game::drain_cmds`] after every call.
#[derive(Clone, Debug, PartialEq)]
pub enum HostCmd {
    PlayMusic { track: i32, looped: bool },
    StopMusic,
    PlaySfx(i32),
    /// Show a dialogue box and reply [`HostReply::DialogueDone`] once dismissed.
    ShowDialogue { speaker: String, text: String, side: u8, portrait: i32 },
    HideDialogue,
    /// Ask a yes/no question and reply [`HostReply::Choice`].
    AskYesNo,
    /// Open a shop and reply [`HostReply::ModalDone`] when closed.
    OpenShop(i32),
    OpenPawnshop,
    /// A battle started / ended: switch the HUD into / out of battle mode.
    BattleStarted,
    BattleEnded,
    /// Ask the player for hero `hero`'s command; answer with [`Game::battle_command`].
    BattlePrompt { hero: usize },
    /// Show the victory screen and reply [`HostReply::ModalDone`] when dismissed.
    BattleVictory { exp: i32, cash: i32, level_ups: Vec<String> },
    /// Play an RNG cutscene and reply [`HostReply::Done`].
    PlayRng { id: i32, start: i32, end: i32, speed: i32 },
    Toast(String),
    /// Load the scene's collision map and sprite metadata, then reply [`HostReply::SceneLoaded`].
    LoadScene { scene_id: u16, map_id: u16 },
    /// Load a script chapter (via [`Game::load_chapter`]) and reply [`HostReply::Done`].
    LoadChapter(u32),
    ReturnToTitle,
    /// The party was defeated in a battle without a defeat branch.
    GameOver,
    SceneChanged(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BattleOutcome {
    Victory,
    Defeat,
    Flee,
}

/// Answers to blocking [`HostCmd`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostReply {
    DialogueDone,
    Choice(bool),
    ModalDone,
    SceneLoaded,
    Done,
}

/// Player input sampled for one tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickInput {
    /// Held movement direction (`0=SW, 1=NW, 2=NE, 3=SE`; `4=W, 5=N, 6=E, 7=S` alternate the two
    /// adjacent diagonals, see `held_step`) or `-1`.
    pub held_dir: i8,
    /// Interact/confirm pressed this tick (Space/Enter in exploration).
    pub interact: bool,
    /// Clicked world position this tick (click-to-walk / click-to-talk).
    pub click: Option<(i32, i32)>,
    /// Press-and-move steering target (world position) while the pointer is held.
    pub steer: Option<(i32, i32)>,
}

impl TickInput {
    pub const IDLE: TickInput = TickInput { held_dir: -1, interact: false, click: None, steer: None };
}

/// Script-VM context flags shared by all foreground chains (legacy `ScriptVM` fields).
#[derive(Clone, Debug, Default)]
pub struct VmContext {
    pub dialogue: DialogueStyle,
    pub need_fade_in: bool,
    pub pending_scene_enter: bool,
    pub current_target_role: usize,
    pub target_item_all: bool,
    pub pending_item_triggered_event: u16,
    pub item_script_failed: bool,
    pub using_item: bool,
    pub last_label: LabelId,
}

/// Optional VM execution coverage counter (Layer 4 testing; `None` in production for zero overhead).
#[derive(Clone, Debug, Default)]
pub struct ScriptCoverage {
    /// Count of executions per `(label_id, step_idx)`.
    pub steps: HashMap<(LabelId, u32), u64>,
    /// Count of executions per opcode (`u16`).
    pub opcodes: HashMap<u16, u64>,
    /// Number of times a frame hit its `max_ops` step guard.
    pub guard_trips: u64,
}

pub struct Game {
    pub data: GameData,
    pub prog: Program,
    pub world: World,
    base_grid: CollisionGrid,
    pub grid: CollisionGrid,
    /// Frame counts per MGO sprite id, reported by the host from `mgo_XXX.json`.
    pub sprite_frame_counts: HashMap<u16, i32>,
    /// Per-frame logical `(width, height)` per MGO sprite id from `mgo_XXX.json` (#234).
    pub sprite_frame_dims: HashMap<u16, Vec<(i32, i32)>>,
    /// Cached per-map GOP tile visual flags (black frame-0 void & opaque elevated tiles, #234).
    pub gop_visual_flags: HashMap<u16, crate::isometric::GopVisualFlags>,
    npc_walks: HashMap<u16, NpcWalkTracker>,
    fg: Option<vm::FgThread>,
    cmds: Vec<HostCmd>,
    pub vmc: VmContext,
    pub battle: Option<BattleCtx>,
    pub tick_count: u64,
    pub in_title: bool,
    pub menu_open: bool,
    /// Set while the host is loading scene assets requested outside a foreground script.
    loading_scene: bool,
    move_cd: u32,
    auto_cd: u32,
    auto_walk: VecDeque<(i32, i32)>,
    pending_click_event: u16,
    suppressed: BTreeMap<u16, (i32, bool)>,
    just_changed_scene: bool,
    /// Deferred manual-save replay split waiting for the next clean idle overworld tick (#272).
    pending_manual_split: bool,
    /// Result of the most recent menu item use (`true` = used / consumed as applicable).
    pub last_item_result: Option<bool>,
    /// Heroes of the battle that just ended (for scripted-defeat revival).
    last_battle_heroes: Vec<usize>,
    /// Hidden pre-battle autosave snapshot (not accessible from the player's Save/Load menu).
    pre_battle_save: Option<serde_json::Value>,
    /// Unified engine-level deterministic input recorder (`RePal_CoreReplay_v2`).
    pub recorder: ReplayRecorder,
    /// Explicit session debug-mode gate (#235): when `false`, all `DebugAction` mutations are
    /// rejected / treated as no-ops (both live and during replay playback). When `true`, debug
    /// actions execute deterministically, record `ReplayEvent::Debug`, and render `DEBUG ENABLED`.
    pub debug_mode: bool,
    /// Deterministic god-mode flag (active only when `debug_mode == true`, #235).
    pub god_mode: bool,
    /// Ring buffer of the last foreground ops (`label[idx]:op`) for diagnostics.
    pub trace: VecDeque<String>,
    /// Optional VM execution coverage counter (enabled in tests only).
    pub coverage: Option<ScriptCoverage>,
}

impl Game {
    /// Create a core from fully loaded static tables. Scripts are added with [`Game::load_chapter`].
    pub fn new(data: GameData) -> Self {
        let mut prog = Program::new();
        let world = World::new_game(&data, &mut prog, crate::rng::GameRng::default().state);
        Self {
            data,
            prog,
            world,
            base_grid: CollisionGrid::empty(),
            grid: CollisionGrid::empty(),
            sprite_frame_counts: HashMap::new(),
            sprite_frame_dims: HashMap::new(),
            gop_visual_flags: HashMap::new(),
            npc_walks: HashMap::new(),
            fg: None,
            cmds: Vec::new(),
            vmc: VmContext::default(),
            battle: None,
            tick_count: 0,
            in_title: true,
            menu_open: false,
            loading_scene: false,
            move_cd: 0,
            auto_cd: 0,
            auto_walk: VecDeque::new(),
            pending_click_event: 0,
            suppressed: BTreeMap::new(),
            just_changed_scene: false,
            pending_manual_split: false,
            last_item_result: None,
            last_battle_heroes: Vec::new(),
            pre_battle_save: None,
            recorder: ReplayRecorder::default(),
            debug_mode: false,
            god_mode: false,
            trace: VecDeque::new(),
            coverage: None,
        }
    }

    /// Assemble a chapter YAML document (as JSON) into the program.
    pub fn load_chapter(&mut self, ch: u32, doc: &serde_json::Value) -> Result<usize, String> {
        self.prog.load_chapter(ch, doc).map_err(|e| e.0)
    }

    pub fn drain_cmds(&mut self) -> Vec<HostCmd> {
        std::mem::take(&mut self.cmds)
    }

    pub(crate) fn emit(&mut self, cmd: HostCmd) {
        self.cmds.push(cmd);
    }

    pub fn is_busy(&self) -> bool {
        self.fg.is_some()
    }

    /// What the foreground script is currently waiting for, if anything.
    pub fn waiting_on(&self) -> Option<&Wait> {
        self.fg.as_ref().map(|t| &t.wait)
    }

    /// Ticks are no-ops while the host is loading assets or playing a cutscene.
    pub fn is_blocked_on_host(&self) -> bool {
        self.loading_scene || matches!(self.waiting_on(), Some(Wait::SceneAssets | Wait::Chapter(_) | Wait::Host))
    }

    pub fn is_loading_scene(&self) -> bool {
        self.loading_scene || matches!(self.waiting_on(), Some(Wait::SceneAssets))
    }

    pub fn is_auto_walking(&self) -> bool {
        !self.auto_walk.is_empty()
    }

    pub fn move_cooldown(&self) -> u32 {
        self.move_cd
    }

    // ---- Collision ------------------------------------------------------------------------

    /// Install the 64 KiB `MAP.MKF` tile buffer of the current scene's map.
    pub fn set_map_collision(&mut self, raw_map: &[u8]) -> bool {
        match CollisionGrid::from_raw_map64k(raw_map) {
            Some(g) => {
                self.base_grid = g.clone();
                self.grid = g;
                true
            }
            None => false,
        }
    }

    /// Install the 64 KiB `MAP.MKF` tile buffer along with per-map GOP visual flags
    /// (black exterior void tiles & genuinely opaque elevated roofs, #234).
    pub fn set_map_collision_with_visuals(
        &mut self,
        raw_map: &[u8],
        frame0_is_void: bool,
        opaque_frames: &[bool],
    ) -> bool {
        match CollisionGrid::from_raw_map64k_with_visuals(raw_map, frame0_is_void, opaque_frames) {
            Some(g) => {
                self.base_grid = g.clone();
                self.grid = g;
                true
            }
            None => false,
        }
    }

    /// Read-only access to the map-tile-only collision & occluder grid (without solid events, #234).
    pub fn base_grid(&self) -> &CollisionGrid {
        &self.base_grid
    }

    /// Returns `true` if `ev`'s auto script block contains `0x0083 branch_if_event_not_in_zone`
    /// targeting `target` (broadened across the full auto block, not just the current IP, #200).
    fn is_zone_sensor_for(&self, ev: &world::Event, target: u16) -> bool {
        if ev.auto.is_none() {
            return false;
        }
        self.prog
            .block(ev.auto.label)
            .is_some_and(|blk| blk.iter().any(|ins| ins.op == 0x0083 && ins.a[0] == target as i32))
    }

    /// Rebuild `grid` = map tiles + solid scene events (legacy `syncSceneObstacles`).
    pub(crate) fn sync_obstacles(&mut self, exclude: u16) {
        self.grid.blocked.copy_from_slice(&self.base_grid.blocked);
        for id in self.world.scene_event_range(self.world.scene_id) {
            if id == exclude {
                continue;
            }
            if let Some(ev) = self.world.event(id) {
                if ev.state >= 2 && ev.vanish_time <= 0 {
                    if exclude > 0 && self.is_zone_sensor_for(ev, exclude) {
                        continue;
                    }
                    let (x, y, h) = world_to_half_tile(ev.x, ev.y);
                    self.grid.set_blocked(x, y, h, true);
                }
            }
        }
    }

    /// Run one background auto-script pass immediately (tests and scripted tools).
    pub fn run_auto_tick(&mut self) {
        self.tick_auto_scripts();
    }

    /// Public wrapper of the obstacle rebuild (map tiles + solid events) for hosts and tests.
    pub fn sync_scene_obstacles(&mut self) {
        self.sync_obstacles(0);
    }

    /// Obstacle rebuild that leaves out event `exclude` (e.g. to test where that NPC stands).
    pub fn sync_scene_obstacles_excluding(&mut self, exclude: u16) {
        self.sync_obstacles(exclude);
    }

    pub fn set_sprite_frame_count(&mut self, sprite_id: u16, frames: i32) {
        self.sprite_frame_counts.insert(sprite_id, frames);
    }

    /// Record per-frame logical `(w, h)` from `mgo_XXX.json` (also updates `sprite_frame_counts`, #234).
    pub fn set_sprite_frame_dims(&mut self, sprite_id: u16, dims: Vec<(i32, i32)>) {
        if !dims.is_empty() {
            self.sprite_frame_counts.insert(sprite_id, dims.len() as i32);
            self.sprite_frame_dims.insert(sprite_id, dims);
        }
    }

    /// Fill `sprite_frames_auto` for current-scene events from known atlas metadata.
    pub(crate) fn sync_sprite_frames_auto(&mut self) {
        for id in self.world.current_event_ids() {
            let counts = &self.sprite_frame_counts;
            if let Some(ev) = self.world.event_mut(id) {
                if ev.sprite_id > 0 {
                    if let Some(&total) = counts.get(&ev.sprite_id) {
                        if total > 0 {
                            ev.sprite_frames_auto = if ev.sprite_frames > 0 { ev.sprite_frames } else { total };
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn clear_walk_state(&mut self, ev_id: u16) {
        self.npc_walks.remove(&ev_id);
    }

    // ---- Lifecycle ------------------------------------------------------------------------

    /// Human-readable scene label for replay chapters/segments.
    pub fn scene_label(&self, scene_id: u16) -> String {
        let name = self
            .data
            .scenes
            .get(&scene_id)
            .map(|s| s.name.as_str())
            .unwrap_or("");
        if name.is_empty() {
            format!("Scene #{scene_id}")
        } else {
            name.to_string()
        }
    }

    /// Start a new game with an explicit RNG seed and run scene 1's enter script.
    pub fn start_new_game(&mut self, seed: u32) {
        self.abort_foreground();
        let prev_tick = self.tick_count;
        let prev_lang = self.world.lang;
        self.debug_mode = false;
        self.god_mode = false;
        self.world = World::new_game(&self.data, &mut self.prog, seed);
        self.world.set_lang(prev_lang);
        // SDLPAL `PAL_UpdateEquipments`: `roles.yaml` stores base stats; add equipped gear on top (#271).
        self.recompute_all_equip_effects();
        self.reset_runtime();
        let label = self.scene_label(1);
        self.recorder.reset_with_anchor(
            prev_tick,
            1,
            label,
            replay_rec::ReplayAnchor::NewGame { seed },
        );
        self.recorder.seg_start_abs_tick = 0;
        self.enter_scene(1, true);
    }

    /// Export the current segmented deterministic replay (`RePal_CoreReplay_v3`).
    pub fn export_replay_bundle(&self) -> replay_rec::ReplayBundle {
        self.recorder.export_bundle(self.tick_count)
    }

    /// Export the current deterministic replay as JSON (for Web WASM / legacy consumers).
    pub fn export_replay_json(&self) -> serde_json::Value {
        self.recorder.export_json(self.tick_count)
    }

    /// Manually archive the previous recording (if any) and start a fresh recording anchored here.
    /// Preserves `debug_mode`/`tainted_by_debug` if this session was already debug-tainted so
    /// restarting a recording cannot launder a cheated state into an untainted replay (#241).
    pub fn restart_recording_here(&mut self) -> Option<replay_rec::ReplayBundle> {
        let was_debug = self.debug_mode;
        let was_tainted = self.debug_mode || self.recorder.tainted_by_debug;
        let sid = self.world.scene_id;
        let label = self.scene_label(sid);
        let snap = self.save_json(&label, "anchor");
        self.pending_manual_split = false;
        self.recorder.reset_with_anchor(
            self.tick_count,
            sid,
            label,
            replay_rec::ReplayAnchor::SaveJson(snap),
        );
        self.recorder.debug_mode = was_debug;
        self.recorder.tainted_by_debug = was_tainted;
        self.recorder.finished_archive.take()
    }

    /// Transition cleanly from replay playback into live Take Over gameplay (#241):
    /// disables active `debug_mode`/`god_mode` so replay cheats don't leak into live play,
    /// re-enables recording anchored here, and retains `tainted_by_debug` if the replay was tainted.
    pub fn take_over_from_replay(&mut self, was_tainted: bool) {
        self.debug_mode = false;
        self.god_mode = false;
        self.recorder.enabled = true;
        self.recorder.debug_mode = false;
        self.recorder.tainted_by_debug = was_tainted;
        let _ = self.restart_recording_here();
        if was_tainted {
            self.recorder.tainted_by_debug = true;
        }
    }

    /// Re-anchor segment 0 at the current world state after a CLI launch option like `--scene`
    /// without tainting the session with `debug_mode` (#252).
    pub fn reanchor_segment_zero(&mut self) {
        let was_debug = self.debug_mode;
        let was_tainted = self.recorder.tainted_by_debug;
        let sid = self.world.scene_id;
        let label = self.scene_label(sid);
        let snap = self.save_json(&label, "scene_anchor");
        self.pending_manual_split = false;
        self.recorder.segments.clear();
        self.recorder.seg_start_abs_tick = self.tick_count;
        self.recorder.cumulative_ticks = 0;
        self.recorder.total_events = 0;
        self.recorder.last_held_dir = -1;
        self.recorder.last_steer = None;
        self.recorder.pending_split_hint = false;
        self.recorder.debug_mode = was_debug;
        self.recorder.tainted_by_debug = was_tainted;
        self.recorder.segments.push(replay_rec::ReplaySegment {
            index: 0,
            start_tick: 0,
            duration_ticks: 0,
            scene_id: sid,
            label,
            anchor: replay_rec::ReplayAnchor::SaveJson(snap),
            events: Vec::new(),
            annotations: Vec::new(),
            thumb_png: None,
        });
    }

    /// Record a purely cosmetic AI thought/commentary subtitle into the active replay segment's
    /// separate `annotations` track. Does not affect RNG, game state, `total_events`, or `tainted_by_debug`.
    pub fn record_ai_thought(&mut self, text: String) -> replay_rec::AnnotationRecordResult {
        self.recorder.push_annotation(self.tick_count, text)
    }

    /// Take any finished recording waiting to be auto-archived by the host (`RePal/replays/`).
    pub fn take_archived_replay(&mut self) -> Option<replay_rec::ReplayBundle> {
        self.recorder.finished_archive.take()
    }

    /// Hint to the recorder that a scene transition or battle occurred.
    pub(crate) fn record_auto_checkpoint(&mut self, _reason: &str, _force: bool) {
        if self.recorder.is_active() && !self.in_title {
            self.recorder.pending_split_hint = true;
        }
    }

    /// Returns `true` when the overworld is completely at rest so a `save_json` replay segment
    /// anchor captures 100% of live continuation state with zero unsaved transient drift (#272).
    pub(crate) fn can_split_segment_cleanly(&self, input: &TickInput) -> bool {
        self.recorder.is_active()
            && !self.in_title
            && !self.menu_open
            && self.fg.is_none()
            && self.battle.is_none()
            && !self.loading_scene
            && self.auto_walk.is_empty()
            && self.pending_click_event == 0
            && self.move_cd == 0
            && self.suppressed.is_empty()
            && !self.just_changed_scene
            && input.held_dir < 0
            && input.steer.is_none()
    }

    fn perform_segment_split(&mut self, note: &str) {
        self.pending_manual_split = false;
        self.auto_cd = 0;
        self.npc_walks.clear();
        let sid = self.world.scene_id;
        let label = self.scene_label(sid);
        let snap = self.save_json(&label, note);
        self.recorder
            .split_segment(self.tick_count, sid, label, snap);
    }

    /// Manually split a new replay segment (called when the player performs a manual Save).
    /// If transient movement/suppression state is active, defers the split until the next
    /// tick satisfying [`Game::can_split_segment_cleanly`] so playback never diverges (#272).
    pub fn split_segment_on_manual_save(&mut self) {
        if !self.recorder.is_active() || self.in_title || self.fg.is_some() || self.battle.is_some()
        {
            return;
        }
        if self.recorder.seg_rel_tick(self.tick_count) == 0 {
            return;
        }
        if self.can_split_segment_cleanly(&TickInput::IDLE)
            && self.recorder.last_held_dir < 0
            && self.recorder.last_steer.is_none()
        {
            self.perform_segment_split("manual_save_segment");
        } else {
            self.pending_manual_split = true;
        }
    }

    /// Enable or disable explicit session `debug_mode` (#235).
    /// Once enabled during a recording session, `recorder.debug_mode` and `recorder.tainted_by_debug`
    /// remain set so exported `.repal` archives faithfully carry the flag and watermark.
    pub fn set_debug_mode(&mut self, enabled: bool) {
        self.debug_mode = enabled;
        if enabled {
            self.recorder.debug_mode = true;
            self.recorder.tainted_by_debug = true;
        } else {
            self.god_mode = false;
        }
    }

    /// Validate a [`replay_rec::DebugAction`] without mutating state or interning labels (#246).
    pub fn validate_debug_action(&self, action: &replay_rec::DebugAction) -> Result<(), String> {
        use replay_rec::DebugAction;
        match action {
            DebugAction::Tp { scene_id, .. } => {
                if !self.data.scenes.contains_key(scene_id) {
                    return Err(format!("scene #{scene_id} not found"));
                }
            }
            DebugAction::EventState { event_id, .. } => {
                if self.world.event(*event_id).is_none() {
                    return Err(format!("event #{event_id} not found"));
                }
            }
            DebugAction::Battle(gid) => {
                if self.is_busy() {
                    return Err("cannot start battle right now (foreground busy)".to_string());
                }
                if !self.data.teams.contains_key(gid) {
                    return Err(format!("enemy group #{gid} not found"));
                }
            }
            DebugAction::Script(lbl) => {
                if self.is_busy() {
                    return Err(format!("cannot run script '{lbl}' (foreground busy)"));
                }
                let exists = self
                    .prog
                    .label_id(lbl)
                    .is_some_and(|id| self.prog.block(id).is_some());
                if !exists {
                    return Err(format!("script label '{lbl}' not found"));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Execute a deterministic [`replay_rec::DebugAction`] gated by `self.debug_mode` (#235).
    /// Returns `Err` without mutating world state when `self.debug_mode == false`.
    pub fn apply_debug_action(&mut self, action: &replay_rec::DebugAction) -> Result<String, String> {
        use replay_rec::DebugAction;
        if !self.debug_mode {
            return Err("debug_mode is disabled".to_string());
        }
        // Validate fallible actions BEFORE recording so rejected debug ops never pollute the log (#246).
        self.validate_debug_action(action)?;

        self.recorder.debug_mode = true;
        self.recorder.tainted_by_debug = true;
        self.recorder
            .push(self.tick_count, ReplayEvent::Debug(action.clone()));

        match action {
            DebugAction::Tp { scene_id, coords } => {
                self.in_title = false;
                self.abort_foreground();
                self.battle = None;
                self.enter_scene(*scene_id, false);
                if let Some((x, y)) = *coords {
                    self.world.set_party_world_pos(x, y);
                }
                if self.god_mode {
                    for r in &mut self.world.roles {
                        r.hp = r.max_hp.max(1);
                        r.mp = r.max_mp.max(1);
                        r.poisons.clear();
                    }
                }
                Ok(format!("[/tp] scene #{scene_id}"))
            }
            DebugAction::Heal => {
                for r in &mut self.world.roles {
                    r.hp = r.max_hp.max(1);
                    r.mp = r.max_mp.max(1);
                    r.poisons.clear();
                }
                Ok("[/heal] party restored".to_string())
            }
            DebugAction::God(on) => {
                self.god_mode = *on;
                if *on {
                    for r in &mut self.world.roles {
                        r.hp = r.max_hp.max(1);
                        r.mp = r.max_mp.max(1);
                        r.poisons.clear();
                    }
                }
                Ok(format!("[/god] {}", if *on { "ON" } else { "OFF" }))
            }
            DebugAction::Cash(n) => {
                let sum = (self.world.cash as i64).saturating_add(*n as i64);
                self.world.cash = sum.clamp(0, u32::MAX as i64) as u32;
                Ok(format!("[/cash] {n:+}"))
            }
            DebugAction::Item { item_id, qty } => {
                let cur = self.world.inventory.get(item_id).copied().unwrap_or(0);
                let next = cur.saturating_add(*qty).clamp(0, 99);
                if next == 0 {
                    self.world.inventory.remove(item_id);
                } else {
                    self.world.inventory.insert(*item_id, next);
                }
                Ok(format!("[/item] #{item_id} {qty:+}"))
            }
            DebugAction::EventState { event_id, state } => {
                if let Some(ev) = self.world.event_mut(*event_id) {
                    ev.state = *state;
                }
                self.sync_obstacles(0);
                Ok(format!("[/event] #{event_id}.state = {state}"))
            }
            DebugAction::Battle(gid) => {
                if !self.start_test_battle(*gid as i32) {
                    return Err("cannot start battle right now (foreground busy)".to_string());
                }
                Ok(format!("[/battle] group #{gid}"))
            }
            DebugAction::Script(lbl) => {
                if !self.run_script(lbl, 0) {
                    return Err(format!("cannot run script '{lbl}' (busy or missing)"));
                }
                Ok(format!("[/script] ran {lbl}"))
            }
        }
    }

    /// Reset per-session runtime state (tick clock, movement cadence, walk queues). Called on new
    /// game and on load so recorded inputs replay against the same logical timeline.
    pub(crate) fn reset_runtime(&mut self) {
        self.in_title = false;
        self.tick_count = 0;
        self.move_cd = 0;
        self.auto_cd = 0;
        self.auto_walk.clear();
        self.pending_click_event = 0;
        self.suppressed.clear();
        self.npc_walks.clear();
        self.pending_manual_split = false;
        self.battle = None;
    }

    /// Drop any running foreground script (legacy `abortActiveScript`).
    pub fn abort_foreground(&mut self) {
        self.fg = None;
        self.vmc.pending_scene_enter = false;
        self.vmc.pending_item_triggered_event = 0;
        self.vmc.using_item = false;
        self.vmc.need_fade_in = false;
    }

    /// Switch to `scene_id`, ask the host for its assets and optionally run its enter script.
    pub fn enter_scene(&mut self, scene_id: u16, run_enter_script: bool) {
        if scene_id != self.world.scene_id {
            self.world.party_layer = 0;
        }
        self.world.scene_id = scene_id;
        if self.world.chase_alert_range == 0 {
            self.world.chase_alert_range = 3;
        }
        self.world.view.active_fbp = -1;
        self.world.view.cam_override = None;
        for id in self.world.current_event_ids() {
            self.clear_walk_state(id);
        }
        self.suppressed.clear();
        self.just_changed_scene = true;
        self.record_auto_checkpoint("scene", false);
        let map_id = self.world.current_scene().map(|s| s.map_id).unwrap_or(0);
        self.emit(HostCmd::LoadScene { scene_id, map_id });
        self.emit(HostCmd::SceneChanged(scene_id));
        if run_enter_script {
            self.start_foreground(FgKind::SceneEnter(scene_id));
            if let Some(t) = &mut self.fg {
                t.wait = Wait::SceneAssets;
            }
        } else {
            self.loading_scene = true;
        }
    }

    /// Answer a blocking host command.
    pub fn reply(&mut self, r: HostReply) {
        if r == HostReply::SceneLoaded && self.loading_scene {
            self.loading_scene = false;
            self.after_scene_loaded();
            if self.fg.is_none() && std::mem::take(&mut self.vmc.pending_scene_enter) {
                let sid = self.world.scene_id;
                self.start_foreground(FgKind::SceneEnter(sid));
                self.run_foreground_tick();
            }
            return;
        }
        match r {
            HostReply::DialogueDone => self.recorder.push(self.tick_count, ReplayEvent::DialogueDone),
            HostReply::Choice(yes) => self.recorder.push(self.tick_count, ReplayEvent::Choice(yes)),
            HostReply::ModalDone => self.recorder.push(self.tick_count, ReplayEvent::ModalDone),
            _ => {}
        }
        self.fg_reply(r);
    }

    pub(crate) fn after_scene_loaded(&mut self) {
        self.sync_sprite_frames_auto();
        self.sync_obstacles(0);
    }

    /// Advance the game by one fixed logic tick.
    pub fn tick(&mut self, input: &TickInput) {
        // Waiting on asset loads or real-time host playback does not consume logic time, so
        // replays stay frame-exact regardless of network speed.
        if self.is_blocked_on_host() {
            return;
        }
        // Clean idle-overworld segment split (shared by automatic gaps and deferred manual saves, #272).
        if self.can_split_segment_cleanly(input) {
            let seg_ticks = self.recorder.seg_rel_tick(self.tick_count);
            let manual = self.pending_manual_split && seg_ticks > 0;
            if manual || seg_ticks >= replay_rec::SEGMENT_MIN_GAP_TICKS {
                self.perform_segment_split(if manual { "manual_save_segment" } else { "segment" });
            }
        }
        self.recorder.record_tick_input(
            self.tick_count,
            input.held_dir,
            input.interact,
            input.click,
            input.steer,
        );
        self.tick_count += 1;
        if self.debug_mode && self.god_mode {
            for r in &mut self.world.roles {
                r.hp = r.max_hp.max(1);
                r.mp = r.max_mp.max(1);
                r.poisons.clear();
            }
        }
        if self.world.view.shake_ticks > 0 {
            self.world.view.shake_ticks -= 1;
        }
        if self.fg.is_some() {
            self.run_foreground_tick();
            return;
        }
        if self.in_title || self.menu_open {
            return;
        }
        self.explore_tick(input);
    }

    /// Start a stand-alone battle outside any script (debug "quick battle").
    pub fn start_test_battle(&mut self, team: i32) -> bool {
        if self.fg.is_some() {
            return false;
        }
        self.in_title = false;
        self.start_foreground(FgKind::Battle(team));
        self.run_foreground_tick();
        true
    }

    /// Run block `label` in the foreground as event `event_id`. Returns `false` if busy or missing (#246).
    pub fn run_script(&mut self, label: &str, event_id: u16) -> bool {
        if self.fg.is_some() {
            return false;
        }
        let Some(l) = self.prog.label_id(label) else {
            return false;
        };
        if self.prog.block(l).is_none() {
            return false;
        }
        self.start_foreground(FgKind::Script(l, event_id));
        self.run_foreground_tick();
        true
    }

    /// Request a foreground use of an item from the menu (legacy `ScriptVM.useItem`).
    /// Returns `false` if the item cannot be used right now.
    pub fn use_item(&mut self, item_id: u16, target_role: Option<usize>) -> bool {
        let Some(it) = self.data.items.get(&item_id) else {
            return false;
        };
        if !it.usable || self.world.item_count(item_id) <= 0 || self.fg.is_some() {
            return false;
        }
        self.recorder
            .push(self.tick_count, ReplayEvent::UseItem { item_id, role: target_role });
        let target = target_role.unwrap_or(self.world.leader_role() as usize);
        self.vmc.current_target_role = target;
        self.vmc.target_item_all = it.target_all;
        self.vmc.pending_item_triggered_event = 0;
        self.vmc.item_script_failed = false;
        self.vmc.using_item = true;
        self.last_item_result = None;
        self.start_foreground(FgKind::UseItem(item_id));
        self.run_foreground_tick();
        true
    }
}
