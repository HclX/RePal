//! Pure-Rust Field Movement, Trigger Detection, Camera & Y-Sorted DrawList Builder.

use crate::isometric::{half_tile_to_world, step_half_tile, world_to_half_tile, CollisionGrid};
use crate::state::GameState;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrawSprite {
    /// 0 = party member, 1 = event/NPC, 2 = follower sprite
    pub kind: u32,
    pub id: u32,
    pub world_x: i32,
    pub world_y: i32,
    pub sprite_id: u32,
    pub dir: u32,
    pub frame: u32,
    pub layer: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CameraViewport {
    pub cam_x: i32,
    pub cam_y: i32,
}

pub fn compute_camera(leader_x: i32, leader_y: i32, view_w: i32, view_h: i32) -> CameraViewport {
    let max_x = (2048 - view_w).max(0);
    let max_y = (2048 - view_h).max(0);
    CameraViewport {
        cam_x: (leader_x - view_w / 2).clamp(0, max_x),
        cam_y: (leader_y - view_h / 2).clamp(0, max_y),
    }
}

/// Step party leader by 1 isometric half-tile in `dir` (0=SW,1=SE,2=NE,3=NW) if unblocked,
/// shifting trailing party members cleanly along the leader's previous coordinates.
pub fn try_manual_step(state: &mut GameState, grid: &CollisionGrid, dir: u8) -> bool {
    let Some(lead) = state.party.first().cloned() else {
        return false;
    };
    let (tx, ty, h) = world_to_half_tile(lead.x, lead.y);
    let (nx, ny, nh) = step_half_tile(tx, ty, h, dir);
    if grid.is_blocked(nx, ny, nh) {
        if let Some(l) = state.party.first_mut() {
            l.dir = dir & 3;
        }
        return false;
    }
    let (wx, wy) = half_tile_to_world(nx, ny, nh);
    let mut prev_x = lead.x;
    let mut prev_y = lead.y;
    let mut prev_dir = lead.dir;

    if let Some(l) = state.party.first_mut() {
        l.x = wx;
        l.y = wy;
        l.dir = dir & 3;
        l.frame = (l.frame + 1) % 3;
    }

    for pm in state.party.iter_mut().skip(1) {
        let cur_x = pm.x;
        let cur_y = pm.y;
        let cur_dir = pm.dir;
        pm.x = prev_x;
        pm.y = prev_y;
        pm.dir = prev_dir;
        pm.frame = (pm.frame + 1) % 3;
        prev_x = cur_x;
        prev_y = cur_y;
        prev_dir = cur_dir;
    }
    true
}

/// Build Y-sorted sprite draw list for the current scene in Rust.
pub fn build_field_draw_list(state: &GameState, ev_start: u16, ev_end: u16) -> Vec<DrawSprite> {
    let mut out = Vec::with_capacity(64);
    for (idx, pm) in state.party.iter().enumerate() {
        let sprite_id = state
            .roles
            .get(pm.role_id as usize)
            .map(|r| r.walk_sprite_mgo as u32)
            .unwrap_or(0);
        out.push(DrawSprite {
            kind: 0,
            id: idx as u32,
            world_x: pm.x,
            world_y: pm.y,
            sprite_id,
            dir: pm.dir as u32,
            frame: pm.frame as u32,
            layer: state.party_layer,
        });
    }
    let last_pm = state.party.last().cloned().unwrap_or_default();
    for (fi, &spr_id) in state.follower_sprites.iter().enumerate() {
        if spr_id == 0 {
            continue;
        }
        let (fx, fy, fdir) = state.follower_positions.get(fi).copied().unwrap_or((
            last_pm.x + (fi as i32 + 1) * 16,
            last_pm.y + (fi as i32 + 1) * 8,
            last_pm.dir,
        ));
        out.push(DrawSprite {
            kind: 2,
            id: fi as u32,
            world_x: fx,
            world_y: fy,
            sprite_id: spr_id as u32,
            dir: fdir as u32,
            frame: last_pm.frame as u32,
            layer: state.party_layer,
        });
    }
    for ev in &state.events {
        if ev.id < ev_start || ev.id > ev_end {
            continue;
        }
        if ev.state <= 0 || ev.vanish_time > 0 || ev.sprite_id == 0 {
            continue;
        }
        out.push(DrawSprite {
            kind: 1,
            id: ev.id as u32,
            world_x: ev.x as i32,
            world_y: ev.y as i32,
            sprite_id: ev.sprite_id as u32,
            dir: ev.dir as u32,
            frame: ev.current_frame as u32,
            layer: ev.layer as i32,
        });
    }
    // On equal Y+layer baseline, draw events (e.g. rideable lotus leaf mgo_084, layer=0)
    // BEFORE party/follower members so the player stands on top of ground/ride sprites (#98).
    out.sort_by_key(|s| (s.world_y + s.layer * 8, if s.kind != 1 { 1u32 } else { 0u32 }, s.kind, s.id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::EventState;

    #[test]
    fn test_step_and_ysort_draw_list() {
        let mut st = GameState::new();
        st.party[0].x = 640;
        st.party[0].y = 320;
        st.events.push(EventState {
            id: 10,
            x: 640,
            y: 300,
            state: 2,
            sprite_id: 21,
            trigger_mode: 2,
            script_trigger: 100,
            ..Default::default()
        });
        let grid = CollisionGrid::empty();
        assert!(try_manual_step(&mut st, &grid, 1));
        assert_eq!(st.party[0].y, 312);

        let list = build_field_draw_list(&st, 1, 20);
        assert_eq!(list.len(), 2);
        // Event at y=300 sorts before leader at y=328
        assert_eq!(list[0].kind, 1);

        // Follower sprites (`0x0098` party_set_follower) must be emitted as `kind == 2`.
        st.follower_sprites = vec![45];
        st.follower_positions = vec![(624, 304, 1)];
        let list2 = build_field_draw_list(&st, 1, 20);
        assert_eq!(list2.len(), 3);
        let follower = list2.iter().find(|s| s.kind == 2).expect("follower sprite must be rendered");
        assert_eq!(follower.sprite_id, 45);
        assert_eq!((follower.world_x, follower.world_y), (624, 304));
        assert_eq!(list[1].kind, 0);
    }
}
