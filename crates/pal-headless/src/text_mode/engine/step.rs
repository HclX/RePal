//! Multi-tick `ActiveTask` state machine stepped once per 20 ms logic tick.

use super::*;

impl TextEngine {
    /// Advance the active multi-tick task by one 20 ms tick.
    ///
    /// Returns `(TickInput to feed to PalRuntime::tick, Option<TextReply> once completed)`.
    pub fn step_tick<H: TextHost>(&mut self, host: &mut H) -> (TickInput, Option<TextReply>) {
        self.apply_god_mode(host);
        let Some(task) = self.active_task.as_mut() else {
            return (TickInput::IDLE, None);
        };
        let mode = current_mode(host.runtime());
        let (cur_scene, cur_pos) = leader_scene_pos(host.runtime());

        match task {
            ActiveTask::WalkDir {
                cmd,
                dir,
                target_steps,
                moved_steps,
                last_pos,
                stall_ticks,
                start_scene,
            } => {
                if cur_pos != *last_pos {
                    *moved_steps += 1;
                    *last_pos = cur_pos;
                    *stall_ticks = 0;
                } else {
                    *stall_ticks += 1;
                }
                let blocked = *stall_ticks > MOVE_TICKS + 3;
                let scene_changed = cur_scene != *start_scene;
                if *moved_steps >= *target_steps || scene_changed || mode != "explore" || blocked {
                    let cmd_s = cmd.clone();
                    let d_name = dir_code(*dir);
                    let moved = *moved_steps;
                    let target = *target_steps;
                    let log = vec![format!("[walk] {d_name} {moved}/{target} 步")];
                    let interrupted = if scene_changed {
                        Some("scene_change")
                    } else if matches!(mode, "battle" | "battle_anim") {
                        Some("battle")
                    } else if matches!(mode, "dialog" | "choice" | "cutscene") {
                        Some("dialog")
                    } else {
                        None
                    };
                    if mode == "cutscene" || mode == "battle_anim" || scene_changed {
                        self.active_task = Some(ActiveTask::Settle {
                            cmd: cmd_s,
                            min_ticks: 0,
                            max_ticks: if mode == "battle_anim" { 240 } else { 120 },
                            elapsed: 0,
                            show_events_on_explore: true,
                            use_item_snapshot: None,
                            expect_leave_choice: false,
                            talk_check: None,
                            interrupted_by: interrupted,
                            log,
                        });
                        return (TickInput::IDLE, None);
                    }
                    let walk_art = serde_json::json!({
                        "walk": {
                            "requested": target,
                            "moved": moved,
                            "blocked": blocked,
                        }
                    });
                    self.active_task = None;
                    let reply = if moved == 0 && blocked && mode == "explore" {
                        self.make_reply(
                            host,
                            false,
                            &cmd_s,
                            true,
                            walk_art,
                            log,
                            None,
                            Some("movement blocked (0 steps moved)"),
                            Some("blocked"),
                        )
                    } else {
                        self.make_reply(host, true, &cmd_s, true, walk_art, log, interrupted, None, None)
                    };
                    return (TickInput::IDLE, Some(reply));
                }
                let input = TickInput {
                    held_dir: *dir as i8,
                    ..TickInput::IDLE
                };
                (input, None)
            }

            ActiveTask::Goto {
                cmd,
                target_xy,
                click_xy,
                target_ev_id,
                target_is_touch_exit,
                talk_after,
                start_scene,
                steps_taken,
                last_pos,
                stall_ticks,
                max_ticks,
            } => {
                if cur_pos != *last_pos {
                    *steps_taken += 1;
                    *last_pos = cur_pos;
                    *stall_ticks = 0;
                } else {
                    *stall_ticks += 1;
                }
                let walking = host
                    .runtime()
                    .game
                    .as_ref()
                    .map(|g| g.is_auto_walking())
                    .unwrap_or(false);
                let move_cd = host
                    .runtime()
                    .game
                    .as_ref()
                    .map(|g| g.move_cooldown())
                    .unwrap_or(0);
                let scene_changed = cur_scene != *start_scene;
                let arrived = !walking && move_cd == 0;
                let timed_out = *max_ticks == 0 || *stall_ticks > MOVE_TICKS + 8;

                if scene_changed || mode != "explore" || arrived || timed_out {
                    let cmd_s = cmd.clone();
                    let (tx, ty) = *target_xy;
                    let (cx, cy) = *click_xy;
                    let target_ev = *target_ev_id;
                    let is_touch_exit = *target_is_touch_exit;
                    let do_talk = *talk_after && mode == "explore" && !scene_changed;
                    let start_sc = *start_scene;
                    let steps = *steps_taken;
                    let mut log = vec![format!("[goto] walked {steps} steps toward ({tx},{ty})")];
                    if do_talk {
                        // Counter/table fallback (#244): if the leader isn't facing the target and
                        // the tile toward the target is blocked (counter or solid NPC), send 1 tick
                        // of standard `HeldDir` to face in-place before pressing `Interact`.
                        let need_turn = host.runtime().game.as_ref().and_then(|g| {
                            let lead = g.world.leader();
                            let face_dir = pal_core::isometric::direction_between(lead.x, lead.y, tx, ty);
                            if lead.dir == face_dir {
                                return None;
                            }
                            let (hx, hy, hh) = pal_core::isometric::world_to_half_tile(lead.x, lead.y);
                            let (nx, ny, nh) = pal_core::isometric::step_half_tile(hx, hy, hh, face_dir);
                            let tile_blocked = g.base_grid().is_blocked(nx, ny, nh)
                                || g.world.current_event_ids().into_iter().any(|id| {
                                    g.world.event(id).is_some_and(|e| {
                                        e.state >= 2
                                            && e.vanish_time <= 0
                                            && pal_core::isometric::world_to_half_tile(e.x, e.y) == (nx, ny, nh)
                                    })
                                });
                            tile_blocked.then_some(face_dir)
                        });
                        if let Some(face_dir) = need_turn {
                            self.active_task = Some(ActiveTask::TurnAndTalk {
                                cmd: cmd_s,
                                dir: face_dir,
                                target_xy: (tx, ty),
                                start_scene: start_sc,
                                turned: false,
                                log,
                            });
                            return (
                                TickInput {
                                    held_dir: face_dir as i8,
                                    ..TickInput::IDLE
                                },
                                None,
                            );
                        }
                        let notice_seq = host.runtime().latest_notice_seq();
                        let snapshot = UseItemSnapshot::capture(host.runtime(), 0);
                        self.active_task = Some(ActiveTask::Settle {
                            cmd: cmd_s,
                            min_ticks: self.pace_ticks.min(6).max(1),
                            max_ticks: 180,
                            elapsed: 0,
                            show_events_on_explore: true,
                            use_item_snapshot: None,
                            expect_leave_choice: false,
                            talk_check: Some(TalkCheck {
                                target_xy: (tx, ty),
                                start_scene: start_sc,
                                start_notice_seq: notice_seq,
                                snapshot,
                            }),
                            interrupted_by: None,
                            log,
                        });
                        return (
                            TickInput {
                                interact: true,
                                ..TickInput::IDLE
                            },
                            None,
                        );
                    }
                    let interrupted = if scene_changed {
                        Some("scene_change")
                    } else if matches!(mode, "battle" | "battle_anim") {
                        Some("battle")
                    } else if matches!(mode, "dialog" | "choice" | "cutscene") {
                        Some("dialog")
                    } else {
                        None
                    };
                    if mode == "cutscene" || mode == "battle_anim" || scene_changed {
                        self.active_task = Some(ActiveTask::Settle {
                            cmd: cmd_s,
                            min_ticks: 0,
                            max_ticks: if mode == "battle_anim" { 240 } else { 120 },
                            elapsed: 0,
                            show_events_on_explore: true,
                            use_item_snapshot: None,
                            expect_leave_choice: false,
                            talk_check: None,
                            interrupted_by: interrupted,
                            log,
                        });
                        return (TickInput::IDLE, None);
                    }
                    let dist = (cur_pos.0 - tx).abs() + (cur_pos.1 - ty).abs() * 2;
                    let click_dist = (cur_pos.0 - cx).abs() + (cur_pos.1 - cy).abs() * 2;
                    // When `goto <npc>` intentionally offset the walk goal outside the NPC's 32px
                    // click circle (#244, #271), arriving at `click_xy` (typically dist=48..80)
                    // counts as reached & adjacent rather than false-failing `unreachable`.
                    // However, a touch exit (`trigger_mode >= 4`) is NOT reached if we stayed in
                    // `explore` outside it (e.g. deflected by closed double doors #138/#139, #278).
                    let offset_target = (cx, cy) != (tx, ty);
                    let reached = if is_touch_exit {
                        dist <= 16
                    } else {
                        dist <= 48 || (offset_target && click_dist <= 32 && dist <= 96)
                    };
                    let adjacent = offset_target && reached;
                    let fair = self.effective_obs_mode().is_fair();
                    let blockers = if !reached || offset_target {
                        find_goto_blockers(
                            host.runtime(),
                            cur_pos,
                            (tx, ty),
                            target_ev,
                            fair,
                            offset_target && !reached,
                            is_touch_exit,
                        )
                    } else {
                        Vec::new()
                    };
                    let blocker_ids: Vec<u16> = blockers.iter().map(|(id, _, _)| *id).collect();
                    let blocker_hint = format_goto_blockers_hint(&blockers);
                    let disconnected = !reached
                        && mode == "explore"
                        && blockers.is_empty()
                        && is_target_region_disconnected(
                            host.runtime(),
                            cur_pos,
                            (tx, ty),
                            is_touch_exit,
                        );
                    let reachable_exits = if disconnected {
                        find_reachable_exits_and_stairs(host.runtime(), cur_pos, fair)
                    } else {
                        Vec::new()
                    };
                    let reachable_hops = if disconnected {
                        find_reachable_hops(host.runtime(), fair)
                    } else {
                        Vec::new()
                    };
                    if reached && offset_target && !blocker_hint.is_empty() {
                        log.push(format!("[goto] {blocker_hint}"));
                    }
                    let goto_art = serde_json::json!({
                        "goto": {
                            "reached": reached,
                            "adjacent": adjacent,
                            "final_xy": [cur_pos.0, cur_pos.1],
                            "dist": dist,
                            "blockers": blocker_ids,
                            "disconnected": disconnected,
                            "reachable_exits": reachable_exits
                                .iter()
                                .map(|(id, dir)| serde_json::json!({"id": id, "dir": dir}))
                                .collect::<Vec<_>>(),
                            "reachable_hops": reachable_hops
                                .iter()
                                .map(|(id, dir)| serde_json::json!({"id": id, "dir": dir}))
                                .collect::<Vec<_>>(),
                        }
                    });
                    self.active_task = None;
                    let reply = if !reached && mode == "explore" {
                        let msg = if !blocker_hint.is_empty() {
                            format!(
                                "could not reach ({tx},{ty}); stopped at ({},{}) (dist={dist}); {blocker_hint}",
                                cur_pos.0, cur_pos.1
                            )
                        } else if disconnected {
                            let base = format!(
                                "could not reach ({tx},{ty}); stopped at ({},{}) (dist={dist}); target not connected to your area (different floor/region)",
                                cur_pos.0, cur_pos.1
                            );
                            let list = |v: &[(u16, String)]| {
                                v.iter().map(|(id, dir)| format!("#{id} ({dir})")).collect::<Vec<_>>().join(", ")
                            };
                            let mut msg = base;
                            if !reachable_exits.is_empty() {
                                msg = format!("{msg}; reachable exits/stairs: {}", list(&reachable_exits));
                            }
                            if !reachable_hops.is_empty() {
                                msg = format!(
                                    "{msg}; reachable touch triggers that move the party (e.g. lotus pads): {} — goto one to ride it",
                                    list(&reachable_hops)
                                );
                            }
                            msg
                        } else {
                            format!(
                                "could not reach ({tx},{ty}); stopped at ({},{}) (dist={dist})",
                                cur_pos.0, cur_pos.1
                            )
                        };
                        self.make_reply(
                            host,
                            false,
                            &cmd_s,
                            true,
                            goto_art,
                            log,
                            None,
                            Some(&msg),
                            Some("unreachable"),
                        )
                    } else {
                        self.make_reply(host, true, &cmd_s, true, goto_art, log, interrupted, None, None)
                    };
                    return (TickInput::IDLE, Some(reply));
                }
                *max_ticks -= 1;
                (TickInput::IDLE, None)
            }

            ActiveTask::TurnAndTalk {
                cmd,
                dir,
                target_xy,
                start_scene,
                turned,
                log,
            } => {
                if !*turned {
                    *turned = true;
                    return (
                        TickInput {
                            held_dir: *dir as i8,
                            ..TickInput::IDLE
                        },
                        None,
                    );
                }
                let cmd_s = cmd.clone();
                let (tx, ty) = *target_xy;
                let start_sc = *start_scene;
                let out_log = std::mem::take(log);
                let notice_seq = host.runtime().latest_notice_seq();
                let snapshot = UseItemSnapshot::capture(host.runtime(), 0);
                self.active_task = Some(ActiveTask::Settle {
                    cmd: cmd_s,
                    min_ticks: self.pace_ticks.min(6).max(1),
                    max_ticks: 180,
                    elapsed: 0,
                    show_events_on_explore: true,
                    use_item_snapshot: None,
                    expect_leave_choice: false,
                    talk_check: Some(TalkCheck {
                        target_xy: (tx, ty),
                        start_scene: start_sc,
                        start_notice_seq: notice_seq,
                        snapshot,
                    }),
                    interrupted_by: None,
                    log: out_log,
                });
                (
                    TickInput {
                        interact: true,
                        ..TickInput::IDLE
                    },
                    None,
                )
            }

            ActiveTask::ContinueDialog {
                cmd,
                cooldown,
                pace_ticks,
                max_steps,
                elapsed_ticks,
                max_ticks,
                waited_cutscene,
                last_logged_dialog,
                log,
            } => {
                if *max_steps == 0 || *elapsed_ticks >= *max_ticks {
                    let cmd_s = cmd.clone();
                    let mut out_log = std::mem::take(log);
                    if *elapsed_ticks >= *max_ticks {
                        out_log.push("[continue] timed out waiting to settle".to_string());
                    }
                    self.active_task = None;
                    let reply = self.ok_reply(
                        host,
                        &cmd_s,
                        current_mode(host.runtime()) == "explore",
                        serde_json::json!({}),
                        out_log,
                    );
                    return (TickInput::IDLE, Some(reply));
                }
                *elapsed_ticks += 1;
                if *cooldown > 0 {
                    *cooldown -= 1;
                    return (TickInput::IDLE, None);
                }
                if host.skip_rng_if_active() {
                    *cooldown = *pace_ticks;
                    return (TickInput::IDLE, None);
                }
                match mode {
                    "dialog" | "battle_victory" => {
                        if host.runtime().ui.view == UiView::Dialog {
                            let sp = host.runtime().ui.dialog.speaker.clone();
                            let tx = host.runtime().ui.dialog.text.clone();
                            // Log each dialog once (on first encounter), not once per pagination page (#217).
                            if last_logged_dialog.as_ref() != Some(&(sp.clone(), tx.clone())) {
                                *last_logged_dialog = Some((sp.clone(), tx.clone()));
                                if sp.is_empty() {
                                    log.push(format!("「{tx}」"));
                                } else {
                                    log.push(format!("{sp}: 「{tx}」"));
                                }
                            }
                        } else if mode == "battle_victory" {
                            *last_logged_dialog = None;
                            let bv = &host.runtime().ui.battle_victory;
                            log.push(format!(
                                "[battle_victory] 经验 +{} | 金钱 +{}",
                                bv.exp, bv.cash
                            ));
                        }
                        let act = host.runtime_mut().on_key(UiKey::Confirm);
                        if let Some(act) = act {
                            host.handle_ui_action(act);
                        }
                        host.service_host_cmds();
                        *cooldown = *pace_ticks;
                        *max_steps -= 1;
                        (TickInput::IDLE, None)
                    }
                    "cutscene" | "battle_anim" => {
                        // Keep advancing ticks until the next dialog/choice/explore settles,
                        // bounded by `elapsed_ticks < max_ticks`.
                        *waited_cutscene = true;
                        (TickInput::IDLE, None)
                    }
                    _ => {
                        let cmd_s = cmd.clone();
                        let mut out_log = std::mem::take(log);
                        if out_log.is_empty() {
                            if *waited_cutscene {
                                out_log.push("[continue] cutscene finished".to_string());
                            } else {
                                out_log.push("[continue] no dialogue/victory prompt active".to_string());
                            }
                        }
                        self.active_task = None;
                        let reply = self.ok_reply(
                            host,
                            &cmd_s,
                            mode == "explore",
                            serde_json::json!({}),
                            out_log,
                        );
                        (TickInput::IDLE, Some(reply))
                    }
                }
            }

            ActiveTask::Settle {
                cmd,
                min_ticks,
                max_ticks,
                elapsed,
                show_events_on_explore,
                use_item_snapshot,
                expect_leave_choice,
                talk_check,
                interrupted_by,
                log,
            } => {
                let at_decision_point = matches!(
                    mode,
                    "explore"
                        | "dialog"
                        | "choice"
                        | "battle"
                        | "battle_victory"
                        | "shop"
                        | "menu"
                        | "title"
                        | "modal"
                );
                if (*elapsed >= *min_ticks && at_decision_point) || *elapsed >= *max_ticks {
                    let cmd_s = cmd.clone();
                    let show_ev = *show_events_on_explore && mode == "explore";
                    let use_snap = use_item_snapshot.take();
                    let check_choice = *expect_leave_choice;
                    let check_talk = talk_check.take();
                    let interrupted = *interrupted_by;
                    let out_log = std::mem::take(log);
                    self.active_task = None;

                    if check_choice && mode == "choice" {
                        let reply = self.make_reply(
                            host,
                            false,
                            &cmd_s,
                            show_ev,
                            serde_json::json!({}),
                            out_log,
                            None,
                            Some("choice prompt is still pending"),
                            Some("still_pending"),
                        );
                        return (TickInput::IDLE, Some(reply));
                    }
                    if let Some(snap) = use_snap {
                        let script_success = host
                            .runtime()
                            .game
                            .as_ref()
                            .and_then(|g| g.last_item_result)
                            .unwrap_or(true);
                        let effects = snap.diff_effects(host.runtime(), mode);
                        let use_art = serde_json::json!({
                            "use": {
                                "item_id": snap.item_id,
                                "script_success": script_success,
                                "effects": effects,
                            }
                        });
                        // Only treat as `item_refused` when the script reported failure AND produced
                        // zero observable world effects (so chained SDLPAL `0x0081` items like 破天锤
                        // that smash statues return `ok: true` with `effects: ["event_changed"]`, #234).
                        if !script_success && effects.is_empty() {
                            let reply = self.make_reply(
                                host,
                                false,
                                &cmd_s,
                                show_ev,
                                use_art,
                                out_log,
                                None,
                                Some("item could not be used (no effect / refused)"),
                                Some("item_refused"),
                            );
                            return (TickInput::IDLE, Some(reply));
                        }
                        let reply = self.make_reply(
                            host,
                            true,
                            &cmd_s,
                            show_ev,
                            use_art,
                            out_log,
                            interrupted,
                            None,
                            None,
                        );
                        return (TickInput::IDLE, Some(reply));
                    }
                    if let Some(tc) = check_talk {
                        let (tx, ty) = tc.target_xy;
                        let (now_sc, now_pos) = leader_scene_pos(host.runtime());
                        let notice_emitted = host.runtime().latest_notice_seq() > tc.start_notice_seq;
                        let effects = tc.snapshot.diff_effects(host.runtime(), mode);
                        // Interactions that silently mutate world state (e.g. opening double doors
                        // #138/#139 via `s_0014` -> `event_set_state(-1, 0)`) are genuine successes (#278).
                        if mode == "explore"
                            && now_sc == tc.start_scene
                            && !notice_emitted
                            && effects.is_empty()
                        {
                            let dist = (now_pos.0 - tx).abs() + (now_pos.1 - ty).abs() * 2;
                            let code = if dist > 48 { "unreachable" } else { "no_target" };
                            let msg = if dist > 48 {
                                format!(
                                    "could not reach ({tx},{ty}) to talk; stopped at ({},{}) (dist={dist})",
                                    now_pos.0, now_pos.1
                                )
                            } else {
                                "interaction triggered nothing".to_string()
                            };
                            let art = serde_json::json!({
                                "goto": {
                                    "reached": dist <= 48,
                                    "final_xy": [now_pos.0, now_pos.1],
                                    "dist": dist,
                                }
                            });
                            let reply = self.make_reply(
                                host,
                                false,
                                &cmd_s,
                                true,
                                art,
                                out_log,
                                None,
                                Some(&msg),
                                Some(code),
                            );
                            return (TickInput::IDLE, Some(reply));
                        }
                    }

                    let reply = self.make_reply(
                        host,
                        true,
                        &cmd_s,
                        show_ev,
                        serde_json::json!({}),
                        out_log,
                        interrupted,
                        None,
                        None,
                    );
                    return (TickInput::IDLE, Some(reply));
                }
                *elapsed += 1;
                (TickInput::IDLE, None)
            }

            ActiveTask::WaitTicks {
                cmd,
                remaining,
                total,
            } => {
                if *remaining == 0 {
                    let cmd_s = cmd.clone();
                    let n = *total;
                    self.active_task = None;
                    let reply = self.ok_reply(
                        host,
                        &cmd_s,
                        false,
                        serde_json::json!({}),
                        vec![format!("[tick] advanced {n} ticks")],
                    );
                    return (TickInput::IDLE, Some(reply));
                }
                *remaining -= 1;
                (TickInput::IDLE, None)
            }
        }
    }
}
