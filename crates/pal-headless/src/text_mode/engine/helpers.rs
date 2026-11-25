//! Shared geometry, target-selection, path-confinement, and save-loading helpers for `TextEngine`.

use super::*;

pub(super) fn leader_scene_pos(rt: &pal_runtime::PalRuntime) -> (u16, (i32, i32)) {
    rt.game
        .as_ref()
        .map(|g| {
            let pos = g
                .world
                .party
                .first()
                .map(|p| (p.x, p.y))
                .unwrap_or((0, 0));
            (g.world.scene_id, pos)
        })
        .unwrap_or((0, (0, 0)))
}

/// For `goto` (`talk_after == false`), ensure the world click coordinate does not fall inside any
/// interactive event's 32px click-to-talk hit circle (`find_event_near_point`), choosing the nearest
/// walkable half-tile just outside all interactive hit circles (#240, #244).
pub(super) fn pick_non_talking_click_target(rt: &pal_runtime::PalRuntime, tx: i32, ty: i32) -> (i32, i32) {
    let Some(g) = rt.game.as_ref() else {
        return (tx, ty);
    };
    let hits_interactive = |wx: i32, wy: i32| -> bool {
        g.world.current_event_ids().into_iter().any(|id| {
            g.world.event(id).is_some_and(|ev| {
                ev.state > 0
                    && ev.vanish_time <= 0
                    && !ev.trigger.is_none()
                    && (1..=3).contains(&ev.trigger_mode)
                    && ((ev.x - wx) as f64).hypot((ev.y - 10 - wy) as f64) < 32.0
            })
        })
    };
    if !hits_interactive(tx, ty) {
        return (tx, ty);
    }
    let lead = g.world.leader();
    let (bx, by, bh) = pal_core::isometric::world_to_half_tile(tx, ty);
    let mut best: Option<(i32, i32, i32, i32)> = None;
    for dx in -3..=3i32 {
        for dy in -3..=3i32 {
            for h in 0..=1i32 {
                let (hx, hy) = (bx + dx, by + dy);
                if !(0..64).contains(&hx) || !(0..128).contains(&hy) {
                    continue;
                }
                if g.base_grid().is_blocked(hx, hy, h) {
                    continue;
                }
                let (wx, wy) = pal_core::isometric::half_tile_to_world(hx, hy, h);
                if hits_interactive(wx, wy) {
                    continue;
                }
                let blocked_by_solid = g.world.current_event_ids().into_iter().any(|id| {
                    g.world.event(id).is_some_and(|e| {
                        e.state >= 2
                            && e.vanish_time <= 0
                            && pal_core::isometric::world_to_half_tile(e.x, e.y) == (hx, hy, h)
                    })
                });
                if blocked_by_solid {
                    continue;
                }
                let d_target = (wx - tx).abs() + (wy - ty).abs() * 2;
                let d_lead = (wx - lead.x).abs() + (wy - lead.y).abs() * 2;
                if best.is_none_or(|(bt, bl, _, _)| (d_target, d_lead) < (bt, bl)) {
                    best = Some((d_target, d_lead, wx, wy));
                }
            }
        }
    }
    let _ = bh;
    best.map(|(_, _, wx, wy)| (wx, wy)).unwrap_or((tx, ty))
}

/// Find visible solid/deflecting events genuinely blocking `goto` between `stop_pos` and `target_xy`
/// (#278, #4). Verifies via scratch-grid A* that clearing the candidate blocker events actually makes
/// `target_xy` reachable, preventing false-positive blocker hints when the target is on another floor
/// or disconnected region.
///
/// Strictly fair-safe: in `fair` mode, only reports events whose sprites are visibly rendered inside
/// the current `320x200` viewport, using visual-only labels (never script/trigger internals).
pub(super) fn find_goto_blockers(
    rt: &pal_runtime::PalRuntime,
    stop_pos: (i32, i32),
    target_xy: (i32, i32),
    target_ev_id: Option<u16>,
    fair: bool,
    deflected_and_failed: bool,
    is_touch_exit: bool,
) -> Vec<(u16, String, String)> {
    let Some(g) = rt.game.as_ref() else {
        return Vec::new();
    };
    let (cam_x, cam_y) = runtime_camera(rt);
    let (sx, sy) = stop_pos;
    let (tx, ty) = target_xy;
    let iso_dist = |ax: i32, ay: i32, bx: i32, by: i32| (ax - bx).abs() + (ay - by).abs() * 2;
    let seg_dx = (tx - sx) as f64;
    let seg_dy = ((ty - sy) * 2) as f64;
    let seg_len_sq = seg_dx * seg_dx + seg_dy * seg_dy;

    let mut hits: Vec<(i32, i32, u16, String, String)> = Vec::new();
    for id in g.world.current_event_ids() {
        if Some(id) == target_ev_id {
            continue;
        }
        let Some(ev) = g.world.event(id) else {
            continue;
        };
        if ev.state <= 0 || ev.vanish_time > 0 || ev.sprite_id == 0 {
            continue;
        }
        if fair && !event_visible_in_viewport(rt, ev, cam_x, cam_y) {
            continue;
        }
        let in_click_circle = ((ev.x - tx) as f64).hypot((ev.y - 10 - ty) as f64) < 32.0;
        let is_solid = ev.state >= 2;
        if !is_solid && !(deflected_and_failed && in_click_circle) {
            continue;
        }
        let d_target = iso_dist(ev.x, ev.y, tx, ty);
        let d_stop = iso_dist(ev.x, ev.y, sx, sy);
        let near_target = d_target <= 48 || in_click_circle;
        let on_segment = if seg_len_sq > 1.0 {
            let px = (ev.x - sx) as f64;
            let py = ((ev.y - sy) * 2) as f64;
            let t = (px * seg_dx + py * seg_dy) / seg_len_sq;
            let t_clamped = t.clamp(0.0, 1.0);
            let perp = (px - t_clamped * seg_dx).hypot(py - t_clamped * seg_dy);
            (-0.15..=1.15).contains(&t) && perp <= 32.0
        } else {
            d_stop <= 48
        };
        if !near_target && !on_segment {
            continue;
        }
        let (_kind, kind_label, hint) = crate::text_mode::response::describe_event(rt, ev, fair);
        hits.push((d_target, d_stop, ev.id, kind_label.to_string(), hint));
    }
    hits.sort_by_key(|(dt, ds, id, _, _)| (*dt, *ds, *id));
    hits.truncate(6);
    if hits.is_empty() {
        return Vec::new();
    }

    // Re-run A* on a scratch grid with candidate blockers cleared: only blame them if removing
    // them actually connects `stop_pos` to `target_xy` (fixes #4 cross-floor false positives).
    let mut scratch = g.grid.clone();
    for (_, _, id, _, _) in &hits {
        if let Some(ev) = g.world.event(*id) {
            let (hx, hy, h) = pal_core::isometric::world_to_half_tile(ev.x, ev.y);
            scratch.set_blocked(hx, hy, h, g.base_grid().is_blocked(hx, hy, h));
        }
    }
    let path = pal_core::isometric::find_path_world(&scratch, sx, sy, tx, ty, 200);
    let end = path.last().copied().unwrap_or((sx, sy));
    let end_dist = iso_dist(end.0, end.1, tx, ty);
    let cur_dist = iso_dist(sx, sy, tx, ty);
    let reach_thresh = if is_touch_exit { 16 } else { 48 };
    if end_dist > reach_thresh || end_dist >= cur_dist {
        return Vec::new();
    }

    hits.into_iter()
        .map(|(_, _, id, label, hint)| (id, label, hint))
        .collect()
}

/// Returns `true` when `target_xy` cannot be reached from `from_pos` even on the static `base_grid`
/// (i.e. different floor or walled-off region connected only via stairs/portals, #4).
pub(super) fn is_target_region_disconnected(
    rt: &pal_runtime::PalRuntime,
    from_pos: (i32, i32),
    target_xy: (i32, i32),
    is_touch_exit: bool,
) -> bool {
    let Some(g) = rt.game.as_ref() else {
        return false;
    };
    let path = pal_core::isometric::find_path_world(
        g.base_grid(),
        from_pos.0,
        from_pos.1,
        target_xy.0,
        target_xy.1,
        250,
    );
    let end = path.last().copied().unwrap_or(from_pos);
    let end_dist = (end.0 - target_xy.0).abs() + (end.1 - target_xy.1).abs() * 2;
    let thresh = if is_touch_exit { 16 } else { 48 };
    end_dist > thresh
}

/// List visible exits/stairs reachable from `from_pos` on the current floor/region (#4).
/// Strictly fair-safe: in `fair` mode, only surfaces exits/stairs already visible in the viewport.
pub(super) fn find_reachable_exits_and_stairs(
    rt: &pal_runtime::PalRuntime,
    from_pos: (i32, i32),
    fair: bool,
) -> Vec<(u16, String)> {
    let Some(g) = rt.game.as_ref() else {
        return Vec::new();
    };
    let (cam_x, cam_y) = runtime_camera(rt);
    let mut found: Vec<(i32, u16, String)> = Vec::new();
    for id in g.world.current_event_ids() {
        let Some(ev) = g.world.event(id) else {
            continue;
        };
        if ev.state <= 0 || ev.vanish_time > 0 || ev.sprite_id != 0 {
            continue;
        }
        let is_exit = if fair {
            crate::text_mode::response::is_fair_visible_exit(rt, ev, cam_x, cam_y)
        } else {
            crate::text_mode::response::is_exit_or_stair_event(rt, ev, false)
        };
        if !is_exit {
            continue;
        }
        let path = pal_core::isometric::find_path_world(
            &g.grid,
            from_pos.0,
            from_pos.1,
            ev.x,
            ev.y,
            160,
        );
        let end = path.last().copied().unwrap_or(from_pos);
        let end_dist = (end.0 - ev.x).abs() + (end.1 - ev.y).abs() * 2;
        if end_dist <= 32 {
            let d = (ev.x - from_pos.0).abs() + (ev.y - from_pos.1).abs() * 2;
            let dx = ev.x - from_pos.0;
            let dy = ev.y - from_pos.1;
            let dir = match (dx >= 0, dy >= 0) {
                (true, true) => "se",
                (true, false) => "ne",
                (false, true) => "sw",
                (false, false) => "nw",
            };
            found.push((d, ev.id, dir.to_string()));
        }
    }
    found.sort_by_key(|(d, id, _)| (*d, *id));
    found.truncate(4);
    found.into_iter().map(|(_, id, dir)| (id, dir)).collect()
}

pub(super) fn format_goto_blockers_hint(blockers: &[(u16, String, String)]) -> String {
    if blockers.is_empty() {
        return String::new();
    }
    let list = blockers
        .iter()
        .map(|(id, label, hint)| {
            if !hint.is_empty() && !hint.starts_with("sprite=") {
                format!("#{id} ({label}: {hint})")
            } else {
                format!("#{id} ({label})")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let first_id = blockers[0].0;
    format!("blocked by {list}; try 'talk {first_id}'")
}

pub(super) fn first_alive_enemy(rt: &pal_runtime::PalRuntime) -> usize {
    rt.game
        .as_ref()
        .and_then(|g| g.battle.as_ref())
        .and_then(|b| b.enemies.iter().position(|e| e.hp > 0))
        .unwrap_or(0)
}

/// Pick the default target slot for `magic <id>` when the caller omits `[slot]`:
/// - Offensive spells (`target_enemy == true`): first alive enemy slot.
/// - Support/healing spells (`target_enemy == false`): KO'd ally for revive spells, otherwise the
///   alive ally with the lowest HP ratio (tie-breaking toward the casting hero).
pub fn default_magic_target(rt: &pal_runtime::PalRuntime, magic_id: u16) -> usize {
    let Some(g) = rt.game.as_ref() else {
        return 0;
    };
    let target_enemy = g
        .data
        .magics
        .get(&magic_id)
        .map(|m| m.target_enemy)
        .unwrap_or(true);
    if target_enemy {
        return first_alive_enemy(rt);
    }
    let Some(b) = g.battle.as_ref() else {
        return 0;
    };
    if matches!(magic_id, 301 | 302) {
        if let Some(ko_idx) = b
            .heroes
            .iter()
            .position(|h| g.world.roles.get(h.role).is_some_and(|r| r.hp <= 0))
        {
            return ko_idx;
        }
    }
    let fallback = b.prompting.unwrap_or(0);
    let mut best_idx = fallback;
    let mut best_ratio = f64::INFINITY;
    for (idx, h) in b.heroes.iter().enumerate() {
        let Some(r) = g.world.roles.get(h.role) else {
            continue;
        };
        if r.hp <= 0 {
            continue;
        }
        let ratio = (r.hp as f64) / (r.max_hp.max(1) as f64);
        if ratio < best_ratio - 1e-9 || ((ratio - best_ratio).abs() <= 1e-9 && idx == fallback) {
            best_ratio = ratio;
            best_idx = idx;
        }
    }
    best_idx
}

pub(super) fn logical_to_world(rt: &pal_runtime::PalRuntime, lx: i32, ly: i32) -> (i32, i32) {
    let (cx, cy) = runtime_camera(rt);
    (cx + lx.clamp(0, 319), cy + ly.clamp(0, 199))
}

pub(super) fn resolve_out_path(out_dir: &Path, raw: &str) -> PathBuf {
    let pb = PathBuf::from(raw);
    if pb.is_absolute() {
        pb
    } else {
        out_dir.join(pb)
    }
}

/// Confine a socket-supplied file path strictly inside `base_dir` (#193):
/// rejects absolute paths, OS prefixes, `..` traversal, empty names, and symlink escapes.
pub fn confine_socket_path(base_dir: &Path, raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty path is not allowed over sockets".to_string());
    }
    let rel = Path::new(trimmed);
    if rel.is_absolute() {
        return Err(format!(
            "absolute path '{trimmed}' is forbidden over TCP sockets (use a relative filename)"
        ));
    }
    let mut joined = base_dir.to_path_buf();
    let mut has_normal = false;
    for comp in rel.components() {
        match comp {
            Component::Normal(seg) => {
                joined.push(seg);
                has_normal = true;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "path traversal '{trimmed}' is forbidden over TCP sockets"
                ));
            }
        }
    }
    if !has_normal {
        return Err(format!("invalid relative path '{trimmed}'"));
    }
    // Check canonical containment on the deepest existing ancestor (even when `base_dir`
    // itself hasn't been created yet) to prevent symlink escapes (#193, #262).
    #[cfg(feature = "disk")]
    {
        let deepest_existing = |mut p: &Path| -> Option<PathBuf> {
            loop {
                if p.exists() {
                    return fs::canonicalize(p).ok();
                }
                match p.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => p = parent,
                    _ => return None,
                }
            }
        };
        if let Some(canon_base) = deepest_existing(base_dir) {
            if let Some(canon_probe) = deepest_existing(&joined) {
                if !canon_probe.starts_with(&canon_base) {
                    return Err(format!(
                        "symlink path '{trimmed}' escapes allowed directory"
                    ));
                }
            }
        }
    }
    Ok(joined)
}

pub(super) fn is_debug_mutation_busy<H: TextHost>(host: &H) -> bool {
    host.runtime()
        .game
        .as_ref()
        .is_none_or(|g| g.is_busy() || g.battle.as_ref().is_some_and(|b| b.active))
}

/// Confine a socket-supplied write path inside `base_dir` AND require a safe allowlisted
/// extension (`.json` / `.png` / `.repal`), blocking writes to dotfiles or arbitrary targets (#236).
pub fn confine_socket_write_path(
    base_dir: &Path,
    raw: &str,
    allowed_exts: &[&str],
) -> Result<PathBuf, String> {
    let pb = confine_socket_path(base_dir, raw)?;
    let ext = pb
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    let allowed = ext
        .as_deref()
        .is_some_and(|e| allowed_exts.iter().any(|a| a.eq_ignore_ascii_case(e)));
    if !allowed {
        return Err(format!(
            "socket write path '{}' must have one of allowed extensions: {}",
            raw.trim(),
            allowed_exts.join(", ")
        ));
    }
    Ok(pb)
}

pub(super) fn resolve_write_path_for_origin(
    out_dir: &Path,
    raw: &str,
    is_socket: bool,
    allowed_exts: &[&str],
) -> Result<PathBuf, String> {
    if is_socket {
        confine_socket_write_path(out_dir, raw, allowed_exts)
    } else {
        Ok(resolve_out_path(out_dir, raw))
    }
}

pub(super) fn party_idx_to_role_id(rt: &pal_runtime::PalRuntime, party_idx: usize) -> Option<usize> {
    rt.game
        .as_ref()?
        .world
        .party
        .get(party_idx)
        .map(|m| m.role_id as usize)
}

fn names_match(query: &str, candidate: &str) -> bool {
    let q = query.trim();
    let c = candidate.trim();
    !q.is_empty() && !c.is_empty() && (q == c || q.eq_ignore_ascii_case(c))
}

/// Resolve a `TargetRef` (numeric ID or localized name) to a magic ID, preferring spells known
/// by `prefer_role` (if given), then any party member, then the full magic catalog (#276).
pub(super) fn resolve_magic_ref(
    rt: &pal_runtime::PalRuntime,
    target: &crate::text_mode::command::TargetRef,
    prefer_role: Option<usize>,
) -> Result<u16, (&'static str, String)> {
    match target {
        crate::text_mode::command::TargetRef::Id(id) => Ok(*id),
        crate::text_mode::command::TargetRef::Name(name) => {
            let Some(g) = rt.game.as_ref() else {
                return Err(("wrong_mode", "game not initialized".to_string()));
            };
            let matches_magic = |mid: u16| -> bool {
                if names_match(name, &g.magic_display_name(mid)) {
                    return true;
                }
                g.data.magics.get(&mid).is_some_and(|m| {
                    names_match(name, &m.name) || names_match(name, &m.name_hant)
                })
            };
            if let Some(rid) = prefer_role {
                if let Some(r) = g.world.roles.get(rid) {
                    if let Some(&mid) = r.magics.iter().find(|&&mid| mid > 0 && matches_magic(mid)) {
                        return Ok(mid);
                    }
                }
            }
            for pm in &g.world.party {
                if let Some(r) = g.world.roles.get(pm.role_id as usize) {
                    if let Some(&mid) = r.magics.iter().find(|&&mid| mid > 0 && matches_magic(mid)) {
                        return Ok(mid);
                    }
                }
            }
            let mut all_ids: Vec<u16> = g.data.magics.keys().copied().collect();
            all_ids.sort_unstable();
            if let Some(mid) = all_ids.into_iter().find(|&mid| matches_magic(mid)) {
                return Ok(mid);
            }
            Err((
                "magic_not_known",
                format!("unknown magic '{name}' (run 'status' to list known spells with #ID and effects)"),
            ))
        }
    }
}

/// Resolve a `TargetRef` (numeric ID or localized name) to an item ID, preferring items currently
/// in inventory (or pawnshop list) when `prefer_inventory` is true, then falling back to the full
/// item catalog so errors can clearly distinguish "not in bag" from "unknown item" (#276).
pub(super) fn resolve_item_ref(
    rt: &pal_runtime::PalRuntime,
    target: &crate::text_mode::command::TargetRef,
    prefer_inventory: bool,
) -> Result<u16, (&'static str, String)> {
    match target {
        crate::text_mode::command::TargetRef::Id(id) => Ok(*id),
        crate::text_mode::command::TargetRef::Name(name) => {
            let Some(g) = rt.game.as_ref() else {
                return Err(("wrong_mode", "game not initialized".to_string()));
            };
            let matches_item = |iid: u16| -> bool {
                if names_match(name, &g.item_display_name(iid)) {
                    return true;
                }
                g.data.items.get(&iid).is_some_and(|it| {
                    names_match(name, &it.name) || names_match(name, &it.name_hant)
                })
            };
            if prefer_inventory {
                let mut inv_ids: Vec<u16> = g
                    .world
                    .inventory
                    .iter()
                    .filter(|(_, &q)| q > 0)
                    .map(|(&id, _)| id)
                    .collect();
                inv_ids.sort_unstable();
                if let Some(iid) = inv_ids.into_iter().find(|&iid| matches_item(iid)) {
                    return Ok(iid);
                }
            }
            let mut all_ids: Vec<u16> = g.data.items.keys().copied().collect();
            all_ids.sort_unstable();
            if let Some(iid) = all_ids.into_iter().find(|&iid| matches_item(iid)) {
                return Ok(iid);
            }
            Err((
                "invalid_target",
                format!("unknown item '{name}' (run 'bag' to list inventory items with #ID and effects)"),
            ))
        }
    }
}

/// Validate that `item_id` exists in inventory and is usable or throwable in battle (#276).
pub(super) fn validate_battle_item(
    rt: &pal_runtime::PalRuntime,
    item_id: u16,
) -> Result<(), (&'static str, String)> {
    let Some(g) = rt.game.as_ref() else {
        return Err(("wrong_mode", "game not initialized".to_string()));
    };
    let Some(it) = g.data.items.get(&item_id) else {
        return Err(("invalid_target", format!("unknown item #{item_id}")));
    };
    let name = g.item_display_name(item_id);
    if g.world.item_count(item_id) <= 0 {
        return Err((
            "item_refused",
            format!("no {name} (#{item_id}) in bag"),
        ));
    }
    if !it.usable && !it.throwable {
        return Err((
            "item_refused",
            format!("item {name} (#{item_id}) cannot be used or thrown in battle"),
        ));
    }
    Ok(())
}

/// Flatten multi-line YAML/DOS item or magic descriptions into a single clean line for plain-text output.
pub(super) fn flatten_desc(raw: &str) -> String {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Format a concise plain-text summary line for an inventory item (with ID, tags, and effect desc, #276).
pub(super) fn format_bag_plain_line(
    g: &pal_core::game::Game,
    id: u16,
    qty: i32,
    lang: pal_core::Language,
) -> String {
    let name = g.item_display_name(id);
    let it = g.data.items.get(&id);
    let equip_slot = g.item_slot(id);
    let is_en = lang == pal_core::Language::En;
    let is_hant = lang == pal_core::Language::ZhHant;
    let mut tags: Vec<&str> = Vec::new();
    if let Some(x) = it {
        if x.usable && x.throwable {
            tags.push(if is_en { "use/throw" } else if is_hant { "可用/投擲" } else { "可用/投掷" });
        } else if x.usable {
            tags.push(if is_en { "usable" } else { "可用" });
        } else if x.throwable {
            tags.push(if is_en { "throw" } else if is_hant { "投擲" } else { "投掷" });
        }
        if equip_slot.is_some() {
            tags.push(if is_en { "equip" } else if is_hant { "裝備" } else { "装备" });
        }
        if x.usable || x.throwable {
            let tgt = match item_target_kind(x) {
                "enemy" => if is_en { "1 enemy" } else if is_hant { "敵單體" } else { "敌单体" },
                "all_enemies" => if is_en { "all enemies" } else if is_hant { "敵全體" } else { "敌全体" },
                "all_allies" => if is_en { "all allies" } else if is_hant { "我全體" } else { "我全体" },
                "ally" => if is_en { "1 ally" } else if is_hant { "我單體" } else { "我单体" },
                _ => "",
            };
            if !tgt.is_empty() {
                tags.push(tgt);
            }
        }
    }
    let tag_str = if tags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", tags.join("·"))
    };
    let desc = flatten_desc(&g.item_display_desc(id));
    if desc.is_empty() {
        format!("  #{id} {name} x{qty}{tag_str}")
    } else {
        format!("  #{id} {name} x{qty}{tag_str} — {desc}")
    }
}

/// Format a concise plain-text summary line for a learned spell (with ID, MP cost, tags, and effect desc, #276).
pub(super) fn format_magic_plain_line(
    g: &pal_core::game::Game,
    mid: u16,
    lang: pal_core::Language,
) -> String {
    let name = g.magic_display_name(mid);
    let md = g.data.magics.get(&mid);
    let cost = md.map(|m| m.cost_mp).unwrap_or(0);
    let is_en = lang == pal_core::Language::En;
    let is_hant = lang == pal_core::Language::ZhHant;
    let where_tag = if md.is_some_and(|m| m.usable_outside_battle) {
        if is_en { "field/battle" } else if is_hant { "野/戰" } else { "野/战" }
    } else if is_en {
        "battle"
    } else if is_hant {
        "戰"
    } else {
        "战"
    };
    let scope_tag = match md.map(magic_target_kind).unwrap_or("enemy") {
        "enemy" => if is_en { "1 enemy" } else if is_hant { "敵單體" } else { "敌单体" },
        "all_enemies" => if is_en { "all enemies" } else if is_hant { "敵全體" } else { "敌全体" },
        "all_allies" => if is_en { "all allies" } else if is_hant { "我全體" } else { "我全体" },
        _ => if is_en { "1 ally" } else if is_hant { "我單體" } else { "我单体" },
    };
    let desc = flatten_desc(&g.magic_display_desc(mid));
    if desc.is_empty() {
        format!("    #{mid} {name} ({cost}MP, {where_tag}·{scope_tag})")
    } else {
        format!("    #{mid} {name} ({cost}MP, {where_tag}·{scope_tag}) — {desc}")
    }
}

pub fn resolve_save_path(assets_root: &Path, p: &Path) -> PathBuf {
    if p.exists() {
        return p.to_path_buf();
    }
    let in_orig = assets_root.join("orig_saves").join(p);
    if in_orig.exists() {
        return in_orig;
    }
    p.to_path_buf()
}

#[cfg(feature = "disk")]
pub fn load_save_into_rt(rt: &mut pal_runtime::PalRuntime, path: &Path) -> Result<(), String> {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    {
        let text = fs::read_to_string(path).map_err(|e| format!("read save json: {e}"))?;
        if !rt.load_save_json_str(&text) {
            return Err(format!("invalid save json: {}", path.display()));
        }
        Ok(())
    } else {
        let bytes = fs::read(path).map_err(|e| format!("read rpg: {e}"))?;
        if !rt.load_dos_rpg(&bytes) {
            return Err(format!("invalid rpg save: {}", path.display()));
        }
        Ok(())
    }
}
