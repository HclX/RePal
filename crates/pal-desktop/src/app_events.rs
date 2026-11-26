//! `winit` window lifecycle, keyboard/mouse/drag-and-drop input handling, and keycode mapping.

use crate::*;

fn map_keycode_to_ui(code: KeyCode) -> Option<UiKey> {
    // Original 1995 DOS / SDLPAL keybindings (#274).
    match code {
        KeyCode::ArrowUp | KeyCode::Numpad8 => Some(UiKey::Up),
        KeyCode::ArrowDown | KeyCode::Numpad2 => Some(UiKey::Down),
        KeyCode::ArrowLeft | KeyCode::Numpad4 => Some(UiKey::Left),
        KeyCode::ArrowRight | KeyCode::Numpad6 => Some(UiKey::Right),
        KeyCode::Enter
        | KeyCode::NumpadEnter
        | KeyCode::Space
        | KeyCode::KeyZ
        | KeyCode::ControlLeft => Some(UiKey::Confirm),
        KeyCode::Escape
        | KeyCode::Backspace
        | KeyCode::KeyX
        | KeyCode::Insert
        | KeyCode::Numpad0 => Some(UiKey::Cancel),
        KeyCode::PageUp | KeyCode::Numpad9 => Some(UiKey::PageUp),
        KeyCode::PageDown | KeyCode::Numpad3 => Some(UiKey::PageDown),
        KeyCode::Home | KeyCode::Numpad7 => Some(UiKey::Home),
        KeyCode::End | KeyCode::Numpad1 => Some(UiKey::End),
        // SDLPAL Battle shortcuts (only acted on in BattleHud CommandWheel)
        KeyCode::KeyR => Some(UiKey::BattleRepeat),
        KeyCode::KeyA => Some(UiKey::BattleAuto),
        KeyCode::KeyD => Some(UiKey::BattleDefend),
        KeyCode::KeyE => Some(UiKey::BattleUseItem),
        KeyCode::KeyW => Some(UiKey::BattleThrowItem),
        KeyCode::KeyQ => Some(UiKey::BattleFlee),
        KeyCode::KeyF => Some(UiKey::BattleForce),
        KeyCode::KeyS => Some(UiKey::BattleStatus),
        _ => None,
    }
}

impl ApplicationHandler for DesktopApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let mut attrs = Window::default_attributes()
                .with_title("RePal — 仙劍奇俠傳 HD")
                .with_inner_size(LogicalSize::new(1280.0, 800.0))
                .with_window_icon(load_window_icon());
            if self.fullscreen {
                attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
            }
            let win = Arc::new(
                event_loop
                    .create_window(attrs)
                    .expect("failed to create desktop window"),
            );
            let ctx = Context::new(win.clone()).expect("failed to create softbuffer context");
            let surface = Surface::new(&ctx, win.clone()).expect("failed to create surface");
            self.window = Some(win);
            self.surface = Some(surface);
            self.last_frame = Instant::now();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.archive_active_replay_on_exit();
                event_loop.exit();
            }
            WindowEvent::DroppedFile(path) => {
                if self.rt.is_some() {
                    self.load_file_or_replay(path);
                }
            }
            WindowEvent::Focused(false) => {
                self.keys_down.clear();
                self.alt_used_combo = false;
                self.pointer_drag = false;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                if event.state == ElementState::Released {
                    let was_alt = matches!(code, KeyCode::AltLeft | KeyCode::AltRight)
                        && self.keys_down.contains(&code);
                    self.keys_down.remove(&code);
                    // Standalone Alt release acts as SDLPAL Menu / Cancel (#274) unless Alt was
                    // used in a modifier combo such as Alt+Enter (fullscreen) or Alt+Tab.
                    if was_alt && !self.alt_used_combo {
                        if self.rt.is_some()
                            && self.core_replay.is_none()
                            && self.rng_play.is_none()
                            && !self.agent_task_active()
                        {
                            if self.ui_active() {
                                self.keys_down.clear();
                                let act = self.rt.as_mut().and_then(|rt| rt.on_key(UiKey::Cancel));
                                if let Some(act) = act {
                                    self.handle_ui_action(act);
                                }
                                self.service_host_cmds();
                            } else if !self.core_busy() {
                                self.keys_down.clear();
                                if let Some(rt) = self.rt.as_mut() {
                                    rt.ui.set_view(UiView::RootMenu);
                                    rt.compositor.mark_ui_dirty();
                                }
                            }
                        }
                    }
                    if !self.keys_down.contains(&KeyCode::AltLeft)
                        && !self.keys_down.contains(&KeyCode::AltRight)
                    {
                        self.alt_used_combo = false;
                    }
                    return;
                }
                // Track modifier keys for Alt+Enter (and standalone Alt Menu/Cancel on release)
                if matches!(code, KeyCode::AltLeft | KeyCode::AltRight) {
                    if !self.keys_down.contains(&code) {
                        self.alt_used_combo = false;
                    }
                    self.keys_down.insert(code);
                    return;
                }
                let alt_held = self.keys_down.contains(&KeyCode::AltLeft)
                    || self.keys_down.contains(&KeyCode::AltRight);
                if alt_held {
                    self.alt_used_combo = true;
                }
                if code == KeyCode::F11 || (code == KeyCode::Enter && alt_held) {
                    self.set_fullscreen(!self.fullscreen);
                    return;
                }
                if self.rt.is_none() {
                    if self.splash.error.is_some() {
                        if code == KeyCode::Escape {
                            event_loop.exit();
                        } else if code == KeyCode::KeyU && self.update_rx.is_none() {
                            if let Some(info) = self.pending_update.clone() {
                                self.splash.update_status =
                                    Some(format!("DOWNLOADING {}...", info.version));
                                self.update_rx = Some(updater::spawn_self_update(info));
                            }
                        }
                    }
                    return;
                }
                // F2: Live instant HD <-> 1995 Original toggle everywhere (gameplay, menus, battle, RNG, replay) (#274)
                if code == KeyCode::F2 {
                    if let Some(rt) = self.rt.as_mut() {
                        rt.ui.hd_mode = !rt.ui.hd_mode;
                    }
                    self.handle_ui_action(UiAction::ToggleHdMode);
                    return;
                }
                if code == KeyCode::F8 {
                    if self.core_replay.is_some() {
                        self.stop_core_replay();
                    } else if !self.replay_queue.is_empty() {
                        self.replay_queue.clear();
                        self.cancel_text_task();
                        if let Some(rt) = self.rt.as_mut() {
                            let msg = match rt.ui.lang {
                                pal_core::Language::En => "Replay stopped",
                                pal_core::Language::ZhHans => "已停止回放",
                                pal_core::Language::ZhHant => "已停止回放",
                            };
                            rt.show_toast(msg, 120);
                        }
                    } else {
                        self.open_native_replay_picker();
                    }
                    return;
                }
                if self.core_replay.is_some() {
                    if self.replay_menu_open() {
                        if let Some(uk) = map_keycode_to_ui(code) {
                            let act = self.rt.as_mut().and_then(|rt| rt.on_key(uk));
                            if let Some(act) = act {
                                self.handle_ui_action(act);
                            }
                            self.service_host_cmds();
                        }
                        return;
                    }
                    match code {
                        KeyCode::Escape => {
                            if let Some(p) = self.core_replay.as_mut() {
                                p.touch_ui();
                            }
                            if let Some(rt) = self.rt.as_mut() {
                                rt.ui.root_cursor = 0;
                                rt.ui.set_view(UiView::RootMenu);
                                rt.compositor.mark_ui_dirty();
                            }
                        }
                        KeyCode::Tab => self.handle_replay_hud_hit(ReplayHudHit::Chapters),
                        KeyCode::Space => self.handle_replay_hud_hit(ReplayHudHit::PlayPause),
                        KeyCode::Equal | KeyCode::NumpadAdd => {
                            if let Some(p) = self.core_replay.as_mut() {
                                p.speed_up();
                            }
                        }
                        KeyCode::Minus | KeyCode::NumpadSubtract => {
                            if let Some(p) = self.core_replay.as_mut() {
                                p.speed_down();
                            }
                        }
                        KeyCode::BracketLeft => self.handle_replay_hud_hit(ReplayHudHit::PrevSeg),
                        KeyCode::BracketRight => self.handle_replay_hud_hit(ReplayHudHit::NextSeg),
                        KeyCode::F7 => self.toggle_ai_thought_overlay(),
                        KeyCode::F10 => {
                            let _ = self.save_screenshot();
                        }
                        _ => {}
                    }
                    return;
                }
                if code == KeyCode::F7 {
                    self.toggle_ai_thought_overlay();
                    return;
                }
                if code == KeyCode::F9 {
                    self.export_bug_report();
                    return;
                }
                if code == KeyCode::F10 {
                    let _ = self.save_screenshot();
                    return;
                }
                // Chapter package download in progress (#18): the core is blocked on the host, so
                // swallow gameplay/menu keys until the loading overlay goes away.
                if self.pending_scene_load.is_some() {
                    return;
                }
                if self.rng_play.is_some()
                    && matches!(
                        code,
                        KeyCode::Escape
                            | KeyCode::Space
                            | KeyCode::Enter
                            | KeyCode::NumpadEnter
                            | KeyCode::KeyZ
                            | KeyCode::ControlLeft
                    )
                {
                    self.finish_rng();
                    return;
                }
                // Interim #228 human-vs-agent arbitration: Esc cancels the active AI task;
                // other gameplay keys show a hint instead of fighting the agent silently.
                if self.agent_task_active() {
                    if code == KeyCode::Escape {
                        self.human_takeover_from_agent();
                    } else if code != KeyCode::F2 {
                        self.notify_agent_busy_hint();
                    }
                    return;
                }
                if self.ui_active() {
                    self.keys_down.clear();
                    if let Some(uk) = map_keycode_to_ui(code) {
                        let act = self.rt.as_mut().and_then(|rt| rt.on_key(uk));
                        if let Some(act) = act {
                            self.handle_ui_action(act);
                        }
                        self.service_host_cmds();
                    }
                    return;
                }
                if matches!(code, KeyCode::Escape | KeyCode::Insert | KeyCode::Numpad0) {
                    if !self.core_busy() {
                        self.keys_down.clear();
                        if let Some(rt) = self.rt.as_mut() {
                            rt.ui.set_view(UiView::RootMenu);
                            rt.compositor.mark_ui_dirty();
                        }
                    }
                    return;
                }
                if matches!(
                    code,
                    KeyCode::Space
                        | KeyCode::Enter
                        | KeyCode::NumpadEnter
                        | KeyCode::KeyZ
                        | KeyCode::ControlLeft
                ) {
                    self.pending_interact = true;
                    return;
                }
                self.keys_down.insert(code);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer_phys = (position.x, position.y);
                if let Some(p) = self.core_replay.as_mut() {
                    p.touch_ui();
                    let dh = self
                        .window
                        .as_ref()
                        .map(|w| w.inner_size().height as f64)
                        .unwrap_or(800.0);
                    p.pointer_in_bottom = position.y >= (dh - 54.0).max(0.0);
                }
                if self.ui_active() {
                    let (lx, ly) = self.phys_to_logical(position.x, position.y);
                    let act = self.rt.as_mut().and_then(|rt| {
                        rt.on_pointer(PointerEvent {
                            kind: PointerKind::Move,
                            x: lx,
                            y: ly,
                        })
                    });
                    if let Some(act) = act {
                        self.handle_ui_action(act);
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if self.pending_scene_load.is_some() {
                    self.pointer_drag = false;
                    return;
                }
                if self.rt.is_none() {
                    return;
                }
                match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => {
                        if self.core_replay.is_some() && !self.replay_menu_open() {
                            let (dw, dh) = self
                                .window
                                .as_ref()
                                .map(|w| {
                                    let s = w.inner_size();
                                    (s.width as usize, s.height as usize)
                                })
                                .unwrap_or((1280, 800));
                            let shown = self
                                .core_replay
                                .as_ref()
                                .map(|p| p.controls_visible())
                                .unwrap_or(false);
                            if let Some(hit) = hit_test_replay_bar(
                                self.pointer_phys.0,
                                self.pointer_phys.1,
                                dw,
                                dh,
                                shown,
                            ) {
                                self.handle_replay_hud_hit(hit);
                            } else if let Some(p) = self.core_replay.as_mut() {
                                p.touch_ui();
                            }
                            return;
                        }
                        if self.rng_play.is_some() {
                            self.finish_rng();
                        } else if self.agent_task_active() {
                            self.pointer_drag = false;
                            self.notify_agent_busy_hint();
                        } else if self.ui_active() {
                            self.pointer_drag = false;
                            let (lx, ly) =
                                self.phys_to_logical(self.pointer_phys.0, self.pointer_phys.1);
                            let act = self.rt.as_mut().and_then(|rt| {
                                rt.on_pointer(PointerEvent {
                                    kind: PointerKind::Down,
                                    x: lx,
                                    y: ly,
                                })
                            });
                            if let Some(act) = act {
                                self.handle_ui_action(act);
                            }
                            self.service_host_cmds();
                        } else if !self.core_busy() {
                            self.pointer_drag = true;
                            let (lx, ly) =
                                self.phys_to_logical(self.pointer_phys.0, self.pointer_phys.1);
                            let (wx, wy) = self.logical_to_world(lx, ly);
                            if let Some(rt) = self.rt.as_mut() {
                                rt.world.click_ripple = Some((wx, wy, 0.0));
                            }
                            self.pending_click = Some((wx, wy));
                        }
                    }
                    (MouseButton::Left, ElementState::Released) => {
                        self.pointer_drag = false;
                    }
                    (MouseButton::Right, ElementState::Pressed) => {
                        if self.ui_active() {
                            let act = self.rt.as_mut().and_then(|rt| rt.on_key(UiKey::Cancel));
                            if let Some(act) = act {
                                self.handle_ui_action(act);
                            }
                            self.service_host_cmds();
                        } else if !self.core_busy() {
                            if let Some(rt) = self.rt.as_mut() {
                                rt.ui.set_view(UiView::RootMenu);
                                rt.compositor.mark_ui_dirty();
                            }
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                self.poll_bootstrap();
                self.step_simulation();
                self.render_and_present();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.poll_bootstrap();
        self.poll_bug_report();
        self.poll_file_picker();
        self.poll_updater(event_loop);
        self.poll_channel_lifecycle();
        if self.should_exit {
            self.archive_active_replay_on_exit();
            event_loop.exit();
            return;
        }
        self.flush_archived_replays();
        // True 50 Hz frame pacing: only request a redraw once `next_frame_at` is reached,
        // otherwise let winit sleep in the OS kernel until `next_frame_at` (prevents 100% CPU spin).
        let now = Instant::now();
        if now >= self.next_frame_at {
            self.next_frame_at =
                (self.next_frame_at + TICK_DURATION).max(now + Duration::from_millis(2));
            if let Some(win) = &self.window {
                let sz = win.inner_size();
                if sz.width > 0 && sz.height > 0 {
                    win.request_redraw();
                }
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame_at));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_key_mapping_matches_sdlpal_274() {
        // Arrows + Numpad directions
        assert_eq!(map_keycode_to_ui(KeyCode::ArrowUp), Some(UiKey::Up));
        assert_eq!(map_keycode_to_ui(KeyCode::Numpad8), Some(UiKey::Up));
        assert_eq!(map_keycode_to_ui(KeyCode::ArrowDown), Some(UiKey::Down));
        assert_eq!(map_keycode_to_ui(KeyCode::Numpad2), Some(UiKey::Down));
        assert_eq!(map_keycode_to_ui(KeyCode::ArrowLeft), Some(UiKey::Left));
        assert_eq!(map_keycode_to_ui(KeyCode::Numpad4), Some(UiKey::Left));
        assert_eq!(map_keycode_to_ui(KeyCode::ArrowRight), Some(UiKey::Right));
        assert_eq!(map_keycode_to_ui(KeyCode::Numpad6), Some(UiKey::Right));

        // Confirm & Cancel
        for c in [
            KeyCode::Enter,
            KeyCode::NumpadEnter,
            KeyCode::Space,
            KeyCode::KeyZ,
            KeyCode::ControlLeft,
        ] {
            assert_eq!(map_keycode_to_ui(c), Some(UiKey::Confirm));
        }
        for c in [
            KeyCode::Escape,
            KeyCode::Backspace,
            KeyCode::KeyX,
            KeyCode::Insert,
            KeyCode::Numpad0,
        ] {
            assert_eq!(map_keycode_to_ui(c), Some(UiKey::Cancel));
        }

        // Paging
        assert_eq!(map_keycode_to_ui(KeyCode::PageUp), Some(UiKey::PageUp));
        assert_eq!(map_keycode_to_ui(KeyCode::Numpad9), Some(UiKey::PageUp));
        assert_eq!(map_keycode_to_ui(KeyCode::PageDown), Some(UiKey::PageDown));
        assert_eq!(map_keycode_to_ui(KeyCode::Numpad3), Some(UiKey::PageDown));
        assert_eq!(map_keycode_to_ui(KeyCode::Home), Some(UiKey::Home));
        assert_eq!(map_keycode_to_ui(KeyCode::Numpad7), Some(UiKey::Home));
        assert_eq!(map_keycode_to_ui(KeyCode::End), Some(UiKey::End));
        assert_eq!(map_keycode_to_ui(KeyCode::Numpad1), Some(UiKey::End));

        // Battle shortcuts (WASD are no longer directions!)
        assert_eq!(map_keycode_to_ui(KeyCode::KeyR), Some(UiKey::BattleRepeat));
        assert_eq!(map_keycode_to_ui(KeyCode::KeyA), Some(UiKey::BattleAuto));
        assert_eq!(map_keycode_to_ui(KeyCode::KeyD), Some(UiKey::BattleDefend));
        assert_eq!(map_keycode_to_ui(KeyCode::KeyE), Some(UiKey::BattleUseItem));
        assert_eq!(map_keycode_to_ui(KeyCode::KeyW), Some(UiKey::BattleThrowItem));
        assert_eq!(map_keycode_to_ui(KeyCode::KeyQ), Some(UiKey::BattleFlee));
        assert_eq!(map_keycode_to_ui(KeyCode::KeyF), Some(UiKey::BattleForce));
        assert_eq!(map_keycode_to_ui(KeyCode::KeyS), Some(UiKey::BattleStatus));

        // Tab is NOT bound in gameplay UI
        assert_eq!(map_keycode_to_ui(KeyCode::Tab), None);
    }
}


