//! Desktop UI action dispatch, host command servicing, self-updater polling, and human/agent takeover.

use crate::*;

impl DesktopApp {
    pub(crate) fn handle_ui_action(&mut self, action: UiAction) {
        if self.rt.is_none() {
            return;
        }
        if self.untrusted_cmd_active
            && crate::session_io::is_ui_action_blocked_in_untrusted_replay(&action)
        {
            eprintln!("[repal] Blocked host-privileged UI action {action:?} during untrusted .cmd replay or remote socket command (#231, #236)");
            return;
        }
        match action {
            UiAction::NewGame => {
                self.core_replay = None;
                self.rng_play = None;
                if let Some(audio) = self.audio.as_mut() {
                    audio.stop_music();
                }
                let seed = self.last_frame.elapsed().subsec_nanos() ^ 0x1995_0710;
                if let Some(rt) = self.rt.as_mut() {
                    rt.ui.replay_mode = false;
                    rt.ui.replay_chapters.clear();
                    if let Some(g) = rt.game.as_mut() {
                        g.recorder.enabled = true;
                    }
                    rt.new_game(seed);
                }
                self.service_host_cmds();
            }
            UiAction::LoadSlot(slot) => {
                let was_in_title = self.in_title();
                let loaded = {
                    let rt = self.rt.as_mut().unwrap();
                    if let Some(g) = rt.game.as_mut() {
                        g.recorder.enabled = true;
                    }
                    self.saves.load_slot_vfs(slot, &mut self.vfs, rt)
                };
                if loaded {
                    self.core_replay = None;
                    self.rng_play = None;
                    if let Some(rt) = self.rt.as_mut() {
                        rt.ui.replay_mode = false;
                        rt.ui.replay_chapters.clear();
                    }
                    self.service_host_cmds();
                    let msg = if let Some(rt) = self.rt.as_ref() {
                        match rt.ui.lang {
                            pal_core::Language::En => format!("Loaded Slot {}", slot + 1),
                            pal_core::Language::ZhHans => format!("读取存档 · 进度 {}", slot + 1),
                            pal_core::Language::ZhHant => format!("讀取存檔 · 進度 {}", slot + 1),
                        }
                    } else {
                        format!("读取存档 · 进度 {}", slot + 1)
                    };
                    self.show_transition_banner(msg, 1500);
                } else if let Some(rt) = self.rt.as_mut() {
                    if was_in_title {
                        rt.set_in_title(true);
                    }
                    let n = slot + 1;
                    let msg = tl(
                        rt.ui.lang,
                        &format!("进度 {n} 尚无存档"),
                        &format!("進度 {n} 尚無存檔"),
                        &format!("Slot {n} is empty"),
                    );
                    rt.show_toast(msg, 110);
                }
            }
            UiAction::SaveSlot(slot) => {
                let res = self.saves.save_slot(slot, self.rt.as_ref().unwrap());
                match res {
                    Ok(()) => {
                        let rt = self.rt.as_mut().unwrap();
                        if let Some(g) = rt.game.as_mut() {
                            g.split_segment_on_manual_save();
                        }
                        self.saves.refresh_slots_vfs(&self.vfs, rt);
                        let n = slot + 1;
                        let msg = tl(
                            rt.ui.lang,
                            &format!("已保存至进度 {n}"),
                            &format!("已儲存至進度 {n}"),
                            &format!("Saved to slot {n}"),
                        );
                        rt.show_toast(msg, 110);
                    }
                    Err(e) => {
                        eprintln!("[repal] save failed: {e}");
                        if let Some(rt) = self.rt.as_mut() {
                            rt.show_toast(tl(rt.ui.lang, "保存失败", "儲存失敗", "Save failed"), 110);
                        }
                    }
                }
            }
            UiAction::ToggleHdMode => {
                let next = {
                    let rt = self.rt.as_mut().unwrap();
                    let next = rt.ui.hd_mode;
                    rt.set_hd_mode(next);
                    next
                };
                if let Some(audio) = self.audio.as_mut() {
                    audio.set_hd_mode(next);
                }
                self.persist_user_config();
                if let Some(rt) = self.rt.as_mut() {
                    let msg = if next {
                        tl(rt.ui.lang, "画质切换：HD 高清", "畫質切換：HD 高清", "Graphics: HD")
                    } else {
                        tl(rt.ui.lang, "画质切换：1995 复古", "畫質切換：1995 復古", "Graphics: 1995 Retro")
                    };
                    rt.show_toast(msg, 110);
                }
            }
            UiAction::ToggleMusic => {
                if let Some(audio) = self.audio.as_mut() {
                    let on = audio.toggle_music();
                    if let Some(rt) = self.rt.as_mut() {
                        rt.ui.music_on = on;
                        rt.compositor.mark_ui_dirty();
                    }
                }
                self.persist_user_config();
            }
            UiAction::ToggleSfx => {
                if let Some(audio) = self.audio.as_mut() {
                    let on = audio.toggle_sfx();
                    if let Some(rt) = self.rt.as_mut() {
                        rt.ui.sfx_on = on;
                        rt.compositor.mark_ui_dirty();
                    }
                }
                self.persist_user_config();
            }
            UiAction::ToggleLanguage => {
                let next = {
                    let rt = self.rt.as_mut().unwrap();
                    let next = rt.ui.lang;
                    rt.set_lang(next);
                    self.saves.refresh_slots(&self.cfg.assets_root, rt);
                    next
                };
                self.persist_user_config();
                if let Some(rt) = self.rt.as_mut() {
                    let msg = match next {
                        pal_core::Language::ZhHant => "文字已切換：繁體中文",
                        pal_core::Language::ZhHans => "文字已切换：简体中文",
                        pal_core::Language::En => "Language: English",
                    };
                    rt.show_toast(msg, 110);
                }
                self.refresh_download_all_label();
            }
            UiAction::ToggleFullscreen => {
                let next = self.rt.as_ref().unwrap().ui.fullscreen;
                self.set_fullscreen(next);
            }
            UiAction::ToggleAgentMode => {
                let next = self.rt.as_ref().unwrap().ui.agent_mode;
                self.set_agent_mode(next);
            }
            UiAction::CopyAgentToken => {
                self.copy_agent_token();
            }
            UiAction::RefreshAgentToken => {
                self.refresh_agent_token();
            }
            UiAction::DownloadAllAssets => self.start_download_all(),
            UiAction::CheckForUpdate => {
                if let Some(info) = self.pending_update.clone() {
                    if let Some(rt) = self.rt.as_mut() {
                        let v = &info.version;
                        rt.show_toast(
                            tl(rt.ui.lang, &format!("正在下载更新 {v}..."), &format!("正在下載更新 {v}..."), &format!("Downloading update {v}...")),
                            240,
                        );
                    }
                    self.update_rx = Some(updater::spawn_self_update(info, self.rt.as_ref().map(|r| r.ui.lang).unwrap_or_default()));
                } else {
                    if let Some(rt) = self.rt.as_mut() {
                        rt.show_toast(tl(rt.ui.lang, "正在检查新版本...", "正在檢查新版本...", "Checking for updates..."), 120);
                    }
                    self.update_rx = Some(updater::spawn_update_check(false, self.rt.as_ref().map(|r| r.ui.lang).unwrap_or_default()));
                }
            }
            UiAction::ExportSave => self.export_save_to_folder(),
            UiAction::ImportSave => self.open_saves_for_import(),
            UiAction::RestartRecording => self.restart_recording(),
            UiAction::ExportReplay => self.export_current_replay(),
            UiAction::OpenReplay => self.open_native_replay_picker(),
            UiAction::ReplayTakeOver => self.stop_core_replay(),
            UiAction::ReplayRestart => {
                if let (Some(p), Some(rt)) = (self.core_replay.as_mut(), self.rt.as_mut()) {
                    if p.load_segment(0, rt) {
                        p.paused = false;
                        p.touch_ui();
                        rt.ui.set_view(UiView::None);
                        rt.compositor.mark_ui_dirty();
                    }
                }
                self.service_host_cmds();
                let lang = self.rt.as_ref().map(|rt| rt.ui.lang).unwrap_or(pal_core::Language::ZhHans);
                self.show_transition_banner(tl(lang, "重新播放录像", "重新播放錄像", "Replaying from the start"), 1500);
            }
            UiAction::ReplayJumpChapter(idx) => {
                let jumped = if let (Some(p), Some(rt)) = (self.core_replay.as_mut(), self.rt.as_mut()) {
                    if p.load_segment(idx, rt) {
                        p.paused = false;
                        p.touch_ui();
                        p.bundle.segments.get(idx).map(|s| format!("#{:02} {}", idx + 1, s.label))
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(lbl) = jumped {
                    self.service_host_cmds();
                    let lang = self.rt.as_ref().map(|rt| rt.ui.lang).unwrap_or(pal_core::Language::ZhHans);
                    self.show_transition_banner(
                        tl(lang, &format!("跳转章节 {lbl}"), &format!("跳轉章節 {lbl}"), &format!("Jump to chapter {lbl}")),
                        1500,
                    );
                }
            }
            UiAction::TakeScreenshot => {
                let _ = self.save_screenshot();
            }
            UiAction::CopyReplayReport => {
                let _ = self.export_bug_report_with_note(None);
            }
            UiAction::ReturnToTitle => {
                let confirmed = self
                    .return_title_armed
                    .map(|t| t.elapsed() < Duration::from_secs(4))
                    .unwrap_or(false);
                if !confirmed && !self.in_title() {
                    self.return_title_armed = Some(Instant::now());
                    if let Some(rt) = self.rt.as_mut() {
                        rt.ui.set_view(UiView::SystemSubmenu);
                        rt.compositor.mark_ui_dirty();
                        let msg = match rt.ui.lang {
                            pal_core::Language::En => "Press again to confirm Return to Title",
                            pal_core::Language::ZhHans => "再按一次确认返回标题画面",
                            pal_core::Language::ZhHant => "再按一次確認返回標題畫面",
                        };
                        rt.show_toast(msg, 160);
                    }
                    return;
                }
                self.return_title_armed = None;
                self.core_replay = None;
                self.rng_play = None;
                if let Some(audio) = self.audio.as_mut() {
                    audio.play_music_vfs(4, true, Some(&mut self.vfs));
                }
                let rt = self.rt.as_mut().unwrap();
                rt.ui.replay_mode = false;
                rt.ui.replay_chapters.clear();
                if let Some(g) = rt.game.as_mut() {
                    g.recorder.enabled = true;
                }
                rt.set_in_title(true);
                self.saves.refresh_slots_vfs(&self.vfs, rt);
            }
            _ => {}
        }
    }

    pub(crate) fn poll_updater(&mut self, event_loop: &ActiveEventLoop) {
        let mut clear_rx = false;
        let mut restart_exe: Option<PathBuf> = None;
        if let Some(rx) = &self.update_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    updater::UpdateMsg::Available(info) => {
                        let version = info.version.clone();
                        self.pending_update = Some(info);
                        self.splash.update_available = Some(version.clone());
                        if let Some(rt) = self.rt.as_mut() {
                            rt.ui.update_available = Some(version.clone());
                            rt.show_toast(
                                tl(
                                    rt.ui.lang,
                                    &format!("发现新版本 {version}，可在更多选项中更新"),
                                    &format!("發現新版本 {version}，可在更多選項中更新"),
                                    &format!("Update {version} available — install it from Options"),
                                ),
                                180,
                            );
                            rt.compositor.mark_ui_dirty();
                        }
                        clear_rx = true;
                    }
                    updater::UpdateMsg::UpToDate { version } => {
                        if let Some(rt) = self.rt.as_mut() {
                            rt.show_toast(
                                tl(rt.ui.lang, &format!("已是最新版本 ({version})"), &format!("已是最新版本 ({version})"), &format!("You're up to date ({version})")),
                                120,
                            );
                        }
                        clear_rx = true;
                    }
                    updater::UpdateMsg::Progress(text) => {
                        self.splash.update_status = Some(text.clone());
                        if let Some(rt) = self.rt.as_mut() {
                            rt.show_toast(text, 240);
                        }
                    }
                    updater::UpdateMsg::ReadyToRestart { version, exe_path } => {
                        self.splash.update_status = Some(format!("UPDATED TO {version} - RESTARTING..."));
                        if let Some(rt) = self.rt.as_mut() {
                            rt.show_toast(
                                tl(rt.ui.lang, &format!("已更新至 {version}，正在重启..."), &format!("已更新至 {version}，正在重新啟動..."), &format!("Updated to {version}, restarting...")),
                                120,
                            );
                        }
                        restart_exe = Some(exe_path);
                        clear_rx = true;
                    }
                    updater::UpdateMsg::Error(err) => {
                        self.splash.update_status = Some(format!("UPDATE ERROR: {err}"));
                        if let Some(rt) = self.rt.as_mut() {
                            rt.show_toast(err, 150);
                        }
                        clear_rx = true;
                    }
                }
            }
        }
        if clear_rx {
            self.update_rx = None;
        }
        if let Some(exe_path) = restart_exe {
            // Release any listening TCP socket before spawning the updated binary with the same CLI args.
            self.cancel_text_task();
            self.channel_server = None;
            let _ = std::process::Command::new(&exe_path)
                .args(env::args_os().skip(1))
                .spawn();
            event_loop.exit();
        }
    }

    pub(crate) fn service_host_cmds(&mut self) {
        // A chapter package download is pending: keep the remaining host commands queued (order
        // matters) and leave the core blocked until `SceneLoaded` is sent (#18).
        if !self.poll_pending_scene_load() {
            return;
        }
        let mut deferred: Option<(u16, u16, Vec<String>)> = None;
        let (Some(rt), Some(audio)) = (self.rt.as_mut(), self.audio.as_mut()) else {
            return;
        };
        while let Some(cmd) = rt.out_cmds.pop_front() {
            match cmd {
                HostCmd::PlayMusic { track, looped } => {
                    audio.play_music_vfs(track, looped, Some(&mut self.vfs));
                }
                HostCmd::StopMusic => audio.stop_music(),
                HostCmd::PlaySfx(id) => audio.play_sfx_vfs(id, Some(&mut self.vfs)),
                HostCmd::LoadScene { scene_id, map_id } => {
                    let missing = self
                        .pack_fetcher
                        .as_ref()
                        .map(|f| f.missing_scene_packs(scene_id, (map_id > 0).then_some(map_id)))
                        .unwrap_or_default();
                    if !missing.is_empty() {
                        // Download the chapter package off the event-loop thread behind a
                        // loading overlay; finish this LoadScene once it is cached.
                        deferred = Some((scene_id, map_id, missing));
                        break;
                    }
                    complete_scene_load(
                        &mut self.vfs,
                        self.pack_fetcher.as_ref(),
                        rt,
                        scene_id,
                        map_id,
                    );
                }
                HostCmd::LoadChapter(_) => {
                    rt.reply(HostReply::Done);
                }
                HostCmd::PlayRng {
                    id,
                    start,
                    end,
                    speed,
                } => {
                    let rng_id = id.max(0) as u16;
                    if let Some(fetcher) = &self.pack_fetcher {
                        if let Err(e) = fetcher.ensure_rng_pack(rng_id) {
                            eprintln!("[repal] on-demand RNG pack fetch warning (rng {rng_id}): {e}");
                        }
                    }
                    let _ = self.vfs.ensure_rng_pack(rng_id);
                    let meta_key = format!("graphics/rng/rng_{:02}/meta.json", rng_id);
                    let (exported_frames, frame_step) = self
                        .vfs
                        .read_to_string(&meta_key)
                        .ok()
                        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                        .map(|v| {
                            let exp = v["exported_frames"].as_u64().unwrap_or(1).max(1) as u16;
                            let step = v["frame_step"].as_u64().unwrap_or(1).max(1) as u16;
                            (exp, step)
                        })
                        .unwrap_or((1, 1));
                    let max_idx = exported_frames.saturating_sub(1);
                    let s = ((start.max(0) as u16) / frame_step).min(max_idx);
                    let e = if end <= 0 || end < start {
                        max_idx
                    } else {
                        ((end as u16) / frame_step).min(max_idx).max(s)
                    };
                    let fps = ((speed.max(1) as f64) / (frame_step as f64)).clamp(1.0, 30.0);
                    let interval = Duration::from_secs_f64(1.0 / fps);
                    rt.world.active_rng = Some((rng_id, s));
                    self.rng_play = Some(RngPlayback {
                        id: rng_id,
                        frame: s,
                        end: e,
                        frame_interval: interval,
                        next_frame_at: Instant::now() + interval,
                    });
                }
                HostCmd::ReturnToTitle => {
                    self.rng_play = None;
                    audio.stop_music();
                    rt.set_in_title(true);
                }
                HostCmd::GameOver => {
                    self.rng_play = None;
                    audio.stop_music();
                    rt.set_in_title(true);
                    rt.show_toast(
                        tl(rt.ui.lang, "胜败乃兵家常事，请重新来过", "勝敗乃兵家常事，請重新來過", "Victory and defeat are common in battle. Try again!"),
                        180,
                    );
                }
                _ => {}
            }
        }
        if let (Some((scene_id, map_id, missing)), Some(fetcher)) =
            (deferred, self.pack_fetcher.as_ref())
        {
            self.pending_scene_load = Some(crate::pack_download::PendingSceneLoad {
                scene_id,
                map_id,
                job: crate::pack_download::PackDownloadJob::spawn(
                    fetcher,
                    missing,
                    &format!("scene-{scene_id}"),
                ),
                error: None,
                retry_at: None,
            });
        }
    }

    /// Start the background "download all assets" job (#18); play continues meanwhile.
    pub(crate) fn start_download_all(&mut self) {
        if self.download_all_job.is_some() {
            return;
        }
        let Some(fetcher) = self.pack_fetcher.as_ref() else {
            return;
        };
        let missing = fetcher.missing_packs();
        let lang = self
            .rt
            .as_ref()
            .map(|rt| rt.ui.lang)
            .unwrap_or(pal_core::Language::ZhHans);
        if missing.is_empty() {
            if let Some(rt) = self.rt.as_mut() {
                rt.show_toast(pal_ui::download_all_text(lang, 0, None), 120);
            }
            self.refresh_download_all_label();
            return;
        }
        self.download_all_job = Some(crate::pack_download::PackDownloadJob::spawn(
            fetcher, missing, "all",
        ));
        self.refresh_download_all_label();
    }

    /// Poll the background "download all" job, updating the Title Options label and toasting the
    /// outcome when it finishes.
    pub(crate) fn poll_download_all(&mut self) {
        let Some(job) = self.download_all_job.as_mut() else {
            return;
        };
        let before = job.done_bytes;
        match job.poll() {
            None => {
                if job.done_bytes != before {
                    self.refresh_download_all_label();
                }
            }
            Some(res) => {
                self.download_all_job = None;
                if let Some(rt) = self.rt.as_mut() {
                    rt.compositor.cache.forget_missing();
                }
                let lang = self
                    .rt
                    .as_ref()
                    .map(|rt| rt.ui.lang)
                    .unwrap_or(pal_core::Language::ZhHans);
                let msg = match (&res, lang) {
                    (Ok(()), _) => pal_ui::download_all_text(lang, 0, None),
                    (Err(_), pal_core::Language::En) => {
                        "Download failed — try again from Options".to_string()
                    }
                    (Err(_), pal_core::Language::ZhHant) => {
                        "資源下載失敗，請稍後在選項中重試".to_string()
                    }
                    (Err(_), pal_core::Language::ZhHans) => {
                        "资源下载失败，请稍后在选项中重试".to_string()
                    }
                };
                if let Err(e) = res {
                    eprintln!("[repal] download-all failed: {e}");
                }
                if let Some(rt) = self.rt.as_mut() {
                    rt.show_toast(msg, 150);
                }
                self.refresh_download_all_label();
            }
        }
    }

    /// Recompute the "download all assets" item state (hidden without a pack fetcher).
    pub(crate) fn refresh_download_all_label(&mut self) {
        let state = self
            .pack_fetcher
            .as_ref()
            .map(|f| pal_ui::DownloadAllState {
                remaining_bytes: f.missing_packs().iter().map(|p| f.pack_size(p)).sum(),
                progress: self
                    .download_all_job
                    .as_ref()
                    .map(crate::pack_download::PackDownloadJob::fraction),
            });
        if let Some(rt) = self.rt.as_mut() {
            if rt.ui.download_all != state {
                rt.ui.download_all = state;
                rt.ui.dirty = true;
            }
        }
    }

    /// Advance a pending chapter download (#18). Returns `true` when no download is pending (or it
    /// just completed and `SceneLoaded` was sent), i.e. host commands may be serviced again.
    pub(crate) fn poll_pending_scene_load(&mut self) -> bool {
        let Some(p) = self.pending_scene_load.as_mut() else {
            return true;
        };
        if let Some(at) = p.retry_at {
            if Instant::now() >= at {
                if let Some(f) = self.pack_fetcher.as_ref() {
                    let missing =
                        f.missing_scene_packs(p.scene_id, (p.map_id > 0).then_some(p.map_id));
                    p.job = crate::pack_download::PackDownloadJob::spawn(f, missing, "retry");
                }
                p.retry_at = None;
            }
            return false;
        }
        match p.job.poll() {
            None => false,
            Some(Err(e)) => {
                eprintln!(
                    "[repal] chapter package download failed (scene {}): {e}",
                    p.scene_id
                );
                p.error = Some(e);
                p.retry_at = Some(Instant::now() + crate::pack_download::RETRY_DELAY);
                false
            }
            Some(Ok(())) => {
                let (scene_id, map_id) = (p.scene_id, p.map_id);
                self.pending_scene_load = None;
                if let Some(rt) = self.rt.as_mut() {
                    complete_scene_load(
                        &mut self.vfs,
                        self.pack_fetcher.as_ref(),
                        rt,
                        scene_id,
                        map_id,
                    );
                }
                true
            }
        }
    }

    pub(crate) fn finish_rng(&mut self) {
        self.rng_play = None;
        if let Some(rt) = self.rt.as_mut() {
            rt.world.active_rng = None;
            rt.reply(HostReply::Done);
        }
        self.service_host_cmds();
    }

    pub(crate) fn poll_channel_lifecycle(&mut self) {
        let closed_info = self
            .channel_server
            .as_ref()
            .filter(|srv| srv.is_closed())
            .map(|srv| (srv.is_batch(), srv.exit_code()));
        if let Some((is_batch, code)) = closed_info {
            let idle = self
                .text_engine
                .as_ref()
                .map(|eng| !eng.has_active_task())
                .unwrap_or(true);
            if idle {
                self.channel_server = None;
                if let Some(rt) = self.rt.as_mut() {
                    rt.ui.agent_mode = 0;
                    rt.ui.agent_token = None;
                    rt.compositor.mark_ui_dirty();
                }
                if is_batch {
                    self.exit_code = code;
                    self.should_exit = true;
                }
            }
        }
    }

    pub(crate) fn toggle_ai_thought_overlay(&mut self) {
        if let Some(rt) = self.rt.as_mut() {
            rt.ui.show_ai_thought = !rt.ui.show_ai_thought;
            rt.compositor.mark_ui_dirty();
            let msg = match (rt.ui.show_ai_thought, rt.ui.lang) {
                (true, pal_core::Language::En) => "AI Thought Subtitles: On [F7]",
                (false, pal_core::Language::En) => "AI Thought Subtitles: Off [F7]",
                (true, pal_core::Language::ZhHant) => "AI 思考字幕：開 [F7]",
                (false, pal_core::Language::ZhHant) => "AI 思考字幕：關 [F7]",
                (true, pal_core::Language::ZhHans) => "AI 思考字幕：开 [F7]",
                (false, pal_core::Language::ZhHans) => "AI 思考字幕：关 [F7]",
            };
            rt.show_toast(msg, 110);
        }
    }

    /// Returns `true` when an async TextEngine agent task is currently driving gameplay (#228).
    pub(crate) fn agent_task_active(&self) -> bool {
        self.text_engine
            .as_ref()
            .is_some_and(|e| e.has_active_task())
    }

    /// Cancel an active agent task because the human pressed Esc to take over (#228).
    pub(crate) fn human_takeover_from_agent(&mut self) {
        if let Some(mut engine) = self.text_engine.take() {
            if engine.has_active_task() {
                engine.cancel_active_task();
                if let Some(tx) = self.pending_reply_tx.take() {
                    let reply = engine.cancelled_reply_with_reason(
                        self,
                        "human",
                        "task cancelled by human (Esc)",
                    );
                    let _ = tx.send(reply);
                }
            }
            self.text_engine = Some(engine);
        }
        self.untrusted_cmd_active = false;
        self.keys_down.clear();
        self.pending_interact = false;
        self.pending_click = None;
        self.pointer_drag = false;
        if let Some(rt) = self.rt.as_mut() {
            let msg = match rt.ui.lang {
                pal_core::Language::En => "Took over control from AI",
                pal_core::Language::ZhHans => "已按 Esc 接管控制",
                pal_core::Language::ZhHant => "已按 Esc 接管控制",
            };
            rt.show_toast(msg, 120);
        }
    }

    pub(crate) fn notify_agent_busy_hint(&mut self) {
        if let Some(rt) = self.rt.as_mut() {
            let msg = match rt.ui.lang {
                pal_core::Language::En => "AI is acting… (Esc to take over)",
                pal_core::Language::ZhHans => "AI 行动中… (按 Esc 接管)",
                pal_core::Language::ZhHant => "AI 行動中… (按 Esc 接管)",
            };
            rt.show_toast(msg, 80);
        }
    }

}

/// Mount a scene's (already cached) packages, load its collision/sprite metadata, and unblock the
/// core with `SceneLoaded`.
fn complete_scene_load(
    vfs: &mut OverlayVfs,
    fetcher: Option<&crate::pack_fetcher::PackFetcher>,
    rt: &mut PalRuntime,
    scene_id: u16,
    map_id: u16,
) {
    if let Some(fetcher) = fetcher {
        // Cheap once cached: SHA-256 is verified once per pack per session.
        if let Err(e) = fetcher.ensure_scene_packs(scene_id, (map_id > 0).then_some(map_id)) {
            eprintln!("[repal] on-demand pack fetch warning (scene {scene_id}): {e}");
        }
        fetcher.prefetch_adjacent_async(scene_id);
    }
    if let Some(g) = rt.game.as_mut() {
        load_scene_assets_from_vfs(vfs, g, map_id);
    }
    // While a chapter package was downloading, the renderer may already have requested the new
    // scene's map tiles and marked them permanently missing; let them be requested again now.
    rt.compositor.cache.forget_missing();
    rt.reply(HostReply::SceneLoaded);
}
