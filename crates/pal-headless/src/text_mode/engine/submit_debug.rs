//! `/debug` command validation, tainting, and execution handlers for `TextEngine`.

use super::*;

impl TextEngine {
    pub(super) fn submit_debug<H: TextHost>(
        &mut self,
        host: &mut H,
        trimmed: &str,
        parsed: Command,
    ) -> ExecOutcome {
        match parsed {
            Command::DebugTp { scene_id, coords } => {
                let act = pal_core::game::replay_rec::DebugAction::Tp { scene_id, coords };
                if let Err(e) = host
                    .runtime()
                    .game
                    .as_ref()
                    .ok_or_else(|| "game not initialized".to_string())
                    .and_then(|g| g.validate_debug_action(&act))
                {
                    return ExecOutcome::Done(self.err_reply(host, trimmed, &e));
                }
                host.on_session_transition(true, true);
                let res = {
                    let rt = host.runtime_mut();
                    let r = if let Some(g) = rt.game.as_mut() {
                        g.set_debug_mode(true);
                        g.apply_debug_action(&act)
                    } else {
                        Err("game not initialized".to_string())
                    };
                    rt.ui.set_view(UiView::None);
                    rt.world.fbp_override = None;
                    rt.route_cmds();
                    r
                };
                if let Err(e) = res {
                    return ExecOutcome::Done(self.err_reply(host, trimmed, &e));
                }
                host.service_host_cmds();
                {
                    let rt = host.runtime_mut();
                    rt.route_cmds();
                    rt.project();
                }
                self.active_task = Some(ActiveTask::settle(
                    trimmed,
                    0,
                    60,
                    true,
                    vec![format!("[/tp] scene #{scene_id}")],
                ));
                ExecOutcome::Pending
            }

            Command::DebugHeal => {
                if is_debug_mutation_busy(host) {
                    return ExecOutcome::Done(self.err_reply_code(
                        host,
                        trimmed,
                        "busy",
                        "cannot run /heal while a script or battle is active",
                    ));
                }
                let res = {
                    let rt = host.runtime_mut();
                    let r = if let Some(g) = rt.game.as_mut() {
                        g.set_debug_mode(true);
                        g.apply_debug_action(&pal_core::game::replay_rec::DebugAction::Heal)
                    } else {
                        Err("game not initialized".to_string())
                    };
                    rt.route_cmds();
                    rt.project();
                    r
                };
                match res {
                    Ok(msg) => ExecOutcome::Done(self.ok_reply(
                        host,
                        trimmed,
                        false,
                        serde_json::json!({}),
                        vec![msg],
                    )),
                    Err(e) => ExecOutcome::Done(self.err_reply(host, trimmed, &e)),
                }
            }

            Command::DebugGod => {
                if is_debug_mutation_busy(host) {
                    return ExecOutcome::Done(self.err_reply_code(
                        host,
                        trimmed,
                        "busy",
                        "cannot toggle /god while a script or battle is active",
                    ));
                }
                let next_on = !self.god_mode;
                let res = {
                    let rt = host.runtime_mut();
                    let r = if let Some(g) = rt.game.as_mut() {
                        g.set_debug_mode(true);
                        g.apply_debug_action(&pal_core::game::replay_rec::DebugAction::God(next_on))
                    } else {
                        Err("game not initialized".to_string())
                    };
                    rt.route_cmds();
                    rt.project();
                    r
                };
                match res {
                    Ok(msg) => {
                        self.god_mode = next_on;
                        ExecOutcome::Done(self.ok_reply(
                            host,
                            trimmed,
                            false,
                            serde_json::json!({ "god_mode": self.god_mode }),
                            vec![msg],
                        ))
                    }
                    Err(e) => ExecOutcome::Done(self.err_reply(host, trimmed, &e)),
                }
            }

            Command::DebugCash(n) => {
                if is_debug_mutation_busy(host) {
                    return ExecOutcome::Done(self.err_reply_code(
                        host,
                        trimmed,
                        "busy",
                        "cannot run /cash while a script or battle is active",
                    ));
                }
                let res = {
                    let rt = host.runtime_mut();
                    let r = if let Some(g) = rt.game.as_mut() {
                        g.set_debug_mode(true);
                        g.apply_debug_action(&pal_core::game::replay_rec::DebugAction::Cash(n))
                    } else {
                        Err("game not initialized".to_string())
                    };
                    rt.route_cmds();
                    rt.project();
                    r
                };
                match res {
                    Ok(msg) => ExecOutcome::Done(self.ok_reply(
                        host,
                        trimmed,
                        false,
                        serde_json::json!({}),
                        vec![msg],
                    )),
                    Err(e) => ExecOutcome::Done(self.err_reply(host, trimmed, &e)),
                }
            }

            Command::DebugItem { item_id, qty } => {
                if is_debug_mutation_busy(host) {
                    return ExecOutcome::Done(self.err_reply_code(
                        host,
                        trimmed,
                        "busy",
                        "cannot run /item while a script or battle is active",
                    ));
                }
                let res = {
                    let rt = host.runtime_mut();
                    let r = if let Some(g) = rt.game.as_mut() {
                        g.set_debug_mode(true);
                        g.apply_debug_action(&pal_core::game::replay_rec::DebugAction::Item {
                            item_id,
                            qty,
                        })
                    } else {
                        Err("game not initialized".to_string())
                    };
                    rt.route_cmds();
                    rt.project();
                    r
                };
                match res {
                    Ok(msg) => ExecOutcome::Done(self.ok_reply(
                        host,
                        trimmed,
                        false,
                        serde_json::json!({}),
                        vec![msg],
                    )),
                    Err(e) => ExecOutcome::Done(self.err_reply(host, trimmed, &e)),
                }
            }

            Command::DebugEventInspect(eid) => {
                if let Some(ev) = host.runtime().game.as_ref().and_then(|g| g.world.event(eid)) {
                    let msg = format!(
                        "[/event] #{eid} pos=({},{}) state={} sprite={} trigger_mode={}",
                        ev.x, ev.y, ev.state, ev.sprite_id, ev.trigger_mode
                    );
                    return ExecOutcome::Done(self.ok_reply(
                        host,
                        trimmed,
                        false,
                        serde_json::json!({
                            "event": {
                                "id": eid,
                                "x": ev.x,
                                "y": ev.y,
                                "state": ev.state,
                                "sprite_id": ev.sprite_id,
                                "trigger_mode": ev.trigger_mode,
                            }
                        }),
                        vec![msg],
                    ));
                }
                ExecOutcome::Done(self.err_reply(host, trimmed, &format!("event #{eid} not found")))
            }

            Command::DebugEventSetState(eid, st) => {
                if is_debug_mutation_busy(host) {
                    return ExecOutcome::Done(self.err_reply_code(
                        host,
                        trimmed,
                        "busy",
                        "cannot run /event state while a script or battle is active",
                    ));
                }
                let act = pal_core::game::replay_rec::DebugAction::EventState {
                    event_id: eid,
                    state: st,
                };
                let res = {
                    let rt = host.runtime_mut();
                    let r = if let Some(g) = rt.game.as_mut() {
                        g.validate_debug_action(&act).and_then(|()| {
                            g.set_debug_mode(true);
                            g.apply_debug_action(&act)
                        })
                    } else {
                        Err("game not initialized".to_string())
                    };
                    rt.route_cmds();
                    rt.project();
                    r
                };
                match res {
                    Ok(msg) => ExecOutcome::Done(self.ok_reply(
                        host,
                        trimmed,
                        false,
                        serde_json::json!({}),
                        vec![msg],
                    )),
                    Err(e) => ExecOutcome::Done(self.err_reply(host, trimmed, &e)),
                }
            }

            Command::DebugBattle(gid) => {
                let Ok(gid_u16) = u16::try_from(gid) else {
                    return ExecOutcome::Done(self.err_reply_code(
                        host,
                        trimmed,
                        "invalid_args",
                        &format!("invalid enemy group id {gid} (expected 0..={})", u16::MAX),
                    ));
                };
                let act = pal_core::game::replay_rec::DebugAction::Battle(gid_u16);
                let res = {
                    let rt = host.runtime_mut();
                    if let Some(g) = rt.game.as_mut() {
                        g.validate_debug_action(&act).and_then(|()| {
                            g.set_debug_mode(true);
                            g.apply_debug_action(&act)
                        })
                    } else {
                        Err("game not initialized".to_string())
                    }
                };
                let msg = match res {
                    Ok(m) => m,
                    Err(e) => return ExecOutcome::Done(self.err_reply(host, trimmed, &e)),
                };
                host.runtime_mut().route_cmds();
                host.service_host_cmds();
                self.active_task = Some(ActiveTask::settle(
                    trimmed,
                    0,
                    240,
                    false,
                    vec![msg],
                ));
                ExecOutcome::Pending
            }

            Command::DebugScript(lbl) => {
                let act = pal_core::game::replay_rec::DebugAction::Script(lbl.clone());
                let res = {
                    let rt = host.runtime_mut();
                    if let Some(g) = rt.game.as_mut() {
                        g.validate_debug_action(&act).and_then(|()| {
                            g.set_debug_mode(true);
                            g.apply_debug_action(&act)
                        })
                    } else {
                        Err("game not initialized".to_string())
                    }
                };
                let msg = match res {
                    Ok(m) => m,
                    Err(e) => return ExecOutcome::Done(self.err_reply(host, trimmed, &e)),
                };
                host.runtime_mut().route_cmds();
                host.service_host_cmds();
                self.active_task = Some(ActiveTask::settle(
                    trimmed,
                    0,
                    240,
                    false,
                    vec![msg],
                ));
                ExecOutcome::Pending
            }

            Command::DebugState => {
                let st = host
                    .runtime()
                    .game
                    .as_ref()
                    .map(|g| g.canonical_state())
                    .unwrap_or(serde_json::Value::Null);
                ExecOutcome::Done(self.ok_reply(
                    host,
                    trimmed,
                    false,
                    serde_json::json!({ "canonical_state": st }),
                    vec![],
                ))
            }
            _ => ExecOutcome::Done(self.err_reply(host, trimmed, "unhandled command")),
        }
    }
}
