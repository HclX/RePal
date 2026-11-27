//! `repal-text/1` JSON-Lines response envelope and viewport ASCII minimap builder.

use super::*;

/// Optional metadata passed into [`build_response_json_ex`] for notices, battle feed,
/// error codes, interruption reasons, RNG status, minimap inclusion, and fairness profile.
#[derive(Clone, Copy, Default)]
pub struct ResponseOpts<'a> {
    pub obs_mode: ObsMode,
    pub include_minimap: bool,
    pub error: Option<&'a str>,
    pub error_code: Option<&'a str>,
    pub interrupted_by: Option<&'a str>,
    pub notices: &'a [TextNotice],
    pub battle_feed: &'a [BattleFeedEntry],
    pub rng: Option<RngStatus>,
}

/// Build the unified `repal-text/1` JSON response envelope for any `PalRuntime`.
pub fn build_response_json_ex(
    rt: &PalRuntime,
    ok: bool,
    cmd: &str,
    artifacts: serde_json::Value,
    log: &[String],
    opts: ResponseOpts<'_>,
) -> serde_json::Value {
    let mode = current_mode(rt);
    let view_id = ui_view_id(&rt.ui.view);
    let lang = rt.ui.lang;
    let lcode = lang_code(lang);
    let (cam_x, cam_y) = runtime_camera(rt);
    let view_obj = serde_json::json!({
        "camera": { "x": cam_x, "y": cam_y, "w": 320, "h": 200 },
        "hd": rt.ui.hd_mode,
        "lang": lcode,
        "fb": { "w": rt.compositor.fb.width, "h": rt.compositor.fb.height },
    });
    let notices_json: Vec<serde_json::Value> = opts
        .notices
        .iter()
        .map(|n| {
            serde_json::json!({
                "tick": n.tick,
                "kind": n.kind,
                "text": n.text,
            })
        })
        .collect();
    let rng_json = opts.rng.map(|r| {
        serde_json::json!({
            "id": r.id,
            "frame": r.frame,
            "end": r.end,
            "skippable": r.skippable,
        })
    });

    let Some(g) = rt.game.as_ref() else {
        return serde_json::json!({
            "protocol": TEXT_PROTOCOL_VERSION,
            "obs_mode": opts.obs_mode.as_str(),
            "ok": ok,
            "cmd": cmd,
            "mode": mode,
            "lang": lcode,
            "ui_view": view_id,
            "view": view_obj,
            "tick": 0,
            "scene": null,
            "party": [],
            "cash": 0,
            "walkable": null,
            "walkable_dist": null,
            "minimap": null,
            "events": [],
            "prompt": null,
            "battle": null,
            "menu": null,
            "rng": rng_json,
            "notices": notices_json,
            "artifacts": artifacts,
            "log": log,
            "interrupted_by": opts.interrupted_by,
            "error": opts.error,
            "error_code": opts.error_code,
        });
    };

    let fair = opts.obs_mode.is_fair();
    let lead = g.world.party.first().cloned().unwrap_or_default();
    let sid = g.world.scene_id;
    let map_id = g.world.current_scene().map(|s| s.map_id).unwrap_or(0);

    // Build a set of visible solid-event half-tiles so fair-mode minimap & ray lengths never
    // expose `sprite_id == 0` solid invisible triggers (#234).
    let mut visible_solid_tiles: HashSet<(i32, i32, i32)> = HashSet::new();
    let mut visible_event_tiles: HashSet<(i32, i32, i32)> = HashSet::new();
    let mut visible_exit_tiles: HashSet<(i32, i32, i32)> = HashSet::new();
    for id in g.world.current_event_ids() {
        let Some(ev) = g.world.event(id) else {
            continue;
        };
        if ev.state <= 0 || ev.vanish_time > 0 {
            continue;
        }
        if event_visible_in_viewport(rt, ev, cam_x, cam_y) {
            let tile = world_to_half_tile(ev.x, ev.y);
            visible_event_tiles.insert(tile);
            if ev.state >= 2 {
                visible_solid_tiles.insert(tile);
            }
        } else if ev.sprite_id == 0 && is_fair_visible_exit(rt, ev, cam_x, cam_y) {
            visible_exit_tiles.insert(world_to_half_tile(ev.x, ev.y));
        }
    }

    let is_tile_blocked_fair_aware = |x: i32, y: i32, h: i32| -> bool {
        if fair {
            g.base_grid().is_blocked(x, y, h) || visible_solid_tiles.contains(&(x, y, h))
        } else {
            g.grid.is_blocked(x, y, h)
        }
    };

    let (hx, hy, hh) = world_to_half_tile(lead.x, lead.y);
    // On-foot reachability (#17): unblocked tiles the party can't walk to (e.g. water between
    // causeways, only crossable via lotus-pad hops) render as `~` and end walkable_dist rays.
    let hop_zones = crate::text_mode::reach::hop_zones(g);
    let reach = crate::text_mode::reach::flood_reachable((lead.x, lead.y), &is_tile_blocked_fair_aware, &hop_zones);
    let mut walkable_map = serde_json::Map::new();
    let mut walkable_dist_map = serde_json::Map::new();
    for dir in 0..4u8 {
        let (nx, ny, nh) = step_half_tile(hx, hy, hh, dir);
        // Immediate 1-step walkability uses authoritative collision grid (for fair mode, player
        // knows immediately if an adjacent step is walkable when standing right next to it,
        // except occluded cells stay opaque in ray-length scan).
        walkable_map.insert(
            dir_code(dir).to_string(),
            serde_json::Value::Bool(!g.grid.is_blocked(nx, ny, nh)),
        );
        // Per-direction clear ray length strictly within the 320x200 viewport (max 10 steps, #234).
        let mut cx = hx;
        let mut cy = hy;
        let mut ch = hh;
        let mut steps = 0u32;
        for _ in 0..10 {
            let (tx, ty, th) = step_half_tile(cx, cy, ch, dir);
            let (wx, wy) = half_tile_to_world(tx, ty, th);
            if wx < cam_x || wx >= cam_x + 320 || wy < cam_y || wy >= cam_y + 200 {
                break;
            }
            if fair && g.base_grid().is_occluded(tx, ty, th) {
                break;
            }
            if is_tile_blocked_fair_aware(tx, ty, th) || !reach.tiles.contains(&(tx, ty, th)) {
                break;
            }
            steps += 1;
            cx = tx;
            cy = ty;
            ch = th;
        }
        walkable_dist_map.insert(dir_code(dir).to_string(), serde_json::json!(steps));
    }

    // Build exact staggered 20x25 viewport minimap when requested in `explore` mode (#234).
    // Formula: `grid[r][c]` corresponds strictly to world pixel `(origin.x + c * 16, origin.y + r * 8)`.
    // Odd-parity `(wx/16 + wy/8) % 2 != 0` cells are `' '` spacers; even-parity cells are real half-tiles.
    let minimap_json = if opts.include_minimap && mode == "explore" {
        let origin_x = ((cam_x + 15).div_euclid(16)) * 16;
        let origin_y = ((cam_y + 7).div_euclid(8)) * 8;
        let cols: i32 = 20;
        let rows: i32 = 25;
        let mut grid_rows: Vec<String> = Vec::with_capacity(rows as usize);
        for r in 0..rows {
            let wy = origin_y + r * 8;
            let gy = wy.div_euclid(8);
            let mut row = String::with_capacity(cols as usize);
            for c in 0..cols {
                let wx = origin_x + c * 16;
                let gx = wx.div_euclid(16);
                if (gx + gy).rem_euclid(2) != 0 {
                    row.push(' ');
                    continue;
                }
                if wx < cam_x || wx >= cam_x + 320 || wy < cam_y || wy >= cam_y + 200 {
                    row.push(' ');
                    continue;
                }
                let tile = world_to_half_tile(wx, wy);
                let ch = if tile == (hx, hy, hh) {
                    '@'
                } else if visible_exit_tiles.contains(&tile) {
                    '>'
                } else if fair && g.base_grid().is_occluded(tile.0, tile.1, tile.2) {
                    '?'
                } else if visible_event_tiles.contains(&tile) {
                    'E'
                } else if is_tile_blocked_fair_aware(tile.0, tile.1, tile.2) {
                    '#'
                } else if g.base_grid().is_void(tile.0, tile.1, tile.2) {
                    ' '
                } else if !reach.tiles.contains(&tile) {
                    '~'
                } else {
                    '.'
                };
                row.push(ch);
            }
            grid_rows.push(row);
        }
        Some(serde_json::json!({
            "origin": { "x": origin_x, "y": origin_y },
            "step": { "x": 16, "y": 8 },
            "cols": cols,
            "rows": rows,
            "staggered": true,
            "legend": {
                "@": "party",
                ".": "walkable",
                "~": "unreachable_on_foot",
                "#": "blocked",
                "?": "occluded",
                "E": "event",
                ">": "exit",
                " ": "void_or_spacer"
            },
            "grid": grid_rows,
        }))
    } else {
        None
    };

    let party: Vec<serde_json::Value> = g
        .world
        .party
        .iter()
        .enumerate()
        .filter_map(|(idx, m)| {
            let r = g.world.roles.get(m.role_id as usize)?;
            let statuses: Vec<serde_json::Value> = r
                .statuses
                .iter()
                .filter(|(_, &turns)| turns > 0)
                .map(|(&st, &turns)| {
                    serde_json::json!({
                        "id": st,
                        "name": status_display_name(st, lang),
                        "turns": turns,
                    })
                })
                .collect();
            Some(serde_json::json!({
                "idx": idx,
                "role_id": m.role_id,
                "name": g.role_display_name(m.role_id as usize),
                "level": r.level,
                "hp": r.hp,
                "max_hp": r.max_hp,
                "mp": r.mp,
                "max_mp": r.max_mp,
                "x": m.x,
                "y": m.y,
                "dir": dir_code(m.dir),
                "statuses": statuses,
                "poisons": r.poisons,
            }))
        })
        .collect();

    let mut evs: Vec<(i32, serde_json::Value)> = Vec::new();
    for id in g.world.current_event_ids() {
        let Some(ev) = g.world.event(id) else {
            continue;
        };
        if ev.state <= 0 || ev.vanish_time > 0 || (ev.trigger_mode == 0 && ev.sprite_id == 0) {
            continue;
        }
        let sx = ev.x - cam_x;
        let sy = ev.y - cam_y;
        let sprite_visible = event_visible_in_viewport(rt, ev, cam_x, cam_y);
        let fair_exit = ev.sprite_id == 0 && is_fair_visible_exit(rt, ev, cam_x, cam_y);
        let anchor_on_screen = (0..320).contains(&sx) && (0..200).contains(&sy);
        if fair {
            // Fair mode (#224, #234): real MGO frame `bbox ∩ viewport` for visible sprites, plus
            // unconditional initially-active on-screen `0x0059` exits (zero secret/trigger leaks).
            if !sprite_visible && !fair_exit {
                continue;
            }
        }
        let dist = ((ev.x - lead.x).abs() + (ev.y - lead.y).abs() * 2) / 16;
        let rel_dir = direction_between(lead.x, lead.y, ev.x, ev.y);
        let (kind, kind_label, hint) = describe_event(rt, ev, fair);
        let screen = if sprite_visible || anchor_on_screen {
            serde_json::json!({ "x": sx, "y": sy })
        } else {
            serde_json::Value::Null
        };
        let entry = if fair {
            // Do NOT expose `interactable` / `trigger_mode` in fair mode so hidden searchable props
            // (pots/jars/secret chests) cannot be distinguished without trying (#234).
            let mut obj = serde_json::json!({
                "id": ev.id,
                "kind": kind,
                "kind_label": kind_label,
                "hint": hint,
                "x": ev.x,
                "y": ev.y,
                "screen": screen,
                "dist": dist,
                "dir": dir_code(rel_dir),
            });
            if let Some((vkind, vdesc)) = event_visual_info(rt, ev) {
                if let Some(map) = obj.as_object_mut() {
                    if !vkind.is_empty() {
                        map.insert("visual_kind".to_string(), serde_json::Value::String(vkind));
                    }
                    if !vdesc.is_empty() {
                        map.insert("visual".to_string(), serde_json::Value::String(vdesc));
                    }
                }
            }
            obj
        } else {
            serde_json::json!({
                "id": ev.id,
                "kind": kind,
                "kind_label": kind_label,
                "hint": hint,
                "interactable": ev.trigger_mode >= 1 && ev.trigger_mode < 4,
                "x": ev.x,
                "y": ev.y,
                "screen": screen,
                "dist": dist,
                "dir": dir_code(rel_dir),
                "trigger_mode": ev.trigger_mode,
                "state": ev.state,
            })
        };
        evs.push((dist, entry));
    }
    evs.sort_by_key(|(d, _)| *d);
    let events: Vec<serde_json::Value> = evs.into_iter().map(|(_, v)| v).collect();

    let prompt = if rt.ui.view == UiView::Dialog {
        let total_pages = rt.ui.dialog.total_pages.max(1);
        let page = rt.ui.dialog.page.min(total_pages - 1);
        let page_text = rt.ui.current_dialog_page_text().to_string();
        Some(serde_json::json!({
            "kind": if rt.ui.dialog.choices.is_empty() { "dialog" } else { "choice" },
            "speaker": rt.ui.dialog.speaker,
            "text": rt.ui.dialog.text,
            "page": page,
            "total_pages": total_pages,
            "page_text": page_text,
            "choices": rt.ui.dialog.choices,
        }))
    } else if rt.victory_open || rt.ui.view == UiView::BattleVictory {
        Some(serde_json::json!({
            "kind": "battle_victory",
            "exp": rt.ui.battle_victory.exp,
            "cash": rt.ui.battle_victory.cash,
            "level_ups": rt.ui.battle_victory.level_ups,
        }))
    } else if matches!(rt.ui.view, UiView::Shop | UiView::PawnshopSell) {
        let items: Vec<serde_json::Value> = rt
            .ui
            .shop_items
            .iter()
            .enumerate()
            .map(|(i, e)| {
                serde_json::json!({
                    "idx": i,
                    "id": e.item_id,
                    "name": e.name,
                    "price": e.price,
                    "count": e.count,
                })
            })
            .collect();
        Some(serde_json::json!({
            "kind": if rt.ui.view == UiView::PawnshopSell { "pawnshop" } else { "shop" },
            "items": items,
        }))
    } else {
        None
    };

    let battle = g.battle.as_ref().filter(|b| b.active).map(|b| {
        let heroes: Vec<serde_json::Value> = b
            .heroes
            .iter()
            .enumerate()
            .map(|(h_idx, h)| {
                let r = g.world.roles.get(h.role);
                let party_idx = g
                    .world
                    .party
                    .iter()
                    .position(|pm| pm.role_id as usize == h.role)
                    .unwrap_or(h_idx);
                let statuses: Vec<serde_json::Value> = r
                    .map(|r| {
                        r.statuses
                            .iter()
                            .filter(|(_, &turns)| turns > 0)
                            .map(|(&st, &turns)| {
                                serde_json::json!({
                                    "id": st,
                                    "name": status_display_name(st, lang),
                                    "turns": turns,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let poisons = r.map(|r| r.poisons.clone()).unwrap_or_default();
                let hp = r.map(|r| r.hp).unwrap_or(0);
                serde_json::json!({
                    "idx": h_idx,
                    "party_idx": party_idx,
                    "role_id": h.role,
                    "name": g.role_display_name(h.role),
                    "hp": hp,
                    "max_hp": r.map(|r| r.max_hp).unwrap_or(0),
                    "mp": r.map(|r| r.mp).unwrap_or(0),
                    "max_mp": r.map(|r| r.max_mp).unwrap_or(0),
                    "atb_pct": h.atb_pct,
                    "defending": h.defending,
                    "ko": hp <= 0,
                    "statuses": statuses,
                    "poisons": poisons,
                })
            })
            .collect();
        let enemies: Vec<serde_json::Value> = b
            .enemies
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let statuses: Vec<serde_json::Value> = e
                    .statuses
                    .iter()
                    .filter(|(_, &turns)| turns > 0)
                    .map(|(&st, &turns)| {
                        serde_json::json!({
                            "id": st,
                            "name": status_display_name(st, lang),
                            "turns": turns,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "slot": i,
                    "enemy_id": e.enemy_id,
                    "name": g.enemy_display_name(e.enemy_id, &e.name, &e.name_hant),
                    "hp": if fair { serde_json::Value::Null } else { serde_json::json!(e.hp) },
                    "max_hp": if fair { serde_json::Value::Null } else { serde_json::json!(e.max_hp) },
                    "alive": e.hp > 0,
                    "statuses": statuses,
                    "poisons": e.poisons,
                })
            })
            .collect();
        let feed: Vec<serde_json::Value> = opts
            .battle_feed
            .iter()
            .map(|f| {
                serde_json::json!({
                    "tick": f.tick,
                    "actor": f.actor,
                    "action": f.action,
                    "name": f.name,
                    "target": f.target,
                    "value": f.value,
                    "kind": f.kind,
                    "text": f.text,
                })
            })
            .collect();
        serde_json::json!({
            "active": b.active,
            "prompting": b.prompting,
            "can_flee": b.can_flee,
            "spell_banner": b.spell_banner,
            "heroes": heroes,
            "enemies": enemies,
            "feed": feed,
        })
    });

    let menu = build_ui_menu(rt);

    serde_json::json!({
        "protocol": TEXT_PROTOCOL_VERSION,
        "obs_mode": opts.obs_mode.as_str(),
        "ok": ok,
        "cmd": cmd,
        "mode": mode,
        "lang": lcode,
        "ui_view": view_id,
        "view": view_obj,
        "tick": g.tick_count,
        "scene": { "id": sid, "map_id": map_id, "name": scene_name(rt, sid) },
        "party": party,
        "cash": g.world.cash,
        "walkable": serde_json::Value::Object(walkable_map),
        "walkable_dist": serde_json::Value::Object(walkable_dist_map),
        "minimap": minimap_json,
        "events": events,
        "prompt": prompt,
        "battle": battle,
        "menu": menu,
        "rng": rng_json,
        "notices": notices_json,
        "artifacts": artifacts,
        "log": log,
        "interrupted_by": opts.interrupted_by,
        "error": opts.error,
        "error_code": opts.error_code,
    })
}

/// Convenience wrapper for callers constructing a standalone response without incremental cursors.
pub fn build_response_json(
    rt: &PalRuntime,
    ok: bool,
    cmd: &str,
    fog: bool,
    artifacts: serde_json::Value,
    log: &[String],
    error: Option<&str>,
) -> serde_json::Value {
    build_response_json_ex(
        rt,
        ok,
        cmd,
        artifacts,
        log,
        ResponseOpts {
            obs_mode: if fog { ObsMode::Fair } else { ObsMode::Debug },
            error,
            ..Default::default()
        },
    )
}
