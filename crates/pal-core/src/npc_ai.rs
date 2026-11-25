//! Pure-Rust NPC movement, line-of-sight, and A* waypoint-following AI.

use crate::isometric::{
    direction_between, find_path_world, half_tile_to_world, step_half_tile, world_to_half_tile,
    CollisionGrid,
};

#[derive(Clone, Debug, Default)]
pub struct NpcWalkTracker {
    pub leg_key: u64,
    pub using_path: bool,
    pub path: Vec<(i32, i32)>,
    pub path_idx: usize,
    pub is_partial: bool,
    pub stall: u32,
    pub steps: u32,
    pub max_steps: u32,
    pub repaths: u32,
    pub retries: u32,
}

impl NpcWalkTracker {
    pub fn reset(&mut self) {
        self.leg_key = 0;
        self.using_path = false;
        self.path.clear();
        self.path_idx = 0;
        self.is_partial = false;
        self.stall = 0;
        self.steps = 0;
        self.max_steps = 0;
        self.repaths = 0;
        self.retries = 0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NpcWalkOutcome {
    Moved { x: i32, y: i32, dir: u8 },
    Done { x: i32, y: i32 },
    Stalled { dir: u8 },
}

#[inline]
pub fn is_world_blocked(grid: &CollisionGrid, px: i32, py: i32) -> bool {
    let (x, y, h) = world_to_half_tile(px, py);
    grid.is_blocked(x, y, h)
}

/// Exact line-of-sight check between `(x0, y0)` and `(x1, y1)` including 0-width
/// isometric diagonal corner-pinch detection.
pub fn is_line_of_sight_clear(grid: &CollisionGrid, x0: i32, y0: i32, x1: i32, y1: i32) -> bool {
    let dx = (x1 - x0) as f64;
    let dy = (y1 - y0) as f64;
    let d = dx.hypot(dy);
    if d <= 0.0 {
        return true;
    }
    let samples = d.ceil().max(2.0) as i32;
    for i in 1..=samples {
        let t = (i as f64) / (samples as f64);
        let px = ((x0 as f64) + dx * t).round() as i32;
        let py = ((y0 as f64) + dy * t).round() as i32;
        if is_world_blocked(grid, px, py) {
            return false;
        }
        if i < samples {
            if is_world_blocked(grid, px - 2, py) && is_world_blocked(grid, px + 2, py) {
                return false;
            }
            if is_world_blocked(grid, px, py - 2) && is_world_blocked(grid, px, py + 2) {
                return false;
            }
        }
    }
    true
}

fn compute_path_state(
    grid: &CollisionGrid,
    tracker: &mut NpcWalkTracker,
    sx: i32,
    sy: i32,
    gx: i32,
    gy: i32,
    step_spd: i32,
) -> (Vec<(i32, i32)>, bool) {
    let path = find_path_world(grid, sx, sy, gx, gy, 16384);
    if path.is_empty() {
        return (path, true);
    }
    let last_wp = *path.last().unwrap();
    let end_dist = ((last_wp.0 - gx) as f64).hypot((last_wp.1 - gy) as f64);
    let goal_blocked = is_world_blocked(grid, gx, gy);
    let spd = step_spd.max(1) as u32;
    let per_wp = (18 + spd - 1) / spd + 1;
    let path_budget = (tracker.steps + (path.len() as u32) * per_wp + 64).min(4096);
    tracker.max_steps = tracker.max_steps.max(path_budget);
    let is_partial = if goal_blocked {
        end_dist > 24.0
    } else {
        end_dist > 1.0
    };
    (path, is_partial)
}

#[inline]
fn get_max_walk_steps(tracker: &NpcWalkTracker) -> u32 {
    if tracker.max_steps > 0 {
        tracker.max_steps
    } else {
        1024
    }
}

fn finish_walk(
    grid: &CollisionGrid,
    tracker: &mut NpcWalkTracker,
    cur_x: i32,
    cur_y: i32,
    gx: i32,
    gy: i32,
    is_solid: bool,
    snap_to_goal: bool,
) -> NpcWalkOutcome {
    let (fx, fy) = if snap_to_goal && (!is_solid || !is_world_blocked(grid, gx, gy)) {
        (gx, gy)
    } else {
        (cur_x, cur_y)
    };
    tracker.reset();
    NpcWalkOutcome::Done { x: fx, y: fy }
}

fn record_move(
    grid: &CollisionGrid,
    tracker: &mut NpcWalkTracker,
    nx: i32,
    ny: i32,
    dir: u8,
    gx: i32,
    gy: i32,
    is_solid: bool,
) -> NpcWalkOutcome {
    tracker.stall = 0;
    tracker.steps += 1;
    if tracker.steps >= get_max_walk_steps(tracker) {
        return finish_walk(grid, tracker, nx, ny, gx, gy, is_solid, false);
    }
    NpcWalkOutcome::Moved { x: nx, y: ny, dir }
}

fn follow_waypoints(
    grid: &CollisionGrid,
    tracker: &mut NpcWalkTracker,
    ev_x: i32,
    ev_y: i32,
    gx: i32,
    gy: i32,
    step_spd: i32,
    is_solid: bool,
    just_computed_path: bool,
) -> Option<NpcWalkOutcome> {
    if tracker.path.is_empty() && !just_computed_path && tracker.repaths < 12 {
        tracker.repaths += 1;
        let (new_path, is_partial) =
            compute_path_state(grid, tracker, ev_x, ev_y, gx, gy, step_spd);
        if !new_path.is_empty() {
            tracker.path = new_path;
            tracker.path_idx = 0;
            tracker.is_partial = is_partial;
        }
    }

    while tracker.path_idx < tracker.path.len() {
        let (wx, wy) = tracker.path[tracker.path_idx];
        let wdx = (wx - ev_x) as f64;
        let wdy = (wy - ev_y) as f64;
        let w_dist = wdx.hypot(wdy);

        if is_world_blocked(grid, wx, wy) {
            if tracker.repaths < 12 {
                tracker.repaths += 1;
                let (new_path, is_partial) =
                    compute_path_state(grid, tracker, ev_x, ev_y, gx, gy, step_spd);
                if !new_path.is_empty() && !is_world_blocked(grid, new_path[0].0, new_path[0].1) {
                    tracker.path = new_path;
                    tracker.path_idx = 0;
                    tracker.is_partial = is_partial;
                    continue;
                }
            }
            break;
        }

        if w_dist <= (step_spd as f64) * 0.25 && tracker.path_idx + 1 < tracker.path.len() {
            tracker.path_idx += 1;
            continue;
        }

        if w_dist <= step_spd as f64 {
            let dir = direction_between(ev_x, ev_y, wx, wy);
            tracker.path_idx += 1;
            return Some(record_move(grid, tracker, wx, wy, dir, gx, gy, is_solid));
        }

        let cand_x = ev_x + ((wdx / w_dist) * (step_spd as f64)).round() as i32;
        let cand_y = ev_y + ((wdy / w_dist) * (step_spd as f64)).round() as i32;
        if (cand_x != ev_x || cand_y != ev_y) && !is_world_blocked(grid, cand_x, cand_y) {
            let dir = direction_between(ev_x, ev_y, wx, wy);
            return Some(record_move(
                grid, tracker, cand_x, cand_y, dir, gx, gy, is_solid,
            ));
        }

        if w_dist <= step_spd.max(28) as f64 {
            let dir = direction_between(ev_x, ev_y, wx, wy);
            tracker.path_idx += 1;
            return Some(record_move(grid, tracker, wx, wy, dir, gx, gy, is_solid));
        }

        if tracker.repaths < 12 {
            tracker.repaths += 1;
            let (new_path, is_partial) =
                compute_path_state(grid, tracker, ev_x, ev_y, gx, gy, step_spd);
            if !new_path.is_empty() && !is_world_blocked(grid, new_path[0].0, new_path[0].1) {
                tracker.path = new_path;
                tracker.path_idx = 0;
                tracker.is_partial = is_partial;
                continue;
            }
        }
        break;
    }
    None
}

/// Advance one auto-tick of NPC movement toward `(gx, gy)` with sticky A* fallback.
pub fn npc_advance_walk(
    grid: &CollisionGrid,
    tracker: &mut NpcWalkTracker,
    ev_x: i32,
    ev_y: i32,
    gx: i32,
    gy: i32,
    step_spd: i32,
    is_solid: bool,
    leg_key: u64,
) -> NpcWalkOutcome {
    let step_spd = step_spd.max(1);
    let initial_dist = ((gx - ev_x) as f64).hypot((gy - ev_y) as f64);

    let mut just_computed_path = false;
    if tracker.leg_key != leg_key {
        tracker.reset();
        tracker.leg_key = leg_key;
        let init_budget = ((initial_dist / (step_spd as f64)).ceil() as u32) * 6 + 128;
        tracker.max_steps = init_budget.clamp(512, 4096);
        if is_solid && initial_dist > 0.0 && !is_line_of_sight_clear(grid, ev_x, ev_y, gx, gy) {
            let (path, is_partial) =
                compute_path_state(grid, tracker, ev_x, ev_y, gx, gy, step_spd);
            tracker.using_path = true;
            tracker.path = path;
            tracker.path_idx = 0;
            tracker.is_partial = is_partial;
            just_computed_path = true;
        }
    }

    let dx = (gx - ev_x) as f64;
    let dy = (gy - ev_y) as f64;
    let dist = dx.hypot(dy);
    if dist <= step_spd as f64 && (!is_solid || is_line_of_sight_clear(grid, ev_x, ev_y, gx, gy)) {
        return finish_walk(grid, tracker, ev_x, ev_y, gx, gy, is_solid, true);
    }
    if tracker.steps >= get_max_walk_steps(tracker) {
        return finish_walk(grid, tracker, ev_x, ev_y, gx, gy, is_solid, false);
    }

    if !is_solid || !tracker.using_path {
        let sx = ev_x + ((dx / dist) * (step_spd as f64)).round() as i32;
        let sy = ev_y + ((dy / dist) * (step_spd as f64)).round() as i32;
        if (sx != ev_x || sy != ev_y)
            && (!is_solid
                || (!is_world_blocked(grid, sx, sy)
                    && is_line_of_sight_clear(grid, ev_x, ev_y, sx, sy)))
        {
            let dir = direction_between(ev_x, ev_y, gx, gy);
            return record_move(grid, tracker, sx, sy, dir, gx, gy, is_solid);
        }
        if !is_solid {
            return finish_walk(grid, tracker, ev_x, ev_y, gx, gy, is_solid, true);
        }
        let (path, is_partial) =
            compute_path_state(grid, tracker, ev_x, ev_y, gx, gy, step_spd);
        tracker.using_path = true;
        tracker.path = path;
        tracker.path_idx = 0;
        tracker.is_partial = is_partial;
        just_computed_path = true;
    }

    if let Some(res) = follow_waypoints(
        grid,
        tracker,
        ev_x,
        ev_y,
        gx,
        gy,
        step_spd,
        is_solid,
        just_computed_path,
    ) {
        return res;
    }

    if (tracker.path.is_empty() || tracker.path_idx >= tracker.path.len())
        && dist <= 28.0
        && !is_world_blocked(grid, gx, gy)
        && is_line_of_sight_clear(grid, ev_x, ev_y, gx, gy)
    {
        let sx = if dist <= step_spd as f64 {
            gx
        } else {
            ev_x + ((dx / dist) * (step_spd as f64)).round() as i32
        };
        let sy = if dist <= step_spd as f64 {
            gy
        } else {
            ev_y + ((dy / dist) * (step_spd as f64)).round() as i32
        };
        if (sx != ev_x || sy != ev_y)
            && !is_world_blocked(grid, sx, sy)
            && is_line_of_sight_clear(grid, ev_x, ev_y, sx, sy)
        {
            let dir = direction_between(ev_x, ev_y, gx, gy);
            return record_move(grid, tracker, sx, sy, dir, gx, gy, is_solid);
        }
    }

    if !tracker.path.is_empty() && tracker.path_idx >= tracker.path.len() {
        if dist <= step_spd as f64
            && (!is_solid || is_line_of_sight_clear(grid, ev_x, ev_y, gx, gy))
        {
            return finish_walk(grid, tracker, ev_x, ev_y, gx, gy, is_solid, true);
        }
        if tracker.is_partial {
            if tracker.retries < 8 && tracker.stall < 6 {
                tracker.retries += 1;
                let (retry_path, is_partial) =
                    compute_path_state(grid, tracker, ev_x, ev_y, gx, gy, step_spd);
                if !retry_path.is_empty() {
                    let retry_last = *retry_path.last().unwrap();
                    let retry_end_dist =
                        ((retry_last.0 - gx) as f64).hypot((retry_last.1 - gy) as f64);
                    if !is_partial || retry_end_dist + 1.0 < dist {
                        tracker.path = retry_path;
                        tracker.path_idx = 0;
                        tracker.is_partial = is_partial;
                        if let Some(res) = follow_waypoints(
                            grid, tracker, ev_x, ev_y, gx, gy, step_spd, is_solid, false,
                        ) {
                            return res;
                        }
                    }
                }
            }
            if tracker.stall >= 6 {
                return finish_walk(grid, tracker, ev_x, ev_y, gx, gy, is_solid, false);
            }
        }
        if is_world_blocked(grid, gx, gy) && dist <= 28.0 && tracker.stall >= 6 {
            return finish_walk(grid, tracker, ev_x, ev_y, gx, gy, is_solid, false);
        }
    }

    let dir = direction_between(ev_x, ev_y, gx, gy);
    tracker.stall += 1;
    tracker.steps += 1;
    if tracker.stall >= 8 || tracker.steps >= get_max_walk_steps(tracker) {
        return finish_walk(grid, tracker, ev_x, ev_y, gx, gy, is_solid, false);
    }
    NpcWalkOutcome::Stalled { dir }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonsterStepOutcome {
    pub moved: bool,
    pub x: i32,
    pub y: i32,
    pub dir: u8,
    pub animate: bool,
}

#[inline]
fn try_monster_move_dir(
    grid: &CollisionGrid,
    ev_x: i32,
    ev_y: i32,
    dir: u8,
    floating: bool,
) -> Option<(i32, i32)> {
    let (x, y, h) = world_to_half_tile(ev_x, ev_y);
    let (nx, ny, nh) = step_half_tile(x, y, h, dir & 3);
    if !floating && grid.is_blocked(nx, ny, nh) {
        return None;
    }
    let (wx, wy) = half_tile_to_world(nx, ny, nh);
    if !(16..=2032).contains(&wx) || !(16..=2032).contains(&wy) {
        return None;
    }
    Some((wx, wy))
}

#[inline]
fn xorshift32(s: &mut u32) -> u32 {
    let mut x = if *s == 0 { 0x9e3779b9 } else { *s };
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *s = x;
    x
}

/// Pure-Rust monster chase / territory-wander / repel AI step (`npc_chase_player`).
pub fn npc_monster_step(
    grid: &CollisionGrid,
    ev_x: i32,
    ev_y: i32,
    ev_dir: u8,
    spawn_x: i32,
    spawn_y: i32,
    has_leader: bool,
    lead_x: i32,
    lead_y: i32,
    alert_dist: i32,
    non_chasing_patrol: bool,
    chase_disabled: bool,
    floating: bool,
    mut rng_seed: u32,
) -> MonsterStepOutcome {
    let iso_dist = if has_leader {
        (lead_x - ev_x).abs() + (lead_y - ev_y).abs() * 2
    } else {
        i32::MAX
    };

    if !non_chasing_patrol && !chase_disabled && has_leader && iso_dist <= alert_dist {
        if iso_dist == 0 {
            return MonsterStepOutcome {
                moved: false,
                x: ev_x,
                y: ev_y,
                dir: ev_dir & 3,
                animate: true,
            };
        }
        let p_dir = direction_between(ev_x, ev_y, lead_x, lead_y);
        let side_a = (p_dir + 1) & 3;
        let side_b = (p_dir + 3) & 3;
        let n_a = try_monster_move_dir(grid, ev_x, ev_y, side_a, floating);
        let n_b = try_monster_move_dir(grid, ev_x, ev_y, side_b, floating);
        let (second, third) = match (n_a, n_b) {
            (Some((ax, ay)), Some((bx, by))) => {
                let da = (lead_x - ax).abs() + (lead_y - ay).abs() * 2;
                let db = (lead_x - bx).abs() + (lead_y - by).abs() * 2;
                if da <= db {
                    (side_a, side_b)
                } else {
                    (side_b, side_a)
                }
            }
            _ => (side_a, side_b),
        };
        for d in [p_dir, second, third] {
            if let Some((nx, ny)) = try_monster_move_dir(grid, ev_x, ev_y, d, floating) {
                return MonsterStepOutcome {
                    moved: true,
                    x: nx,
                    y: ny,
                    dir: d,
                    animate: true,
                };
            }
        }
        return MonsterStepOutcome {
            moved: false,
            x: ev_x,
            y: ev_y,
            dir: p_dir,
            animate: true,
        };
    }

    if (xorshift32(&mut rng_seed) % 100) < 60 {
        let mut dir = ev_dir & 3;
        if chase_disabled && has_leader && iso_dist <= alert_dist {
            dir = (direction_between(ev_x, ev_y, lead_x, lead_y) + 2) & 3;
        } else if ((ev_x - spawn_x) as f64).hypot((ev_y - spawn_y) as f64) > 160.0 {
            dir = direction_between(ev_x, ev_y, spawn_x, spawn_y);
        } else if (xorshift32(&mut rng_seed) % 100) < 30 {
            dir = (xorshift32(&mut rng_seed) & 3) as u8;
        }
        for cand in [dir, (dir + 1) & 3, (dir + 3) & 3, (dir + 2) & 3] {
            if let Some((nx, ny)) = try_monster_move_dir(grid, ev_x, ev_y, cand, floating) {
                return MonsterStepOutcome {
                    moved: true,
                    x: nx,
                    y: ny,
                    dir: cand,
                    animate: true,
                };
            }
        }
    } else if floating {
        return MonsterStepOutcome {
            moved: false,
            x: ev_x,
            y: ev_y,
            dir: ev_dir & 3,
            animate: true,
        };
    }

    MonsterStepOutcome {
        moved: false,
        x: ev_x,
        y: ev_y,
        dir: ev_dir & 3,
        animate: false,
    }
}

/// Quarter-tile isometric NPC walk step (`npc_walk_dir_0..3`).
pub fn npc_walk_dir(
    grid: &CollisionGrid,
    ev_x: i32,
    ev_y: i32,
    dir: u8,
    is_solid: bool,
) -> (i32, i32) {
    let (dx, dy) = match dir & 3 {
        0 => (-8, 4),
        1 => (-8, -4),
        2 => (8, -4),
        _ => (8, 4),
    };
    let nx = ev_x + dx;
    let ny = ev_y + dy;
    if !is_solid || !is_world_blocked(grid, nx, ny) {
        (nx, ny)
    } else {
        (ev_x, ev_y)
    }
}
