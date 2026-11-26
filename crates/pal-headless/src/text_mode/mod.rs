//! Unified Text Mode Engine for `repal` (`--text`, `--json`, `--listen`, `-c`, `--script`).
//!
//! Built around a single [`TextEngine`] + [`TextHost`] + [`ChannelServer`] shared identically by:
//! - **Headless Text Mode (`repal --text`)**: lazy rendering (zero compositor overhead until
//!   `shot` / `golden` / `click` / `bugreport`).
//! - **GUI + Text Mode (`repal --gui --text` / `repal --listen`)**: stepped inside the 50 Hz
//!   `winit` loop with full 4× HD graphics, DOS UI, and audio.

pub mod command;
pub mod engine;
pub mod help;
pub mod response;
#[cfg(feature = "disk")]
pub mod transport;

pub use command::{parse_command, split_batch_commands, Command, TargetRef};
pub use engine::{ExecOutcome, TextEngine, TextReply};
#[cfg(feature = "disk")]
pub use engine::{load_save_into_rt, resolve_save_path};
pub use response::{
    build_response_json, build_response_json_ex, build_ui_menu, current_mode, event_screen_bbox,
    event_visible_in_viewport, format_plain_response, format_prompt, is_fair_visible_exit,
    runtime_camera, ui_view_id, ResponseOpts, TEXT_PROTOCOL_VERSION,
};
#[cfg(feature = "disk")]
pub use transport::{write_reply, ChannelServer, IncomingCmd};

/// Provenance of an incoming Text Mode command (`Local` stdio/WASM vs `Tcp` socket client).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClientOrigin {
    #[default]
    Local,
    Tcp {
        client_id: u64,
        is_loopback: bool,
    },
}

impl ClientOrigin {
    pub fn client_id(self) -> u64 {
        match self {
            Self::Local => 0,
            Self::Tcp { client_id, .. } => client_id,
        }
    }

    pub fn is_socket(self) -> bool {
        matches!(self, Self::Tcp { .. })
    }

    pub fn is_remote(self) -> bool {
        matches!(self, Self::Tcp { is_loopback: false, .. })
    }
}

/// Minimap emission policy for Text Mode responses (#234).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MinimapMode {
    /// Emit viewport minimap on `look`, `map`, first reply after a scene change, and blocked moves.
    #[default]
    Auto,
    /// Emit viewport minimap on every `explore` response.
    Always,
    /// Only emit viewport minimap on explicit `map` command.
    Off,
}

impl MinimapMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Off => "off",
        }
    }
}

/// Observation & fairness profile for Text Mode (#224).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ObsMode {
    /// Developer / diagnostic visibility: full scene events, exact enemy HP, `/debug` enabled.
    #[default]
    Debug,
    /// Player-equivalent fair vision (`--obs fair` / `--fog`): real clamped `320x200` camera
    /// viewport, visible sprites only, no script spoilers, hidden enemy HP, on-screen `goto`/`talk`
    /// only (no global maze pathfinding), and `/debug` + `dump-state` disabled.
    Fair,
}

impl ObsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Fair => "fair",
        }
    }

    pub fn is_fair(self) -> bool {
        self == Self::Fair
    }
}

/// Active RNG cutscene movie playback status exposed via [`TextHost::rng_status`] (#223).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RngStatus {
    pub id: u16,
    pub frame: u16,
    pub end: u16,
    pub skippable: bool,
}

#[cfg(feature = "disk")]
use crate::HeadlessApp;
use pal_core::Language;
use pal_runtime::PalRuntime;
use pal_ui::UiAction;
#[cfg(feature = "disk")]
use pal_ui::UiView;
#[cfg(feature = "disk")]
use std::fs;
#[cfg(feature = "disk")]
use std::io::{self, Write};
use std::path::Path;
#[cfg(feature = "disk")]
use std::path::PathBuf;

/// Platform-agnostic host contract bridging [`TextEngine`] to either headless or GUI runtimes.
pub trait TextHost {
    fn runtime(&self) -> &PalRuntime;
    fn runtime_mut(&mut self) -> &mut PalRuntime;
    fn assets_root(&self) -> &Path;
    fn out_dir(&self) -> &Path;

    /// Service all queued platform-facing `HostCmd`s (`LoadScene`, audio, RNG, ReturnToTitle, GameOver).
    fn service_host_cmds(&mut self);

    /// Handle a host-level `UiAction` returned by `PalRuntime::on_key` / `on_pointer`.
    fn handle_ui_action(&mut self, action: UiAction);

    /// Render a settled frame (pumping any missing disk assets).
    /// Headless hosts call this lazily only on `shot`, `golden`, `click`, or `bugreport`.
    fn render_settled_frame(&mut self);

    /// Optional GUI-only visual feedback hooks (no-ops in headless).
    fn on_toast(&mut self, _msg: &str) {}
    fn on_transition_banner(&mut self, _msg: &str, _ms: u64) {}
    fn on_click_ripple(&mut self, _wx: i32, _wy: i32) {}

    /// Lifecycle hook invoked before/after session-level transitions (`new`, `load`, `core-replay`, `/tp`).
    /// GUI hosts use this to stop title music, cancel active replay/RNG playback, and sync recorder state.
    fn on_session_transition(&mut self, _stop_music: bool, _enable_recorder: bool) {}

    /// Lifecycle hook invoked when Text Mode switches HD/Retro (`hd` / `retro`).
    /// Distinct from user menu toggles: syncs audio/toast without overwriting `config.json`.
    fn on_hd_changed(&mut self, _hd: bool) {}

    /// Lifecycle hook invoked when Text Mode switches language (`lang`).
    /// Distinct from user menu toggles: refreshes save slot labels/toast without overwriting `config.json`.
    fn on_lang_changed(&mut self, _lang: Language) {}

    /// Skip an active RNG cutscene animation if the host is currently playing one.
    /// Returns `true` if an active RNG animation was skipped.
    fn skip_rng_if_active(&mut self) -> bool {
        false
    }

    /// Active RNG cutscene movie status when the host is playing a skippable movie (#223).
    fn rng_status(&self) -> Option<RngStatus> {
        None
    }

    /// Export a diagnostic bug report bundle and return structured artifact JSON.
    fn export_bugreport(&mut self, desc: &str) -> Result<serde_json::Value, String>;

    /// Drain queued BGM/SFX events since the last tick (used by audio-muxed `capture`).
    fn drain_audio_events(&mut self) -> Vec<crate::AudioEvent> {
        Vec::new()
    }

    /// Read an audio asset key (`audio/bgm_hd/...`, `audio/midi/...`, `audio/sfx/...`) for offline capture.
    fn read_audio_asset(&mut self, _key: &str) -> Option<Vec<u8>> {
        None
    }
}

/// Configuration for headless Text Mode (`repal --text ...`).
#[cfg(feature = "disk")]
mod native_host {
use super::*;

#[derive(Clone, Debug)]
pub struct TextModeConfig {
    pub assets_root: PathBuf,
    /// Optional local `--packs-dir <DIR>` containing `pack-manifest.json` + `.rpk` files (#237).
    pub packs_dir: Option<PathBuf>,
    /// Optional loose `--assets` overlay when `--packs-dir` is also specified.
    pub loose_overlay: Option<PathBuf>,
    /// Optional OS user profile directory for reading `<user_dir>/pack.key`.
    pub user_dir: Option<PathBuf>,
    pub json_mode: bool,
    pub hd_mode: bool,
    pub lang: Language,
    pub fog: bool,
    pub obs_mode: ObsMode,
    pub minimap_mode: MinimapMode,
    pub new_game: bool,
    pub debug_mode: bool,
    pub save_path: Option<PathBuf>,
    pub scene_id: Option<u16>,
    pub listen_addr: Option<String>,
    pub batch_cmds: Option<String>,
    pub script_file: Option<PathBuf>,
    pub record_file: Option<PathBuf>,
    pub keep_going: bool,
    pub out_dir: PathBuf,
}

impl Default for TextModeConfig {
    fn default() -> Self {
        Self {
            assets_root: PathBuf::from("site/assets"),
            packs_dir: None,
            loose_overlay: None,
            user_dir: None,
            json_mode: false,
            hd_mode: true,
            lang: Language::ZhHans,
            fog: false,
            obs_mode: ObsMode::Debug,
            minimap_mode: MinimapMode::Auto,
            new_game: false,
            debug_mode: false,
            save_path: None,
            scene_id: None,
            listen_addr: None,
            batch_cmds: None,
            script_file: None,
            record_file: None,
            keep_going: false,
            out_dir: PathBuf::from("."),
        }
    }
}

/// Headless implementation of [`TextHost`] backed by [`HeadlessApp`] with strict lazy rendering.
pub struct HeadlessTextHost {
    pub app: HeadlessApp,
    pub out_dir: PathBuf,
    bug_counter: usize,
}

    impl HeadlessTextHost {
        pub fn from_config(cfg: &TextModeConfig) -> Result<Self, String> {
            let mut app = if let Some(packs_dir) = &cfg.packs_dir {
                let key = pal_runtime::pack_vfs::load_pack_key_for_packs(
                    cfg.user_dir.as_deref(),
                    packs_dir,
                )?;
                HeadlessApp::from_packs_dir(packs_dir, key, cfg.loose_overlay.clone(), cfg.hd_mode)?
            } else {
                if !cfg.assets_root.join("data/scenes.yaml").is_file() {
                    return Err(format!(
                    "assets not found at {} (expected data/scenes.yaml; pass --assets <DIR>, --packs-dir <DIR>, or run 'repal' once to auto-download assets)",
                    cfg.assets_root.display()
                ));
            }
            HeadlessApp::new(&cfg.assets_root, cfg.hd_mode)
        };
        app.rt.set_lang(cfg.lang);
        app.ensure_chapters_loaded()?;

        if let Some(sp) = &cfg.save_path {
            let resolved = resolve_save_path(&cfg.assets_root, sp);
            load_save_into_rt(&mut app.rt, &resolved)?;
            app.pump_host_cmds();
        } else if cfg.new_game {
            app.new_game(0x5041_4C31)?;
        } else {
            // Default: stay cleanly on Title screen (consistent with GUI mode).
            app.rt.set_in_title(true);
        }

        if cfg.debug_mode {
            if let Some(g) = app.rt.game.as_mut() {
                g.set_debug_mode(true);
            }
            app.rt.ui.debug_mode = true;
        }

        if let Some(sc) = cfg.scene_id {
            if let Some(g) = app.rt.game.as_mut() {
                g.in_title = false;
                g.abort_foreground();
                g.enter_scene(sc, false);
            }
            app.rt.ui.set_view(UiView::None);
            app.rt.world.fbp_override = None;
            app.rt.route_cmds();
            app.pump_host_cmds();
            if let Some(g) = app.rt.game.as_mut() {
                g.reanchor_segment_zero();
            }
            app.rt.project();
        }

        pal_runtime::disk::refresh_save_slots_from_vfs(&cfg.out_dir, &app.vfs, &mut app.rt);

        Ok(Self {
            app,
            out_dir: cfg.out_dir.clone(),
            bug_counter: 0,
        })
    }
}

impl TextHost for HeadlessTextHost {
    fn runtime(&self) -> &PalRuntime {
        &self.app.rt
    }

    fn runtime_mut(&mut self) -> &mut PalRuntime {
        &mut self.app.rt
    }

    fn drain_audio_events(&mut self) -> Vec<crate::AudioEvent> {
        self.app.drain_audio_events()
    }

    fn read_audio_asset(&mut self, key: &str) -> Option<Vec<u8>> {
        self.app.vfs.read_bytes(key).ok()
    }

    fn assets_root(&self) -> &Path {
        &self.app.assets_root
    }

    fn out_dir(&self) -> &Path {
        &self.out_dir
    }

    fn service_host_cmds(&mut self) {
        self.app.pump_host_cmds();
    }

    fn handle_ui_action(&mut self, action: UiAction) {
        match action {
            UiAction::NewGame => {
                let _ = self.app.new_game(0x5041_4C31);
            }
            UiAction::ReturnToTitle => {
                self.app.rt.set_in_title(true);
            }
            UiAction::ToggleHdMode => {
                let hd = self.app.rt.ui.hd_mode;
                self.app.rt.set_hd_mode(hd);
            }
            UiAction::ToggleLanguage => {
                let lang = self.app.rt.ui.lang;
                self.app.rt.set_lang(lang);
                pal_runtime::disk::refresh_save_slots_from_vfs(
                    &self.out_dir,
                    &self.app.vfs,
                    &mut self.app.rt,
                );
            }
            UiAction::SaveSlot(slot) => {
                if let Some(json) = self.app.rt.save_json_str("text_mode", "headless") {
                    // Match `pal-desktop` SaveStore naming (`slot_{slot}.json`, 0-based, #204).
                    let path = self.out_dir.join(format!("slot_{slot}.json"));
                    if let Some(parent) = path.parent() {
                        if !parent.as_os_str().is_empty() {
                            let _ = fs::create_dir_all(parent);
                        }
                    }
                    let _ = fs::write(&path, json);
                    pal_runtime::disk::refresh_save_slots_from_vfs(
                        &self.out_dir,
                        &self.app.vfs,
                        &mut self.app.rt,
                    );
                }
            }
            UiAction::LoadSlot(slot) => {
                let json_path = self.out_dir.join(format!("slot_{slot}.json"));
                if json_path.is_file() {
                    if load_save_into_rt(&mut self.app.rt, &json_path).is_ok() {
                        self.app.pump_host_cmds();
                    }
                } else if slot == 0 {
                    if let Ok(bytes) = self.app.vfs.read_bytes("orig_saves/10.rpg") {
                        if self.app.rt.load_dos_rpg(&bytes) {
                            self.app.pump_host_cmds();
                        }
                    }
                }
            }
            UiAction::TakeScreenshot => {
                self.app.render_settled_frame();
                let path = self.out_dir.join("repal_shot_ui.png");
                let _ = self.app.export_png(&path);
            }
            _ => {}
        }
    }

    fn render_settled_frame(&mut self) {
        self.app.render_settled_frame();
    }

    fn on_lang_changed(&mut self, _lang: Language) {
        pal_runtime::disk::refresh_save_slots_from_vfs(
            &self.out_dir,
            &self.app.vfs,
            &mut self.app.rt,
        );
    }

    fn export_bugreport(&mut self, desc: &str) -> Result<serde_json::Value, String> {
        // Respect `--out-dir` when specified instead of hardcoding `.tmp/bug_reports` (#206).
        let out_dir = if self.out_dir == Path::new(".") {
            PathBuf::from(".tmp/bug_reports")
        } else {
            self.out_dir.join("bug_reports")
        };
        fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir bug_reports: {e}"))?;
        let sid = self.app.rt.state.scene_id;
        let tick = self.app.rt.game.as_ref().map(|g| g.tick_count).unwrap_or(0);
        self.bug_counter += 1;
        let stem = format!("repal_bug_sc{sid}_t{tick}_{:03}", self.bug_counter);
        let png_path = out_dir.join(format!("{stem}.png"));
        let json_path = out_dir.join(format!("{stem}.json"));
        self.app.render_settled_frame();
        self.app.export_png(&png_path)?;
        if let Some(g) = self.app.rt.game.as_ref() {
            let doc = serde_json::json!({
                "kind": "RePal_CoreDiag_v1",
                "note": desc,
                "state": g.canonical_state(),
            });
            fs::write(
                &json_path,
                serde_json::to_string_pretty(&doc).unwrap_or_default(),
            )
            .map_err(|e| format!("write bugreport json: {e}"))?;
        }
        Ok(serde_json::json!({
            "png": png_path.display().to_string(),
            "json": json_path.display().to_string(),
        }))
    }
}

/// Main entrypoint for headless Text Mode (`repal --text ...`).
/// Returns process exit code (`0` = ok, `1` = error, `2` = golden diff failed).
pub fn run_text_mode(cfg: TextModeConfig) -> i32 {
    let mut host = match HeadlessTextHost::from_config(&cfg) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[repal] text mode boot failed: {e}");
            return 1;
        }
    };
    let effective_obs = if cfg.fog || cfg.obs_mode == ObsMode::Fair {
        ObsMode::Fair
    } else {
        ObsMode::Debug
    };
    let mut engine = TextEngine::with_obs(effective_obs, 0);
    engine.minimap_mode = cfg.minimap_mode;
    if cfg.new_game || cfg.save_path.is_some() || cfg.scene_id.is_some() {
        engine.settle_sync(&mut host, 120);
    }

    // 1. Batch mode: `-c` and/or `--script`
    if cfg.batch_cmds.is_some() || cfg.script_file.is_some() {
        let mut cmds = Vec::new();
        if let Some(script_path) = &cfg.script_file {
            match fs::read_to_string(script_path) {
                Ok(text) => cmds.extend(split_batch_commands(&text)),
                Err(e) => {
                    eprintln!(
                        "[repal] failed to read script {}: {e}",
                        script_path.display()
                    );
                    return 1;
                }
            }
        }
        if let Some(inline) = &cfg.batch_cmds {
            cmds.extend(split_batch_commands(inline));
        }

        let mut record_file = cfg.record_file.as_ref().and_then(|p| {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            fs::OpenOptions::new().create(true).append(true).open(p).ok()
        });

        let mut stdout = io::stdout().lock();
        let mut had_err = false;
        for cmd in cmds {
            if let Some(rf) = record_file.as_mut() {
                if !matches!(cmd.as_str(), "quit" | "exit" | "q") {
                    let _ = writeln!(rf, "{cmd}");
                }
            }
            let reply = engine.execute_sync(&mut host, &cmd, false);
            let _ = write_reply(&mut stdout, cfg.json_mode, &reply, false);
            if reply.bye {
                break;
            }
            if !reply.response["ok"].as_bool().unwrap_or(true) && !reply.golden_failed {
                had_err = true;
                if !cfg.keep_going {
                    // Prioritize golden diff exit code (2) consistently with GUI batch mode (#227).
                    return if engine.golden_failed_any { 2 } else { 1 };
                }
            }
        }
        if engine.golden_failed_any {
            return 2;
        }
        return if had_err { 1 } else { 0 };
    }

    // 2. Socket mode (`--listen <IP:PORT>`)
    if let Some(addr) = &cfg.listen_addr {
        let server = match ChannelServer::start_tcp(addr, cfg.json_mode, cfg.record_file.clone()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[repal] failed to start socket server: {e}");
                return 1;
            }
        };
        if !cfg.json_mode {
            if let Some(tok) = server.auth_token() {
                println!("[repal] Listening on {addr} | LAN auth token: {tok} (send 'auth {tok}' as first line)");
            } else {
                println!("[repal] Listening on {addr}");
            }
        }
        while let Ok(req) = server.rx.recv() {
            let reply = engine.execute_sync_from(&mut host, &req.raw, req.interactive_blank, req.origin);
            let _ = req.reply_tx.send(reply);
        }
        return 0;
    }

    // 3. Stdio mode (interactive TTY or piped stdin)
    let server = ChannelServer::start_stdio(cfg.json_mode, cfg.record_file.clone());
    while let Ok(req) = server.rx.recv() {
        let reply = engine.execute_sync_from(&mut host, &req.raw, req.interactive_blank, req.origin);
        let is_bye = reply.bye;
        let _ = req.reply_tx.send(reply);
        if is_bye {
            break;
        }
    }
    if engine.golden_failed_any { 2 } else { 0 }
}
} // mod native_host

#[cfg(feature = "disk")]
pub use native_host::*;
