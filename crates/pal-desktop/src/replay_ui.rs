//! Segmented `RePal_CoreReplay_v3` deterministic replay player (with hard-load on every
//! segment transition) and cinema-style bottom overlay for `pal-desktop`:
//! - **Always visible**: razor-thin translucent progress bar (`5px`) at the very bottom.
//! - **Auto-hiding**: compact playback buttons (`|<`, `||/>`, `>|`, `1X`, `CHAPTERS`, `TAKE OVER`, `EXIT`)
//!   appear above the progress bar on mouse movement / hotkey / pause and fade out after 3s idle.

use crate::splash::{draw_rect_outline, draw_text_at, fill_rect, fill_rect_alpha};
use image::ImageEncoder;
use pal_core::game::replay_rec::{
    is_replay_engine_compatible, ReplayAnchor, ReplayBundle, ReplayEvent,
    MIN_COMPAT_REPLAY_VERSION,
};
use pal_render::framebuffer::Framebuffer;
use pal_runtime::replay_player::{ReplayPlayer, ReplayStepOutcome};
use pal_runtime::PalRuntime;
use pal_ui::ReplayChapterSummary;
use std::ops::{Deref, DerefMut};
use std::time::{Duration, Instant};

pub const THIN_BAR_HEIGHT: usize = 5;
pub const CONTROLS_BAR_HEIGHT: usize = 44;
pub const CONTROLS_IDLE_HIDE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayHudHit {
    PrevSeg,
    PlayPause,
    Speed,
    NextSeg,
    Chapters,
    TakeOver,
    Exit,
    SeekRatio(u16), // 0..=1000
}

#[allow(dead_code)]
pub struct CoreReplayPlayer {
    pub filename: String,
    pub inner: ReplayPlayer,
    pub version_mismatch: bool,
    pub paused: bool,
    pub speed: u32, // 1, 2, 4, 8
    pub last_ui_activity: Instant,
    pub pointer_in_bottom: bool,
    /// Presentation-only wall-clock reading pause before dismissing a dialogue box (zero determinism impact).
    pub dialog_hold_until: Option<Instant>,
    /// Set when an automatic segment transition corrected a position/scene drift (host shows dim banner).
    pub pending_sync_banner: Option<String>,
}

impl Deref for CoreReplayPlayer {
    type Target = ReplayPlayer;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for CoreReplayPlayer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl CoreReplayPlayer {
    pub fn from_bundle(filename: String, bundle: ReplayBundle) -> Result<Self, String> {
        let version_mismatch = !is_replay_engine_compatible(&bundle.engine_version);
        let inner = ReplayPlayer::new(bundle)?;
        Ok(Self {
            filename,
            inner,
            version_mismatch,
            paused: false,
            speed: 1,
            last_ui_activity: Instant::now(),
            pointer_in_bottom: false,
            dialog_hold_until: None,
            pending_sync_banner: None,
        })
    }

    pub fn touch_ui(&mut self) {
        self.last_ui_activity = Instant::now();
    }

    pub fn controls_visible(&self) -> bool {
        self.paused
            || self.finished
            || self.pointer_in_bottom
            || self.last_ui_activity.elapsed() < CONTROLS_IDLE_HIDE
    }

    /// Build `pal-ui` chapter summaries (with decoded RGBA thumbnails) for `UiView::ReplayChapters`.
    pub fn build_chapter_summaries(&self) -> Vec<ReplayChapterSummary> {
        self.bundle
            .segments
            .iter()
            .map(|s| ReplayChapterSummary {
                index: s.index,
                start_tick: s.start_tick,
                scene_id: s.scene_id,
                label: s.label.clone(),
                thumb_rgba: s.thumb_png.as_deref().and_then(decode_png_rgba),
            })
            .collect()
    }

    /// Hard-load segment `idx` into `rt` (guarantees 100% deterministic alignment with zero drift).
    pub fn load_segment(&mut self, idx: usize, rt: &mut PalRuntime) -> bool {
        let ok = self.inner.load_segment(idx, rt);
        if ok {
            self.dialog_hold_until = None;
            rt.ui.replay_chapter_cursor = idx;
        }
        ok
    }

    pub fn start(&mut self, rt: &mut PalRuntime) -> bool {
        self.load_segment(0, rt)
    }

    pub fn prev_segment(&mut self, rt: &mut PalRuntime) -> Option<String> {
        self.touch_ui();
        let target = if self.seg_tick > 50 || self.seg_idx == 0 {
            self.seg_idx
        } else {
            self.seg_idx - 1
        };
        let lbl = self.bundle.segments.get(target)?.label.clone();
        self.load_segment(target, rt);
        Some(format!("#{:02} {lbl}", target + 1))
    }

    pub fn next_segment(&mut self, rt: &mut PalRuntime) -> Option<String> {
        self.touch_ui();
        let target = self.seg_idx + 1;
        let lbl = self.bundle.segments.get(target)?.label.clone();
        self.load_segment(target, rt);
        Some(format!("#{:02} {lbl}", target + 1))
    }

    pub fn seek_by_ratio(&mut self, ratio_milli: u16, rt: &mut PalRuntime) -> Option<String> {
        self.touch_ui();
        let target_tick = (self.bundle.total_ticks * (ratio_milli as u64)) / 1000;
        let idx = self
            .bundle
            .segments
            .iter()
            .rposition(|s| s.start_tick <= target_tick)
            .unwrap_or(0);
        let lbl = self.bundle.segments.get(idx)?.label.clone();
        self.load_segment(idx, rt);
        Some(format!("#{:02} {lbl}", idx + 1))
    }

    pub fn cycle_speed(&mut self) {
        self.touch_ui();
        self.speed = match self.speed {
            1 => 2,
            2 => 4,
            4 => 8,
            _ => 1,
        };
    }

    pub fn speed_up(&mut self) {
        self.touch_ui();
        self.speed = match self.speed {
            1 => 2,
            2 => 4,
            _ => 8,
        };
    }

    pub fn speed_down(&mut self) {
        self.touch_ui();
        self.speed = match self.speed {
            8 => 4,
            4 => 2,
            _ => 1,
        };
    }

    /// Advance the replay by one deterministic logic tick (50 Hz), delegating core event
    /// dispatch & segment stepping to [`pal_runtime::replay_player::ReplayPlayer`].
    pub fn step_one_tick(
        &mut self,
        rt: &mut PalRuntime,
        vfs: &mut pal_runtime::pack_vfs::OverlayVfs,
    ) {
        if self.paused || self.finished {
            return;
        }

        // Presentation-only dialogue reading pause (1X/2X): freezes logic tick progression without
        // altering determinism so fast headless-recorded dialogues remain comfortably readable!
        if self.speed <= 2
            && matches!(rt.ui.view, pal_ui::UiView::Dialog)
            && matches!(
                self.inner.peek_due_event(),
                Some(ReplayEvent::DialogueDone | ReplayEvent::Choice(_))
            )
        {
            let now = Instant::now();
            match self.dialog_hold_until {
                None => {
                    let chars = rt.ui.dialog.text.chars().count() as u64;
                    let ms = (1200 + chars * 50).clamp(1200, 3400) / (self.speed.max(1) as u64);
                    self.dialog_hold_until = Some(now + Duration::from_millis(ms));
                    return;
                }
                Some(until) if now < until => return,
                Some(_) => {
                    self.dialog_hold_until = None;
                }
            }
        } else if !matches!(rt.ui.view, pal_ui::UiView::Dialog) {
            self.dialog_hold_until = None;
        }

        let outcome = self.inner.step_one_tick(
            rt,
            |rt, ev| {
                if let Some(toast) = format_high_level_action_toast(rt, ev) {
                    rt.show_toast(toast, 90);
                }
                if let ReplayEvent::Click(x, y) = *ev {
                    rt.world.click_ripple = Some((x, y, 0.0));
                }
            },
            |rt, _ev| {
                // Service synchronous asset-load commands between same-tick events so a subsequent
                // event on the same tick (e.g. after `LoadScene`) sees the new map (#242).
                let mut deferred = std::collections::VecDeque::new();
                while let Some(cmd) = rt.out_cmds.pop_front() {
                    match cmd {
                        pal_core::game::HostCmd::LoadScene { map_id, .. } => {
                            if let Some(g) = rt.game.as_mut() {
                                pal_runtime::disk::load_scene_assets_from_vfs(vfs, g, map_id);
                            }
                            rt.reply(pal_core::game::HostReply::SceneLoaded);
                        }
                        pal_core::game::HostCmd::LoadChapter(_) => {
                            rt.reply(pal_core::game::HostReply::Done);
                        }
                        other => deferred.push_back(other),
                    }
                }
                rt.out_cmds.extend(deferred);
            },
        );

        match outcome {
            ReplayStepOutcome::Stepped => {}
            ReplayStepOutcome::SegmentCompleted { next_idx } => {
                let drift_banner = self.bundle.segments.get(next_idx).and_then(|ns| {
                    let g = rt.game.as_ref()?;
                    let ReplayAnchor::SaveJson(save) = &ns.anchor else {
                        return None;
                    };
                    let exp_sc = save["sceneId"].as_u64().unwrap_or(0) as u16;
                    let exp_x = save["party"][0]["x"].as_i64().unwrap_or(0) as i32;
                    let exp_y = save["party"][0]["y"].as_i64().unwrap_or(0) as i32;
                    let lead = g.world.leader();
                    if g.world.scene_id != exp_sc
                        || (lead.x - exp_x).abs() > 24
                        || (lead.y - exp_y).abs() > 24
                    {
                        Some(format!("章节同步 #{:02} · {}", next_idx + 1, ns.label))
                    } else {
                        None
                    }
                });
                self.load_segment(next_idx, rt);
                if drift_banner.is_some() {
                    self.pending_sync_banner = drift_banner;
                }
            }
            ReplayStepOutcome::Finished => {
                self.paused = true;
                rt.ui.replay_finished_cursor = 0;
                rt.ui.set_view(pal_ui::UiView::ReplayFinished);
                rt.compositor.mark_ui_dirty();
            }
        }
    }
}

/// Format a concise high-level command label for replay & spectator on-screen notifications.
pub fn format_high_level_action_toast(rt: &PalRuntime, ev: &ReplayEvent) -> Option<String> {
    let g = rt.game.as_ref()?;
    match *ev {
        ReplayEvent::UseItem { item_id, .. } => Some(format!("使用 · {}", g.item_display_name(item_id))),
        ReplayEvent::EquipItem { item_id, .. } => Some(format!("装备 · {}", g.item_display_name(item_id))),
        ReplayEvent::CastFieldMagic { magic_id, .. } => {
            Some(format!("施放 · {}", g.magic_display_name(magic_id)))
        }
        ReplayEvent::BattleAttack(target) => {
            let ename = g
                .battle
                .as_ref()
                .and_then(|b| b.enemies.get(target))
                .map(|e| g.enemy_display_name(e.enemy_id, &e.name, &e.name_hant))
                .unwrap_or_default();
            Some(if ename.is_empty() {
                "攻击".to_string()
            } else {
                format!("攻击 · {ename}")
            })
        }
        ReplayEvent::BattleMagic { magic, .. } => {
            Some(format!("仙术 · {}", g.magic_display_name(magic)))
        }
        ReplayEvent::BattleItem(id) => Some(format!("道具 · {}", g.item_display_name(id))),
        ReplayEvent::BattleDefend => Some("防御".to_string()),
        ReplayEvent::BattleFlee => Some("逃跑".to_string()),
        ReplayEvent::BuyItem { item_id, .. } => Some(format!("购入 · {}", g.item_display_name(item_id))),
        ReplayEvent::SellItem { item_id, .. } => Some(format!("典当 · {}", g.item_display_name(item_id))),
        _ => None,
    }
}

/// Render a full-screen dim + centered CJK text banner onto `rt.compositor.fb` (for load / chapter jump transitions).
pub fn render_transition_banner(rt: &mut PalRuntime, text: &str, alpha: u8) {
    let fb = &mut rt.compositor.fb;
    let w = fb.width as i32;
    let h = fb.height as i32;
    let s = if rt.ui.hd_mode { 4 } else { 1 };
    fb.fill_rect(0, 0, w, h, pal_render::framebuffer::Color::rgba(6, 10, 16, alpha));
    let bw = (w * 3 / 5).clamp(180 * s, 260 * s);
    let bh = 32 * s;
    let bx = (w - bw) / 2;
    let by = (h - bh) / 2;
    fb.fill_rect(bx, by, bw, bh, pal_render::framebuffer::Color::rgba(14, 22, 32, alpha.min(235)));
    fb.stroke_rect(bx, by, bw, bh, s.max(1), pal_render::framebuffer::Color::rgba(216, 178, 92, alpha));
    rt.compositor.fonts.draw_text_centered(
        fb,
        w / 2,
        by + 10 * s,
        text,
        (11 * s) as u16,
        pal_render::framebuffer::Color::GOLD_HI,
        Some(pal_render::framebuffer::Color::BLACK),
    );
}

/// Responsive 3-zone bottom bar layout:
/// - Left: transport icons (`|<`, `▶/⏸`, `>|`) + speed pill (`1X`)
/// - Center: elapsed/total time & segment counter
/// - Right: `CHAPTERS`, `TAKE OVER`, `EXIT`
fn layout_bottom_buttons(width: usize) -> [(ReplayHudHit, usize, usize, &'static str); 7] {
    let pad = 16usize;
    let gap = 8usize;
    let prev_x = pad;
    let prev_w = 34;
    let play_x = prev_x + prev_w + gap;
    let play_w = 40;
    let next_x = play_x + play_w + gap;
    let next_w = 34;
    let speed_x = next_x + next_w + 18;
    let speed_w = 44;

    let exit_w = 56;
    let take_w = 94;
    let chap_w = 88;
    let right_gap = 10;
    let right_total = chap_w + right_gap + take_w + right_gap + exit_w;
    let min_right_x = speed_x + speed_w + 20;
    let chap_x = width.saturating_sub(pad + right_total).max(min_right_x);
    let take_x = chap_x + chap_w + right_gap;
    let exit_x = take_x + take_w + right_gap;

    [
        (ReplayHudHit::PrevSeg, prev_x, prev_w, ""),
        (ReplayHudHit::PlayPause, play_x, play_w, ""),
        (ReplayHudHit::NextSeg, next_x, next_w, ""),
        (ReplayHudHit::Speed, speed_x, speed_w, "1X"),
        (ReplayHudHit::Chapters, chap_x, chap_w, "CHAPTERS"),
        (ReplayHudHit::TakeOver, take_x, take_w, "TAKE OVER"),
        (ReplayHudHit::Exit, exit_x, exit_w, "EXIT"),
    ]
}

fn draw_play_icon(buf: &mut [u32], width: usize, height: usize, cx: usize, cy: usize, color: u32) {
    // Crisp 10x12 right-pointing triangle centered at (cx, cy)
    for dy in -5i32..=5 {
        let len = (6 - dy.abs()).max(1) as usize * 2;
        let x0 = cx.saturating_sub(4);
        let y = (cy as i32 + dy) as usize;
        fill_rect(buf, width, height, x0, y, len.min(10), 1, color);
    }
}

fn draw_pause_icon(buf: &mut [u32], width: usize, height: usize, cx: usize, cy: usize, color: u32) {
    let top = cy.saturating_sub(5);
    fill_rect(buf, width, height, cx.saturating_sub(5), top, 3, 11, color);
    fill_rect(buf, width, height, cx + 2, top, 3, 11, color);
}

fn draw_prev_icon(buf: &mut [u32], width: usize, height: usize, cx: usize, cy: usize, color: u32) {
    // Vertical bar on left + left-pointing triangle
    fill_rect(buf, width, height, cx.saturating_sub(6), cy.saturating_sub(5), 2, 11, color);
    for dy in -4i32..=4 {
        let len = (5 - dy.abs()).max(1) as usize;
        let x0 = cx + 4 - len;
        let y = (cy as i32 + dy) as usize;
        fill_rect(buf, width, height, x0, y, len, 1, color);
    }
}

fn draw_next_icon(buf: &mut [u32], width: usize, height: usize, cx: usize, cy: usize, color: u32) {
    // Right-pointing triangle + vertical bar on right
    for dy in -4i32..=4 {
        let len = (5 - dy.abs()).max(1) as usize;
        let x0 = cx.saturating_sub(5);
        let y = (cy as i32 + dy) as usize;
        fill_rect(buf, width, height, x0, y, len, 1, color);
    }
    fill_rect(buf, width, height, cx + 4, cy.saturating_sub(5), 2, 11, color);
}

fn draw_replay_icon(buf: &mut [u32], width: usize, height: usize, cx: usize, cy: usize, color: u32) {
    // Clean square-ring restart arrow
    let l = cx.saturating_sub(5);
    let t = cy.saturating_sub(5);
    fill_rect(buf, width, height, l, t, 10, 2, color);
    fill_rect(buf, width, height, l, t + 9, 10, 2, color);
    fill_rect(buf, width, height, l, t, 2, 11, color);
    fill_rect(buf, width, height, l + 8, t + 5, 2, 6, color);
    fill_rect(buf, width, height, l + 7, t.saturating_sub(1), 3, 4, color);
}

/// Hit-test the bottom razor-thin progress bar (always active) and auto-hiding control buttons.
pub fn hit_test_replay_bar(
    px: f64,
    py: f64,
    width: usize,
    height: usize,
    controls_shown: bool,
) -> Option<ReplayHudHit> {
    if width < 200 || height < 60 || px < 0.0 || py < 0.0 {
        return None;
    }
    let x = px as usize;
    let y = py as usize;
    // Razor-thin progress bar zone at bottom (give 10px generous click target)
    if y >= height.saturating_sub(10) && y < height {
        let ratio = ((x as f64 / width.max(1) as f64).clamp(0.0, 1.0) * 1000.0).round() as u16;
        return Some(ReplayHudHit::SeekRatio(ratio));
    }
    if !controls_shown {
        return None;
    }
    let bar_top = height.saturating_sub(THIN_BAR_HEIGHT + CONTROLS_BAR_HEIGHT);
    let bar_bot = height.saturating_sub(THIN_BAR_HEIGHT);
    if y >= bar_top + 6 && y <= bar_bot.saturating_sub(4) {
        for (hit, bx, bw, _) in layout_bottom_buttons(width) {
            if x >= bx && x <= bx + bw {
                return Some(hit);
            }
        }
    }
    None
}

/// Render the bottom cinema-style replay overlay:
/// - Always draws the razor-thin (`5px`) translucent progress bar along the bottom edge.
/// - Shows a clean top-center compatibility warning pill when the replay is older than `MIN_COMPAT_REPLAY_VERSION`.
/// - Only draws the spacious 3-zone playback controls when `player.controls_visible()` is true.
pub fn render_replay_bar(buf: &mut [u32], width: usize, height: usize, player: &CoreReplayPlayer) {
    if width < 200 || height < 60 {
        return;
    }

    // 0. Compatibility warning pill at top-center for legacy replays (< MIN_COMPAT_REPLAY_VERSION)
    if player.version_mismatch {
        let rec = player.bundle.engine_version.trim().trim_start_matches(['v', 'V']);
        let rec_disp = if rec.is_empty() { "?" } else { rec };
        let warn = format!(
            "LEGACY REPLAY V{} (< V{}) - MAY DESYNC",
            rec_disp.to_ascii_uppercase(),
            MIN_COMPAT_REPLAY_VERSION
        );
        let text_w = warn.len() * 12;
        let pill_w = (text_w + 24).min(width.saturating_sub(24));
        let pill_h = 26usize;
        let pill_x = (width.saturating_sub(pill_w)) / 2;
        let pill_y = 12usize;
        fill_rect_alpha(buf, width, height, pill_x, pill_y, pill_w, pill_h, 0x002B1A08, 210);
        draw_rect_outline(buf, width, height, pill_x, pill_y, pill_w, pill_h, 1, 0x00F0A848);
        draw_text_at(buf, width, height, &warn, pill_x + 12, pill_y + 7, 2, 0x00FFD27D);
    }

    let abs_tick = player.current_abs_tick();
    let pct = if player.bundle.total_ticks > 0 {
        ((abs_tick as f32) / (player.bundle.total_ticks as f32)).clamp(0.0, 1.0)
    } else {
        1.0
    };

    // 1. Always-visible razor-thin translucent progress bar along the very bottom edge
    let bar_y = height - THIN_BAR_HEIGHT;
    fill_rect_alpha(buf, width, height, 0, bar_y, width, THIN_BAR_HEIGHT, 0x00081018, 165);
    let fill_w = ((width as f32) * pct).round() as usize;
    if fill_w > 0 {
        fill_rect_alpha(
            buf,
            width,
            height,
            0,
            bar_y + 1,
            fill_w.min(width),
            THIN_BAR_HEIGHT.saturating_sub(1),
            0x00D8B25C,
            225,
        );
    }

    // 2. Auto-hiding spacious 3-zone control strip above the progress bar
    if !player.controls_visible() {
        return;
    }
    let ctrl_y = bar_y.saturating_sub(CONTROLS_BAR_HEIGHT);
    fill_rect_alpha(buf, width, height, 0, ctrl_y, width, CONTROLS_BAR_HEIGHT, 0x000A141E, 212);
    fill_rect_alpha(buf, width, height, 0, ctrl_y, width, 1, 0x00C8A458, 160);

    let buttons = layout_bottom_buttons(width);
    let speed_lbl = format!("{}X", player.speed);
    let btn_y = ctrl_y + 8;
    let btn_h = 28;
    let btn_cy = btn_y + btn_h / 2;

    for (hit, bx, bw, default_lbl) in buttons {
        if bx + bw > width {
            continue;
        }
        let (bg, border, fg) = match hit {
            ReplayHudHit::PlayPause => (0x00263F56, 0x00F0CB72, 0x00FFF2CC),
            ReplayHudHit::Exit => (0x004A1E24, 0x00D96B6B, 0x00F6D8D8),
            ReplayHudHit::TakeOver => (0x001A3B2D, 0x0062D4A0, 0x00DCF8EA),
            _ => (0x00162636, 0x008E7A4E, 0x00EBE2CE),
        };
        fill_rect_alpha(buf, width, height, bx, btn_y, bw, btn_h, bg, 235);
        draw_rect_outline(buf, width, height, bx, btn_y, bw, btn_h, 1, border);
        let cx = bx + bw / 2;
        match hit {
            ReplayHudHit::PrevSeg => draw_prev_icon(buf, width, height, cx, btn_cy, fg),
            ReplayHudHit::PlayPause => {
                if player.finished {
                    draw_replay_icon(buf, width, height, cx, btn_cy, fg);
                } else if player.paused {
                    draw_play_icon(buf, width, height, cx, btn_cy, fg);
                } else {
                    draw_pause_icon(buf, width, height, cx, btn_cy, fg);
                }
            }
            ReplayHudHit::NextSeg => draw_next_icon(buf, width, height, cx, btn_cy, fg),
            ReplayHudHit::Speed => {
                let tw = speed_lbl.len() * 12;
                let tx = bx + bw.saturating_sub(tw) / 2;
                draw_text_at(buf, width, height, &speed_lbl, tx, btn_y + 8, 2, fg);
            }
            _ => {
                let tw = default_lbl.len() * 12;
                let tx = bx + bw.saturating_sub(tw) / 2;
                draw_text_at(buf, width, height, default_lbl, tx, btn_y + 8, 2, fg);
            }
        }
    }

    // Center-aligned time & segment status in the breathing space between Speed and Chapters
    let left_end = buttons[3].1 + buttons[3].2 + 16;
    let right_start = buttons[4].1.saturating_sub(16);
    if right_start > left_end + 110 {
        let secs = abs_tick / 50;
        let tot_secs = player.bundle.total_ticks / 50;
        let mut status = format!(
            "{:02}:{:02} / {:02}:{:02}   SEG {}/{}",
            secs / 60,
            secs % 60,
            tot_secs / 60,
            tot_secs % 60,
            player.seg_idx + 1,
            player.bundle.segments.len()
        );
        if player.bundle.tainted_by_debug || player.bundle.debug_mode {
            status.push_str("  [DEBUG]");
        }
        let text_w = status.len() * 12;
        let avail = right_start - left_end;
        let info_x = if text_w < avail {
            left_end + (avail - text_w) / 2
        } else {
            left_end
        };
        let color = if player.version_mismatch {
            0x00FFAA66
        } else {
            0x00D8E8F4
        };
        draw_text_at(buf, width, height, &status, info_x, btn_y + 8, 2, color);
    }
}

/// Encode the full framebuffer as PNG bytes (for `end_screenshot.png`).
pub fn encode_fb_png(fb: &Framebuffer) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let enc = image::codecs::png::PngEncoder::new(&mut out);
    enc.write_image(&fb.pixels, fb.width, fb.height, image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(out)
}

/// Downsample `fb` to `tw x th` (e.g. `160x100`) and encode as PNG bytes (for `thumb.png`).
pub fn encode_fb_thumb_png(fb: &Framebuffer, tw: u32, th: u32) -> Option<Vec<u8>> {
    if fb.width == 0 || fb.height == 0 || tw == 0 || th == 0 {
        return None;
    }
    let mut dst = vec![0u8; (tw * th * 4) as usize];
    for y in 0..th {
        let sy = ((y as u64 * fb.height as u64) / th as u64).min(fb.height as u64 - 1) as usize;
        for x in 0..tw {
            let sx = ((x as u64 * fb.width as u64) / tw as u64).min(fb.width as u64 - 1) as usize;
            let si = (sy * fb.width as usize + sx) * 4;
            let di = ((y * tw + x) * 4) as usize;
            if si + 3 < fb.pixels.len() {
                dst[di..di + 4].copy_from_slice(&fb.pixels[si..si + 4]);
            }
        }
    }
    let mut out = Vec::new();
    let enc = image::codecs::png::PngEncoder::new(&mut out);
    enc.write_image(&dst, tw, th, image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(out)
}

/// Decode PNG bytes into `(width, height, rgba8)` for `UiView::ReplayChapters` thumbnail display.
pub fn decode_png_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((w, h, img.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pal_core::game::replay_rec::{ReplayAnnotation, ReplaySegment, ENGINE_VERSION};

    #[test]
    fn min_compat_replay_version_check() {
        assert!(!is_replay_engine_compatible("0.3.8"));
        assert!(!is_replay_engine_compatible("v0.3.11"));
        assert!(!is_replay_engine_compatible(""));
        assert!(is_replay_engine_compatible("0.4.0"));
        assert!(is_replay_engine_compatible("v0.5.4"));
        assert!(is_replay_engine_compatible(ENGINE_VERSION));
    }

    #[test]
    fn three_zone_replay_bar_layout_and_hit_test() {
        let w = 1280usize;
        let h = 800usize;
        let btns = layout_bottom_buttons(w);
        // Right group (Chapters/TakeOver/Exit) must be right-aligned with generous center gap
        assert!(btns[4].1 > btns[3].1 + btns[3].2 + 200);
        let y = (h - THIN_BAR_HEIGHT - CONTROLS_BAR_HEIGHT / 2) as f64;
        assert_eq!(
            hit_test_replay_bar((btns[1].1 + 10) as f64, y, w, h, true),
            Some(ReplayHudHit::PlayPause)
        );
        assert_eq!(
            hit_test_replay_bar((btns[6].1 + 10) as f64, y, w, h, true),
            Some(ReplayHudHit::Exit)
        );
    }

    #[test]
    fn restore_annotation_at_binary_searches_and_respects_scene_boundaries() {
        let bundle = ReplayBundle {
            engine_version: ENGINE_VERSION.into(),
            total_ticks: 400,
            segments: vec![
                ReplaySegment {
                    index: 0,
                    start_tick: 0,
                    duration_ticks: 100,
                    scene_id: 1,
                    label: "Inn A".into(),
                    anchor: ReplayAnchor::NewGame { seed: 1 },
                    events: vec![],
                    annotations: vec![
                        ReplayAnnotation { tick: 10, text: "刚醒来".into() },
                        ReplayAnnotation { tick: 60, text: "下楼看看".into() },
                    ],
                    thumb_png: None,
                },
                ReplaySegment {
                    index: 1,
                    start_tick: 100,
                    duration_ticks: 100,
                    scene_id: 1, // same scene as seg 0
                    label: "Inn B".into(),
                    anchor: ReplayAnchor::NewGame { seed: 1 },
                    events: vec![],
                    annotations: vec![
                        ReplayAnnotation { tick: 30, text: String::new() },
                        ReplayAnnotation { tick: 70, text: "准备出门".into() },
                    ],
                    thumb_png: None,
                },
                ReplaySegment {
                    index: 2,
                    start_tick: 200,
                    duration_ticks: 200,
                    scene_id: 2, // new scene
                    label: "Town".into(),
                    anchor: ReplayAnchor::NewGame { seed: 1 },
                    events: vec![],
                    annotations: vec![],
                    thumb_png: None,
                },
            ],
            end_screenshot_png: None,
            state_json: None,
            note: None,
            tainted_by_debug: false,
            debug_mode: false,
        };
        let mut player = CoreReplayPlayer::from_bundle("test.repal".into(), bundle).unwrap();
        let mut rt = PalRuntime::new(true);

        // Before first annotation -> None
        player.restore_annotation_at(0, 5, &mut rt);
        assert_eq!(rt.ui.ai_thought, None);
        assert_eq!(player.annotation_idx, 0);

        // Between tick 10 and tick 60 -> "刚醒来"
        player.restore_annotation_at(0, 35, &mut rt);
        assert_eq!(rt.ui.ai_thought.as_deref(), Some("刚醒来"));
        assert_eq!(player.annotation_idx, 1);

        // At/after tick 60 -> "下楼看看"
        player.restore_annotation_at(0, 60, &mut rt);
        assert_eq!(rt.ui.ai_thought.as_deref(), Some("下楼看看"));
        assert_eq!(player.annotation_idx, 2);

        // Segment 1 tick 0 (same scene 1) inherits seg 0's last thought
        player.restore_annotation_at(1, 0, &mut rt);
        assert_eq!(rt.ui.ai_thought.as_deref(), Some("下楼看看"));
        assert_eq!(player.annotation_idx, 0);

        // Segment 1 tick 40 (after explicit clear at tick 30) -> None
        player.restore_annotation_at(1, 40, &mut rt);
        assert_eq!(rt.ui.ai_thought, None);
        assert_eq!(player.annotation_idx, 1);

        // Segment 2 tick 0 (different scene 2) does NOT leak seg 1's thought
        player.restore_annotation_at(2, 0, &mut rt);
        assert_eq!(rt.ui.ai_thought, None);
        assert_eq!(player.annotation_idx, 0);
    }
}
