//! `pal-desktop` — Native Desktop Frontend for RePal.
//!
//! Shares 100% of the deterministic engine (`pal-core`), software renderer (`pal-render`),
//! 1995 DOS UI (`pal-ui`), and orchestration (`pal-runtime`) with the web WASM and headless
//! builds, streaming assets directly from local disk with zero browser/hosting limits.

#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod assets_bootstrap;
mod app_actions;
mod app_events;
mod app_sim;
mod audio;
mod cli;
#[cfg(target_os = "linux")]
mod linux_integration;
mod os_bridge;
mod pack_download;
mod pack_fetcher;
mod replay_ui;
mod saves;
mod session_io;
mod splash;
mod text_host;
mod updater;

use assets_bootstrap::{spawn_asset_download, try_resolve_local_assets, BootstrapMsg};
use audio::NativeAudio;
use cli::{parse_cli, CliConfig, LaunchMode};
use pal_core::game::{HostCmd, HostReply, TickInput};
use pal_headless::text_mode::{
    split_batch_commands, ChannelServer, ClientOrigin, ExecOutcome, IncomingCmd, TextEngine,
    TextReply,
};
use pal_runtime::disk::{
    boot_runtime_from_vfs, load_scene_assets_from_vfs, pump_render_assets_from_vfs,
};
use pal_runtime::pack_vfs::{load_pack_key_for_packs, OverlayVfs};
use pal_runtime::PalRuntime;
use pal_ui::{PointerEvent, PointerKind, UiAction, UiKey, UiView};
use replay_ui::{
    encode_fb_thumb_png, hit_test_replay_bar, render_replay_bar, render_transition_banner,
    CoreReplayPlayer, ReplayHudHit,
};
use saves::{SaveStore, UserConfig};
use softbuffer::{Context, Surface};
use splash::{render_splash, SplashState};
use std::collections::{HashSet, VecDeque};
use std::env;
use std::fs;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Fullscreen, Icon, Window, WindowId};

const TICK_DURATION: Duration = Duration::from_millis(20);
const APP_ICON_PNG: &[u8] = include_bytes!("../../../site/icons/repal_icon.png");

fn load_window_icon() -> Option<Icon> {
    let img = image::load_from_memory(APP_ICON_PNG).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// Localize a host-side message (toast / banner) for the current UI language.
pub(crate) fn tl(lang: pal_core::Language, hans: &str, hant: &str, en: &str) -> String {
    match lang {
        pal_core::Language::En => en,
        pal_core::Language::ZhHant => hant,
        pal_core::Language::ZhHans => hans,
    }
    .to_string()
}

pub(crate) struct RngPlayback {
    id: u16,
    frame: u16,
    end: u16,
    frame_interval: Duration,
    next_frame_at: Instant,
}

pub(crate) struct DesktopApp {
    pub(crate) cfg: CliConfig,
    pub(crate) vfs: OverlayVfs,
    pub(crate) pack_fetcher: Option<pack_fetcher::PackFetcher>,
    /// Chapter package download in progress for a `LoadScene` (#18): `SceneLoaded` is withheld
    /// and a loading overlay is shown until it completes.
    pub(crate) pending_scene_load: Option<pack_download::PendingSceneLoad>,
    /// Optional background "download all assets" job started from Title Options (#18).
    pub(crate) download_all_job: Option<pack_download::PackDownloadJob>,
    pub(crate) rt: Option<PalRuntime>,
    pub(crate) audio: Option<NativeAudio>,
    pub(crate) saves: SaveStore,
    bootstrap_rx: Option<Receiver<BootstrapMsg>>,
    splash: SplashState,
    window: Option<Arc<Window>>,
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
    surface_size: (u32, u32),
    sx_lut: Vec<usize>,
    sx_lut_key: (usize, usize),
    next_frame_at: Instant,
    keys_down: HashSet<KeyCode>,
    /// True if another key was pressed while Alt was held (e.g. Alt+Enter fullscreen),
    /// suppressing the standalone-Alt Menu/Cancel release trigger (#274).
    alt_used_combo: bool,
    pending_interact: bool,
    pending_click: Option<(i32, i32)>,
    pointer_drag: bool,
    pointer_phys: (f64, f64),
    last_frame: Instant,
    /// Window focus / visibility, for background render throttling (the simulation keeps 50 Hz).
    pub(crate) window_focused: bool,
    pub(crate) window_occluded: bool,
    pub(crate) last_present: Instant,
    tick_acc: Duration,
    pub(crate) rng_play: Option<RngPlayback>,
    pub(crate) fullscreen: bool,
    pub(crate) shot_counter: u32,
    return_title_armed: Option<Instant>,
    channel_server: Option<ChannelServer>,
    text_engine: Option<TextEngine>,
    pending_reply_tx: Option<Sender<TextReply>>,
    pub(crate) replay_queue: VecDeque<String>,
    /// True while a file-sourced `.cmd` replay command is executing, blocking host-privileged UI actions (#231).
    untrusted_cmd_active: bool,
    pub(crate) core_replay: Option<CoreReplayPlayer>,
    /// Presentation-only full-screen dim + centered text transition banner (load / chapter jump).
    pub(crate) transition_banner: Option<(String, Instant, Duration)>,
    /// Reusable present-time framebuffer for compositing the HD AI thought overlay without mutating `rt.compositor.fb`.
    present_fb: pal_render::framebuffer::Framebuffer,
    last_thought_gen: u64,
    last_thought_at: Option<Instant>,
    pub(crate) file_picker_rx: Option<Receiver<Option<PathBuf>>>,
    pub(crate) bug_report_rx: Option<Receiver<Result<String, String>>>,
    update_rx: Option<Receiver<updater::UpdateMsg>>,
    pending_update: Option<updater::AvailableUpdate>,
    should_exit: bool,
    exit_code: i32,
}

impl DesktopApp {
    fn new(mut cfg: CliConfig) -> Self {
        updater::cleanup_old_binary();
        let user_cfg = UserConfig::load(&cfg.config_path);
        let fullscreen = cfg.fullscreen_override.unwrap_or(user_cfg.fullscreen);
        let saves = SaveStore::new(cfg.saves_dir.clone());
        let bootstrap_rx = if let Some(packs_dir) = &cfg.packs_dir_arg {
            cfg.assets_root = cfg
                .assets_arg
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| packs_dir.clone());
            None
        } else {
            let local_assets =
                try_resolve_local_assets(cfg.assets_arg.as_deref(), &cfg.assets_cache_dir);
            if let Some(path) = local_assets {
                cfg.assets_root = path;
                None
            } else {
                Some(spawn_asset_download(
                    cfg.assets_cache_dir.clone(),
                    cfg.assets_url_arg.clone(),
                ))
            }
        };
        let vfs = OverlayVfs::from_loose_dir(cfg.assets_root.clone());

        let pace_ticks = (cfg.pace_ms / 20).max(2);
        let obs_mode = if cfg.fog || cfg.obs_mode == pal_headless::text_mode::ObsMode::Fair {
            pal_headless::text_mode::ObsMode::Fair
        } else {
            pal_headless::text_mode::ObsMode::Debug
        };

        let mut text_engine = TextEngine::with_obs(obs_mode, pace_ticks);
        text_engine.minimap_mode = cfg.minimap_mode;
        let mut app = Self {
            cfg,
            vfs,
            pack_fetcher: None,
            pending_scene_load: None,
            download_all_job: None,
            rt: None,
            audio: None,
            saves,
            bootstrap_rx,
            splash: SplashState::default(),
            window: None,
            surface: None,
            surface_size: (0, 0),
            sx_lut: Vec::new(),
            sx_lut_key: (0, 0),
            next_frame_at: Instant::now(),
            keys_down: HashSet::new(),
            alt_used_combo: false,
            pending_interact: false,
            pending_click: None,
            pointer_drag: false,
            pointer_phys: (0.0, 0.0),
            last_frame: Instant::now(),
            window_focused: true,
            window_occluded: false,
            last_present: Instant::now(),
            tick_acc: Duration::ZERO,
            rng_play: None,
            fullscreen,
            shot_counter: 0,
            return_title_armed: None,
            channel_server: None,
            text_engine: Some(text_engine),
            pending_reply_tx: None,
            replay_queue: VecDeque::new(),
            untrusted_cmd_active: false,
            core_replay: None,
            transition_banner: None,
            present_fb: pal_render::framebuffer::Framebuffer::new(1, 1),
            last_thought_gen: 0,
            last_thought_at: None,
            file_picker_rx: None,
            bug_report_rx: None,
            update_rx: Some(updater::spawn_update_check(true, pal_core::Language::default())),
            pending_update: None,
            should_exit: false,
            exit_code: 0,
        };

        if app.bootstrap_rx.is_none() {
            app.finish_boot();
        }
        app
    }

    pub(crate) fn in_title(&self) -> bool {
        self.rt.as_ref().map(|rt| rt.is_in_title()).unwrap_or(true)
    }

    pub(crate) fn cancel_text_task(&mut self) {
        if let Some(mut eng) = self.text_engine.take() {
            eng.cancel_active_task();
            if let Some(tx) = self.pending_reply_tx.take() {
                if self.rt.is_some() {
                    let _ = tx.send(eng.cancelled_reply(self));
                }
            }
            self.text_engine = Some(eng);
        } else {
            self.pending_reply_tx = None;
        }
    }

    fn is_pack_key_error(err: &str) -> bool {
        err.contains("is encrypted")
            || err.contains("REPAL_PACK_KEY")
            || err.contains("unwrap_pack_key")
            || err.contains("authentication failed")
            || err.contains("Invalid pack key")
    }

    fn mount_packs_with_key_prompt(
        &self,
        packs_dir: &std::path::Path,
        loose_overlay: Option<PathBuf>,
    ) -> Result<OverlayVfs, String> {
        let interactive = !self.cfg.text_stdio
            && self.cfg.batch_cmds.is_none()
            && self.cfg.script_file.is_none();

        let mut retry_hint: Option<String> = None;
        match load_pack_key_for_packs(Some(&self.cfg.user_dir), packs_dir) {
            Ok(key) => {
                match OverlayVfs::from_packs_dir(
                    packs_dir.to_path_buf(),
                    key,
                    loose_overlay.clone(),
                    true,
                ) {
                    Ok(v) => return Ok(v),
                    Err(e) if !interactive || !Self::is_pack_key_error(&e) => {
                        return Err(format!(
                            "Failed to mount asset packs from {}: {e}",
                            packs_dir.display()
                        ));
                    }
                    Err(_) if key.is_some() => {
                        retry_hint = Some(
                            "Stored key could not decrypt the asset packs. Please re-enter the 64-hex key.\n本地密钥无法解密资源包，请重新输入 64 位十六进制密钥。"
                                .to_string(),
                        );
                    }
                    Err(_) => {}
                }
            }
            Err(e) if !interactive => {
                return Err(format!("Invalid pack key: {e}"));
            }
            Err(_) => {
                retry_hint = Some(
                    "Stored pack.key is invalid. Please enter a valid 64-hex key.\n本地 pack.key 格式无效，请重新输入 64 位十六进制密钥。"
                        .to_string(),
                );
            }
        }

        let key_file_hint = self.cfg.user_dir.join("pack.key");
        loop {
            let prompt = match &retry_hint {
                Some(h) => h.clone(),
                None => "Enter the asset pack passphrase (or the 64-hex key); saved locally on success:\n请输入资源包口令（或 64 位十六进制密钥），验证成功后将自动保存到本地："
                    .to_string(),
            };
            let Some(entered) = os_bridge::run_os_pack_key_prompt(&prompt) else {
                return Err(format!(
                    "Encrypted asset packs require the passphrase or 64-hex key (set REPAL_PACK_KEY or save to {}).",
                    key_file_hint.display()
                ));
            };
            // Accept either the 64-hex key or the release passphrase (unlocked via the manifest).
            let manifest = std::fs::read_to_string(packs_dir.join("pack-manifest.json"))
                .ok()
                .and_then(|t| pal_runtime::rpk_manifest::PackManifest::from_json_str(&t).ok());
            let parsed_key = match pal_runtime::pack_vfs::resolve_pack_key_input(
                &entered,
                manifest.as_ref(),
            ) {
                Ok(k) => k,
                Err(_) => {
                    retry_hint = Some(
                            "Wrong passphrase or invalid key — please try again.\n口令错误或密钥格式无效，请重试。"
                                .to_string(),
                        );
                    continue;
                }
            };
            match OverlayVfs::from_packs_dir(
                packs_dir.to_path_buf(),
                Some(parsed_key),
                loose_overlay.clone(),
                true,
            ) {
                Ok(vfs) => {
                    // Always persist the resolved 64-hex master key (never the passphrase itself).
                    let key_hex = pal_runtime::crypto::hex_encode(&parsed_key);
                    if let Err(e) = os_bridge::save_pack_key_file(&self.cfg.user_dir, &key_hex) {
                        eprintln!("[repal] warning: failed to save pack.key: {e}");
                    }
                    return Ok(vfs);
                }
                Err(e) if Self::is_pack_key_error(&e) => {
                    retry_hint = Some(
                        "Decryption failed — wrong key. Please try again.\n解密失败，密钥不正确，请重新输入。"
                            .to_string(),
                    );
                }
                Err(e) => {
                    return Err(format!(
                        "Failed to mount asset packs from {}: {e}",
                        packs_dir.display()
                    ));
                }
            }
        }
    }

    pub(crate) fn finish_boot(&mut self) {
        #[cfg(target_os = "linux")]
        linux_integration::ensure_desktop_integration(APP_ICON_PNG);
        let user_cfg = UserConfig::load(&self.cfg.config_path);
        let hd_mode = self.cfg.hd_mode_override.unwrap_or(user_cfg.hd_mode);
        self.vfs = if let Some(packs_dir) = &self.cfg.packs_dir_arg {
            let loose_overlay = self.cfg.assets_arg.as_deref().map(PathBuf::from);
            match self.mount_packs_with_key_prompt(packs_dir, loose_overlay) {
                Ok(v) => v,
                Err(e) => {
                    self.splash.error = Some(e);
                    return;
                }
            }
        } else {
            OverlayVfs::from_loose_dir(self.cfg.assets_root.clone())
        };
        let mut rt = match boot_runtime_from_vfs(&mut self.vfs, hd_mode) {
            Ok(rt) => rt,
            Err(e) => {
                self.splash.error = Some(format!(
                    "Failed to boot RePal from {}: {e}",
                    self.cfg.assets_root.display()
                ));
                return;
            }
        };
        rt.set_lang(user_cfg.lang);
        rt.ui.fullscreen = self.fullscreen;
        if let Some(info) = &self.pending_update {
            rt.ui.update_available = Some(info.version.clone());
        }

        let mut audio = NativeAudio::new(self.cfg.assets_root.clone(), hd_mode);
        if !user_cfg.music_on {
            audio.toggle_music();
            rt.ui.music_on = false;
        }
        if !user_cfg.sfx_on {
            audio.toggle_sfx();
            rt.ui.sfx_on = false;
        }
        self.saves.refresh_slots_vfs(&self.vfs, &mut rt);

        if let Some(save_path) = &self.cfg.initial_save {
            let resolved =
                pal_headless::text_mode::resolve_save_path(&self.cfg.assets_root, save_path);
            match pal_headless::text_mode::load_save_into_rt(&mut rt, &resolved) {
                Ok(()) => {
                    if let Some(g) = rt.game.as_mut() {
                        g.recorder.enabled = true;
                    }
                }
                Err(e) => {
                    eprintln!("[repal] failed to load --save {}: {e}", resolved.display());
                }
            }
        } else if self.cfg.new_game {
            let seed = self.last_frame.elapsed().subsec_nanos() ^ 0x1995_0710;
            if let Some(g) = rt.game.as_mut() {
                g.recorder.enabled = true;
            }
            rt.new_game(seed);
        }

        if self.cfg.debug_mode {
            if let Some(g) = rt.game.as_mut() {
                g.set_debug_mode(true);
            }
            rt.ui.debug_mode = true;
        }

        if let Some(sc) = self.cfg.initial_scene {
            if let Some(g) = rt.game.as_mut() {
                g.in_title = false;
                g.enter_scene(sc, false);
            }
            rt.ui.set_view(UiView::None);
            rt.world.fbp_override = None;
            rt.route_cmds();
            rt.project();
        }

        if let Some(lang) = self.cfg.lang_override {
            rt.set_lang(lang);
        }

        if let Some(addr) = self.cfg.listen_addr.clone() {
            let is_loopback = addr.starts_with("127.0.0.1") || addr.starts_with("localhost");
            let preset_tok = if is_loopback {
                None
            } else {
                Some(self.load_or_create_lan_token())
            };
            match ChannelServer::start_tcp_with_token(
                &addr,
                self.cfg.json_mode,
                self.cfg.record_file.clone(),
                preset_tok,
            ) {
                Ok(srv) => {
                    rt.ui.agent_token = srv.auth_token().map(str::to_string);
                    self.channel_server = Some(srv);
                    rt.ui.agent_mode = if is_loopback { 1 } else { 2 };
                }
                Err(e) => {
                    eprintln!("[repal] Failed to start GUI+Text socket server: {e}");
                    rt.ui.agent_mode = 0;
                    rt.ui.agent_token = None;
                }
            }
        } else if self.cfg.batch_cmds.is_some() || self.cfg.script_file.is_some() {
            let mut cmds = Vec::new();
            if let Some(p) = &self.cfg.script_file {
                match fs::read_to_string(p) {
                    Ok(txt) => cmds.extend(split_batch_commands(&txt)),
                    Err(e) => eprintln!("[repal] failed to read script {}: {e}", p.display()),
                }
            }
            if let Some(inline) = &self.cfg.batch_cmds {
                cmds.extend(split_batch_commands(inline));
            }
            self.channel_server = Some(ChannelServer::start_batch(
                cmds,
                self.cfg.json_mode,
                self.cfg.record_file.clone(),
                self.cfg.keep_going,
            ));
            rt.ui.agent_mode = 1;
        } else if self.cfg.text_stdio {
            self.channel_server = Some(ChannelServer::start_stdio(
                self.cfg.json_mode,
                self.cfg.record_file.clone(),
            ));
            rt.ui.agent_mode = 1;
        }

        self.rt = Some(rt);
        self.audio = Some(audio);
        self.last_frame = Instant::now();
        self.refresh_download_all_label();
        self.service_host_cmds();
        if self.cfg.initial_scene.is_some() {
            if let Some(g) = self.rt.as_mut().and_then(|rt| rt.game.as_mut()) {
                g.reanchor_segment_zero();
            }
        }
        if self.cfg.listen_addr.is_some()
            || self.cfg.batch_cmds.is_some()
            || self.cfg.script_file.is_some()
            || self.cfg.text_stdio
        {
            eprintln!("[repal] Text Mode out_dir: {}", self.cfg.out_dir.display());
        }
        if self.in_title() {
            if let Some(audio) = self.audio.as_mut() {
                audio.play_music_vfs(4, true, Some(&mut self.vfs));
            }
        }
        if let Some(replay_path) = self.cfg.initial_replay.clone() {
            self.load_file_or_replay(replay_path);
        }
    }

    pub(crate) fn poll_bootstrap(&mut self) {
        let mut ready_path = None;
        let mut ready_packs = None;
        if let Some(rx) = &self.bootstrap_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    BootstrapMsg::Progress { pct, stage, detail } => {
                        self.splash.progress = pct;
                        self.splash.stage = stage;
                        self.splash.detail = detail;
                    }
                    BootstrapMsg::Ready(path) => {
                        ready_path = Some(path);
                    }
                    BootstrapMsg::ReadyPacks { packs_dir, fetcher } => {
                        ready_packs = Some((packs_dir, fetcher));
                    }
                    BootstrapMsg::Error(err) => {
                        self.splash.error = Some(err);
                    }
                }
            }
        }
        if let Some((packs_dir, fetcher)) = ready_packs {
            self.cfg.packs_dir_arg = Some(packs_dir.clone());
            self.cfg.assets_root = packs_dir;
            self.pack_fetcher = Some(fetcher);
            self.bootstrap_rx = None;
            self.finish_boot();
        } else if let Some(path) = ready_path {
            self.cfg.assets_root = path;
            self.bootstrap_rx = None;
            self.finish_boot();
        }
    }

    pub(crate) fn ui_active(&self) -> bool {
        let Some(rt) = self.rt.as_ref() else {
            return false;
        };
        rt.is_in_title() || rt.ui.is_modal_open()
    }

    pub(crate) fn core_busy(&self) -> bool {
        self.rt
            .as_ref()
            .and_then(|rt| rt.game.as_ref())
            .map(|g| g.is_busy() || g.is_loading_scene())
            .unwrap_or(false)
    }

    pub(crate) fn held_direction(&self) -> i8 {
        // Classic 1995 DOS/SDLPAL movement keys: Arrows + Numpad 2/4/8/6 (#274).
        // WASD are intentionally NOT bound to movement so A/D/W/S work as battle shortcuts.
        if self.keys_down.contains(&KeyCode::ArrowDown)
            || self.keys_down.contains(&KeyCode::Numpad2)
        {
            0
        } else if self.keys_down.contains(&KeyCode::ArrowLeft)
            || self.keys_down.contains(&KeyCode::Numpad4)
        {
            1
        } else if self.keys_down.contains(&KeyCode::ArrowUp)
            || self.keys_down.contains(&KeyCode::Numpad8)
        {
            2
        } else if self.keys_down.contains(&KeyCode::ArrowRight)
            || self.keys_down.contains(&KeyCode::Numpad6)
        {
            3
        } else {
            -1
        }
    }

    /// Map physical window coordinates into logical `320x200` coordinates respecting 16:10 letterboxing.
    pub(crate) fn phys_to_logical(&self, px: f64, py: f64) -> (i32, i32) {
        let Some(win) = &self.window else {
            return (0, 0);
        };
        let sz = win.inner_size();
        let (dw, dh) = (sz.width as f64, sz.height as f64);
        if dw <= 0.0 || dh <= 0.0 {
            return (0, 0);
        }
        let target_aspect = 16.0 / 10.0;
        let win_aspect = dw / dh;
        let (vx, vy, vw, vh) = if win_aspect > target_aspect {
            let vw = dh * target_aspect;
            ((dw - vw) * 0.5, 0.0, vw, dh)
        } else {
            let vh = dw / target_aspect;
            (0.0, (dh - vh) * 0.5, dw, vh)
        };
        let lx = (((px - vx) / vw) * 320.0).floor() as i32;
        let ly = (((py - vy) / vh) * 200.0).floor() as i32;
        (lx.clamp(0, 319), ly.clamp(0, 199))
    }

    pub(crate) fn logical_to_world(&self, lx: i32, ly: i32) -> (i32, i32) {
        let Some(g) = self.rt.as_ref().and_then(|rt| rt.game.as_ref()) else {
            return (lx, ly);
        };
        let (cx, cy) = match g.world.view.cam_override {
            Some((x, y)) => (x, y),
            None => {
                let l = g.world.leader();
                (l.x - 160, l.y - 100)
            }
        };
        (
            cx.clamp(0, 2048 - 320) + lx,
            cy.clamp(0, 2048 - 200) + ly,
        )
    }

    pub(crate) fn set_fullscreen(&mut self, enable: bool) {
        self.fullscreen = enable;
        if let Some(win) = &self.window {
            win.set_fullscreen(if enable {
                Some(Fullscreen::Borderless(None))
            } else {
                None
            });
        }
        if let Some(rt) = self.rt.as_mut() {
            rt.ui.fullscreen = enable;
            rt.compositor.mark_ui_dirty();
            let msg = match (enable, rt.ui.lang) {
                (true, pal_core::Language::En) => "Display: Fullscreen",
                (false, pal_core::Language::En) => "Display: Windowed",
                (true, pal_core::Language::ZhHans) => "显示模式：全屏",
                (false, pal_core::Language::ZhHans) => "显示模式：窗口",
                (true, pal_core::Language::ZhHant) => "顯示模式：全螢幕",
                (false, pal_core::Language::ZhHant) => "顯示模式：視窗",
            };
            rt.show_toast(msg, 100);
        }
        self.persist_user_config();
    }

    pub(crate) fn set_agent_mode(&mut self, mode: u8) {
        self.set_agent_mode_inner(mode, false);
    }

    pub(crate) fn copy_agent_token(&mut self) {
        let Some((tok, lang)) = self.rt.as_ref().and_then(|rt| {
            rt.ui
                .agent_token
                .as_deref()
                .filter(|t| !t.is_empty())
                .map(|t| (t.to_string(), rt.ui.lang))
        }) else {
            return;
        };
        let copied = session_io::copy_text_to_clipboard(&tok);
        if let Some(rt) = self.rt.as_mut() {
            let msg = match (copied, lang) {
                (true, pal_core::Language::En) => "LAN token copied to clipboard",
                (true, pal_core::Language::ZhHans) => "LAN token 已复制到剪贴板",
                (true, pal_core::Language::ZhHant) => "LAN token 已複製到剪貼簿",
                (false, pal_core::Language::En) => "Clipboard unavailable; see agent_lan_token.txt",
                (false, pal_core::Language::ZhHans) => "无法访问剪贴板，请查看 agent_lan_token.txt",
                (false, pal_core::Language::ZhHant) => "無法存取剪貼簿，請查看 agent_lan_token.txt",
            };
            rt.show_toast(msg, 150);
        }
    }

    pub(crate) fn refresh_agent_token(&mut self) {
        if self.untrusted_cmd_active {
            return;
        }
        let _ = self.rotate_lan_token();
        self.set_agent_mode_inner(2, true);
    }

    fn set_agent_mode_inner(&mut self, mode: u8, refreshed: bool) {
        // Stop any existing server first so switching Local <-> LAN or refreshing releases the socket cleanly.
        if self.channel_server.is_some() {
            self.channel_server = None;
            self.cancel_text_task();
            if mode > 0 {
                std::thread::sleep(Duration::from_millis(70));
            }
        }
        let port = self.cfg.listen_port;
        match mode {
            1 | 2 => {
                let host = if mode == 2 { "0.0.0.0" } else { "127.0.0.1" };
                let preset_tok = if mode == 2 {
                    Some(self.load_or_create_lan_token())
                } else {
                    None
                };
                match ChannelServer::start_tcp_with_token(
                    &format!("{host}:{port}"),
                    self.cfg.json_mode,
                    self.cfg.record_file.clone(),
                    preset_tok,
                ) {
                    Ok(srv) => {
                        let tok_opt = srv.auth_token().map(str::to_string);
                        let copied = if mode == 2 {
                            tok_opt
                                .as_deref()
                                .map(session_io::copy_text_to_clipboard)
                                .unwrap_or(false)
                        } else {
                            false
                        };
                        self.channel_server = Some(srv);
                        if let Some(rt) = self.rt.as_mut() {
                            rt.ui.agent_mode = mode;
                            rt.ui.agent_token = tok_opt;
                            rt.compositor.mark_ui_dirty();
                            let msg = match (mode, refreshed, copied, rt.ui.lang) {
                                (2, true, true, pal_core::Language::En) => {
                                    "LAN token refreshed & copied to clipboard".to_string()
                                }
                                (2, true, true, pal_core::Language::ZhHans) => {
                                    "LAN token 已刷新并复制到剪贴板".to_string()
                                }
                                (2, true, true, pal_core::Language::ZhHant) => {
                                    "LAN token 已刷新並複製到剪貼簿".to_string()
                                }
                                (2, true, false, pal_core::Language::En) => {
                                    "LAN token refreshed (saved in agent_lan_token.txt)".to_string()
                                }
                                (2, true, false, pal_core::Language::ZhHans) => {
                                    "LAN token 已刷新（已写入 agent_lan_token.txt）".to_string()
                                }
                                (2, true, false, pal_core::Language::ZhHant) => {
                                    "LAN token 已刷新（已寫入 agent_lan_token.txt）".to_string()
                                }
                                (2, false, true, pal_core::Language::En) => {
                                    format!("Agent LAN (0.0.0.0:{port}) — token copied")
                                }
                                (2, false, true, pal_core::Language::ZhHans) => {
                                    format!("AI 旁观：局域网 (:{port}) — token 已复制")
                                }
                                (2, false, true, pal_core::Language::ZhHant) => {
                                    format!("AI 旁觀：區網 (:{port}) — token 已複製")
                                }
                                (2, false, false, pal_core::Language::En) => {
                                    format!("Agent LAN (0.0.0.0:{port}) — see agent_lan_token.txt")
                                }
                                (2, false, false, pal_core::Language::ZhHans) => {
                                    format!("AI 旁观：局域网 (:{port}) — 见 agent_lan_token.txt")
                                }
                                (2, false, false, pal_core::Language::ZhHant) => {
                                    format!("AI 旁觀：區網 (:{port}) — 見 agent_lan_token.txt")
                                }
                                (1, _, _, pal_core::Language::En) => {
                                    format!("Agent Mode: Local (127.0.0.1:{port})")
                                }
                                (1, _, _, pal_core::Language::ZhHans) => {
                                    format!("AI 旁观模式：本机 (127.0.0.1:{port})")
                                }
                                _ => format!("AI 旁觀模式：本機 (127.0.0.1:{port})"),
                            };
                            rt.show_toast(msg, if mode == 2 { 220 } else { 150 });
                        }
                    }
                    Err(e) => {
                        eprintln!("[repal] Failed to start Agent server on {host}:{port}: {e}");
                        if let Some(rt) = self.rt.as_mut() {
                            rt.ui.agent_mode = 0;
                            rt.ui.agent_token = None;
                            rt.compositor.mark_ui_dirty();
                            rt.show_toast(format!("Agent Mode error: {e}"), 140);
                        }
                    }
                }
            }
            _ => {
                if let Some(rt) = self.rt.as_mut() {
                    rt.ui.agent_mode = 0;
                    rt.ui.agent_token = None;
                    rt.compositor.mark_ui_dirty();
                    let msg = match rt.ui.lang {
                        pal_core::Language::En => "Agent Mode: Off",
                        pal_core::Language::ZhHans => "AI 旁观模式：已关闭",
                        pal_core::Language::ZhHant => "AI 旁觀模式：已關閉",
                    };
                    rt.show_toast(msg, 110);
                }
            }
        }
    }

}

fn main() {
    match parse_cli() {
        LaunchMode::ExportSrt { input, output } => {
            let bundle = match pal_runtime::repal_archive::read_repal_file(&input) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("[repal] failed to read {}: {e}", input.display());
                    std::process::exit(1);
                }
            };
            let srt = pal_runtime::repal_archive::export_repal_to_srt(&bundle);
            if let Some(parent) = output.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            if let Err(e) = fs::write(&output, &srt) {
                eprintln!("[repal] failed to write {}: {e}", output.display());
                std::process::exit(1);
            }
            let cues = srt.matches(" --> ").count();
            println!(
                "[repal] Exported {cues} AI thought subtitle cue(s) to {}",
                output.display()
            );
            std::process::exit(0);
        }
        LaunchMode::ExportVideo {
            input,
            output,
            opts,
            assets_dir,
            packs_dir,
        } => {
            let mut app = if let Some(pdir) = packs_dir {
                let key = std::env::var("REPAL_PACK_KEY")
                    .ok()
                    .and_then(|s| pal_runtime::crypto::parse_key_hex(&s).ok());
                match pal_headless::HeadlessApp::from_packs_dir(
                    &pdir,
                    key,
                    assets_dir,
                    opts.hd_mode,
                ) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("[repal] failed to open packs dir {}: {e}", pdir.display());
                        std::process::exit(1);
                    }
                }
            } else {
                let dir = pal_runtime::disk::resolve_assets_dir(
                    assets_dir.as_deref().and_then(|p| p.to_str()),
                );
                pal_headless::HeadlessApp::new(dir, opts.hd_mode)
            };
            match pal_headless::capture::export_repal_video(&mut app, &input, &output, &opts) {
                Ok(sum) => {
                    println!(
                        "[repal] Exported {} ({} frames / {} ticks, {}x{} @ {}fps, format={}, audio={})",
                        sum.path.display(),
                        sum.frames,
                        sum.ticks,
                        sum.width,
                        sum.height,
                        sum.fps,
                        sum.format,
                        sum.has_audio
                    );
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("[repal] video export failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        LaunchMode::Text(tcfg) => {
            let code = pal_headless::text_mode::run_text_mode(tcfg);
            std::process::exit(code);
        }
        LaunchMode::Gui(cfg) => {
            let event_loop = EventLoop::new().expect("failed to create event loop");
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(16),
            ));
            let mut app = DesktopApp::new(cfg);
            event_loop.run_app(&mut app).expect("event loop error");
            if app.exit_code != 0 {
                std::process::exit(app.exit_code);
            }
        }
    }
}
