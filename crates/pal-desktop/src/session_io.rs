//! Replay archive management, screenshots, save export/import, and bug-report submission
//! helpers for `DesktopApp`.

use crate::os_bridge::{
    open_folder_in_os, run_os_file_picker, sanitize_local_path_for_issue, spawn_gh_issue_create,
};
use crate::replay_ui::{encode_fb_png, CoreReplayPlayer, ReplayHudHit};
use crate::saves::UserConfig;
use crate::DesktopApp;
use pal_runtime::repal_archive::{read_repal_file, write_repal_archive, write_repal_to_dir};
use pal_ui::UiView;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

impl DesktopApp {
    pub(crate) fn persist_user_config(&self) {
        if self.untrusted_cmd_active {
            return;
        }
        let Some(rt) = self.rt.as_ref() else {
            return;
        };
        UserConfig {
            hd_mode: rt.ui.hd_mode,
            lang: rt.ui.lang,
            music_on: rt.ui.music_on,
            sfx_on: rt.ui.sfx_on,
            fullscreen: self.fullscreen,
        }
        .save(&self.cfg.config_path);
    }

    pub(crate) fn load_or_create_lan_token(&self) -> String {
        load_or_create_lan_token_at(&self.cfg.user_dir.join("agent_lan_token.txt"))
    }

    pub(crate) fn rotate_lan_token(&self) -> String {
        rotate_lan_token_at(&self.cfg.user_dir.join("agent_lan_token.txt"))
    }

    pub(crate) fn save_screenshot(&mut self) -> Option<PathBuf> {
        self.save_screenshot_to(None)
    }

    pub(crate) fn save_screenshot_to(&mut self, custom_path: Option<&str>) -> Option<PathBuf> {
        let rt = self.rt.as_mut()?;
        self.shot_counter += 1;
        let path = match custom_path.filter(|s| !s.trim().is_empty()) {
            Some(p) => {
                let pb = PathBuf::from(p.trim());
                if let Some(parent) = pb.parent() {
                    if !parent.as_os_str().is_empty() {
                        let _ = fs::create_dir_all(parent);
                    }
                }
                pb
            }
            None => {
                let out_dir = self.cfg.user_dir.join("screenshots");
                let _ = fs::create_dir_all(&out_dir);
                out_dir.join(format!("repal_shot_{:03}.png", self.shot_counter))
            }
        };
        let fb = &rt.compositor.fb;
        if let Some(img) = image::RgbaImage::from_raw(fb.width, fb.height, fb.pixels.clone()) {
            if img.save(&path).is_ok() {
                eprintln!("[repal] Screenshot saved: {}", path.display());
                rt.show_toast(format!("截圖已儲存：{}", path.display()), 120);
                return Some(path);
            }
        }
        None
    }

    pub(crate) fn export_save_to_folder(&mut self) {
        let Some(rt) = self.rt.as_mut() else {
            return;
        };
        self.shot_counter += 1;
        let _ = fs::create_dir_all(&self.cfg.saves_dir);
        let path = self
            .cfg
            .saves_dir
            .join(format!("repal_export_{:03}.json", self.shot_counter));
        if let Some(text) = rt.save_json_str("exported", "desktop") {
            if fs::write(&path, text).is_ok() {
                open_folder_in_os(&self.cfg.saves_dir);
                let msg = match rt.ui.lang {
                    pal_core::Language::En => format!("Exported: {}", path.display()),
                    pal_core::Language::ZhHans => format!("已导出存档：{}", path.display()),
                    pal_core::Language::ZhHant => format!("已匯出存檔：{}", path.display()),
                };
                rt.show_toast(msg, 160);
            }
        }
    }

    pub(crate) fn open_saves_for_import(&mut self) {
        open_folder_in_os(&self.cfg.saves_dir);
        if let Some(rt) = self.rt.as_mut() {
            let msg = match rt.ui.lang {
                pal_core::Language::En => {
                    format!("Place slot_N.json in {}", self.cfg.saves_dir.display())
                }
                pal_core::Language::ZhHans => {
                    format!("请将 slot_N.json 放入 {}", self.cfg.saves_dir.display())
                }
                pal_core::Language::ZhHant => {
                    format!("請將 slot_N.json 放入 {}", self.cfg.saves_dir.display())
                }
            };
            rt.show_toast(msg, 180);
        }
    }

    pub(crate) fn export_bug_report(&mut self) {
        let _ = self.export_bug_report_with_note(None);
    }

    pub(crate) fn export_bug_report_with_note(&mut self, note: Option<&str>) -> Option<PathBuf> {
        let rt = self.rt.as_mut()?;
        self.shot_counter += 1;
        let out_dir = self.cfg.user_dir.join("bug_reports");
        let _ = fs::create_dir_all(&out_dir);
        let (scene_id, tick, state_json) = rt
            .game
            .as_ref()
            .map(|g| (g.world.scene_id, g.tick_count, g.canonical_state()))
            .unwrap_or((0, 0, serde_json::Value::Null));
        let stem = format!("repal_bug_sc{}_t{}_{:03}", scene_id, tick, self.shot_counter);
        let repal_path = out_dir.join(format!("{stem}.repal"));
        let note_str = note.unwrap_or("repal");

        // Ensure the framebuffer is freshly rendered (including on Title/home screen) before capturing PNG (#167).
        rt.render_frame();
        let end_png = encode_fb_png(&rt.compositor.fb);
        let png_path = out_dir.join(format!("{stem}.png"));
        let saved_png = end_png
            .as_deref()
            .and_then(|bytes| fs::write(&png_path, bytes).ok().map(|_| png_path.clone()));
        let mut bundle = rt
            .game
            .as_ref()
            .map(|g| g.export_replay_bundle())
            .unwrap_or_else(|| pal_core::game::replay_rec::ReplayBundle {
                engine_version: pal_core::game::replay_rec::ENGINE_VERSION.to_string(),
                total_ticks: 0,
                segments: Vec::new(),
                end_screenshot_png: None,
                state_json: None,
                note: None,
                tainted_by_debug: false,
                debug_mode: false,
            });
        bundle.end_screenshot_png = end_png;
        bundle.state_json = Some(state_json.clone());
        bundle.note = Some(note_str.to_string());
        let tainted_by_debug = bundle.tainted_by_debug || bundle.debug_mode;
        let _ = write_repal_archive(&repal_path, &bundle);
        eprintln!("[repal] Bug report exported: {}", repal_path.display());
        rt.show_toast(
            match rt.ui.lang {
                pal_core::Language::En => {
                    "Bug report (.repal) saved; submitting via gh...".to_string()
                }
                pal_core::Language::ZhHans => {
                    "诊断包 (.repal) 已保存，正在通过 gh 自动提交 Issue...".to_string()
                }
                pal_core::Language::ZhHant => {
                    "診斷包 (.repal) 已儲存，正在透過 gh 自動提交 Issue...".to_string()
                }
            },
            140,
        );

        let title = match note.filter(|s| !s.trim().is_empty()) {
            Some(desc) => format!("Bug Report: {} (Scene #{scene_id})", desc.trim()),
            None => format!("Desktop Bug Report: Scene #{scene_id} (tick {tick})"),
        };
        let state_pretty = serde_json::to_string_pretty(&state_json).unwrap_or_default();
        let repal_display = sanitize_local_path_for_issue(&repal_path);
        let taint_line = if tainted_by_debug {
            "- **Tainted by Debug**: `true` (debug/cheat commands were used in this session)\n"
        } else {
            "- **Tainted by Debug**: `false`\n"
        };
        let body = format!(
            "Auto-submitted from `repal`.\n\n\
             - **Note**: {note_str}\n\
             - **OS**: `{}` (`{}`)\n\
             - **Scene**: `#{scene_id}` (tick `{tick}`)\n\
             {taint_line}\
             - **Replay Bundle (`.repal`, kept locally)**: `{repal_display}`\n\n\
             <details>\n<summary>Canonical State Snapshot</summary>\n\n```json\n{}\n```\n</details>",
            std::env::consts::OS,
            std::env::consts::ARCH,
            state_pretty,
        );
        self.bug_report_rx = Some(spawn_gh_issue_create(title, body, saved_png));
        Some(repal_path)
    }

    pub(crate) fn open_native_replay_picker(&mut self) {
        if self.file_picker_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.file_picker_rx = Some(rx);
        std::thread::spawn(move || {
            let picked = run_os_file_picker();
            let _ = tx.send(picked);
        });
    }

    pub(crate) fn poll_file_picker(&mut self) {
        let picked = self
            .file_picker_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(opt_path) = picked {
            self.file_picker_rx = None;
            if let Some(path) = opt_path {
                self.load_file_or_replay(path);
            }
        }
    }

    pub(crate) fn replays_dir(&self) -> PathBuf {
        versioned_replays_dir(&self.cfg.user_dir, crate::updater::current_version())
    }

    pub(crate) fn show_transition_banner(&mut self, text: impl Into<String>, ms: u64) {
        self.transition_banner = Some((text.into(), Instant::now(), Duration::from_millis(ms)));
    }

    pub(crate) fn transition_active(&self) -> bool {
        self.transition_banner
            .as_ref()
            .map(|(_, started, dur)| started.elapsed() < *dur)
            .unwrap_or(false)
    }

    pub(crate) fn replay_menu_open(&self) -> bool {
        self.rt
            .as_ref()
            .map(|rt| {
                matches!(
                    rt.ui.view,
                    UiView::RootMenu
                        | UiView::ReplayChapters
                        | UiView::ReplayFinished
                        | UiView::SystemSubmenu
                        | UiView::RemakeSubmenu
                        | UiView::SaveLoadSlots(_)
                )
            })
            .unwrap_or(false)
    }

    pub(crate) fn flush_archived_replays(&mut self) {
        let archived = self.rt.as_mut().and_then(|rt| {
            let end_png = encode_fb_png(&rt.compositor.fb);
            rt.game.as_mut().and_then(|g| {
                g.take_archived_replay().map(|mut bundle| {
                    if bundle.end_screenshot_png.is_none() {
                        bundle.end_screenshot_png = end_png;
                    }
                    let sid = bundle.last_scene_id();
                    (bundle, sid)
                })
            })
        });
        if let Some((bundle, sid)) = archived {
            if let Some(p) = write_repal_to_dir(&self.replays_dir(), &bundle, "auto", sid, true) {
                eprintln!("[repal] Auto-archived replay: {}", p.display());
            }
        }
    }

    pub(crate) fn archive_active_replay_on_exit(&mut self) {
        self.flush_archived_replays();
        if self.core_replay.is_some() {
            return;
        }
        let active = self.rt.as_ref().and_then(|rt| {
            let g = rt.game.as_ref()?;
            if g.recorder.is_active()
                && g.recorder.total_events >= pal_core::game::replay_rec::MIN_EVENTS_FOR_ARCHIVE
            {
                let mut bundle = g.export_replay_bundle();
                bundle.end_screenshot_png = encode_fb_png(&rt.compositor.fb);
                let sid = g.world.scene_id;
                Some((bundle, sid))
            } else {
                None
            }
        });
        if let Some((bundle, sid)) = active {
            if let Some(p) = write_repal_to_dir(&self.replays_dir(), &bundle, "auto", sid, true) {
                eprintln!("[repal] Auto-archived session replay on exit: {}", p.display());
            }
        }
    }

    pub(crate) fn restart_recording(&mut self) {
        let prev = self.rt.as_mut().and_then(|rt| {
            let end_png = encode_fb_png(&rt.compositor.fb);
            rt.game.as_mut().and_then(|g| {
                g.restart_recording_here().map(|mut bundle| {
                    bundle.end_screenshot_png = end_png;
                    let sid = bundle.last_scene_id();
                    (bundle, sid)
                })
            })
        });
        if let Some((bundle, sid)) = prev {
            let _ = write_repal_to_dir(&self.replays_dir(), &bundle, "auto", sid, true);
        }
        if let Some(rt) = self.rt.as_mut() {
            let msg = match rt.ui.lang {
                pal_core::Language::En => "Started new recording from here (previous archived)",
                pal_core::Language::ZhHans => "已从此刻重新开始录制（旧录像已自动归档）",
                pal_core::Language::ZhHant => "已從此刻重新開始錄製（舊錄像已自動歸檔）",
            };
            rt.show_toast(msg, 150);
        }
    }

    pub(crate) fn export_current_replay(&mut self) {
        let Some((bundle, sid)) = self.rt.as_ref().and_then(|rt| {
            let g = rt.game.as_ref()?;
            let mut bundle = g.export_replay_bundle();
            bundle.end_screenshot_png = encode_fb_png(&rt.compositor.fb);
            Some((bundle, g.world.scene_id))
        }) else {
            return;
        };
        let dir = self.replays_dir();
        if let Some(path) = write_repal_to_dir(&dir, &bundle, "manual", sid, false) {
            open_folder_in_os(&dir);
            if let Some(rt) = self.rt.as_mut() {
                let msg = match rt.ui.lang {
                    pal_core::Language::En => format!("Exported replay: {}", path.display()),
                    pal_core::Language::ZhHans => format!("已导出录像：{}", path.display()),
                    pal_core::Language::ZhHant => format!("已匯出錄像：{}", path.display()),
                };
                rt.show_toast(msg, 160);
            }
        }
    }

    pub(crate) fn stop_core_replay(&mut self) {
        if let Some(player) = self.core_replay.take() {
            let was_tainted = player.bundle.debug_mode || player.bundle.tainted_by_debug;
            if let Some(rt) = self.rt.as_mut() {
                rt.ui.replay_mode = false;
                rt.ui.replay_chapters.clear();
                rt.ui.set_view(UiView::None);
                rt.ui.debug_mode = was_tainted;
                rt.compositor.mark_ui_dirty();
                if let Some(g) = rt.game.as_mut() {
                    g.take_over_from_replay(was_tainted);
                }
                let msg = match rt.ui.lang {
                    pal_core::Language::En => "Took over control from replay",
                    pal_core::Language::ZhHans => "已停止回放，你可以从此处继续操作",
                    pal_core::Language::ZhHant => "已停止回放，你可以從此處繼續操作",
                };
                rt.show_toast(msg, 140);
            }
        }
    }

    pub(crate) fn exit_core_replay_to_title(&mut self) {
        self.core_replay = None;
        self.rng_play = None;
        if let Some(audio) = self.audio.as_mut() {
            audio.play_music_vfs(4, true, Some(&mut self.vfs));
        }
        if let Some(rt) = self.rt.as_mut() {
            rt.ui.replay_mode = false;
            rt.ui.replay_chapters.clear();
            if let Some(g) = rt.game.as_mut() {
                g.recorder.enabled = true;
            }
            rt.set_in_title(true);
            self.saves.refresh_slots(&self.cfg.assets_root, rt);
        }
    }

    pub(crate) fn handle_replay_hud_hit(&mut self, hit: ReplayHudHit) {
        match hit {
            ReplayHudHit::TakeOver => self.stop_core_replay(),
            ReplayHudHit::Exit => self.exit_core_replay_to_title(),
            ReplayHudHit::Chapters => {
                if let Some(p) = self.core_replay.as_mut() {
                    p.touch_ui();
                    let cur = p.seg_idx;
                    if let Some(rt) = self.rt.as_mut() {
                        rt.ui.replay_chapter_cursor = cur;
                        rt.ui.set_view(UiView::ReplayChapters);
                        rt.compositor.mark_ui_dirty();
                    }
                }
            }
            ReplayHudHit::PlayPause => {
                if let Some(p) = self.core_replay.as_mut() {
                    p.touch_ui();
                    if p.finished {
                        if let Some(rt) = self.rt.as_mut() {
                            p.start(rt);
                        }
                        p.paused = false;
                        self.service_host_cmds();
                    } else {
                        p.paused = !p.paused;
                    }
                }
            }
            ReplayHudHit::Speed => {
                if let Some(p) = self.core_replay.as_mut() {
                    p.cycle_speed();
                }
            }
            ReplayHudHit::PrevSeg => {
                let jumped =
                    if let (Some(p), Some(rt)) = (self.core_replay.as_mut(), self.rt.as_mut()) {
                        p.prev_segment(rt)
                    } else {
                        None
                    };
                if let Some(lbl) = jumped {
                    self.service_host_cmds();
                    self.show_transition_banner(format!("跳转章节 {lbl}"), 1500);
                }
            }
            ReplayHudHit::NextSeg => {
                let jumped =
                    if let (Some(p), Some(rt)) = (self.core_replay.as_mut(), self.rt.as_mut()) {
                        p.next_segment(rt)
                    } else {
                        None
                    };
                if let Some(lbl) = jumped {
                    self.service_host_cmds();
                    self.show_transition_banner(format!("跳转章节 {lbl}"), 1500);
                }
            }
            ReplayHudHit::SeekRatio(ratio) => {
                let jumped =
                    if let (Some(p), Some(rt)) = (self.core_replay.as_mut(), self.rt.as_mut()) {
                        p.seek_by_ratio(ratio, rt)
                    } else {
                        None
                    };
                if let Some(lbl) = jumped {
                    self.service_host_cmds();
                    self.show_transition_banner(format!("跳转章节 {lbl}"), 1500);
                }
            }
        }
    }

    pub(crate) fn load_file_or_replay(&mut self, path: PathBuf) {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string();

        if ext == "repal" || ext == "json" || ext == "gz" {
            if let Ok(bundle) = read_repal_file(&path) {
                if let Ok(mut player) = CoreReplayPlayer::from_bundle(name.clone(), bundle) {
                    if !player.bundle.segments.is_empty() {
                        self.flush_archived_replays();
                        let chapters = player.build_chapter_summaries();
                        let ok = self
                            .rt
                            .as_mut()
                            .map(|rt| {
                                rt.ui.replay_mode = true;
                                rt.ui.replay_chapters = chapters;
                                rt.ui.replay_chapter_cursor = 0;
                                rt.ui.set_view(UiView::None);
                                rt.compositor.mark_ui_dirty();
                                player.start(rt)
                            })
                            .unwrap_or(false);
                        if ok {
                            self.rng_play = None;
                            self.replay_queue.clear();
                            self.cancel_text_task();
                            let ver_warn = player.version_mismatch;
                            let taint_warn = player.bundle.tainted_by_debug || player.bundle.debug_mode;
                            let n_seg = player.bundle.segments.len();
                            self.core_replay = Some(player);
                            self.service_host_cmds();
                            if let Some(rt) = self.rt.as_mut() {
                                let mut tags = Vec::new();
                                if ver_warn {
                                    tags.push("旧版录像(<v0.4.0)可能不同步");
                                }
                                if taint_warn {
                                    tags.push("含调试指令");
                                }
                                let msg = if tags.is_empty() {
                                    format!("开始回放 {name}（{n_seg} 章节 | Esc菜单 Tab选章节）")
                                } else {
                                    format!("回放 {name}（{n_seg} 章节 | {}）", tags.join(" / "))
                                };
                                rt.show_toast(msg, 180);
                            }
                            return;
                        }
                    }
                }
            }
        }

        if ext == "json" || ext == "rpg" {
            let ok = {
                let Some(rt) = self.rt.as_mut() else { return };
                if ext == "rpg" {
                    fs::read(&path)
                        .ok()
                        .map(|b| rt.load_dos_rpg(&b))
                        .unwrap_or(false)
                } else {
                    fs::read_to_string(&path)
                        .ok()
                        .map(|t| rt.load_save_json_str(&t))
                        .unwrap_or(false)
                }
            };
            if ok {
                // Exit any active replay playback and re-enable recording so the loaded save
                // is playable immediately instead of being overwritten by stale replay ticks (#197).
                self.core_replay = None;
                self.rng_play = None;
                self.replay_queue.clear();
                self.cancel_text_task();
                if let Some(rt) = self.rt.as_mut() {
                    rt.ui.replay_mode = false;
                    rt.ui.replay_chapters.clear();
                    rt.ui.set_view(UiView::None);
                    rt.compositor.mark_ui_dirty();
                    if let Some(g) = rt.game.as_mut() {
                        g.recorder.enabled = true;
                        let _ = g.restart_recording_here();
                    }
                }
                self.service_host_cmds();
                if let Some(rt) = self.rt.as_mut() {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("save");
                    rt.show_toast(format!("已載入存檔：{name}"), 140);
                }
            } else if let Some(rt) = self.rt.as_mut() {
                rt.show_toast("存檔載入失敗", 140);
            }
            return;
        }

        let Ok(raw) = fs::read_to_string(&path) else {
            if let Some(rt) = self.rt.as_mut() {
                rt.show_toast("無法讀取回放檔案", 140);
            }
            return;
        };
        let (cmds, skipped) = filter_safe_cmd_replay(&raw);
        if skipped > 0 {
            eprintln!(
                "[repal] .cmd replay filtered {skipped} disallowed file/debug command(s) from {}",
                path.display()
            );
        }
        let count = cmds.len();
        self.replay_queue = cmds;
        self.cancel_text_task();
        if let Some(rt) = self.rt.as_mut() {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("replay");
            let msg = match rt.ui.lang {
                pal_core::Language::En => format!("Replaying {name} ({count} cmds, F8 to stop)"),
                pal_core::Language::ZhHans => format!("开始回放 {name}（共 {count} 步，按 F8 停止）"),
                pal_core::Language::ZhHant => format!("開始回放 {name}（共 {count} 步，按 F8 停止）"),
            };
            rt.show_toast(msg, 160);
        }
    }

    pub(crate) fn poll_bug_report(&mut self) {
        let result = self
            .bug_report_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(res) = result {
            self.bug_report_rx = None;
            if let Some(rt) = self.rt.as_mut() {
                match res {
                    Ok(url) => {
                        let msg = match rt.ui.lang {
                            pal_core::Language::En => format!("Issue submitted: {url}"),
                            pal_core::Language::ZhHans => format!("已自动提交 Issue：{url}"),
                            pal_core::Language::ZhHant => format!("已自動提交 Issue：{url}"),
                        };
                        rt.show_toast(msg, 220);
                    }
                    Err(_) => {
                        let msg = match rt.ui.lang {
                            pal_core::Language::En => "Saved locally (gh CLI not logged in)",
                            pal_core::Language::ZhHans => "已保存本地诊断包 (未检测到 gh 登录)",
                            pal_core::Language::ZhHant => "已儲存本機診斷包 (未偵測到 gh 登入)",
                        };
                        rt.show_toast(msg, 160);
                    }
                }
            }
        }
    }
}

/// Bucket desktop user replays under `<user_dir>/replays/v<version>` so recordings
/// from different engine versions stay cleanly separated.
pub(crate) fn versioned_replays_dir(user_dir: &Path, version: &str) -> PathBuf {
    let trimmed = version.trim().trim_start_matches('v');
    let folder = if trimmed.is_empty() {
        "v0.0.0".to_string()
    } else {
        format!("v{trimmed}")
    };
    user_dir.join("replays").join(folder)
}

/// Strictly parse and allowlist gameplay-only commands from dropped/picked `.cmd` replay files
/// so untrusted `.cmd` files cannot write arbitrary paths, spawn `gh`, force-open arbitrary views,
/// or invoke debug cheats (#198, #231).
pub(crate) fn filter_safe_cmd_replay(raw: &str) -> (VecDeque<String>, usize) {
    use pal_headless::text_mode::{parse_command, Command};
    let mut cmds = VecDeque::new();
    let mut skipped = 0usize;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        match parse_command(trimmed) {
            Ok(
                Command::Look
                | Command::Map
                | Command::Walk { .. }
                | Command::GotoEvent(_)
                | Command::GotoCoords(_, _)
                | Command::Talk(_)
                | Command::Search
                | Command::Wait(_)
                | Command::Next
                | Command::Continue
                | Command::Yes
                | Command::No
                | Command::Close
                | Command::OpenMenu
                | Command::Key(_)
                | Command::UiShortcut(_)
                | Command::Click { .. }
                | Command::BattleAttack { .. }
                | Command::BattleMagic { .. }
                | Command::BattleItem { .. }
                | Command::BattleCoop { .. }
                | Command::BattleDefend
                | Command::BattleFlee
                | Command::BattleAuto
                | Command::BattleCancelAuto
                | Command::Status
                | Command::Bag
                | Command::UseItem { .. }
                | Command::EquipItem { .. }
                | Command::Buy { .. }
                | Command::Sell { .. }
                | Command::SetHd(_)
                | Command::SetLang(_)
                | Command::Think(_),
            ) => cmds.push_back(trimmed.to_string()),
            _ => skipped += 1,
        }
    }
    (cmds, skipped)
}

/// Allowlist of pure gameplay and session-only presentation `UiAction`s permitted while an
/// untrusted file-sourced `.cmd` replay or remote TCP client is driving UI navigation (`key`/`ui`/`click`).
/// All host-privileged / destructive / disk-touching actions (including `LoadSlot` and any future
/// host actions) are denied by default (#231, #236, #247).
pub(crate) fn is_ui_action_allowed_in_untrusted_replay(action: &pal_ui::UiAction) -> bool {
    use pal_ui::UiAction;
    matches!(
        action,
        UiAction::None
            | UiAction::DialogAdvance
            | UiAction::DialogPickChoice(_)
            | UiAction::UseItem { .. }
            | UiAction::EquipItem { .. }
            | UiAction::CastMagic { .. }
            | UiAction::BuyItem { .. }
            | UiAction::SellItem { .. }
            | UiAction::BattleAttack { .. }
            | UiAction::BattleCastMagic { .. }
            | UiAction::BattleCoop { .. }
            | UiAction::BattleUseItem { .. }
            | UiAction::BattleUseItemTarget { .. }
            | UiAction::BattleDefend
            | UiAction::BattleFlee
            | UiAction::BattleAuto
            | UiAction::BattleCancelAuto
            | UiAction::ToggleHdMode
            | UiAction::ToggleMusic
            | UiAction::ToggleSfx
            | UiAction::ToggleLanguage
            | UiAction::ToggleFullscreen
    )
}

pub(crate) fn is_ui_action_blocked_in_untrusted_replay(action: &pal_ui::UiAction) -> bool {
    !is_ui_action_allowed_in_untrusted_replay(action)
}

fn is_valid_lan_token(s: &str) -> bool {
    s.len() >= 16
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub(crate) fn write_lan_token_file(path: &Path, tok: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
        {
            let _ = f.write_all(tok.as_bytes());
            let _ = f.write_all(b"\n");
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
            return;
        }
    }
    let _ = fs::write(path, format!("{tok}\n"));
}

pub(crate) fn load_or_create_lan_token_at(path: &Path) -> String {
    if let Ok(raw) = fs::read_to_string(path) {
        let trimmed = raw.trim();
        if is_valid_lan_token(trimmed) {
            return trimmed.to_string();
        }
    }
    rotate_lan_token_at(path)
}

pub(crate) fn rotate_lan_token_at(path: &Path) -> String {
    let tok = pal_headless::text_mode::transport::generate_session_token();
    write_lan_token_file(path, &tok);
    tok
}

/// Zero-dependency cross-platform system clipboard writer (`pbcopy` / `clip.exe` / `wl-copy` / `xclip` / `xsel`).
pub(crate) fn copy_text_to_clipboard(text: &str) -> bool {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    fn run_clip(bin: &str, args: &[&str], text: &str) -> bool {
        let Ok(mut child) = Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        if let Some(mut stdin) = child.stdin.take() {
            if stdin.write_all(text.as_bytes()).is_err() {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
        child.wait().map(|s| s.success()).unwrap_or(false)
    }

    #[cfg(target_os = "macos")]
    {
        run_clip("pbcopy", &[], text)
    }
    #[cfg(target_os = "windows")]
    {
        run_clip("clip", &[], text)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let on_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        if on_wayland && run_clip("wl-copy", &[], text) {
            return true;
        }
        if run_clip("xclip", &["-selection", "clipboard"], text) {
            return true;
        }
        if run_clip("xsel", &["--clipboard", "--input"], text) {
            return true;
        }
        !on_wayland && run_clip("wl-copy", &[], text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_replay_allowlist_blocks_file_io_view_and_debug_commands() {
        let script = "\
# comment
sw 3
think 准备找NPC对话
talk 10
next
view pawnshop
new
save /tmp/pwn.json
shot /tmp/pwn.png
dump-state /tmp/state.json
bugreport leak
load /etc/passwd
core-replay foo.repal
/god
/tp 1
/cash 99999
quit
";
        let (cmds, skipped) = filter_safe_cmd_replay(script);
        let list: Vec<String> = cmds.into_iter().collect();
        assert_eq!(list, vec!["sw 3", "think 准备找NPC对话", "talk 10", "next"]);
        assert_eq!(skipped, 12);
    }

    #[test]
    fn untrusted_cmd_replay_blocks_privileged_ui_actions() {
        use pal_ui::UiAction;
        assert!(is_ui_action_blocked_in_untrusted_replay(&UiAction::ToggleAgentMode));
        assert!(is_ui_action_blocked_in_untrusted_replay(&UiAction::CopyAgentToken));
        assert!(is_ui_action_blocked_in_untrusted_replay(&UiAction::RefreshAgentToken));
        assert!(is_ui_action_blocked_in_untrusted_replay(&UiAction::SaveSlot(0)));
        assert!(is_ui_action_blocked_in_untrusted_replay(&UiAction::CheckForUpdate));
        assert!(is_ui_action_blocked_in_untrusted_replay(&UiAction::ExportSave));
        assert!(is_ui_action_blocked_in_untrusted_replay(&UiAction::CopyReplayReport));
        assert!(is_ui_action_blocked_in_untrusted_replay(&UiAction::NewGame));
        assert!(is_ui_action_blocked_in_untrusted_replay(&UiAction::ReturnToTitle));
        // Gameplay UI actions stay allowed during replay.
        assert!(!is_ui_action_blocked_in_untrusted_replay(&UiAction::UseItem {
            item_id: 49,
            role_idx: 0,
        }));
        assert!(!is_ui_action_blocked_in_untrusted_replay(&UiAction::BattleDefend));
    }

    #[test]
    fn versioned_replays_dir_buckets_by_v_prefixed_version() {
        let base = Path::new("/tmp/repal_user");
        assert_eq!(
            versioned_replays_dir(base, "0.3.11"),
            PathBuf::from("/tmp/repal_user/replays/v0.3.11")
        );
        assert_eq!(
            versioned_replays_dir(base, "v0.3.11"),
            PathBuf::from("/tmp/repal_user/replays/v0.3.11")
        );
    }

    #[test]
    fn lan_token_persists_until_explicit_rotate() {
        let dir = std::env::temp_dir().join(format!(
            "repal_lan_tok_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("agent_lan_token.txt");

        let t1 = load_or_create_lan_token_at(&path);
        assert_eq!(t1.len(), 32);
        // Second load must reuse the exact same persisted token.
        let t2 = load_or_create_lan_token_at(&path);
        assert_eq!(t1, t2);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // Explicit rotate produces a fresh token and persists it.
        let t3 = rotate_lan_token_at(&path);
        assert_eq!(t3.len(), 32);
        assert_ne!(t3, t1);
        assert_eq!(load_or_create_lan_token_at(&path), t3);

        let _ = fs::remove_dir_all(&dir);
    }
}
