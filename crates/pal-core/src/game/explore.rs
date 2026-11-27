//! Exploration-mode logic: walking, click-to-walk, interaction and proximity triggers.
//! Port of the legacy `PalRemakeApp.frameLoop` / `checkProximityTriggers` (`site/js/main.js`).

use super::{Game, TickInput, AUTO_TICKS, MOVE_TICKS};
use crate::isometric::{direction_between, find_path_world, half_tile_to_world, step_half_tile, world_to_half_tile};
use crate::script::op;

/// Check whether any intermediate obstacle separates `(x0, y0)` from an interactive event at
/// `(x1, y1)`. Unlike walking LOS (`npc_ai::is_line_of_sight_clear`), the target event's own
/// half-tile is allowed to be blocked so props/NPCs placed on rocks, counters, or shorelines
/// (e.g. Scene 14 Ev#204 Ling'er's clothes at `(1104, 1432)`, #134) remain clickable.
fn is_interact_los_clear(grid: &crate::isometric::CollisionGrid, x0: i32, y0: i32, x1: i32, y1: i32) -> bool {
    let start_ht = world_to_half_tile(x0, y0);
    let end_ht = world_to_half_tile(x1, y1);
    let dx = (x1 - x0) as f64;
    let dy = (y1 - y0) as f64;
    let d = dx.hypot(dy);
    if d <= 0.0 {
        return true;
    }
    let samples = d.ceil().max(2.0) as i32;
    for i in 1..samples {
        let t = (i as f64) / (samples as f64);
        let px = ((x0 as f64) + dx * t).round() as i32;
        let py = ((y0 as f64) + dy * t).round() as i32;
        let ht = world_to_half_tile(px, py);
        if ht != start_ht && ht != end_ht && grid.is_blocked(ht.0, ht.1, ht.2) {
            return false;
        }
    }
    true
}

impl Game {
    pub(crate) fn explore_tick(&mut self, input: &TickInput) {
        if input.interact {
            self.interact_in_front();
        }
        if let Some((wx, wy)) = input.click {
            if self.fg.is_none() {
                self.click_world(wx, wy);
            }
        }
        if self.fg.is_some() {
            return;
        }

        if self.move_cd > 0 {
            self.move_cd -= 1;
        }
        if self.move_cd == 0 {
            if input.held_dir >= 0 {
                self.auto_walk.clear();
                self.pending_click_event = 0;
                self.held_step(input.held_dir as u8);
                self.move_cd = MOVE_TICKS;
            } else if let Some((wx, wy)) = input.steer {
                let lead = self.world.leader().clone();
                if ((wx - lead.x) as f64).hypot((wy - lead.y) as f64) > 12.0 {
                    self.auto_walk.clear();
                    self.pending_click_event = 0;
                    let dir = direction_between(lead.x, lead.y, wx, wy);
                    self.try_manual_step(dir);
                    self.move_cd = MOVE_TICKS;
                } else {
                    self.world.leader_mut().frame = 0;
                }
            } else if let Some((x, y)) = self.auto_walk.pop_front() {
                let lead = self.world.leader().clone();
                let dir = direction_between(lead.x, lead.y, x, y);
                self.world.step_party_to(x, y, dir);
                self.move_cd = MOVE_TICKS;
                self.check_proximity_triggers();
                if self.auto_walk.is_empty() && self.pending_click_event > 0 && self.fg.is_none() {
                    let id = std::mem::take(&mut self.pending_click_event);
                    self.talk_if_close(id, 56.0);
                }
            } else {
                self.world.leader_mut().frame = 0;
            }
        }
        if self.fg.is_some() {
            return;
        }

        if self.auto_cd > 0 {
            self.auto_cd -= 1;
        }
        if self.auto_cd == 0 {
            self.auto_cd = AUTO_TICKS;
            self.tick_auto_scripts();
        }
    }

    /// Face and trigger event `id` if it is within `max_dist` and no intermediate wall blocks it.
    fn talk_if_close(&mut self, id: u16, max_dist: f64) -> bool {
        let lead = self.world.leader().clone();
        let Some(ev) = self.world.event(id) else { return false };
        let (ex, ey, frames) = (ev.x, ev.y, ev.sprite_frames);
        let reachable = is_interact_los_clear(&self.grid, lead.x, lead.y, ex, ey);
        if ((ex - lead.x) as f64).hypot((ey - lead.y) as f64) <= max_dist && reachable {
            let dir = direction_between(lead.x, lead.y, ex, ey);
            self.world.leader_mut().dir = dir;
            if frames > 0 {
                if let Some(ev) = self.world.event_mut(id) {
                    ev.direction = ((dir + 2) & 3) as i32;
                }
            }
            self.trigger_event(id);
            return true;
        }
        false
    }

    /// Returns `true` if stepping from blocked tile `start_ht` (e.g. Scene #1 secret ladder bottom
    /// at `(640, 608)`) onto `(nx, ny, nh)` would strand the player inside a small eventless
    /// behind-the-wall pocket with no way back (#150).
    fn is_trapped_void_pocket(&self, start_ht: (i32, i32, i32), nx: i32, ny: i32, nh: i32) -> bool {
        use std::collections::{HashSet, VecDeque};
        const MAX_POCKET_TILES: usize = 64;
        let mut seen: HashSet<(i32, i32, i32)> = HashSet::new();
        let mut q: VecDeque<(i32, i32, i32)> = VecDeque::new();
        seen.insert((nx, ny, nh));
        q.push_back((nx, ny, nh));
        let mut active_event_tiles: HashSet<(i32, i32, i32)> = HashSet::new();
        for id in self.world.current_event_ids() {
            if let Some(ev) = self.world.event(id) {
                if ev.state > 0 && ev.vanish_time <= 0 && !ev.trigger.is_none() {
                    let ht = world_to_half_tile(ev.x, ev.y);
                    if ht != start_ht {
                        active_event_tiles.insert(ht);
                    }
                }
            }
        }
        while let Some((cx, cy, ch)) = q.pop_front() {
            if active_event_tiles.contains(&(cx, cy, ch)) {
                return false;
            }
            if seen.len() >= MAX_POCKET_TILES {
                return false;
            }
            for d in 0..4u8 {
                let (ax, ay, ah) = step_half_tile(cx, cy, ch, d);
                if (ax, ay, ah) == start_ht {
                    continue;
                }
                if active_event_tiles.contains(&(ax, ay, ah)) {
                    return false;
                }
                if !self.grid.is_blocked(ax, ay, ah) && seen.insert((ax, ay, ah)) {
                    q.push_back((ax, ay, ah));
                }
            }
        }
        true
    }

    /// Step the leader one half-tile in `dir` if not blocked (legacy `tryManualStep`).
    /// One held-direction step. `0..=3` are PAL's diagonals (SW, NW, NE, SE). `4..=7` are the
    /// in-between screen directions (W, N, E, S): they alternate the two adjacent diagonals
    /// `a = held - 4` and `(a + 1) & 3`, so the net movement is straight, and fall back to the
    /// other diagonal when one is blocked (slides along walls). Every diagonal step moves x by
    /// ±16, so the parity of `x / 16` alternates each step; deriving the order from it keeps
    /// replays and save-anchored recordings deterministic without extra state.
    pub fn held_step(&mut self, held: u8) -> bool {
        if held < 4 {
            return self.try_manual_step(held);
        }
        let a = (held - 4) & 3;
        let b = (a + 1) & 3;
        let odd = self.world.leader().x.div_euclid(16) & 1 == 1;
        let (first, second) = if odd { (b, a) } else { (a, b) };
        self.try_manual_step(first) || self.try_manual_step(second)
    }

    pub fn try_manual_step(&mut self, dir: u8) -> bool {
        self.world.view.cam_override = None;
        self.world.leader_mut().dir = dir & 3;
        self.sync_obstacles(0);
        let lead = self.world.leader().clone();
        let (x, y, h) = world_to_half_tile(lead.x, lead.y);
        let (nx, ny, nh) = step_half_tile(x, y, h, dir & 3);
        let (wx, wy) = half_tile_to_world(nx, ny, nh);
        if self.grid.is_blocked(nx, ny, nh) || wx < 0 || wy < 0 {
            self.world.leader_mut().frame = 0;
            return false;
        }
        // When a script places the party on a blocked wall/ladder tile (e.g. Scene #1 secret
        // passage landing at `(640, 608)`), refuse stepping across the wall into an eventless
        // enclosed void pocket (`(624, 600)`) from which the player could never step back (#150).
        if self.grid.is_blocked(x, y, h) && self.is_trapped_void_pocket((x, y, h), nx, ny, nh) {
            self.world.leader_mut().frame = 0;
            return false;
        }
        self.world.step_party_to(wx, wy, dir);
        self.check_proximity_triggers();
        true
    }

    /// Talk to / search the event in front of the leader (legacy `interactInFront`).
    pub fn interact_in_front(&mut self) -> bool {
        if self.fg.is_some() {
            return false;
        }
        let lead = self.world.leader().clone();
        let (dx, dy) = match lead.dir & 3 {
            0 => (-16, 8),
            1 => (-16, -8),
            2 => (16, -8),
            _ => (16, 8),
        };
        let (px, py) = (lead.x + dx, lead.y + dy);
        let mut best: Option<(i32, i32, i32, u16)> = None;
        for id in self.world.current_event_ids() {
            let Some(ev) = self.world.event(id) else { continue };
            if ev.state <= 0 || ev.vanish_time > 0 || ev.trigger.is_none() || !(1..=3).contains(&ev.trigger_mode) {
                continue;
            }
            let max = (ev.trigger_mode - 1) * 32 + 16;
            let d_probe = (ev.x - px).abs() + (ev.y - py).abs() * 2;
            if d_probe < max {
                let d_lead = (ev.x - lead.x).abs() + (ev.y - lead.y).abs() * 2;
                let key = (d_probe, i32::from(ev.state < 2), d_lead, id);
                if best.map(|b| key < b).unwrap_or(true) {
                    best = Some(key);
                }
            }
        }
        let Some((_, _, _, id)) = best else { return false };
        if let Some(ev) = self.world.event_mut(id) {
            if ev.sprite_frames > 0 {
                ev.direction = ((lead.dir + 2) & 3) as i32;
            }
        }
        self.trigger_event(id)
    }

    /// Plan a portal-aware A* path to `(wx, wy)` and enqueue it into `self.auto_walk` without
    /// setting `pending_click_event`. Returns `true` if a path was enqueued or a fallback step moved.
    pub fn plan_walk_portal_aware(&mut self, wx: i32, wy: i32) -> bool {
        self.world.view.cam_override = None;
        self.sync_obstacles(0);
        let lead = self.world.leader().clone();

        // Build a portal-aware grid so A* routes around non-target scene-change doorways/stairs
        // AND same-scene displacement portals (e.g. lotus-leaf ferries in Fairy Island, #149).
        let build_avoid_grid = |avoid_displacement: bool| {
            let mut g = self.grid.clone();
            let start_ht = world_to_half_tile(lead.x, lead.y);
            let goal_ht = world_to_half_tile(wx, wy);
            // If standing on a blocked tile (e.g. Scene #1 secret ladder landing `(640, 608)`),
            // mask out any adjacent eventless void pocket so A* never walks into a softlock (#150).
            if self.grid.is_blocked(start_ht.0, start_ht.1, start_ht.2) {
                for d in 0..4u8 {
                    let (ax, ay, ah) = step_half_tile(start_ht.0, start_ht.1, start_ht.2, d);
                    if !self.grid.is_blocked(ax, ay, ah) && self.is_trapped_void_pocket(start_ht, ax, ay, ah) {
                        g.set_blocked(ax, ay, ah, true);
                    }
                }
            }
            for id in self.world.current_event_ids() {
                let Some(ev) = self.world.event(id) else { continue };
                if ev.state <= 0 || ev.vanish_time > 0 || ev.trigger.is_none() || ev.trigger_mode < 4 {
                    continue;
                }
                let should_avoid = if avoid_displacement {
                    self.is_displacement_portal(id)
                } else {
                    self.is_scene_change_portal(id)
                };
                if !should_avoid {
                    continue;
                }
                let radius = (ev.trigger_mode - 4) * 32 + 16;
                // If the user intentionally targeted inside (or is already standing inside) this portal,
                // leave it walkable.
                let goal_dist = (ev.x - wx).abs() + (ev.y - wy).abs() * 2;
                let lead_dist = (ev.x - lead.x).abs() + (ev.y - lead.y).abs() * 2;
                if goal_dist < radius + 24 || lead_dist < radius {
                    continue;
                }
                let min_x = (ev.x - radius).max(0);
                let max_x = (ev.x + radius).min(2047);
                let min_y = (ev.y - radius / 2).max(0);
                let max_y = (ev.y + radius / 2).min(2047);
                let mut ty = min_y - (min_y % 8);
                while ty <= max_y {
                    let mut tx = min_x - (min_x % 16);
                    while tx <= max_x {
                        if (ev.x - tx).abs() + (ev.y - ty).abs() * 2 < radius {
                            let ht = world_to_half_tile(tx, ty);
                            if ht != start_ht && ht != goal_ht {
                                g.set_blocked(ht.0, ht.1, ht.2, true);
                            }
                        }
                        tx += 16;
                    }
                    ty += 8;
                }
            }
            g
        };

        let start_ht = world_to_half_tile(lead.x, lead.y);
        let path_grid = build_avoid_grid(true);
        let mut path = find_path_world(&path_grid, lead.x, lead.y, wx, wy, 1200);
        if path.is_empty() {
            // Fallback: only avoid scene-change doorways if avoiding same-scene displacement
            // triggers left no route at all.
            let scene_only_grid = build_avoid_grid(false);
            path = find_path_world(&scene_only_grid, lead.x, lead.y, wx, wy, 1200);
        }
        if !path.is_empty() {
            self.auto_walk = path.into_iter().collect();
            true
        } else {
            self.auto_walk.clear();
            let dir = direction_between(lead.x, lead.y, wx, wy);
            let (nx, ny, nh) = step_half_tile(start_ht.0, start_ht.1, start_ht.2, dir & 3);
            if !path_grid.is_blocked(nx, ny, nh) {
                self.try_manual_step(dir)
            } else {
                false
            }
        }
    }

    /// Click-to-walk / click-to-talk at world position (legacy `handleCanvasPointerWorld`).
    pub fn click_world(&mut self, wx: i32, wy: i32) {
        let clicked = self.find_event_near_point(wx, wy);
        if let Some(id) = clicked {
            if self.talk_if_close(id, 44.0) {
                return;
            }
            self.pending_click_event = id;
        } else {
            self.pending_click_event = 0;
        }
        self.plan_walk_portal_aware(wx, wy);
    }

    fn find_event_near_point(&self, wx: i32, wy: i32) -> Option<u16> {
        let mut best: Option<(f64, f64, u16)> = None;
        for id in self.world.current_event_ids() {
            let Some(ev) = self.world.event(id) else { continue };
            if ev.state <= 0 || ev.vanish_time > 0 || ev.trigger.is_none() || !(1..=3).contains(&ev.trigger_mode) {
                continue;
            }
            let d = ((ev.x - wx) as f64).hypot((ev.y - 10 - wy) as f64);
            if d < 32.0 {
                let score = d - if ev.state >= 2 { 8.0 } else { 0.0 };
                let better = match best {
                    None => true,
                    Some((bs, bd, bid)) => {
                        score < bs - 1e-6 || ((score - bs).abs() <= 1e-6 && (d < bd - 1e-6 || ((d - bd).abs() <= 1e-6 && id < bid)))
                    }
                };
                if better {
                    best = Some((score, d, id));
                }
            }
        }
        best.map(|b| b.2)
    }

    fn proximity_dist(&self, id: u16) -> i32 {
        let lead = self.world.leader();
        let Some(ev) = self.world.event(id) else { return i32::MAX };
        (ev.x - lead.x).abs() + (ev.y - lead.y).abs() * 2
    }

    fn is_within_proximity(&self, id: u16) -> bool {
        match self.world.event(id) {
            Some(ev) if ev.trigger_mode >= 4 => self.proximity_dist(id) < (ev.trigger_mode - 4) * 32 + 16,
            _ => false,
        }
    }

    /// Does the event's trigger script lead straight into a `scene_change` (a doorway)?
    fn is_scene_change_portal(&self, id: u16) -> bool {
        let Some(ev) = self.world.event(id) else { return false };
        let Some(block) = self.prog.block(ev.trigger.label) else { return true };
        for ins in block.iter().skip(ev.trigger.idx as usize) {
            match ins.op {
                op::SCENE_CHANGE => return true,
                op::END | op::PAUSE_YIELD | op::REPLACE_AND_YIELD => break,
                _ => {}
            }
        }
        false
    }

    /// Does the event's trigger script forcibly relocate the party (scene change, same-scene
    /// teleport, or lotus-leaf/boat ride)?
    fn is_displacement_portal(&self, id: u16) -> bool {
        let Some(ev) = self.world.event(id) else { return false };
        let Some(block) = self.prog.block(ev.trigger.label) else { return true };
        for ins in block.iter().skip(ev.trigger.idx as usize) {
            match ins.op {
                op::SCENE_CHANGE | op::PARTY_SET_POS => return true,
                op::END | op::PAUSE_YIELD | op::REPLACE_AND_YIELD => break,
                _ => {
                    if matches!(
                        crate::script::opcode_name(ins.op),
                        "ride_npc_to" | "ride_npc_to_normal" | "ride_npc_to_fast"
                    ) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn suppress_standing_proximity_events(
        &mut self,
        from_scene_change: bool,
        finished_event: Option<u16>,
    ) {
        if !from_scene_change {
            // Only suppress the event that just finished so it cannot immediately re-trigger itself,
            // while allowing newly-enabled or teleported-onto proximity events (e.g. Suzhou boat ev#127
            // activated by Boss Fang ev#126) to fire cleanly on the next tick.
            if let Some(id) = finished_event {
                if let Some(ev) = self.world.event(id) {
                    if ev.state > 0
                        && ev.vanish_time <= 0
                        && !ev.trigger.is_none()
                        && ev.trigger_mode >= 4
                        && self.is_within_proximity(id)
                    {
                        let d = self.proximity_dist(id);
                        self.suppressed.insert(id, (d, false));
                    }
                }
            }
            return;
        }
        for id in self.world.current_event_ids() {
            let Some(ev) = self.world.event(id) else { continue };
            if ev.state > 0 && ev.vanish_time <= 0 && !ev.trigger.is_none() && ev.trigger_mode >= 4 && self.is_within_proximity(id) {
                // After a scene transition only doorway triggers are suppressed, so story cutscenes
                // the party lands inside still fire immediately.
                if !self.is_scene_change_portal(id) {
                    continue;
                }
                let d = self.proximity_dist(id);
                self.suppressed.insert(id, (d, true));
            }
        }
    }

    /// Legacy `checkProximityTriggers`.
    pub(crate) fn check_proximity_triggers(&mut self) {
        if self.fg.is_some() {
            return;
        }
        let entries: Vec<(u16, (i32, bool))> = self.suppressed.iter().map(|(&k, &v)| (k, v)).collect();
        for (id, (dist, from_scene)) in entries {
            if self.world.event(id).is_none() || !self.is_within_proximity(id) || (from_scene && self.proximity_dist(id) < dist) {
                self.suppressed.remove(&id);
            }
        }
        let mut best: Option<(i32, i32, u16)> = None;
        for id in self.world.current_event_ids() {
            if self.suppressed.contains_key(&id) {
                continue;
            }
            let Some(ev) = self.world.event(id) else { continue };
            if ev.state <= 0 || ev.vanish_time > 0 || ev.trigger.is_none() || ev.trigger_mode < 4 {
                continue;
            }
            let iso = self.proximity_dist(id);
            if iso < (ev.trigger_mode - 4) * 32 + 16 {
                let key = (iso, i32::from(ev.state < 2), id);
                if best.map(|b| key < b).unwrap_or(true) {
                    best = Some(key);
                }
            }
        }
        if let Some((_, _, id)) = best {
            self.auto_walk.clear();
            self.pending_click_event = 0;
            self.trigger_event(id);
        }
    }

    /// Legacy `onForegroundScriptEnd` hook.
    pub(crate) fn on_foreground_end(&mut self, finished_event: Option<u16>) {
        let was_scene_change = std::mem::take(&mut self.just_changed_scene);
        self.suppress_standing_proximity_events(was_scene_change, finished_event);
        self.check_proximity_triggers();
    }
}
