//! Isometric staggered-diamond map geometry, obstacle grid, depth sorting, and A* pathfinding.
//!
//! DOS PAL map dimensions:
//! - 128 rows (`y = 0..128`), 64 tile columns (`x = 0..64`), 2 staggered half-columns (`h = 0..2`).
//! - World logical pixel coordinate of half-tile `(x, y, h)`:
//!   `px = x * 32 + h * 16`, `py = y * 16 + h * 8`.
//! - Four diagonal walking directions (`PalDir`):
//!   `0 = SouthWest (-16, +8)`, `1 = NorthWest (-16, -8)`,
//!   `2 = NorthEast (+16, -8)`, `3 = SouthEast (+16, +8)`.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

pub const MAP_WIDTH: usize = 64;
pub const MAP_HEIGHT: usize = 128;
pub const HALF_COLS: usize = MAP_WIDTH * 2; // 128 half-columns (hx = x * 2 + h)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedTile {
    pub bottom_sprite: u16,
    pub bottom_layer: u8,
    pub blocked: bool,
    pub top_sprite: Option<u16>,
    pub top_layer: u8,
}

impl DecodedTile {
    /// Decode a 4-byte tile record from decompressed `MAP.MKF` (`65,536` bytes = `128 * 64 * 2 * 4`).
    #[inline]
    pub fn from_bytes(b: [u8; 4]) -> Self {
        let bottom_sprite = (b[0] as u16) | (((b[1] as u16) & 0x10) << 4);
        let bottom_layer = b[1] & 0x0F;
        let blocked = (b[1] & 0x20) != 0;

        let raw_top = (b[2] as u16) | (((b[3] as u16) & 0x10) << 4);
        let top_sprite = if raw_top == 0 {
            None
        } else {
            Some(raw_top - 1)
        };
        let top_layer = b[3] & 0x0F;

        Self {
            bottom_sprite,
            bottom_layer,
            blocked,
            top_sprite,
            top_layer,
        }
    }
}

/// Convert half-tile `(x, y, h)` to logical world pixel `(px, py)`.
#[inline]
pub fn half_tile_to_world(x: i32, y: i32, h: i32) -> (i32, i32) {
    (x * 32 + h * 16, y * 16 + h * 8)
}

/// Convert logical world pixel `(px, py)` to nearest staggered half-tile `(x, y, h)`.
#[inline]
pub fn world_to_half_tile(px: i32, py: i32) -> (i32, i32, i32) {
    // Compare distance to nearest h=0 center vs nearest h=1 center
    let x0 = (px + 16).div_euclid(32);
    let y0 = (py + 8).div_euclid(16);
    let dx0 = px - x0 * 32;
    let dy0 = py - y0 * 16;
    let d0 = dx0.abs() + dy0.abs() * 2;

    let x1 = px.div_euclid(32);
    let y1 = py.div_euclid(16);
    let dx1 = px - (x1 * 32 + 16);
    let dy1 = py - (y1 * 16 + 8);
    let d1 = dx1.abs() + dy1.abs() * 2;

    if d1 < d0 {
        (x1, y1, 1)
    } else {
        (x0, y0, 0)
    }
}

/// Step one half-tile in DOS PAL direction `dir` (`0=SW, 1=NW, 2=NE, 3=SE`).
#[inline]
pub fn step_half_tile(x: i32, y: i32, h: i32, dir: u8) -> (i32, i32, i32) {
    let (px, py) = half_tile_to_world(x, y, h);
    let (nx, ny) = match dir & 3 {
        0 => (px - 16, py + 8), // South-West
        1 => (px - 16, py - 8), // North-West
        2 => (px + 16, py - 8), // North-East
        _ => (px + 16, py + 8), // South-East
    };
    world_to_half_tile(nx, ny)
}

/// Determine PAL facing direction `0..3` from `(px1, py1)` toward `(px2, py2)`.
#[inline]
pub fn direction_between(px1: i32, py1: i32, px2: i32, py2: i32) -> u8 {
    let dx = px2 - px1;
    let dy = py2 - py1;
    if dx < 0 {
        if dy >= 0 { 0 } else { 1 }
    } else if dy < 0 {
        2
    } else {
        3
    }
}

#[derive(Clone, Debug, Default)]
pub struct GopVisualFlags {
    /// `true` when GOP tile frame `0` is pure black / fully transparent (interior & dungeon void).
    pub frame0_is_void: bool,
    /// Per-GOP-frame flag: `true` when the elevated 32x16 tile covers `>= 90%` (`>= 230/256` px)
    /// of its isometric diamond with opaque pixels, genuinely occluding the floor underneath.
    pub opaque_frames: Vec<bool>,
}

#[derive(Clone)]
pub struct CollisionGrid {
    /// `blocked[y * 128 + (x * 2 + h)]`
    pub blocked: Vec<u8>,
    /// `occluded[y * 128 + (x * 2 + h)]`: `1` when covered by a genuinely opaque elevated
    /// top-layer canopy/roof (`raw_top != 0 && top_layer > 0 && opaque_frames[raw_top - 1]`, #234).
    pub occluded: Vec<u8>,
    /// `void_tiles[y * 128 + (x * 2 + h)]`: `1` when `frame0_is_void && bottom_sprite == 0 && raw_top == 0 && !blocked`
    /// (black exterior void outside interior room walls, #234).
    pub void_tiles: Vec<u8>,
}

impl CollisionGrid {
    pub fn empty() -> Self {
        Self {
            blocked: vec![0u8; MAP_HEIGHT * HALF_COLS],
            occluded: vec![0u8; MAP_HEIGHT * HALF_COLS],
            void_tiles: vec![0u8; MAP_HEIGHT * HALF_COLS],
        }
    }

    pub fn from_raw_map64k(raw: &[u8]) -> Option<Self> {
        Self::from_raw_map64k_with_visuals(raw, false, &[])
    }

    pub fn from_raw_map64k_with_visuals(
        raw: &[u8],
        frame0_is_void: bool,
        opaque_frames: &[bool],
    ) -> Option<Self> {
        if raw.len() < 65536 {
            return None;
        }
        let mut blocked = vec![0u8; MAP_HEIGHT * HALF_COLS];
        let mut occluded = vec![0u8; MAP_HEIGHT * HALF_COLS];
        let mut void_tiles = vec![0u8; MAP_HEIGHT * HALF_COLS];
        // Note: DOS PAL MAP.MKF stores `[128 (y)][64 (x)][2 (h)]` of 4-byte tiles!
        for y in 0..MAP_HEIGHT {
            for x in 0..MAP_WIDTH {
                for h in 0..2 {
                    let off = ((y * MAP_WIDTH + x) * 2 + h) * 4;
                    let btm_sprite = (raw[off] as u16) | (((raw[off + 1] as u16) & 0x10) << 4);
                    let is_blk = (raw[off + 1] & 0x20) != 0;
                    let raw_top = (raw[off + 2] as u16) | (((raw[off + 3] as u16) & 0x10) << 4);
                    let top_layer = raw[off + 3] & 0x0F;
                    let idx = y * HALF_COLS + x * 2 + h;
                    blocked[idx] = u8::from(is_blk);
                    let top_is_opaque = raw_top != 0
                        && opaque_frames
                            .get(raw_top as usize - 1)
                            .copied()
                            .unwrap_or(false);
                    occluded[idx] = u8::from(top_layer > 0 && top_is_opaque);
                    void_tiles[idx] =
                        u8::from(frame0_is_void && btm_sprite == 0 && raw_top == 0 && !is_blk);
                }
            }
        }
        Some(Self {
            blocked,
            occluded,
            void_tiles,
        })
    }

    #[inline]
    pub fn is_blocked(&self, x: i32, y: i32, h: i32) -> bool {
        if x < 0 || x >= MAP_WIDTH as i32 || y < 0 || y >= MAP_HEIGHT as i32 || (h != 0 && h != 1) {
            return true;
        }
        self.blocked[(y as usize) * HALF_COLS + (x as usize) * 2 + (h as usize)] != 0
    }

    #[inline]
    pub fn is_occluded(&self, x: i32, y: i32, h: i32) -> bool {
        if x < 0 || x >= MAP_WIDTH as i32 || y < 0 || y >= MAP_HEIGHT as i32 || (h != 0 && h != 1) {
            return false;
        }
        self.occluded
            .get((y as usize) * HALF_COLS + (x as usize) * 2 + (h as usize))
            .copied()
            .unwrap_or(0)
            != 0
    }

    #[inline]
    pub fn is_void(&self, x: i32, y: i32, h: i32) -> bool {
        if x < 0 || x >= MAP_WIDTH as i32 || y < 0 || y >= MAP_HEIGHT as i32 || (h != 0 && h != 1) {
            return false;
        }
        self.void_tiles
            .get((y as usize) * HALF_COLS + (x as usize) * 2 + (h as usize))
            .copied()
            .unwrap_or(0)
            != 0
    }

    pub fn set_blocked(&mut self, x: i32, y: i32, h: i32, val: bool) {
        if x >= 0 && x < MAP_WIDTH as i32 && y >= 0 && y < MAP_HEIGHT as i32 && (h == 0 || h == 1) {
            self.blocked[(y as usize) * HALF_COLS + (x as usize) * 2 + (h as usize)] =
                u8::from(val);
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
struct AStarNode {
    f_score: i32,
    g_score: i32,
    idx: usize,
}

impl Ord for AStarNode {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .f_score
            .cmp(&self.f_score)
            .then_with(|| self.g_score.cmp(&other.g_score))
    }
}

impl PartialOrd for AStarNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Find a walkable diagonal path of world `(px, py)` waypoints from `(sx, sy)` to `(tx, ty)`.
/// If target half-tile itself is blocked (e.g. player clicked an NPC/chest), searches to the closest
/// adjacent walkable half-tile within `max_nodes` expansions.
pub fn find_path_world(
    grid: &CollisionGrid,
    sx: i32,
    sy: i32,
    tx: i32,
    ty: i32,
    max_steps: usize,
) -> Vec<(i32, i32)> {
    let (shx, shy, sh) = world_to_half_tile(sx, sy);
    let (thx, thy, th) = world_to_half_tile(tx, ty);
    if shx < 0 || shx >= MAP_WIDTH as i32 || shy < 0 || shy >= MAP_HEIGHT as i32 {
        return Vec::new();
    }
    let tc_x = thx.clamp(0, MAP_WIDTH as i32 - 1);
    let tc_y = thy.clamp(0, MAP_HEIGHT as i32 - 1);
    let tc_h = th.clamp(0, 1);

    let to_idx = |x: i32, y: i32, h: i32| -> usize {
        (y as usize) * HALF_COLS + (x as usize) * 2 + (h as usize)
    };
    let from_idx = |idx: usize| -> (i32, i32, i32) {
        let y = (idx / HALF_COLS) as i32;
        let rem = idx % HALF_COLS;
        ((rem / 2) as i32, y, (rem % 2) as i32)
    };
    let heuristic = |x: i32, y: i32, h: i32| -> i32 {
        let (px1, py1) = half_tile_to_world(x, y, h);
        let (px2, py2) = half_tile_to_world(tc_x, tc_y, tc_h);
        ((px1 - px2).abs() / 16).max((py1 - py2).abs() / 8)
    };

    let start_idx = to_idx(shx, shy, sh);
    let goal_idx = to_idx(tc_x, tc_y, tc_h);
    if start_idx == goal_idx {
        return Vec::new();
    }

    let total = MAP_HEIGHT * HALF_COLS;
    let mut g_best = vec![i32::MAX; total];
    let mut came_from = vec![usize::MAX; total];
    let mut heap = BinaryHeap::new();

    g_best[start_idx] = 0;
    heap.push(AStarNode {
        f_score: heuristic(shx, shy, sh),
        g_score: 0,
        idx: start_idx,
    });

    let mut best_fallback = start_idx;
    let mut best_h = heuristic(shx, shy, sh);
    let mut expanded = 0usize;

    while let Some(node) = heap.pop() {
        if node.idx == goal_idx {
            best_fallback = goal_idx;
            break;
        }
        if node.g_score > g_best[node.idx] {
            continue;
        }
        expanded += 1;
        if expanded > max_steps {
            break;
        }

        let (cx, cy, ch) = from_idx(node.idx);
        let ch_est = heuristic(cx, cy, ch);
        if ch_est < best_h {
            best_h = ch_est;
            best_fallback = node.idx;
        }

        for dir in 0..4u8 {
            let (nx, ny, nh) = step_half_tile(cx, cy, ch, dir);
            if nx < 0 || nx >= MAP_WIDTH as i32 || ny < 0 || ny >= MAP_HEIGHT as i32 {
                continue;
            }
            let nidx = to_idx(nx, ny, nh);
            // Allow stepping onto goal if goal is walkable, otherwise stop adjacent to goal
            if grid.is_blocked(nx, ny, nh) {
                if nidx == goal_idx && ch_est <= 1 {
                    best_fallback = node.idx;
                    heap.clear();
                    break;
                }
                continue;
            }
            let ng = node.g_score + 1;
            if ng < g_best[nidx] {
                g_best[nidx] = ng;
                came_from[nidx] = node.idx;
                heap.push(AStarNode {
                    f_score: ng + heuristic(nx, ny, nh),
                    g_score: ng,
                    idx: nidx,
                });
            }
        }
    }

    if best_fallback == start_idx {
        return Vec::new();
    }

    let mut path = Vec::new();
    let mut cur = best_fallback;
    while cur != start_idx && cur != usize::MAX {
        let (x, y, h) = from_idx(cur);
        path.push(half_tile_to_world(x, y, h));
        cur = came_from[cur];
    }
    path.reverse();
    path
}
