//! RPG party/inventory/shop and I/O (shot, golden, save/load, replay, think, bugreport) command handlers.

use super::*;

impl TextEngine {
    pub(super) fn submit_rpg_io<H: TextHost>(
        &mut self,
        host: &mut H,
        trimmed: &str,
        origin: ClientOrigin,
        parsed: Command,
    ) -> ExecOutcome {
        match parsed {
            Command::Status => {
                let mut log = Vec::new();
                let mut status_list = Vec::new();
                if let Some(g) = host.runtime().game.as_ref() {
                    for (idx, m) in g.world.party.iter().enumerate() {
                        let rid = m.role_id as usize;
                        if let Some(r) = g.world.roles.get(rid) {
                            let name = g.role_display_name(rid);
                            log.push(format!(
                                "[#{idx}] {name} Lv.{} (EXP {}) HP {}/{} MP {}/{} 武{} 灵{} 防{} 身{} 吉{}",
                                r.level,
                                r.exp,
                                r.hp,
                                r.max_hp,
                                r.mp,
                                r.max_mp,
                                r.stats[0],
                                r.stats[1],
                                r.stats[2],
                                r.stats[3],
                                r.stats[4]
                            ));
                            let equip_summary: Vec<String> = r
                                .equipment
                                .iter()
                                .copied()
                                .filter(|&iid| iid > 0)
                                .map(|iid| format!("{}#{iid}", g.item_display_name(iid)))
                                .collect();
                            if !equip_summary.is_empty() {
                                log.push(format!("  装备: {}", equip_summary.join(" / ")));
                            }
                            let magic_lines: Vec<String> = r
                                .magics
                                .iter()
                                .copied()
                                .filter(|&mid| mid > 0)
                                .map(|mid| format_magic_plain_line(g, mid, host.runtime().ui.lang))
                                .collect();
                            if !magic_lines.is_empty() {
                                let magic_hdr = match host.runtime().ui.lang {
                                    pal_core::Language::En => "  Magic:",
                                    pal_core::Language::ZhHant => "  仙術:",
                                    pal_core::Language::ZhHans => "  仙术:",
                                };
                                log.push(magic_hdr.to_string());
                                log.extend(magic_lines);
                            }
                            let magics: Vec<serde_json::Value> = r
                                .magics
                                .iter()
                                .copied()
                                .filter(|&mid| mid > 0)
                                .map(|mid| {
                                    let md = g.data.magics.get(&mid);
                                    let cost = md.map(|mm| mm.cost_mp).unwrap_or(0);
                                    serde_json::json!({
                                        "id": mid,
                                        "name": g.magic_display_name(mid),
                                        "cost_mp": cost,
                                        "desc": g.magic_display_desc(mid),
                                        "target": md.map(magic_target_kind).unwrap_or("enemy"),
                                        "usable_field": md.map(|mm| mm.usable_outside_battle).unwrap_or(false),
                                        "usable_battle": true,
                                        "affordable": r.mp >= cost,
                                    })
                                })
                                .collect();
                            let equip: Vec<serde_json::Value> = r
                                .equipment
                                .iter()
                                .enumerate()
                                .map(|(slot, &iid)| {
                                    serde_json::json!({
                                        "slot": slot,
                                        "item_id": iid,
                                        "name": if iid > 0 { g.item_display_name(iid) } else { String::new() },
                                    })
                                })
                                .collect();
                            status_list.push(serde_json::json!({
                                "idx": idx,
                                "role_id": rid,
                                "name": name,
                                "level": r.level,
                                "exp": r.exp,
                                "hp": r.hp,
                                "max_hp": r.max_hp,
                                "mp": r.mp,
                                "max_mp": r.max_mp,
                                "stats": {
                                    "atk": r.stats[0],
                                    "mag": r.stats[1],
                                    "def": r.stats[2],
                                    "dex": r.stats[3],
                                    "flee": r.stats[4],
                                },
                                "equipment": equip,
                                "magics": magics,
                            }));
                        }
                    }
                }
                ExecOutcome::Done(self.ok_reply(
                    host,
                    trimmed,
                    false,
                    serde_json::json!({ "status": status_list }),
                    log,
                ))
            }

            Command::Bag => {
                let mut log = Vec::new();
                let mut bag_list = Vec::new();
                if let Some(g) = host.runtime().game.as_ref() {
                    let mut items: Vec<(u16, i32)> = g
                        .world
                        .inventory
                        .iter()
                        .filter(|(_, &q)| q > 0)
                        .map(|(&id, &q)| (id, q))
                        .collect();
                    items.sort_by_key(|(id, _)| *id);
                    for (id, qty) in items {
                        let name = g.item_display_name(id);
                        let it = g.data.items.get(&id);
                        let equip_slot = g.item_slot(id);
                        let equippable_by: Vec<usize> = if equip_slot.is_some() {
                            (0..6).filter(|&rid| g.can_role_equip(rid, id)).collect()
                        } else {
                            Vec::new()
                        };
                        log.push(format_bag_plain_line(g, id, qty, host.runtime().ui.lang));
                        bag_list.push(serde_json::json!({
                            "id": id,
                            "name": name,
                            "count": qty,
                            "desc": g.item_display_desc(id),
                            "usable_field": it.map(|x| x.usable).unwrap_or(false),
                            "usable_battle": it.map(|x| x.usable || x.throwable).unwrap_or(false),
                            "throwable": it.map(|x| x.throwable).unwrap_or(false),
                            "consumable": it.map(|x| x.consumable).unwrap_or(false),
                            "equip_slot": equip_slot,
                            "equippable_by": equippable_by,
                            "target": it.map(item_target_kind).unwrap_or("none"),
                        }));
                    }
                    if bag_list.is_empty() {
                        log.push("  (背包为空)".to_string());
                    }
                }
                ExecOutcome::Done(self.ok_reply(
                    host,
                    trimmed,
                    false,
                    serde_json::json!({ "bag": bag_list }),
                    log,
                ))
            }

            Command::UseItem { item, party_idx } => {
                let item_id = match resolve_item_ref(host.runtime(), &item, true) {
                    Ok(id) => id,
                    Err((code, msg)) => {
                        return ExecOutcome::Done(self.err_reply_code(host, trimmed, code, &msg));
                    }
                };
                // Auto-route `use <id|name> [slot]` to BattleItem when in battle (#276),
                // matching `magic`'s seamless explore+battle UX!
                if current_mode(host.runtime()) == "battle" {
                    if let Err((code, msg)) = validate_battle_item(host.runtime(), item_id) {
                        return ExecOutcome::Done(self.err_reply_code(host, trimmed, code, &msg));
                    }
                    let bc = match party_idx {
                        Some(t) => BattleCmd::ItemTarget {
                            item: item_id,
                            target: t,
                        },
                        None => BattleCmd::Item { item: item_id },
                    };
                    return self.submit_battle_cmd(host, trimmed, bc);
                }
                let role_id = match party_idx {
                    Some(idx) => match party_idx_to_role_id(host.runtime(), idx) {
                        Some(rid) => Some(rid),
                        None => {
                            return ExecOutcome::Done(self.err_reply(
                                host,
                                trimmed,
                                &format!("party index {idx} out of range"),
                            ));
                        }
                    },
                    None => None,
                };
                let item_name = host
                    .runtime()
                    .game
                    .as_ref()
                    .map(|g| g.item_display_name(item_id))
                    .unwrap_or_default();
                let pre_snap = UseItemSnapshot::capture(host.runtime(), item_id);
                let used = host
                    .runtime_mut()
                    .game
                    .as_mut()
                    .map(|g| g.use_item(item_id, role_id))
                    .unwrap_or(false);
                if !used {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        &format!("cannot use item #{item_id}"),
                    ));
                }
                if !item_name.is_empty() {
                    host.on_toast(&format!("使用 · {item_name}"));
                }
                host.runtime_mut().route_cmds();
                host.service_host_cmds();
                self.active_task = Some(ActiveTask::Settle {
                    cmd: trimmed.to_string(),
                    min_ticks: self.pace_ticks.min(10).max(1),
                    max_ticks: 180,
                    elapsed: 0,
                    show_events_on_explore: false,
                    use_item_snapshot: Some(pre_snap),
                    expect_leave_choice: false,
                    talk_check: None,
                    interrupted_by: None,
                    log: vec![format!("[use] item #{item_id}")],
                });
                ExecOutcome::Pending
            }

            Command::EquipItem { item, party_idx } => {
                let item_id = match resolve_item_ref(host.runtime(), &item, true) {
                    Ok(id) => id,
                    Err((code, msg)) => {
                        return ExecOutcome::Done(self.err_reply_code(host, trimmed, code, &msg));
                    }
                };
                let Some(role_id) = party_idx_to_role_id(host.runtime(), party_idx) else {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        &format!("party index {party_idx} out of range"),
                    ));
                };
                let item_name = host
                    .runtime()
                    .game
                    .as_ref()
                    .map(|g| g.item_display_name(item_id))
                    .unwrap_or_default();
                let ok = host
                    .runtime_mut()
                    .game
                    .as_mut()
                    .map(|g| g.equip_from_menu(role_id, item_id))
                    .unwrap_or(false);
                host.runtime_mut().route_cmds();
                host.runtime_mut().project();
                host.service_host_cmds();
                if !ok {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        &format!("cannot equip item #{item_id} on party[{party_idx}] (role_id={role_id})"),
                    ));
                }
                if !item_name.is_empty() {
                    host.on_toast(&format!("装备 · {item_name}"));
                }
                ExecOutcome::Done(self.ok_reply(
                    host,
                    trimmed,
                    false,
                    serde_json::json!({}),
                    vec![format!("[equip] item #{item_id} -> party[{party_idx}] (role_id={role_id})")],
                ))
            }

            Command::Buy { shop_idx, qty } => {
                if host.runtime().ui.view != UiView::Shop {
                    let msg = if host.runtime().ui.view == UiView::PawnshopSell {
                        "this is a sell-only pawnshop (use 'sell <id|name> [qty]' or 'close')"
                    } else {
                        "not in shop"
                    };
                    return ExecOutcome::Done(self.err_reply(host, trimmed, msg));
                }
                let Some(entry) = host.runtime().ui.shop_items.get(shop_idx).cloned() else {
                    return ExecOutcome::Done(self.err_reply(host, trimmed, "shop index out of range"));
                };
                host.runtime_mut().ui.shop_cursor = shop_idx;
                let mut bought = 0usize;
                for _ in 0..qty {
                    let ok = host
                        .runtime_mut()
                        .game
                        .as_mut()
                        .map(|g| g.buy_item(entry.item_id, entry.price))
                        .unwrap_or(false);
                    if !ok {
                        break;
                    }
                    bought += 1;
                }
                host.runtime_mut().route_cmds();
                host.runtime_mut().project();
                host.service_host_cmds();
                if bought == 0 {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        &format!("cannot buy {} (not enough cash)", entry.name),
                    ));
                }
                ExecOutcome::Done(self.ok_reply(
                    host,
                    trimmed,
                    false,
                    serde_json::json!({ "bought": bought, "item_id": entry.item_id }),
                    vec![format!("[buy] {} x{bought}/{qty}", entry.name)],
                ))
            }

            Command::Sell { item, qty } => {
                if host.runtime().ui.view != UiView::PawnshopSell {
                    let msg = if host.runtime().ui.view == UiView::Shop {
                        "this is a buy-only shop — items can only be sold at a pawnshop (当铺)"
                    } else {
                        "not in pawnshop — items can only be sold at a pawnshop (当铺)"
                    };
                    return ExecOutcome::Done(self.err_reply(host, trimmed, msg));
                }
                let item_id = match resolve_item_ref(host.runtime(), &item, true) {
                    Ok(id) => id,
                    Err((code, msg)) => {
                        return ExecOutcome::Done(self.err_reply_code(host, trimmed, code, &msg));
                    }
                };
                let Some(entry) = host
                    .runtime()
                    .ui
                    .shop_items
                    .iter()
                    .find(|e| e.item_id == item_id)
                    .cloned()
                else {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        &format!("item #{item_id} not sellable"),
                    ));
                };
                let mut sold = 0usize;
                for _ in 0..qty {
                    let ok = host
                        .runtime_mut()
                        .game
                        .as_mut()
                        .map(|g| g.sell_item(item_id, entry.price))
                        .unwrap_or(false);
                    if !ok {
                        break;
                    }
                    sold += 1;
                }
                host.runtime_mut().refresh_pawnshop();
                host.runtime_mut().route_cmds();
                host.runtime_mut().project();
                host.service_host_cmds();
                if sold == 0 {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        &format!("item #{item_id} could not be sold"),
                    ));
                }
                ExecOutcome::Done(self.ok_reply(
                    host,
                    trimmed,
                    false,
                    serde_json::json!({ "sold": sold, "item_id": item_id }),
                    vec![format!("[sell] {} x{sold}/{qty}", entry.name)],
                ))
            }

            Command::SetHd(hd) => {
                host.runtime_mut().set_hd_mode(hd);
                host.on_hd_changed(hd);
                ExecOutcome::Done(self.ok_reply(
                    host,
                    trimmed,
                    false,
                    serde_json::json!({}),
                    vec![if hd {
                        "[display] switched to 4x HD (1280x800)".to_string()
                    } else {
                        "[display] switched to 1x Retro (320x200)".to_string()
                    }],
                ))
            }
            Command::SetLang(lang) => {
                host.runtime_mut().set_lang(lang);
                host.on_lang_changed(lang);
                ExecOutcome::Done(self.ok_reply(
                    host,
                    trimmed,
                    false,
                    serde_json::json!({}),
                    vec![format!("[lang] switched to {lang:?}")],
                ))
            }

            #[cfg(feature = "disk")]
            Command::Shot { path: opt_path, with_thought, small } => {
                if let Err(e) = self.check_remote_write_quota(origin, 256 * 1024) {
                    return ExecOutcome::Done(self.err_reply_code(host, trimmed, "quota_exceeded", &e));
                }
                self.shot_counter += 1;
                let out_path = match opt_path {
                    Some(p) => match resolve_write_path_for_origin(
                        host.out_dir(),
                        &p,
                        origin.is_socket(),
                        &["png"],
                    ) {
                        Ok(pb) => pb,
                        Err(e) => {
                            return ExecOutcome::Done(self.err_reply_code(
                                host,
                                trimmed,
                                "path_forbidden",
                                &e,
                            ));
                        }
                    },
                    None => {
                        let sid = host.runtime().state.scene_id;
                        let name = format!("shot_{:03}_sc{:03}.png", self.shot_counter, sid);
                        if origin.is_socket() {
                            match confine_socket_write_path(host.out_dir(), &name, &["png"]) {
                                Ok(pb) => pb,
                                Err(e) => {
                                    return ExecOutcome::Done(self.err_reply_code(
                                        host,
                                        trimmed,
                                        "path_forbidden",
                                        &e,
                                    ));
                                }
                            }
                        } else if host.out_dir() == Path::new(".") {
                            PathBuf::from(".tmp/repal_shots").join(name)
                        } else {
                            host.out_dir().join(name)
                        }
                    }
                };
                host.render_settled_frame();
                let (w, h, non_black) = if small || (with_thought && host.runtime().has_visible_ai_thought_overlay()) {
                    let mut fb_copy = host.runtime().compositor.fb.clone();
                    if with_thought && host.runtime().has_visible_ai_thought_overlay() {
                        host.runtime_mut().draw_ai_thought_overlay_onto(&mut fb_copy);
                    }
                    if small {
                        fb_copy = match crate::small_fair_view(&fb_copy) {
                            Ok(f) => f,
                            Err(e) => return ExecOutcome::Done(self.err_reply(host, trimmed, &e)),
                        };
                    }
                    if let Err(e) = export_fb_to_png(&fb_copy, &out_path) {
                        return ExecOutcome::Done(self.err_reply(host, trimmed, &e));
                    }
                    let nb = (non_black_pixel_ratio_fb(&fb_copy) * 10000.0).round() / 100.0;
                    (fb_copy.width, fb_copy.height, nb)
                } else {
                    let fb = &host.runtime().compositor.fb;
                    if let Err(e) = export_fb_to_png(fb, &out_path) {
                        return ExecOutcome::Done(self.err_reply(host, trimmed, &e));
                    }
                    let nb = (non_black_pixel_ratio_fb(fb) * 10000.0).round() / 100.0;
                    (fb.width, fb.height, nb)
                };
                let written = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
                self.record_remote_write(origin, written);
                let artifacts = serde_json::json!({
                    "shot": {
                        "path": out_path.display().to_string(),
                        "width": w,
                        "height": h,
                        "non_black_pct": non_black,
                        "with_thought": with_thought,
                    }
                });
                let log = vec![format!(
                    "[shot] saved {} ({w}x{h}, non_black={non_black:.1}%)",
                    out_path.display()
                )];
                ExecOutcome::Done(self.ok_reply(host, trimmed, false, artifacts, log))
            }

            #[cfg(feature = "disk")]
            Command::CaptureStart {
                path,
                stride,
                with_thought,
            } => {
                if origin.is_socket() && !self.allow_unsafe_remote {
                    return ExecOutcome::Done(self.err_reply_code(
                        host,
                        trimmed,
                        "forbidden",
                        "capture is disabled over TCP sockets",
                    ));
                }
                if self.capture.is_some() {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        "capture already active (run `capture stop` first)",
                    ));
                }
                let out_path = if Path::new(&path).is_absolute() || host.out_dir() == Path::new(".") {
                    PathBuf::from(&path)
                } else {
                    host.out_dir().join(&path)
                };
                host.render_settled_frame();
                let (w, h) = (
                    host.runtime().compositor.fb.width,
                    host.runtime().compositor.fb.height,
                );
                let hd_mode = host.runtime().ui.hd_mode;
                match crate::capture::FrameRecorder::open_with_audio(
                    &out_path,
                    w,
                    h,
                    stride,
                    with_thought,
                    true,
                    hd_mode,
                ) {
                    Ok(mut rec) => {
                        let audio_events = host.drain_audio_events();
                        let fb_owned = host.runtime().compositor.fb.clone();
                        let _ = rec.push_tick_with_audio(&fb_owned, &audio_events, hd_mode, |k| {
                            host.read_audio_asset(k)
                        });
                        let fmt = rec.format();
                        let fps = rec.fps();
                        let has_audio = rec.has_audio();
                        self.capture = Some(rec);
                        let artifacts = serde_json::json!({
                            "capture": {
                                "active": true,
                                "path": out_path.display().to_string(),
                                "format": fmt,
                                "width": w,
                                "height": h,
                                "fps": fps,
                                "stride": stride,
                                "with_thought": with_thought,
                                "audio": has_audio,
                            }
                        });
                        let log = vec![format!(
                            "[capture] started {} ({w}x{h}, {fps}fps, stride={stride}, format={fmt}, audio={has_audio})",
                            out_path.display()
                        )];
                        ExecOutcome::Done(self.ok_reply(host, trimmed, false, artifacts, log))
                    }
                    Err(e) => ExecOutcome::Done(self.err_reply(host, trimmed, &e)),
                }
            }

            #[cfg(feature = "disk")]
            Command::CaptureStop => {
                let Some(rec) = self.capture.take() else {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        "no active capture session",
                    ));
                };
                match rec.finish() {
                    Ok(sum) => {
                        let artifacts = serde_json::json!({
                            "capture": {
                                "active": false,
                                "path": sum.path.display().to_string(),
                                "format": sum.format,
                                "frames": sum.frames,
                                "ticks": sum.ticks,
                                "width": sum.width,
                                "height": sum.height,
                                "fps": sum.fps,
                                "stride": sum.stride,
                            }
                        });
                        let log = vec![format!(
                            "[capture] saved {} ({} frames from {} ticks, {}x{} @ {}fps)",
                            sum.path.display(),
                            sum.frames,
                            sum.ticks,
                            sum.width,
                            sum.height,
                            sum.fps
                        )];
                        ExecOutcome::Done(self.ok_reply(host, trimmed, false, artifacts, log))
                    }
                    Err(e) => ExecOutcome::Done(self.err_reply(host, trimmed, &e)),
                }
            }

            #[cfg(feature = "disk")]
            Command::CaptureStatus => {
                let (art, msg) = if let Some(rec) = &self.capture {
                    (
                        serde_json::json!({
                            "active": true,
                            "path": rec.out_path().display().to_string(),
                            "format": rec.format(),
                            "frames": rec.frames_written(),
                            "ticks": rec.ticks_seen(),
                            "fps": rec.fps(),
                            "stride": rec.stride(),
                        }),
                        format!(
                            "[capture] active: {} ({} frames / {} ticks, {}fps)",
                            rec.out_path().display(),
                            rec.frames_written(),
                            rec.ticks_seen(),
                            rec.fps()
                        ),
                    )
                } else {
                    (
                        serde_json::json!({ "active": false }),
                        "[capture] inactive".to_string(),
                    )
                };
                ExecOutcome::Done(self.ok_reply(
                    host,
                    trimmed,
                    false,
                    serde_json::json!({ "capture": art }),
                    vec![msg],
                ))
            }

            #[cfg(feature = "disk")]
            Command::Golden {
                ref_path,
                max_diff,
                tolerance,
                diff_out,
            } => {
                let ref_resolved = if origin.is_socket() {
                    match confine_socket_path(host.out_dir(), &ref_path) {
                        Ok(pb) => pb.to_string_lossy().into_owned(),
                        Err(e) => {
                            return ExecOutcome::Done(self.err_reply_code(
                                host,
                                trimmed,
                                "path_forbidden",
                                &e,
                            ));
                        }
                    }
                } else {
                    ref_path
                };
                let diff_pb = match diff_out {
                    Some(p) => match resolve_write_path_for_origin(
                        host.out_dir(),
                        &p,
                        origin.is_socket(),
                        &["png"],
                    ) {
                        Ok(pb) => Some(pb),
                        Err(e) => {
                            return ExecOutcome::Done(self.err_reply_code(
                                host,
                                trimmed,
                                "path_forbidden",
                                &e,
                            ));
                        }
                    },
                    None => None,
                };
                host.render_settled_frame();
                match compare_with_golden_png_fb(
                    &host.runtime().compositor.fb,
                    &ref_resolved,
                    diff_pb.as_deref(),
                    tolerance,
                ) {
                    Ok(rep) => {
                        let passed = rep.diff_pct <= max_diff;
                        if !passed {
                            self.golden_failed_any = true;
                        }
                        let artifacts = serde_json::json!({
                            "golden": {
                                "diff_pixels": rep.diff_pixels,
                                "total_pixels": rep.total_pixels,
                                "diff_pct": rep.diff_pct,
                                "mae": rep.mean_abs_error,
                                "max_diff_pct": max_diff,
                                "passed": passed,
                            }
                        });
                        let log = vec![format!(
                            "[golden] diff={}/{} px ({:.4}%, MAE={:.3}) -> {}",
                            rep.diff_pixels,
                            rep.total_pixels,
                            rep.diff_pct,
                            rep.mean_abs_error,
                            if passed { "PASS" } else { "FAIL" }
                        )];
                        let mut reply = self.make_reply(
                            host,
                            passed,
                            trimmed,
                            false,
                            artifacts,
                            log,
                            None,
                            if passed {
                                None
                            } else {
                                Some("golden diff exceeded max_diff_pct")
                            },
                            if passed { None } else { Some("golden_mismatch") },
                        );
                        reply.golden_failed = !passed;
                        ExecOutcome::Done(reply)
                    }
                    Err(e) => ExecOutcome::Done(self.err_reply(host, trimmed, &e)),
                }
            }

            #[cfg(feature = "disk")]
            Command::CoreReplay { path, until_tick } => {
                let replay_path = if origin.is_socket() {
                    match confine_socket_path(host.out_dir(), &path) {
                        Ok(pb) => pb.to_string_lossy().into_owned(),
                        Err(e) => {
                            return ExecOutcome::Done(self.err_reply_code(
                                host,
                                trimmed,
                                "path_forbidden",
                                &e,
                            ));
                        }
                    }
                } else {
                    path.clone()
                };
                host.on_session_transition(true, false);
                let assets_root = host.assets_root().to_path_buf();
                match run_core_replay_rt(host.runtime_mut(), &assets_root, &replay_path, until_tick) {
                    Ok(_) => {
                        host.service_host_cmds();
                        ExecOutcome::Done(self.ok_reply(
                            host,
                            trimmed,
                            false,
                            serde_json::json!({}),
                            vec![format!("[core-replay] replayed {replay_path}")],
                        ))
                    }
                    Err(e) => ExecOutcome::Done(self.err_reply(host, trimmed, &e)),
                }
            }

            #[cfg(feature = "disk")]
            Command::DumpState(opt_path) => {
                let state_json = host
                    .runtime()
                    .game
                    .as_ref()
                    .map(|g| g.canonical_state())
                    .unwrap_or(serde_json::Value::Null);
                let mut log = Vec::new();
                let mut artifacts = serde_json::json!({});
                if let Some(p) = opt_path {
                    let out_path = match resolve_write_path_for_origin(
                        host.out_dir(),
                        &p,
                        origin.is_socket(),
                        &["json"],
                    ) {
                        Ok(pb) => pb,
                        Err(e) => {
                            return ExecOutcome::Done(self.err_reply_code(
                                host,
                                trimmed,
                                "path_forbidden",
                                &e,
                            ));
                        }
                    };
                    if let Some(parent) = out_path.parent() {
                        if !parent.as_os_str().is_empty() {
                            let _ = fs::create_dir_all(parent);
                        }
                    }
                    if let Err(e) = fs::write(
                        &out_path,
                        serde_json::to_string_pretty(&state_json).unwrap_or_default(),
                    ) {
                        return ExecOutcome::Done(self.err_reply(
                            host,
                            trimmed,
                            &format!("write dump-state: {e}"),
                        ));
                    }
                    log.push(format!("[dump-state] wrote {}", out_path.display()));
                } else {
                    artifacts = serde_json::json!({ "canonical_state": state_json });
                }
                ExecOutcome::Done(self.ok_reply(host, trimmed, false, artifacts, log))
            }

            Command::NewGame(seed) => {
                host.on_session_transition(true, true);
                if let Some(seed) = seed {
                    host.runtime_mut().new_game(seed);
                    host.service_host_cmds();
                } else {
                    host.handle_ui_action(UiAction::NewGame);
                    host.service_host_cmds();
                }
                self.apply_god_mode(host);
                self.active_task = Some(ActiveTask::settle(
                    trimmed,
                    self.pace_ticks.min(10),
                    240,
                    true,
                    vec!["[new] started new game".to_string()],
                ));
                ExecOutcome::Pending
            }

            #[cfg(feature = "disk")]
            Command::Save(path) => {
                let mode = current_mode(host.runtime());
                if mode != "explore" {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        &format!("cannot save in mode '{mode}' (only allowed in explore mode)"),
                    ));
                }
                let Some(json_str) = host.runtime().save_json_str("text_mode", "repal") else {
                    return ExecOutcome::Done(self.err_reply(host, trimmed, "no active game to save"));
                };
                if let Err(e) = self.check_remote_write_quota(origin, json_str.len() as u64) {
                    return ExecOutcome::Done(self.err_reply_code(host, trimmed, "quota_exceeded", &e));
                }
                let out_path = match resolve_write_path_for_origin(
                    host.out_dir(),
                    &path,
                    origin.is_socket(),
                    &["json"],
                ) {
                    Ok(pb) => pb,
                    Err(e) => {
                        return ExecOutcome::Done(self.err_reply_code(
                            host,
                            trimmed,
                            "path_forbidden",
                            &e,
                        ));
                    }
                };
                if let Some(parent) = out_path.parent() {
                    if !parent.as_os_str().is_empty() {
                        let _ = fs::create_dir_all(parent);
                    }
                }
                let byte_len = json_str.len() as u64;
                if let Err(e) = fs::write(&out_path, json_str) {
                    return ExecOutcome::Done(self.err_reply(host, trimmed, &format!("save failed: {e}")));
                }
                self.record_remote_write(origin, byte_len);
                let out_str = out_path.display().to_string();
                ExecOutcome::Done(self.ok_reply(
                    host,
                    trimmed,
                    false,
                    serde_json::json!({ "save": out_str }),
                    vec![format!("[save] wrote {out_str}")],
                ))
            }

            #[cfg(feature = "disk")]
            Command::Load(path) => {
                // Socket clients are confined to `out_dir` (and bare names in `assets_root/orig_saves`, #193).
                let resolved = if origin.is_socket() {
                    match confine_socket_path(host.out_dir(), &path) {
                        Ok(in_out) if in_out.is_file() => in_out,
                        Ok(in_out) => {
                            let in_orig = confine_socket_path(&host.assets_root().join("orig_saves"), &path);
                            match in_orig {
                                Ok(orig) if orig.is_file() => orig,
                                _ => in_out,
                            }
                        }
                        Err(e) => {
                            return ExecOutcome::Done(self.err_reply_code(
                                host,
                                trimmed,
                                "path_forbidden",
                                &e,
                            ));
                        }
                    }
                } else {
                    // Check `out_dir` first so `save cp.json; load cp.json` is symmetric under `--out-dir` (#175).
                    let in_out = resolve_out_path(host.out_dir(), &path);
                    if in_out.is_file() {
                        in_out
                    } else {
                        resolve_save_path(host.assets_root(), Path::new(&path))
                    }
                };
                if let Err(e) = load_save_into_rt(host.runtime_mut(), &resolved) {
                    return ExecOutcome::Done(self.err_reply(host, trimmed, &e));
                }
                host.on_session_transition(true, true);
                self.apply_god_mode(host);
                host.service_host_cmds();
                host.on_transition_banner(&format!("读取存档 · {path}"), 1500);
                self.active_task = Some(ActiveTask::settle(
                    trimmed,
                    0,
                    120,
                    true,
                    vec![format!("[load] loaded {}", resolved.display())],
                ));
                ExecOutcome::Pending
            }

            #[cfg(feature = "disk")]
            Command::ExportReplay(path) => {
                if let Err(e) = self.check_remote_write_quota(origin, 256 * 1024) {
                    return ExecOutcome::Done(self.err_reply_code(host, trimmed, "quota_exceeded", &e));
                }
                let Some(bundle) = host
                    .runtime()
                    .game
                    .as_ref()
                    .map(|g| g.export_replay_bundle())
                else {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        "no active game to export replay from",
                    ));
                };
                if bundle.segments.is_empty() {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        "no recorded replay segments available",
                    ));
                }
                let out_path = match resolve_write_path_for_origin(
                    host.out_dir(),
                    &path,
                    origin.is_socket(),
                    &["gz", "repal"],
                ) {
                    Ok(pb) => pb,
                    Err(e) => {
                        return ExecOutcome::Done(self.err_reply_code(
                            host,
                            trimmed,
                            "path_forbidden",
                            &e,
                        ));
                    }
                };
                if let Some(parent) = out_path.parent() {
                    if !parent.as_os_str().is_empty() {
                        let _ = fs::create_dir_all(parent);
                    }
                }
                if let Err(e) = pal_runtime::repal_archive::write_repal_archive(&out_path, &bundle) {
                    return ExecOutcome::Done(self.err_reply(
                        host,
                        trimmed,
                        &format!("replay export failed: {e}"),
                    ));
                }
                let written = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
                self.record_remote_write(origin, written);
                let out_str = out_path.display().to_string();
                ExecOutcome::Done(self.ok_reply(
                    host,
                    trimmed,
                    false,
                    serde_json::json!({
                        "replay": {
                            "path": out_str,
                            "segments": bundle.segments.len(),
                            "total_ticks": bundle.total_ticks,
                            "engine_version": bundle.engine_version,
                            "tainted_by_debug": bundle.tainted_by_debug,
                            "debug_mode": bundle.debug_mode,
                        }
                    }),
                    vec![format!(
                        "[replay-export] wrote {} ({} segs, {} ticks)",
                        out_path.display(),
                        bundle.segments.len(),
                        bundle.total_ticks
                    )],
                ))
            }

            #[cfg(not(feature = "disk"))]
            Command::Shot { .. }
            | Command::CaptureStart { .. }
            | Command::CaptureStop
            | Command::CaptureStatus
            | Command::Golden { .. }
            | Command::CoreReplay { .. }
            | Command::DumpState(_)
            | Command::Save(_)
            | Command::Load(_)
            | Command::ExportReplay(_) => ExecOutcome::Done(self.err_reply(
                host,
                trimmed,
                "this disk/file command is not available in web wasm",
            )),

            Command::Think(opt_text) => {
                if host.runtime().game.is_none() {
                    return ExecOutcome::Done(self.err_reply(host, trimmed, "game not initialized"));
                }
                match opt_text {
                    None => {
                        host.runtime_mut().set_ai_thought(None);
                        let res = host
                            .runtime_mut()
                            .game
                            .as_mut()
                            .map(|g| g.record_ai_thought(String::new()))
                            .unwrap_or(pal_core::game::replay_rec::AnnotationRecordResult {
                                recorded: false,
                                coalesced: false,
                            });
                        ExecOutcome::Done(self.ok_reply(
                            host,
                            trimmed,
                            false,
                            serde_json::json!({
                                "think": {
                                    "text": serde_json::Value::Null,
                                    "char_count": 0,
                                    "truncated": false,
                                    "shown": false,
                                    "coalesced": res.coalesced,
                                    "recorded": res.recorded,
                                }
                            }),
                            vec!["[think] cleared".to_string()],
                        ))
                    }
                    Some(raw_text) => {
                        let (clean, truncated) =
                            pal_core::game::replay_rec::sanitize_ai_thought(&raw_text);
                        if clean.is_empty() {
                            return ExecOutcome::Done(self.err_reply_code(
                                host,
                                trimmed,
                                "invalid_args",
                                "think text is empty after sanitization (avoid emoji-only strings)",
                            ));
                        }
                        let char_count = clean.chars().count();
                        // Compare against live `rt.ui.ai_thought` so repeating a thought after a
                        // scene-change auto-clear still records a fresh annotation (#250).
                        let is_dup = host.runtime().ui.ai_thought.as_deref() == Some(clean.as_str());
                        host.runtime_mut().set_ai_thought(Some(clean.clone()));
                        let res = if is_dup {
                            pal_core::game::replay_rec::AnnotationRecordResult {
                                recorded: false,
                                coalesced: true,
                            }
                        } else {
                            host.runtime_mut()
                                .game
                                .as_mut()
                                .map(|g| g.record_ai_thought(clean.clone()))
                                .unwrap_or(pal_core::game::replay_rec::AnnotationRecordResult {
                                    recorded: false,
                                    coalesced: false,
                                })
                        };
                        ExecOutcome::Done(self.ok_reply(
                            host,
                            trimmed,
                            false,
                            serde_json::json!({
                                "think": {
                                    "text": clean,
                                    "char_count": char_count,
                                    "truncated": truncated,
                                    "shown": true,
                                    "coalesced": res.coalesced,
                                    "recorded": res.recorded,
                                }
                            }),
                            vec![format!("[AI] {clean}")],
                        ))
                    }
                }
            }

            Command::BugReport(desc) => match host.export_bugreport(&desc) {
                Ok(info) => {
                    let msg = format!("[bugreport] exported ({desc})");
                    ExecOutcome::Done(self.ok_reply(
                        host,
                        trimmed,
                        false,
                        serde_json::json!({ "bugreport": info }),
                        vec![msg],
                    ))
                }
                Err(e) => ExecOutcome::Done(self.err_reply(host, trimmed, &e)),
            },

            // Debug commands (routed through `pal-core::Game::apply_debug_action` with explicit
            // `debug_mode` gate + `DEBUG ENABLED` watermark and first-class deterministic
            // `ReplayEvent::Debug` recording, #235; validated BEFORE enabling `debug_mode`, #246).
            other => self.submit_debug(host, trimmed, other),
        }
    }
}
