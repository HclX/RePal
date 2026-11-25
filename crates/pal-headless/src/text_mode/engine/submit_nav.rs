//! Policy gates and Exploration / UI / Battle command handlers for `TextEngine`.

use super::*;

impl TextEngine {
    pub fn submit_from<H: TextHost>(
        &mut self,
        host: &mut H,
        raw: &str,
        interactive_blank: bool,
        origin: ClientOrigin,
    ) -> ExecOutcome {
        self.active_origin = origin;
        // Initialize each newly connected TCP client's notice cursor at the current seq so
        // clients don't receive the stale boot backlog or consume another client's notices (#233).
        let cid = origin.client_id();
        if cid > 0 && !self.client_notice_cursors.contains_key(&cid) {
            let cur = host.runtime().latest_notice_seq();
            self.client_notice_cursors.insert(cid, cur);
        }

        let trimmed = raw.trim();
        if trimmed.is_empty() {
            if !interactive_blank {
                return ExecOutcome::Done(self.ok_reply(host, "", false, serde_json::json!({}), vec![]));
            }
            let mode = current_mode(host.runtime());
            if matches!(mode, "dialog" | "battle_victory") {
                return self.submit_from(host, "next", false, origin);
            }
            return self.submit_from(host, "look", false, origin);
        }

        let parsed = match parse_command(trimmed) {
            Ok(c) => {
                self.last_cmd_was_think = matches!(c, Command::Think(_));
                c
            }
            Err(mut msg) => {
                if std::mem::take(&mut self.last_cmd_was_think)
                    && msg.starts_with("unknown command")
                {
                    msg.push_str(" — if this was part of a 'think', wrap the thought in quotes");
                }
                return ExecOutcome::Done(self.err_reply_code(host, trimmed, "parse_error", &msg));
            }
        };

        // Socket policy for ALL TCP clients (including loopback, #193): disable `bugreport` (`gh` submit).
        if origin.is_socket() && matches!(parsed, Command::BugReport(_)) {
            return ExecOutcome::Done(self.err_reply_code(
                host,
                trimmed,
                "forbidden_over_socket",
                "bugreport is disabled over TCP sockets",
            ));
        }

        // Remote (non-loopback) capability gate (#193): block debug, load, core-replay, view, dump-state
        // unless `--allow-unsafe-remote` is explicitly enabled.
        if origin.is_remote()
            && !self.allow_unsafe_remote
            && matches!(
                parsed,
                Command::Load(_)
                    | Command::CoreReplay { .. }
                    | Command::View(_)
                    | Command::DumpState(_)
                    | Command::DebugTp { .. }
                    | Command::DebugHeal
                    | Command::DebugGod
                    | Command::DebugCash(_)
                    | Command::DebugItem { .. }
                    | Command::DebugEventInspect(_)
                    | Command::DebugEventSetState(_, _)
                    | Command::DebugBattle(_)
                    | Command::DebugScript(_)
                    | Command::DebugState
            )
        {
            return ExecOutcome::Done(self.err_reply_code(
                host,
                trimmed,
                "forbidden_remote",
                "command is disabled for remote TCP clients",
            ));
        }

        // In fair observation mode (`--obs fair` / `--fog`), debug, view, core-replay & dump-state are forbidden (#224, #230).
        if self.effective_obs_mode().is_fair()
            && matches!(
                parsed,
                Command::View(_)
                    | Command::CoreReplay { .. }
                    | Command::DumpState(_)
                    | Command::DebugTp { .. }
                    | Command::DebugHeal
                    | Command::DebugGod
                    | Command::DebugCash(_)
                    | Command::DebugItem { .. }
                    | Command::DebugEventInspect(_)
                    | Command::DebugEventSetState(_, _)
                    | Command::DebugBattle(_)
                    | Command::DebugScript(_)
                    | Command::DebugState
            )
        {
            return ExecOutcome::Done(self.err_reply_code(
                host,
                trimmed,
                "forbidden_in_fair_mode",
                "debug, view, core-replay and dump-state commands are disabled in fair observation mode",
            ));
        }

        self.apply_god_mode(host);

        match parsed {
            Command::Quit => {
                let mut reply = self.ok_reply(host, trimmed, false, serde_json::json!({}), vec![]);
                if let Some(obj) = reply.response.as_object_mut() {
                    obj.insert("bye".to_string(), serde_json::Value::Bool(true));
                }
                reply.bye = true;
                ExecOutcome::Quit(reply)
            }
            Command::Help(topic) => {
                let lang = host.runtime().ui.lang;
                let fair = self.effective_obs_mode().is_fair();
                match crate::text_mode::help::render_help(topic.as_deref(), lang, fair) {
                    Ok((lines, help_json)) => ExecOutcome::Done(self.ok_reply(
                        host,
                        trimmed,
                        false,
                        serde_json::json!({ "help": help_json }),
                        lines,
                    )),
                    Err(msg) => ExecOutcome::Done(self.err_reply_code(
                        host,
                        trimmed,
                        "invalid_args",
                        &msg,
                    )),
                }
            }
            Command::Look | Command::Map => ExecOutcome::Done(self.ok_reply(
                host,
                trimmed,
                true,
                serde_json::json!({}),
                vec![],
            )),

            Command::Walk { dir, steps } => {
                if current_mode(host.runtime()) != "explore" {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        &format!(
                            "cannot walk in mode '{}'",
                            current_mode(host.runtime())
                        ),
                    ));
                }
                let (start_scene, last_pos) = leader_scene_pos(host.runtime());
                self.active_task = Some(ActiveTask::WalkDir {
                    cmd: trimmed.to_string(),
                    dir,
                    target_steps: steps,
                    moved_steps: 0,
                    last_pos,
                    stall_ticks: 0,
                    start_scene,
                });
                ExecOutcome::Pending
            }

            Command::GotoEvent(ev_id) => {
                let fair = self.effective_obs_mode().is_fair();
                let rt = host.runtime();
                let ev_opt = rt
                    .game
                    .as_ref()
                    .filter(|g| g.world.is_event_in_current_scene(ev_id))
                    .and_then(|g| g.world.event(ev_id))
                    .filter(|e| e.state > 0 && e.vanish_time <= 0);
                let Some(ev) = ev_opt else {
                    let msg = if fair {
                        format!("event #{ev_id} is not visible in the current viewport")
                    } else {
                        format!("event #{ev_id} not active in current scene")
                    };
                    return ExecOutcome::Done(self.err_reply_code(
                        host,
                        trimmed,
                        "no_target",
                        &msg,
                    ));
                };
                let (tx, ty) = (ev.x, ev.y);
                let is_touch_exit = ev.trigger_mode >= 4;
                if fair {
                    let (cam_x, cam_y) = runtime_camera(rt);
                    let visible = event_visible_in_viewport(rt, ev, cam_x, cam_y)
                        || (ev.sprite_id == 0 && is_fair_visible_exit(rt, ev, cam_x, cam_y));
                    if !visible {
                        // Identical code/message as missing event so fair mode has no existence oracle (#233, #234).
                        return ExecOutcome::Done(self.err_reply_code(
                            host,
                            trimmed,
                            "no_target",
                            &format!("event #{ev_id} is not visible in the current viewport"),
                        ));
                    }
                }
                self.start_goto(
                    host,
                    trimmed,
                    (tx, ty),
                    format!("ev#{ev_id}"),
                    Some((ev_id, is_touch_exit)),
                    false,
                )
            }
            Command::GotoCoords(tx, ty) => {
                if self.effective_obs_mode().is_fair() {
                    let (cam_x, cam_y) = runtime_camera(host.runtime());
                    let on_screen = (cam_x..cam_x + 320).contains(&tx) && (cam_y..cam_y + 200).contains(&ty);
                    if !on_screen {
                        return ExecOutcome::Done(self.err_reply_code(
                            host,
                            trimmed,
                            "out_of_view",
                            &format!("target ({tx},{ty}) is outside the current 320x200 viewport"),
                        ));
                    }
                }
                self.start_goto(host, trimmed, (tx, ty), format!("({tx}, {ty})"), None, false)
            }
            Command::Talk(opt_ev) => {
                if let Some(ev_id) = opt_ev {
                    let fair = self.effective_obs_mode().is_fair();
                    let rt = host.runtime();
                    let ev_opt = rt
                        .game
                        .as_ref()
                        .filter(|g| g.world.is_event_in_current_scene(ev_id))
                        .and_then(|g| g.world.event(ev_id))
                        .filter(|e| e.state > 0 && e.vanish_time <= 0);
                    let Some(ev) = ev_opt else {
                        let msg = if fair {
                            format!("event #{ev_id} is not visible in the current viewport")
                        } else {
                            format!("event #{ev_id} not active in current scene")
                        };
                        return ExecOutcome::Done(self.err_reply_code(
                            host,
                            trimmed,
                            "no_target",
                            &msg,
                        ));
                    };
                    let (tx, ty) = (ev.x, ev.y);
                    if fair {
                        let (cam_x, cam_y) = runtime_camera(rt);
                        let visible = event_visible_in_viewport(rt, ev, cam_x, cam_y)
                            || (ev.sprite_id == 0 && is_fair_visible_exit(rt, ev, cam_x, cam_y));
                        if !visible {
                            // Identical code/message as missing event to avoid hidden-event oracle (#233, #234).
                            return ExecOutcome::Done(self.err_reply_code(
                                host,
                                trimmed,
                                "no_target",
                                &format!("event #{ev_id} is not visible in the current viewport"),
                            ));
                        }
                    }
                    self.start_goto(
                        host,
                        trimmed,
                        (tx, ty),
                        format!("ev#{ev_id}"),
                        Some((ev_id, false)),
                        true,
                    )
                } else {
                    self.trigger_interact_and_settle(host, trimmed, None)
                }
            }
            Command::Search => self.trigger_interact_and_settle(host, trimmed, None),

            Command::Wait(n) => {
                self.active_task = Some(ActiveTask::WaitTicks {
                    cmd: trimmed.to_string(),
                    remaining: n,
                    total: n,
                });
                ExecOutcome::Pending
            }

            Command::Next => {
                if !host.skip_rng_if_active() {
                    let act = host.runtime_mut().on_key(UiKey::Confirm);
                    if let Some(act) = act {
                        host.handle_ui_action(act);
                    }
                    host.service_host_cmds();
                }
                self.active_task = Some(ActiveTask::settle(
                    trimmed,
                    self.pace_ticks,
                    240,
                    true,
                    Vec::new(),
                ));
                ExecOutcome::Pending
            }
            Command::Continue => {
                self.active_task = Some(ActiveTask::ContinueDialog {
                    cmd: trimmed.to_string(),
                    cooldown: 0,
                    pace_ticks: self.pace_ticks,
                    max_steps: 80,
                    elapsed_ticks: 0,
                    max_ticks: 2400,
                    waited_cutscene: false,
                    last_logged_dialog: None,
                    log: Vec::new(),
                });
                ExecOutcome::Pending
            }
            Command::Yes | Command::No => {
                let is_yes = matches!(parsed, Command::Yes);
                if current_mode(host.runtime()) != "choice" {
                    return ExecOutcome::Done(self.err_reply_code(
                        host,
                        trimmed,
                        "wrong_mode",
                        "no yes/no choice active",
                    ));
                }
                // Advance any remaining pagination pages first so a multi-page choice prompt
                // is reliably answered by a single `yes` / `no` in both headless and GUI (#217).
                let mut page_guard = 0usize;
                while host.runtime().ui.view == UiView::Dialog
                    && host.runtime().ui.dialog.page + 1 < host.runtime().ui.dialog.total_pages
                    && page_guard < 32
                {
                    let _ = host.runtime_mut().on_key(UiKey::Confirm);
                    page_guard += 1;
                }
                host.runtime_mut().ui.dialog.choice_cursor = if is_yes { 0 } else { 1 };
                let act = host.runtime_mut().on_key(UiKey::Confirm);
                if let Some(act) = act {
                    host.handle_ui_action(act);
                }
                host.service_host_cmds();
                self.active_task = Some(ActiveTask::Settle {
                    cmd: trimmed.to_string(),
                    min_ticks: self.pace_ticks,
                    max_ticks: 240,
                    elapsed: 0,
                    show_events_on_explore: true,
                    use_item_snapshot: None,
                    expect_leave_choice: true,
                    talk_check: None,
                    interrupted_by: None,
                    log: Vec::new(),
                });
                ExecOutcome::Pending
            }
            Command::Close => {
                if host.skip_rng_if_active() {
                    self.active_task = Some(ActiveTask::Settle {
                        cmd: trimmed.to_string(),
                        min_ticks: self.pace_ticks.min(6),
                        max_ticks: 120,
                        elapsed: 0,
                        show_events_on_explore: true,
                        use_item_snapshot: None,
                        expect_leave_choice: false,
                        talk_check: None,
                        interrupted_by: None,
                        log: vec!["[close] skipped RNG cutscene".to_string()],
                    });
                    return ExecOutcome::Pending;
                }
                let ui_open = host.runtime().is_in_title() || host.runtime().ui.is_modal_open();
                if !ui_open {
                    return ExecOutcome::Done(self.make_reply(
                        host,
                        false,
                        trimmed,
                        false,
                        serde_json::json!({ "close": { "closed": false } }),
                        vec!["[close] nothing to close".to_string()],
                        None,
                        Some("nothing to close"),
                        Some("wrong_mode"),
                    ));
                }
                let act = host.runtime_mut().on_key(UiKey::Cancel);
                if let Some(act) = act {
                    host.handle_ui_action(act);
                }
                host.service_host_cmds();
                self.active_task = Some(ActiveTask::Settle {
                    cmd: trimmed.to_string(),
                    min_ticks: self.pace_ticks.min(6),
                    max_ticks: 120,
                    elapsed: 0,
                    show_events_on_explore: true,
                    use_item_snapshot: None,
                    expect_leave_choice: false,
                    talk_check: None,
                    interrupted_by: None,
                    log: Vec::new(),
                });
                ExecOutcome::Pending
            }

            Command::OpenMenu => {
                if current_mode(host.runtime()) != "explore" {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        &format!("cannot open menu in mode '{}'", current_mode(host.runtime())),
                    ));
                }
                {
                    let rt = host.runtime_mut();
                    // Clear any stale overworld toast so it does not cover the freshly opened menu.
                    rt.ui.toast_text = None;
                    rt.toast_ticks = 0;
                    // Preserve root_cursor across menu open/close (SDLPAL gMenuSel, #274).
                    rt.ui.set_view(UiView::RootMenu);
                    rt.compositor.mark_ui_dirty();
                }
                ExecOutcome::Done(self.ok_reply(
                    host,
                    trimmed,
                    false,
                    serde_json::json!({}),
                    vec!["[menu] opened".to_string()],
                ))
            }
            Command::Key(uk) => self.dispatch_ui_key(host, trimmed, uk),
            Command::UiShortcut(uk) => {
                let ui_open = host.runtime().is_in_title() || host.runtime().ui.is_modal_open();
                if !ui_open {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        "no UI/menu active (use sw/nw/ne/se to walk, or 'key <k>')",
                    ));
                }
                self.dispatch_ui_key(host, trimmed, uk)
            }
            Command::View(v) => {
                if let Err(e) = apply_named_view_rt(host.runtime_mut(), &v) {
                    return ExecOutcome::Done(self.err_reply(host, trimmed, &e));
                }
                host.service_host_cmds();
                ExecOutcome::Done(self.ok_reply(
                    host,
                    trimmed,
                    false,
                    serde_json::json!({}),
                    vec![format!("[view] switched to {v}")],
                ))
            }
            Command::Click { x, y } => {
                if current_mode(host.runtime()) == "explore" {
                    let (start_scene, cur_pos) = leader_scene_pos(host.runtime());
                    let (wx, wy) = logical_to_world(host.runtime(), x, y);
                    host.on_click_ripple(wx, wy);
                    host.runtime_mut().tick(TickInput {
                        click: Some((wx, wy)),
                        ..TickInput::IDLE
                    });
                    host.service_host_cmds();
                    let walking = host
                        .runtime()
                        .game
                        .as_ref()
                        .map(|g| g.is_auto_walking())
                        .unwrap_or(false);
                    if walking {
                        self.active_task = Some(ActiveTask::Goto {
                            cmd: trimmed.to_string(),
                            target_xy: (wx, wy),
                            click_xy: (wx, wy),
                            target_ev_id: None,
                            target_is_touch_exit: false,
                            talk_after: false,
                            start_scene,
                            steps_taken: 0,
                            last_pos: cur_pos,
                            stall_ticks: 0,
                            max_ticks: 800,
                        });
                    } else {
                        self.active_task = Some(ActiveTask::settle(
                            trimmed,
                            self.pace_ticks.min(4),
                            90,
                            true,
                            vec![format!("[click] ({x}, {y}) -> world ({wx}, {wy})")],
                        ));
                    }
                    return ExecOutcome::Pending;
                }
                host.render_settled_frame();
                let act = host.runtime_mut().on_pointer(PointerEvent {
                    kind: PointerKind::Down,
                    x,
                    y,
                });
                if let Some(act) = act {
                    host.handle_ui_action(act);
                }
                host.service_host_cmds();
                self.active_task = Some(ActiveTask::settle(
                    trimmed,
                    self.pace_ticks.min(4),
                    60,
                    false,
                    vec![format!("[click] ({x}, {y})")],
                ));
                ExecOutcome::Pending
            }

            Command::BattleCancelAuto => {
                let cancelled = host
                    .runtime_mut()
                    .game
                    .as_mut()
                    .map(|g| g.battle_cancel_auto())
                    .unwrap_or(false);
                if !cancelled {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        "auto-battle is not active",
                    ));
                }
                self.active_task = Some(ActiveTask::settle(
                    trimmed,
                    0,
                    180,
                    false,
                    vec!["[battle] auto-battle cancelled".to_string()],
                ));
                ExecOutcome::Pending
            }
            Command::BattleAttack { slot } => {
                let t = slot.unwrap_or_else(|| first_alive_enemy(host.runtime()));
                self.submit_battle_cmd(host, trimmed, BattleCmd::Attack { target: t })
            }
            Command::BattleMagic {
                magic,
                slot,
                caster,
            } => {
                let mode = current_mode(host.runtime());
                let prefer_role = if mode == "battle" {
                    host.runtime().game.as_ref().and_then(|g| {
                        let b = g.battle.as_ref().filter(|b| b.active)?;
                        let h_idx = b.prompting?;
                        b.heroes.get(h_idx).map(|h| h.role)
                    })
                } else {
                    caster.and_then(|ci| party_idx_to_role_id(host.runtime(), ci))
                };
                let magic_id = match resolve_magic_ref(host.runtime(), &magic, prefer_role) {
                    Ok(id) => id,
                    Err((code, msg)) => {
                        return ExecOutcome::Done(self.err_reply_code(host, trimmed, code, &msg));
                    }
                };
                if mode != "battle" {
                    // Field magic casting in `explore` mode (`magic <id|name> [target_party_idx] [caster_party_idx]`, #234, #276).
                    if mode == "cutscene" || host.runtime().game.as_ref().is_some_and(|g| g.is_busy()) {
                        return ExecOutcome::Done(self.err_reply_code(
                            host,
                            trimmed,
                            "busy",
                            "cannot cast field magic while a script/cutscene is running",
                        ));
                    }
                    if mode != "explore" {
                        return ExecOutcome::Done(self.err_reply_code(
                            host,
                            trimmed,
                            "wrong_mode",
                            &format!("cannot cast magic in mode '{mode}' (only in battle or explore)"),
                        ));
                    }
                    let (caster_idx, caster_role_id, cost_mp, usable_field, has_mp) = {
                        let Some(g) = host.runtime().game.as_ref() else {
                            return ExecOutcome::Done(self.err_reply(host, trimmed, "game not initialized"));
                        };
                        let Some(md) = g.data.magics.get(&magic_id) else {
                            return ExecOutcome::Done(self.err_reply_code(
                                host,
                                trimmed,
                                "magic_not_known",
                                &format!("unknown magic #{magic_id}"),
                            ));
                        };
                        let chosen = if let Some(ci) = caster {
                            let Some(pm) = g.world.party.get(ci) else {
                                return ExecOutcome::Done(self.err_reply_code(
                                    host,
                                    trimmed,
                                    "invalid_target",
                                    &format!("caster party index {ci} out of range"),
                                ));
                            };
                            let rid = pm.role_id as usize;
                            let knows = g.world.roles.get(rid).is_some_and(|r| r.magics.contains(&magic_id));
                            if !knows {
                                return ExecOutcome::Done(self.err_reply_code(
                                    host,
                                    trimmed,
                                    "magic_not_known",
                                    &format!("party member #{ci} has not learned magic #{magic_id}"),
                                ));
                            }
                            (ci, rid)
                        } else {
                            let Some((ci, rid)) = g
                                .world
                                .party
                                .iter()
                                .enumerate()
                                .find_map(|(i, pm)| {
                                    let rid = pm.role_id as usize;
                                    let r = g.world.roles.get(rid)?;
                                    (r.hp > 0 && r.magics.contains(&magic_id)).then_some((i, rid))
                                })
                            else {
                                return ExecOutcome::Done(self.err_reply_code(
                                    host,
                                    trimmed,
                                    "magic_not_known",
                                    &format!("no living party member has learned magic #{magic_id}"),
                                ));
                            };
                            (ci, rid)
                        };
                        let cur_mp = g.world.roles.get(chosen.1).map(|r| r.mp).unwrap_or(0);
                        (chosen.0, chosen.1, md.cost_mp, md.usable_outside_battle, cur_mp >= md.cost_mp)
                    };
                    if !usable_field {
                        return ExecOutcome::Done(self.err_reply_code(
                            host,
                            trimmed,
                            "magic_not_field",
                            &format!("magic #{magic_id} can only be cast in battle"),
                        ));
                    }
                    if !has_mp {
                        return ExecOutcome::Done(self.err_reply_code(
                            host,
                            trimmed,
                            "insufficient_mp",
                            &format!("not enough MP to cast magic #{magic_id} (requires {cost_mp} MP)"),
                        ));
                    }
                    let target_role_id = match slot {
                        Some(idx) => match party_idx_to_role_id(host.runtime(), idx) {
                            Some(rid) => Some(rid),
                            None => {
                                return ExecOutcome::Done(self.err_reply_code(
                                    host,
                                    trimmed,
                                    "invalid_target",
                                    &format!("party index {idx} out of range"),
                                ));
                            }
                        },
                        None => None,
                    };
                    let ok = host
                        .runtime_mut()
                        .game
                        .as_mut()
                        .map(|g| g.cast_field_magic(caster_role_id, magic_id, target_role_id))
                        .unwrap_or(false);
                    host.runtime_mut().route_cmds();
                    host.runtime_mut().project();
                    host.service_host_cmds();
                    if !ok {
                        return ExecOutcome::Done(self.err_reply_code(
                            host,
                            trimmed,
                            "magic_refused",
                            &format!("magic #{magic_id} had no effect / was refused"),
                        ));
                    }
                    let art = serde_json::json!({
                        "field_magic": {
                            "caster_idx": caster_idx,
                            "magic_id": magic_id,
                            "target_idx": slot,
                            "mp_cost": cost_mp,
                        }
                    });
                    return ExecOutcome::Done(self.ok_reply(
                        host,
                        trimmed,
                        false,
                        art,
                        vec![format!("[magic] cast #{magic_id} (MP -{cost_mp})")],
                    ));
                }

                // Battle mode: validate that the currently prompting hero actually knows `magic_id` (#175).
                let knows_magic = host
                    .runtime()
                    .game
                    .as_ref()
                    .and_then(|g| {
                        let b = g.battle.as_ref().filter(|b| b.active)?;
                        let h_idx = b.prompting?;
                        let role_id = b.heroes.get(h_idx)?.role;
                        g.world
                            .roles
                            .get(role_id)
                            .map(|r| r.magics.contains(&magic_id))
                    });
                if knows_magic == Some(false) {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        &format!("active hero has not learned magic #{magic_id}"),
                    ));
                }
                let t = slot.unwrap_or_else(|| default_magic_target(host.runtime(), magic_id));
                self.submit_battle_cmd(
                    host,
                    trimmed,
                    BattleCmd::Magic {
                        magic: magic_id,
                        target: t,
                    },
                )
            }
            Command::BattleItem { item, slot } => {
                let item_id = match resolve_item_ref(host.runtime(), &item, true) {
                    Ok(id) => id,
                    Err((code, msg)) => {
                        return ExecOutcome::Done(self.err_reply_code(host, trimmed, code, &msg));
                    }
                };
                if current_mode(host.runtime()) == "battle" {
                    if let Err((code, msg)) = validate_battle_item(host.runtime(), item_id) {
                        return ExecOutcome::Done(self.err_reply_code(host, trimmed, code, &msg));
                    }
                }
                let bc = match slot {
                    Some(t) => BattleCmd::ItemTarget {
                        item: item_id,
                        target: t,
                    },
                    None => BattleCmd::Item { item: item_id },
                };
                self.submit_battle_cmd(host, trimmed, bc)
            }
            Command::BattleCoop { slot } => {
                let t = slot.unwrap_or_else(|| first_alive_enemy(host.runtime()));
                self.submit_battle_cmd(host, trimmed, BattleCmd::Coop { target: t })
            }
            Command::BattleDefend => self.submit_battle_cmd(host, trimmed, BattleCmd::Defend),
            Command::BattleFlee => self.submit_battle_cmd(host, trimmed, BattleCmd::Flee),
            Command::BattleAuto => self.submit_battle_cmd(host, trimmed, BattleCmd::Auto),

            other => self.submit_rpg_io(host, trimmed, origin, other),
        }
    }
}
