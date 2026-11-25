//! `pal-headless` — Native Browserless Headless Runner & Deterministic PNG Exporter.
//!
//! Runs the exact same shared `pal-runtime` (`pal-core` + `pal-render` + `pal-ui`) pipeline
//! natively on x86_64 using synchronous local disk asset loading (`site/assets/`).

pub mod capture;
pub mod replay;
pub mod text_mode;

use pal_core::game::{HostCmd, HostReply, TickInput};
use pal_core::isometric::CollisionGrid;
use pal_core::render::try_manual_step;
use pal_render::world::{map_bin_key, BattleEnemyVisual, BattleHeroVisual};
use pal_runtime::disk::{
    load_all_chapters_from_vfs, load_fonts_from_vfs, load_game_data_from_vfs,
    load_scene_assets_from_vfs, pump_render_assets_from_dir, pump_render_assets_from_vfs,
};
use pal_runtime::pack_vfs::OverlayVfs;
use pal_runtime::PalRuntime;
use pal_ui::{PointerEvent, UiAction, UiKey, UiView};
use std::fs;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

pub struct HeadlessApp {
    pub assets_root: PathBuf,
    pub vfs: OverlayVfs,
    pub rt: PalRuntime,
    chapters_loaded: bool,
    collision_map_id: u16,
    collision_grid: Option<CollisionGrid>,
}

impl Deref for HeadlessApp {
    type Target = PalRuntime;
    fn deref(&self) -> &Self::Target {
        &self.rt
    }
}

impl DerefMut for HeadlessApp {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.rt
    }
}

impl HeadlessApp {
    pub fn new(assets_root: impl Into<PathBuf>, hd_mode: bool) -> Self {
        let assets_root = assets_root.into();
        let mut vfs = OverlayVfs::from_loose_dir(assets_root.clone());
        let mut rt = PalRuntime::new(hd_mode);
        load_fonts_from_vfs(&mut vfs, &mut rt.compositor);
        // Load static YAML tables so `ui.catalog` has real item/magic/role names & equip slots.
        if let Ok(data) = load_game_data_from_vfs(&mut vfs) {
            rt.data = data;
            rt.init_game();
        }
        Self {
            assets_root,
            vfs,
            rt,
            chapters_loaded: false,
            collision_map_id: 0,
            collision_grid: None,
        }
    }

    /// Boot a headless runtime directly from a local `--packs-dir` (plain or AES-256-GCM encrypted
    /// `.rpk` packs), with an optional loose `--assets` overlay taking priority (#237).
    pub fn from_packs_dir(
        packs_dir: impl Into<PathBuf>,
        master_key: Option<[u8; 32]>,
        loose_overlay: Option<PathBuf>,
        hd_mode: bool,
    ) -> Result<Self, String> {
        let packs_dir = packs_dir.into();
        let assets_root = loose_overlay.clone().unwrap_or_else(|| packs_dir.clone());
        let mut vfs = OverlayVfs::from_packs_dir(packs_dir, master_key, loose_overlay, true)?;
        let mut rt = PalRuntime::new(hd_mode);
        load_fonts_from_vfs(&mut vfs, &mut rt.compositor);
        rt.data = load_game_data_from_vfs(&mut vfs)?;
        rt.init_game();
        Ok(Self {
            assets_root,
            vfs,
            rt,
            chapters_loaded: false,
            collision_map_id: 0,
            collision_grid: None,
        })
    }

    /// Lazily load all 11 YAML script chapters into the `Game` VM.
    pub fn ensure_chapters_loaded(&mut self) -> Result<(), String> {
        if self.chapters_loaded {
            return Ok(());
        }
        if let Some(g) = self.rt.game.as_mut() {
            load_all_chapters_from_vfs(&mut self.vfs, g)?;
            self.chapters_loaded = true;
        }
        Ok(())
    }

    /// Start a real deterministic new game through `pal_core::game::Game`.
    pub fn new_game(&mut self, seed: u32) -> Result<(), String> {
        self.ensure_chapters_loaded()?;
        self.rt.new_game(seed);
        self.pump_host_cmds();
        Ok(())
    }

    /// Advance the game core by one 20 ms tick and service any blocking host commands.
    pub fn tick(&mut self, input: TickInput) {
        self.rt.tick(input);
        self.pump_host_cmds();
    }

    /// Service all queued platform-facing `HostCmd`s synchronously from local disk or `.rpk` VFS.
    pub fn pump_host_cmds(&mut self) {
        while let Some(cmd) = self.rt.out_cmds.pop_front() {
            match cmd {
                HostCmd::LoadScene { map_id, .. } => {
                    if let Some(g) = self.rt.game.as_mut() {
                        load_scene_assets_from_vfs(&mut self.vfs, g, map_id);
                    }
                    self.rt.reply(HostReply::SceneLoaded);
                }
                HostCmd::LoadChapter(_) => {
                    let _ = self.ensure_chapters_loaded();
                    self.rt.reply(HostReply::Done);
                }
                HostCmd::PlayRng { id, .. } => {
                    let _ = self.vfs.ensure_rng_pack(id.max(0) as u16);
                    self.rt.reply(HostReply::Done);
                }
                HostCmd::ReturnToTitle | HostCmd::GameOver => {
                    self.rt.set_in_title(true);
                }
                _ => {}
            }
        }
    }

    pub fn active_map_id(&self) -> u16 {
        self.rt
            .world
            .map_id_override
            .filter(|&id| id > 0)
            .or_else(|| {
                self.rt
                    .state
                    .scenes
                    .iter()
                    .find(|s| s.id == self.rt.state.scene_id)
                    .map(|s| s.map_id)
                    .filter(|&id| id > 0)
            })
            .unwrap_or(self.rt.state.scene_id)
            .max(1)
    }

    /// Ensure the active map's 64 KB `CollisionGrid` is loaded and take one isometric half-tile step.
    pub fn step_walk(&mut self, dir: u8) -> bool {
        if self.rt.game.is_some() {
            let moved = self
                .rt
                .game
                .as_mut()
                .map(|g| g.try_manual_step(dir & 3))
                .unwrap_or(false);
            self.rt.route_cmds();
            self.pump_host_cmds();
            self.rt.project();
            self.rt.compositor.mark_ui_dirty();
            return moved;
        }
        let map_id = self.active_map_id();
        if self.collision_map_id != map_id || self.collision_grid.is_none() {
            let key = map_bin_key(map_id);
            if let Ok(bytes) = self.vfs.read_bytes(&key) {
                if bytes.len() >= 65536 {
                    self.collision_grid = CollisionGrid::from_raw_map64k(&bytes[..65536]);
                    self.collision_map_id = map_id;
                }
            }
        }
        let Some(grid) = self.collision_grid.as_ref() else {
            return false;
        };
        let moved = try_manual_step(&mut self.rt.state, grid, dir & 3);
        self.rt.compositor.mark_ui_dirty();
        moved
    }

    pub fn set_scene(&mut self, scene_id: u16) {
        self.rt.state.scene_id = scene_id;
        if let Some(g) = self.rt.game.as_mut() {
            g.world.scene_id = scene_id;
            if let Some(sc) = g.world.scenes.get(&scene_id) {
                if sc.map_id > 0 {
                    self.rt.world.map_id_override = Some(sc.map_id);
                }
                self.rt.world.ev_start = sc.event_start;
                self.rt.world.ev_end = sc.event_end;
            }
        }
        self.rt.world.fbp_override = None;
        self.rt.compositor.mark_ui_dirty();
    }

    pub fn load_rpg_file(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let bytes = fs::read(path.as_ref()).map_err(|e| format!("read rpg: {}", e))?;
        if self.rt.game.is_some() {
            if !self.rt.load_dos_rpg(&bytes) {
                return Err("parse rpg failed".to_string());
            }
            self.pump_host_cmds();
            return Ok(());
        }
        self.rt
            .state
            .load_dos_rpg(&bytes)
            .map_err(|e| format!("parse rpg: {}", e))?;
        self.rt.world.battle.active = false;
        self.rt.world.active_rng = None;
        self.rt.world.fbp_override = None;
        self.rt.world.night_palette = self.rt.state.night_palette;
        self.rt.world.wave_amplitude = if self.rt.state.screen_wave > 0 {
            (self.rt.state.screen_wave as f32 * 1.8).min(16.0)
        } else {
            0.0
        };
        self.rt.ui.set_view(UiView::None);
        self.rt.compositor.mark_ui_dirty();
        Ok(())
    }

    pub fn set_hd_mode(&mut self, hd_mode: bool) {
        self.rt.set_hd_mode(hd_mode);
    }

    pub fn press_key(&mut self, key: UiKey) -> UiAction {
        let act = self.rt.on_key(key).unwrap_or(UiAction::None);
        self.pump_host_cmds();
        act
    }

    pub fn send_pointer(&mut self, ev: PointerEvent) -> UiAction {
        let act = self.rt.on_pointer(ev).unwrap_or(UiAction::None);
        self.pump_host_cmds();
        act
    }

    /// Populate a realistic sample battle scene using real `fbp`, `abc`, and `f` sprites.
    pub fn enter_sample_battle(&mut self, battlefield_fbp: u16) {
        populate_sample_battle(&mut self.rt, battlefield_fbp);
    }

    /// Synchronously fulfill all pending asset requests from VFS (loose overlay or `.rpk` packs).
    pub fn pump_assets(&mut self) -> usize {
        pump_render_assets_from_vfs(&mut self.vfs, &mut self.rt.compositor.cache)
    }

    /// Render a complete frame, synchronously pumping any newly-requested assets so the
    /// resulting framebuffer has zero missing textures on the very first call.
    pub fn render_settled_frame(&mut self) {
        render_settled_frame_vfs(&mut self.rt, &mut self.vfs);
    }

    /// Export the current RGBA framebuffer to a PNG file on disk.
    pub fn export_png(&self, path: impl AsRef<Path>) -> Result<(), String> {
        export_fb_to_png(&self.rt.compositor.fb, path.as_ref())
    }

    /// Compute percentage of pixels that are non-black (sanity metric for visual tests).
    pub fn non_black_pixel_ratio(&self) -> f64 {
        non_black_pixel_ratio_fb(&self.rt.compositor.fb)
    }
}

/// Populate a realistic sample battle scene on any `PalRuntime` using real `fbp`, `abc`, and `f` sprites.
pub fn populate_sample_battle(rt: &mut PalRuntime, battlefield_fbp: u16) {
    if let Some(g) = rt.game.as_mut() {
        g.in_title = false;
    }
    rt.world.fbp_override = None;
    rt.world.battle.active = true;
    rt.world.battle.prompting = true;
    rt.world.battle.battlefield_fbp = battlefield_fbp;
    rt.world.battle.selected_enemy = Some(0);
    rt.world.battle.active_hero = 0;

    rt.world.battle.enemies = vec![
        BattleEnemyVisual {
            slot: 0,
            sprite_id: 2,
            frame: 0,
            x: 84,
            y: 112,
            hp: 180,
            max_hp: 180,
            alive: true,
            flash: false,
            name: "苗人頭領".into(),
            ..Default::default()
        },
        BattleEnemyVisual {
            slot: 1,
            sprite_id: 1,
            frame: 0,
            x: 138,
            y: 92,
            hp: 120,
            max_hp: 120,
            alive: true,
            flash: false,
            name: "黑苗戰士".into(),
            ..Default::default()
        },
    ];

    rt.world.battle.heroes = rt
        .state
        .party
        .iter()
        .take(3)
        .enumerate()
        .map(|(i, pm)| {
            let role = &rt.state.roles[(pm.role_id as usize).min(5)];
            BattleHeroVisual {
                role_id: role.role_id,
                sprite_id: role.battle_sprite_f,
                frame: 0,
                x: 222 + (i as i32) * 28,
                y: 132 + (i as i32) * 10,
                hp: role.hp,
                max_hp: role.max_hp,
                mp: role.mp,
                max_mp: role.max_mp,
                flash: false,
                name: rt.ui.catalog.role_name(role.role_id).to_string(),
                avatar_rgm: role.avatar_rgm,
                atb_pct: if i == 0 { 100 } else { 75 },
                ..Default::default()
            }
        })
        .collect();
    rt.ui.open_battle();
    rt.compositor.mark_ui_dirty();
}

/// Render a complete frame on any `PalRuntime`, synchronously pumping missing disk assets.
pub fn render_settled_frame_rt(rt: &mut PalRuntime, assets_root: &Path) {
    // Flush any pending core-tick projection (`render_dirty`) without clobbering manual
    // `rt.world` overrides used by visual harness tests.
    rt.flush_projection();
    for _ in 0..8 {
        rt.compositor.mark_ui_dirty();
        rt.render_frame();
        if !rt.compositor.cache.has_pending_requests() {
            break;
        }
        // Note: do NOT break when `loaded == 0` — marking a missing `maps_hd/map_XXX/meta.json`
        // permanently absent changes the next render pass to fall back to 1x GOP tiles (#178/#189).
        let _ = pump_render_assets_from_dir(assets_root, &mut rt.compositor.cache);
    }
}

/// Render a complete frame on any `PalRuntime`, synchronously pumping missing assets via [`OverlayVfs`].
pub fn render_settled_frame_vfs(rt: &mut PalRuntime, vfs: &mut OverlayVfs) {
    rt.flush_projection();
    for _ in 0..8 {
        rt.compositor.mark_ui_dirty();
        rt.render_frame();
        if !rt.compositor.cache.has_pending_requests() {
            break;
        }
        let _ = pump_render_assets_from_vfs(vfs, &mut rt.compositor.cache);
    }
}

/// Export an RGBA framebuffer to a PNG file on disk (creating parent directories automatically).
pub fn export_fb_to_png(fb: &pal_render::framebuffer::Framebuffer, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = fs::create_dir_all(parent);
        }
    }
    let img = image::RgbaImage::from_raw(fb.width, fb.height, fb.pixels.clone())
        .ok_or_else(|| "framebuffer dimensions mismatch".to_string())?;
    img.save(path).map_err(|e| format!("save png: {}", e))
}

/// Compute percentage of pixels that are non-black (0.0..=1.0).
pub fn non_black_pixel_ratio_fb(fb: &pal_render::framebuffer::Framebuffer) -> f64 {
    let total = (fb.width as usize) * (fb.height as usize);
    if total == 0 {
        return 0.0;
    }
    let non_black = fb
        .pixels
        .chunks_exact(4)
        .filter(|p| p[0] > 4 || p[1] > 4 || p[2] > 4)
        .count();
    (non_black as f64) / (total as f64)
}
