//! On-foot reachability for Text Mode (#17).
//!
//! The map data only blocks the *edges* of causeways and platforms; water between them is
//! unblocked but unreachable on foot (the only way across is a touch trigger that moves the party,
//! e.g. 仙灵岛 lotus-pad hops). A flood fill from the party over unblocked half-tiles, where
//! displacement touch-trigger zones are reachable but not expanded through, separates
//! "walkable from here" from "unblocked but unreachable" (`~` on the minimap).

use pal_core::game::Game;
use pal_core::isometric::{half_tile_to_world, step_half_tile, world_to_half_tile, MAP_HEIGHT, MAP_WIDTH};
use std::collections::{HashSet, VecDeque};

pub(crate) type Tile = (i32, i32, i32);

/// Active touch trigger (`trigger_mode >= 4`) whose script relocates the party.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HopZone {
    pub id: u16,
    pub x: i32,
    pub y: i32,
    pub radius: i32,
}

impl HopZone {
    pub fn contains(&self, wx: i32, wy: i32) -> bool {
        (self.x - wx).abs() + (self.y - wy).abs() * 2 < self.radius
    }
}

/// Displacement touch-trigger zones in the current scene, same rule as the core's click-to-walk
/// avoid grid. Zones the leader already stands in are skipped (they are suppressed until left).
pub(crate) fn hop_zones(g: &Game) -> Vec<HopZone> {
    let lead = g.world.leader();
    let mut out = Vec::new();
    for id in g.world.current_event_ids() {
        let Some(ev) = g.world.event(id) else { continue };
        if ev.state <= 0 || ev.vanish_time > 0 || ev.trigger.is_none() || ev.trigger_mode < 4 {
            continue;
        }
        if !g.is_displacement_portal(id) {
            continue;
        }
        let z = HopZone { id, x: ev.x, y: ev.y, radius: (ev.trigger_mode - 4) * 32 + 16 };
        if !z.contains(lead.x, lead.y) {
            out.push(z);
        }
    }
    out
}

/// Result of [`flood_reachable`].
pub(crate) struct Reach {
    pub tiles: HashSet<Tile>,
    /// Hop-zone event ids touched by the fill, nearest (BFS order) first.
    pub hops: Vec<u16>,
}

/// Flood-fill from `start` (world px) over tiles where `blocked` is false. Tiles inside a hop zone
/// are reachable but terminal (stepping there moves the party elsewhere).
pub(crate) fn flood_reachable(start: (i32, i32), blocked: &dyn Fn(i32, i32, i32) -> bool, zones: &[HopZone]) -> Reach {
    let s = world_to_half_tile(start.0, start.1);
    let mut tiles = HashSet::from([s]);
    let mut hops = Vec::new();
    let mut queue = VecDeque::from([s]);
    while let Some((x, y, h)) = queue.pop_front() {
        for dir in 0..4u8 {
            let n = step_half_tile(x, y, h, dir);
            if n.0 < 0 || n.0 >= MAP_WIDTH as i32 || n.1 < 0 || n.1 >= MAP_HEIGHT as i32 {
                continue;
            }
            if tiles.contains(&n) || blocked(n.0, n.1, n.2) {
                continue;
            }
            tiles.insert(n);
            let (wx, wy) = half_tile_to_world(n.0, n.1, n.2);
            if let Some(z) = zones.iter().find(|z| z.contains(wx, wy)) {
                if !hops.contains(&z.id) {
                    hops.push(z.id);
                }
                continue; // terminal: the trigger fires here
            }
            queue.push_back(n);
        }
    }
    Reach { tiles, hops }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_zones_are_reachable_but_not_expanded() {
        // Open floor; a hop zone two steps NE of the start cuts nothing (floor is open), but the
        // zone tile itself is reported and its far side stays reachable via other routes.
        let start = half_tile_to_world(10, 20, 0);
        let zone_tile = step_half_tile(10, 20, 0, 2);
        let (zx, zy) = half_tile_to_world(zone_tile.0, zone_tile.1, zone_tile.2);
        let zones = [HopZone { id: 7, x: zx, y: zy, radius: 16 }];
        // Wall everything except a 1-wide NE corridor through the zone.
        let corridor: HashSet<Tile> = {
            let mut c = HashSet::new();
            let mut t = (10, 20, 0);
            for _ in 0..4 {
                c.insert(t);
                t = step_half_tile(t.0, t.1, t.2, 2);
            }
            c
        };
        let blocked = |x, y, h| !corridor.contains(&(x, y, h));
        let r = flood_reachable(start, &blocked, &zones);
        assert_eq!(r.hops, vec![7]);
        assert!(r.tiles.contains(&zone_tile));
        let beyond = step_half_tile(zone_tile.0, zone_tile.1, zone_tile.2, 2);
        assert!(!r.tiles.contains(&beyond), "fill must not pass through a hop zone");
    }
}
