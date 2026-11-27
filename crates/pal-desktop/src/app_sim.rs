//! 50 Hz simulation loop, replay/text-engine stepping, and software framebuffer presentation.

use crate::*;

/// Redraw interval for an unfocused, idle window (~10 fps).
const BACKGROUND_FRAME_INTERVAL: Duration = Duration::from_millis(100);

/// Foreground draw cap. The game logic ticks at 50 Hz, but the picture changes at most every
/// 4 ticks (party steps at 12.5 Hz; NPC scripts every 6; the original DOS game drew ~10 fps), so
/// 15 fps cuts HD software rendering to under a third with no visible difference in play testing.
/// `REPAL_MAX_FPS` overrides (1..=50; 50 = draw every tick).
fn foreground_frame_interval() -> Duration {
    static INTERVAL: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *INTERVAL.get_or_init(|| {
        let fps = std::env::var("REPAL_MAX_FPS")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(15)
            .clamp(1, 50);
        Duration::from_micros(1_000_000 / fps as u64)
    })
}

impl DesktopApp {
    pub(crate) fn step_simulation(&mut self) {
        if self.rt.is_none() {
            self.splash.tick = self.splash.tick.wrapping_add(1);
            return;
        }
        self.poll_download_all();
        let now = Instant::now();
        let dt = (now - self.last_frame).min(TICK_DURATION * 8);
        self.last_frame = now;

        if let Some(rng) = self.rng_play.as_mut() {
            let decay = (dt.as_millis() / 20).max(1) as u32;
            if let Some(rt) = self.rt.as_mut() {
                rt.decay_toast_presentation(decay);
            }
            if now >= rng.next_frame_at {
                if rng.frame >= rng.end {
                    self.finish_rng();
                } else {
                    rng.frame += 1;
                    rng.next_frame_at = now + rng.frame_interval;
                    if let Some(rt) = self.rt.as_mut() {
                        rt.world.active_rng = Some((rng.id, rng.frame));
                    }
                }
            }
            // Still service TextEngine commands (e.g. `look`, `next`/`close` to skip RNG) while
            // an RNG cutscene is playing so TCP/stdio clients never hang.
            let _ = self.step_text_engine();
            self.poll_channel_lifecycle();
            return;
        }

        self.tick_acc += dt;
        while self.tick_acc >= TICK_DURATION {
            self.tick_acc -= TICK_DURATION;
            if self.core_replay.is_some() {
                let replay_ticked = !self.replay_menu_open()
                    && !self.transition_active()
                    && self.core_replay.as_ref().is_some_and(|p| !p.paused && !p.finished);
                if !replay_ticked {
                    if let Some(rt) = self.rt.as_mut() {
                        rt.decay_toast_presentation(1);
                    }
                }
                // Pause deterministic playback while Replay Menu or full-screen transition banner is active
                // (never pause for in-game Dialog or BattleHud spawned by the replay itself!).
                if !self.replay_menu_open() && !self.transition_active() {
                    let speed = self.core_replay.as_ref().map(|p| p.speed).unwrap_or(1);
                    let mut sync_msg = None;
                    for _ in 0..speed {
                        if let (Some(player), Some(rt)) =
                            (self.core_replay.as_mut(), self.rt.as_mut())
                        {
                            player.step_one_tick(rt, &mut self.vfs);
                            if let Some(b) = player.pending_sync_banner.take() {
                                sync_msg = Some(b);
                            }
                        }
                        self.service_host_cmds();
                        if sync_msg.is_some() {
                            break;
                        }
                    }
                    if let Some(msg) = sync_msg {
                        self.show_transition_banner(msg, 1500);
                    }
                }
                // Still poll `channel_server` while a `.repal` replay is active so TCP/stdio
                // clients receive immediate read-only responses or a clean rejection (#196).
                self.poll_channel_during_replay();
                self.poll_channel_lifecycle();
                continue;
            }
            let agent_input = self.step_text_engine();
            // Interim #228 arbitration: while a TextEngine agent task is running, ignore human
            // directional/interact/click inputs (Esc cancels and hands control back to the human).
            let input = if self.pending_scene_load.is_some() {
                // Loading overlay up (#18): the core is blocked on the host anyway; drop human input.
                self.pending_interact = false;
                self.pending_click = None;
                TickInput::IDLE
            } else if self.agent_task_active() {
                self.pending_interact = false;
                self.pending_click = None;
                agent_input
            } else {
                let held = if agent_input.held_dir >= 0 {
                    agent_input.held_dir
                } else {
                    self.held_direction()
                };
                let steer = if held < 0 && self.pointer_drag && !self.ui_active() {
                    let (lx, ly) = self.phys_to_logical(self.pointer_phys.0, self.pointer_phys.1);
                    Some(self.logical_to_world(lx, ly))
                } else {
                    None
                };
                TickInput {
                    held_dir: held,
                    interact: std::mem::take(&mut self.pending_interact) || agent_input.interact,
                    click: self.pending_click.take().or(agent_input.click),
                    steer,
                }
            };
            if let Some(rt) = self.rt.as_mut() {
                rt.tick(input);
            }
            self.service_host_cmds();
            self.poll_channel_lifecycle();
        }
    }

    /// Service incoming channel requests while deterministic `.repal` playback is active (#196):
    /// allow read-only inspection commands (`look`, `status`, `bag`, `shot`, `dump-state`, `help`,
    /// `quit`, `/state`, `/event <id>`) and immediately reject mutating commands without hanging.
    pub(crate) fn poll_channel_during_replay(&mut self) {
        use pal_headless::text_mode::{parse_command, Command};
        let Some(req) = self
            .channel_server
            .as_ref()
            .and_then(|srv| srv.rx.try_recv().ok())
        else {
            return;
        };
        let Some(mut engine) = self.text_engine.take() else {
            return;
        };
        let is_read_only = match parse_command(req.raw.trim()) {
            Ok(
                Command::Look
                | Command::Status
                | Command::Bag
                | Command::Help(_)
                | Command::Quit
                | Command::Shot { .. }
                | Command::DumpState(_)
                | Command::DebugState
                | Command::DebugEventInspect(_),
            ) => true,
            _ => false,
        };
        if is_read_only {
            match engine.submit_from(self, &req.raw, false, req.origin) {
                ExecOutcome::Done(reply) | ExecOutcome::Quit(reply) => {
                    let _ = req.reply_tx.send(reply);
                }
                ExecOutcome::Pending => {
                    engine.cancel_active_task();
                    let reply = engine.err_reply_public(
                        self,
                        &req.raw,
                        "cannot execute async command while replay is active",
                    );
                    let _ = req.reply_tx.send(reply);
                }
            }
        } else {
            let reply = engine.err_reply_public(
                self,
                &req.raw,
                "cannot execute mutating command while replay is active (stop replay first)",
            );
            let _ = req.reply_tx.send(reply);
        }
        self.text_engine = Some(engine);
    }

    /// Drive the shared `TextEngine` one 20 ms simulation tick (polling `ChannelServer` or
    /// `.cmd` `replay_queue` when idle, and advancing any active multi-tick task).
    pub(crate) fn step_text_engine(&mut self) -> TickInput {
        if self.rt.is_none() {
            return TickInput::IDLE;
        }
        let Some(mut engine) = self.text_engine.take() else {
            return TickInput::IDLE;
        };

        if !engine.has_active_task() {
            self.untrusted_cmd_active = false;
            let from_channel = self
                .channel_server
                .as_ref()
                .and_then(|srv| srv.rx.try_recv().ok())
                .map(|req| (req, false));
            let next_req = from_channel.or_else(|| {
                self.replay_queue.pop_front().map(|cmd| {
                    let (reply_tx, _rx) = std::sync::mpsc::channel();
                    (
                        IncomingCmd {
                            raw: cmd,
                            interactive_blank: false,
                            origin: ClientOrigin::Local,
                            reply_tx,
                        },
                        true,
                    )
                })
            });
            if let Some((req, is_untrusted_cmd)) = next_req {
                // Guard host-privileged UI actions for both `.cmd` replays (#231) and remote
                // TCP clients (`key`/`ui`/`click`, #236) unless `--allow-unsafe-remote` is set.
                self.untrusted_cmd_active =
                    is_untrusted_cmd || (req.origin.is_remote() && !engine.allow_unsafe_remote);
                match engine.submit_from(self, &req.raw, req.interactive_blank, req.origin) {
                    ExecOutcome::Done(reply) | ExecOutcome::Quit(reply) => {
                        self.untrusted_cmd_active = false;
                        let _ = req.reply_tx.send(reply);
                    }
                    ExecOutcome::Pending => {
                        self.pending_reply_tx = Some(req.reply_tx);
                    }
                }
            }
        }

        let input = if engine.has_active_task() {
            let (tick_in, done) = engine.step_tick(self);
            if let Some(reply) = done {
                self.untrusted_cmd_active = false;
                if let Some(tx) = self.pending_reply_tx.take() {
                    let _ = tx.send(reply);
                }
            }
            tick_in
        } else {
            self.untrusted_cmd_active = false;
            TickInput::IDLE
        };

        self.text_engine = Some(engine);
        input
    }

    /// Background render throttling: HD software rendering costs ~10 ms/frame, so an idle game in
    /// an unfocused window drew half a CPU core for nothing. The simulation always runs at 50 Hz;
    /// only drawing is reduced: capped at 15 fps (`REPAL_MAX_FPS`), ~10 fps when unfocused, none
    /// while hidden. Anything someone may be watching from another window keeps the foreground cap: replay playback, RNG cutscenes, an attached
    /// Text Mode / LAN agent server, or the boot splash.
    pub(crate) fn should_present_frame(&self) -> bool {
        // Redraws arrive at the 50 Hz tick; allow ~4 ms of jitter so the cap lands on tick boundaries
        // (e.g. 15 fps draws every 3rd-4th tick, 25 fps every 2nd).
        let due = |interval: Duration| self.last_present.elapsed() + Duration::from_millis(4) >= interval;
        let fg_interval = foreground_frame_interval();
        if self.window_focused && !self.window_occluded {
            return due(fg_interval);
        }
        let watched = self.rt.is_none()
            || self.core_replay.as_ref().is_some_and(|p| !p.paused && !p.finished)
            || self.rng_play.is_some()
            || self.channel_server.is_some();
        if watched {
            return due(fg_interval);
        }
        !self.window_occluded && due(BACKGROUND_FRAME_INTERVAL.max(fg_interval))
    }

    /// Print `REPAL_PERF=1` main-thread timings every 5 s (ms are per second of wall time).
    pub(crate) fn report_perf(&mut self) {
        let watched = format!(
            "focused={} occluded={} replay={} rng={} agent_server={}",
            self.window_focused,
            self.window_occluded,
            self.core_replay.as_ref().is_some_and(|p| !p.paused && !p.finished),
            self.rng_play.is_some(),
            self.channel_server.is_some()
        );
        let hd = self.rt.as_ref().is_some_and(|rt| rt.compositor.hd_mode);
        let Some(p) = self.perf.as_mut() else { return };
        let secs = p.since.elapsed().as_secs_f64();
        if secs < 5.0 {
            return;
        }
        let ms = |d: Duration| d.as_secs_f64() * 1000.0 / secs;
        eprintln!(
            "[perf] hd={hd} redraws/s={:.1} presents/s={:.1} | busy ms/s: sim={:.1} render+blit={:.1} (blit+present={:.1}) about_to_wait={:.1} | {watched}",
            p.redraws as f64 / secs,
            p.presents as f64 / secs,
            ms(p.sim),
            ms(p.render),
            ms(p.blit_present),
            ms(p.wait),
        );
        *p = crate::PerfStats::new();
    }

    pub(crate) fn render_and_present(&mut self) {
        let (Some(win), Some(surface)) = (&self.window, &mut self.surface) else {
            return;
        };
        let sz = win.inner_size();
        let (Some(dw), Some(dh)) = (NonZeroU32::new(sz.width), NonZeroU32::new(sz.height)) else {
            return;
        };
        if self.surface_size != (sz.width, sz.height) {
            if surface.resize(dw, dh).is_err() {
                return;
            }
            self.surface_size = (sz.width, sz.height);
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };
        let dw_u = sz.width as usize;
        let dh_u = sz.height as usize;
        if buffer.len() < dw_u.saturating_mul(dh_u) {
            return;
        }

        let Some(rt) = self.rt.as_mut() else {
            render_splash(&mut buffer, dw_u, dh_u, &self.splash);
            let _ = buffer.present();
            return;
        };

        // Advance golden click-ripple animation (mouse click, agent goto, or replay Click)
        // Ripple lasts ~0.4 s of wall time regardless of the draw rate.
        let ripple_dt = self.ripple_clock.elapsed().as_secs_f32().min(0.25);
        self.ripple_clock = Instant::now();
        if let Some((rx, ry, age)) = rt.world.click_ripple {
            let next_age = age + ripple_dt * 2.5;
            if next_age >= 1.0 {
                rt.world.click_ripple = None;
            } else {
                rt.world.click_ripple = Some((rx, ry, next_age));
            }
        }

        // Render + synchronously fulfill any newly-requested disk assets (up to 6 passes so
        // 2-stage Track-B `meta.json -> .webp` & 1x fallback `gop_XXX_1x.webp` pop in without blank frames, #178/#189).
        // Keep `world.frame_count` advancing by exactly 1 per presented frame instead of up to 6× (#233).
        let base_frame_count = rt.world.frame_count;
        for _ in 0..6 {
            if rt.ui.dirty {
                rt.compositor.mark_ui_dirty();
                rt.ui.dirty = false;
            }
            rt.world.frame_count = base_frame_count;
            rt.render_frame();
            if !rt.compositor.cache.has_pending_requests() {
                break;
            }
            let _ = pump_render_assets_from_vfs(&mut self.vfs, &mut rt.compositor.cache);
            rt.compositor.mark_ui_dirty();
        }

        // Full-screen dim + centered CJK text banner for load / chapter jump transitions
        if let Some((ref text, started, dur)) = self.transition_banner {
            let elapsed = started.elapsed();
            if elapsed < dur {
                let frac = elapsed.as_secs_f32() / dur.as_secs_f32().max(0.001);
                let alpha = if frac < 0.18 {
                    (frac / 0.18 * 195.0) as u8
                } else if frac > 0.78 {
                    (((1.0 - frac) / 0.22).clamp(0.0, 1.0) * 195.0) as u8
                } else {
                    195u8
                };
                render_transition_banner(rt, text, alpha);
            } else {
                self.transition_banner = None;
            }
        }

        // Chapter package download overlay (#18), shown after a short delay so cached transitions
        // never flash; errors show immediately while the download auto-retries.
        if let Some(p) = &self.pending_scene_load {
            if p.error.is_some()
                || p.job.started.elapsed() >= crate::pack_download::LOADING_OVERLAY_DELAY
            {
                let view = crate::pack_download::loading_view_for(p, rt.ui.lang);
                crate::pack_download::render_loading_overlay(rt, &view);
            }
        }

        // Capture a lightweight 160x100 PNG thumbnail for the active replay segment once rendered
        // (structurally thought-free because `draw_ai_thought_overlay` runs only at present time).
        if !rt.is_in_title() && self.core_replay.is_none() {
            let needs_thumb = rt
                .game
                .as_ref()
                .map(|g| g.recorder.needs_latest_thumb())
                .unwrap_or(false);
            if needs_thumb {
                if let Some(png) = encode_fb_thumb_png(&rt.compositor.fb, 160, 100) {
                    if let Some(g) = rt.game.as_mut() {
                        g.recorder.set_latest_thumb(png);
                    }
                }
            }
        }

        // Wall-clock 12 s auto-dim for the HD AI thought subtitle bar (restores to 100% on new thought).
        if rt.ai_thought_gen != self.last_thought_gen {
            self.last_thought_gen = rt.ai_thought_gen;
            self.last_thought_at = rt.ui.ai_thought.as_ref().map(|_| Instant::now());
            rt.ui.ai_thought_dimmed = false;
        }
        rt.ui.ai_thought_dimmed = self
            .last_thought_at
            .is_some_and(|since| since.elapsed() >= Duration::from_secs(12));

        // Present-time-only HD AI thought overlay: never mutates `rt.compositor.fb` so `golden`,
        // `bugreport`, `end_screenshot`, thumbnails, and default `shot` stay thought-free by construction.
        let fb = if rt.has_visible_ai_thought_overlay() {
            let w = rt.compositor.fb.width;
            let h = rt.compositor.fb.height;
            if self.present_fb.width != w || self.present_fb.height != h {
                self.present_fb.resize(w, h);
            }
            self.present_fb
                .pixels
                .copy_from_slice(&rt.compositor.fb.pixels);
            rt.draw_ai_thought_overlay_onto(&mut self.present_fb);
            &self.present_fb
        } else {
            &rt.compositor.fb
        };
        let sw = fb.width as usize;
        let sh = fb.height as usize;

        let blit_t0 = Instant::now();
        // 16:10 letterboxed nearest-neighbor blit (RGBA8 -> 0x00RRGGBB).
        let target_aspect = 16.0 / 10.0;
        let win_aspect = dw_u as f64 / dh_u as f64;
        let (vx, vy, vw, vh) = if win_aspect > target_aspect {
            let vw = ((dh_u as f64) * target_aspect).round() as usize;
            ((dw_u.saturating_sub(vw)) / 2, 0, vw.max(1).min(dw_u), dh_u)
        } else {
            let vh = ((dw_u as f64) / target_aspect).round() as usize;
            (0, (dh_u.saturating_sub(vh)) / 2, dw_u, vh.max(1).min(dh_u))
        };

        // Clear only the letterbox bars (skip full-buffer fill when 16:10 fills the window).
        if vy > 0 {
            buffer[..vy * dw_u].fill(0);
            let bot = (vy + vh).min(dh_u) * dw_u;
            buffer[bot..dh_u * dw_u].fill(0);
        } else if vx > 0 {
            for y in 0..dh_u {
                let row = y * dw_u;
                buffer[row..row + vx].fill(0);
                let right = (vx + vw).min(dw_u);
                buffer[row + right..row + dw_u].fill(0);
            }
        }

        if vw == sw && vh == sh {
            // Fast 1:1 path (default 1280x800 HD window): zero integer divisions per pixel.
            for y in 0..vh {
                let src = &fb.pixels[y * sw * 4..(y + 1) * sw * 4];
                let dst = &mut buffer[(vy + y) * dw_u + vx..(vy + y) * dw_u + vx + vw];
                for (px, out) in src.chunks_exact(4).zip(dst.iter_mut()) {
                    *out = ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | (px[2] as u32);
                }
            }
        } else {
            // Scaled path (fullscreen / resized window): reuse cached X-index LUT.
            if self.sx_lut_key != (sw, vw) {
                self.sx_lut = (0..vw).map(|x| ((x * sw) / vw) * 4).collect();
                self.sx_lut_key = (sw, vw);
            }
            for y in 0..vh {
                let dy = vy + y;
                if dy >= dh_u {
                    break;
                }
                let sy = (y * sh) / vh;
                let src = &fb.pixels[sy * sw * 4..(sy + 1) * sw * 4];
                let dst = &mut buffer[dy * dw_u + vx..dy * dw_u + vx + vw];
                for (&si, out) in self.sx_lut.iter().zip(dst.iter_mut()) {
                    *out = ((src[si] as u32) << 16)
                        | ((src[si + 1] as u32) << 8)
                        | (src[si + 2] as u32);
                }
            }
        }
        if let Some(player) = &self.core_replay {
            let menu_open = matches!(
                rt.ui.view,
                UiView::RootMenu
                    | UiView::ReplayChapters
                    | UiView::ReplayFinished
                    | UiView::SystemSubmenu
                    | UiView::RemakeSubmenu
                    | UiView::SaveLoadSlots(_)
            );
            if !menu_open {
                render_replay_bar(&mut buffer, dw_u, dh_u, player);
            }
        }
        let _ = buffer.present();
        if let Some(p) = self.perf.as_mut() {
            p.blit_present += blit_t0.elapsed();
        }
    }
}
