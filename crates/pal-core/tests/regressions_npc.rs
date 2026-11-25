//! NPC motion, monster AI and item regressions, ported from the legacy
//! `tools/verify_npc_motion.mjs` suite to the Rust core.

mod common;

use common::NativeHost;
use pal_core::isometric::{half_tile_to_world, step_half_tile, world_to_half_tile};
use pal_core::script::ScriptPos;
use std::collections::BTreeSet;

fn iso(ax: i32, ay: i32, bx: i32, by: i32) -> i32 {
    (ax - bx).abs() + (ay - by).abs() * 2
}

/// Walkable neighbour of `(x, y)` (legacy `wasm.tryStep`).
fn try_step(h: &NativeHost, x: i32, y: i32, dir: u8) -> Option<(i32, i32)> {
    let (tx, ty, th) = world_to_half_tile(x, y);
    let (nx, ny, nh) = step_half_tile(tx, ty, th, dir);
    if h.game.grid.is_blocked(nx, ny, nh) {
        return None;
    }
    Some(half_tile_to_world(nx, ny, nh))
}

#[test]
fn scene3_drunkard_frame_cycle_and_event_sync() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(3, false);
    h.game.world.set_party_tile_pos(39, 92, 0);
    assert_eq!(h.game.world.event(63).unwrap().sprite_frames_auto, 3, "mgo_056 frame count");
    let mut frames = Vec::new();
    for _ in 0..18 {
        h.auto_tick();
        frames.push(h.game.world.event(63).unwrap().current_frame);
    }
    // s_141e 15-tick cycle: frame 1 for two ticks, then frame 0 across `wait_ticks 10`.
    assert_eq!(frames[..15].iter().filter(|&&f| f == 1).count(), 2, "{frames:?}");
    assert!(frames[3..15].iter().all(|&f| f == 0), "{frames:?}");
    assert_eq!(h.game.world.event(64).unwrap().state, 2);
    assert_eq!(h.game.world.event(65).unwrap().state, 2);
}

#[test]
fn scene48_deer_steps_onto_placed_animal_trap_while_respecting_walls() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(48, false);
    h.game.world.set_party_tile_pos(10, 10, 0);

    // Place animal trap ev#798 (state=2, auto=s_39ee) directly on deer ev#797's s_39cb target (31, 27, 1) -> (1008, 440)
    // and start deer ev#797 walking from (28, 30, 1) -> (912, 488) across the trap.
    let (trap_x, trap_y) = half_tile_to_world(31, 27, 1);
    let (deer_x, deer_y) = half_tile_to_world(28, 30, 1);
    {
        let trap = h.game.world.event_mut(798).unwrap();
        trap.x = trap_x;
        trap.y = trap_y;
        trap.state = 2;
    }
    let s39cb = h.game.prog.label_id("s_39cb").expect("s_39cb");
    {
        let deer = h.game.world.event_mut(797).unwrap();
        deer.x = deer_x;
        deer.y = deer_y;
        deer.state = 2;
        deer.auto = ScriptPos::new(s39cb, 0);
    }

    let mut caught = false;
    for _ in 0..80 {
        h.auto_tick();
        if h.game.world.event(798).unwrap().state == 0
            && h.game.world.event(797).unwrap().trigger_mode == 1
        {
            caught = true;
            break;
        }
    }
    assert!(
        caught,
        "Scene #48 deer ev#797 must step onto placed trap ev#798 (state=2) and trigger s_39f2"
    );
}

#[test]
fn scene4_patrols_ambient_npcs_and_double_door() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(4, true);
    h.game.world.set_party_tile_pos(12, 70, 0);
    let patrols = [77u16, 84, 85];
    let ambient = [78u16, 83, 88, 90, 92, 113];
    let mut pos: Vec<BTreeSet<(i32, i32)>> = vec![BTreeSet::new(); 9];
    let mut frm: Vec<BTreeSet<i32>> = vec![BTreeSet::new(); 9];
    for t in 0..60 {
        h.auto_tick();
        for &id in &patrols {
            assert!(!h.event_on_blocked_tile(id), "patrol Ev#{id} on blocked tile at tick {t}");
        }
        for (k, &id) in patrols.iter().chain(&ambient).enumerate() {
            let e = h.game.world.event(id).unwrap();
            pos[k].insert((e.x, e.y));
            frm[k].insert(e.current_frame);
        }
    }
    for k in 0..3 {
        assert!(pos[k].len() >= 5, "patrol Ev#{} positions {:?}", patrols[k], pos[k]);
        assert!(frm[k].len() >= 3, "patrol Ev#{} frames {:?}", patrols[k], frm[k]);
    }
    assert_eq!(h.game.world.event(78).unwrap().sprite_frames_auto, 4);
    assert_eq!(frm[3].len(), 4, "Ev#78 cycles all 4 frames");
    for k in 4..8 {
        assert!(frm[k].len() >= 2, "ambient Ev#{} frames {:?}", ambient[k - 3], frm[k]);
    }
    assert!(frm[8].contains(&4) && frm[8].contains(&5), "fisherman Ev#113 frames {:?}", frm[8]);

    // Double door: opening Ev#102 (s_0014) syncs Ev#103 on the next auto tick (s_1248).
    assert_eq!(h.game.world.event(102).unwrap().state, 2);
    assert!(h.game.trigger_event(102));
    assert!(h.settle(10_000));
    assert_eq!(h.game.world.event(102).unwrap().state, 0);
    h.auto_tick();
    assert_eq!(h.game.world.event(103).unwrap().state, 0, "event_sync_state");
}

#[test]
fn monsters_wander_chase_and_respect_incense() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    {
        let r = &mut h.game.world.roles[0];
        r.max_hp = 9999;
        r.hp = 9999;
        r.stats[0] = 999;
    }
    h.enter_scene(40, false);
    h.game.world.set_party_world_pos(240, 1480);
    let (mut g, mut f) = (BTreeSet::new(), BTreeSet::new());
    for _ in 0..25 {
        h.auto_tick();
        let e = h.game.world.event(638).unwrap();
        g.insert((e.x, e.y));
        let e = h.game.world.event(641).unwrap();
        f.insert((e.x, e.y));
    }
    assert!(g.len() >= 2 && f.len() >= 2, "far monsters wander: {} / {}", g.len(), f.len());

    // Put the player two half-tiles from Ev#638.
    h.game.sync_scene_obstacles();
    let m = h.game.world.event(638).unwrap().clone();
    let pick = |h: &NativeHost, x, y, min| (0..4).filter_map(|d| try_step(h, x, y, d)).find(|&(sx, sy)| iso(sx, sy, m.x, m.y) >= min);
    let s1 = pick(&h, m.x, m.y, 32).expect("open step near Ev#638");
    let s2 = pick(&h, s1.0, s1.1, 64).or_else(|| pick(&h, s1.0, s1.1, 32)).unwrap_or(s1);
    h.game.world.set_party_world_pos(s2.0, s2.1);
    let before = iso(s2.0, s2.1, m.x, m.y);
    assert!(before > 0 && before <= 96, "within alert range: {before}");

    // 驱魔香 (#90) disables chasing for 600 ticks and is consumed.
    h.game.world.add_item(90, 1);
    assert!(h.use_item(90, 0));
    assert_eq!(h.game.world.chase_disabled_ticks, 600);
    assert_eq!(h.game.world.item_count(90), 0);
    h.auto_tick();
    assert_eq!(h.game.world.chase_disabled_ticks, 599);

    // Without incense the monster closes in and its touch starts a battle.
    h.game.world.chase_disabled_ticks = 0;
    let e = h.game.world.event_mut(638).unwrap();
    e.x = m.x;
    e.y = m.y;
    let battles = h.battles.len();
    let mut closest = before;
    for _ in 0..8 {
        h.auto_tick();
        let e = h.game.world.event(638).unwrap();
        let l = h.game.world.leader();
        closest = closest.min(iso(l.x, l.y, e.x, e.y));
        if h.battles.len() > battles {
            break;
        }
    }
    assert!(closest < before, "monster chased closer ({before} -> {closest})");
    assert!(h.battles.len() > battles, "touching the player starts a battle");

    // Fairy Island Ev#231 chases from 4 half-tiles away under 十里香 (#91).
    h.enter_scene(16, false);
    assert_eq!(h.game.world.event(231).unwrap().sprite_frames_auto, 2);
    let m = h.game.world.event(231).unwrap().clone();
    let mut p = (m.x, m.y);
    for _ in 0..4 {
        for d in [2u8, 1, 3, 0] {
            if let Some(n) = try_step(&h, p.0, p.1, d) {
                if iso(n.0, n.1, m.x, m.y) > iso(p.0, p.1, m.x, m.y) {
                    p = n;
                    break;
                }
            }
        }
    }
    h.game.world.set_party_world_pos(p.0, p.1);
    h.game.world.add_item(91, 1);
    assert!(h.use_item(91, 0));
    let d0 = iso(p.0, p.1, m.x, m.y);
    h.auto_tick();
    let e = h.game.world.event(231).unwrap();
    let l = h.game.world.leader();
    assert!(iso(l.x, l.y, e.x, e.y) < d0, "十里香 makes Ev#231 chase");
}

#[test]
fn quest_wine_and_medicine_items() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);
    h.game.world.add_item(272, 1);

    // 桂花酒 has no effect in scene 1 or far from the Daoist, and is kept.
    h.dialogues.clear();
    assert!(!h.use_item(272, 0));
    assert_eq!(h.game.world.item_count(272), 1);
    assert!(h.dialogues.iter().any(|d| d.contains("无任何效果")), "{:?}", h.dialogues);
    h.enter_scene(3, false);
    h.game.world.set_party_world_pos(1312, 1488);
    assert!(!h.use_item(272, 0));
    assert_eq!(h.game.world.item_count(272), 1);

    // Next to the Daoist (even facing away) it chains s_13ca.
    h.game.world.set_party_world_pos(1040, 1672);
    h.game.world.leader_mut().dir = 2;
    h.dialogues.clear();
    assert!(h.use_item(272, 0));
    let w = &h.game.world;
    let lbl = |p: ScriptPos| h.game.prog.label_name(p.label).to_string();
    assert_eq!(lbl(w.event(63).unwrap().trigger), "s_13ca");
    assert_eq!(w.event(63).unwrap().state, 0);
    assert_eq!(w.item_count(272), 0);
    assert_eq!(lbl(w.event(20).unwrap().trigger), "s_1425");
    assert!(h.dialogues.iter().any(|d| d.contains("山神庙")));
    h.auto_tick();
    assert_eq!(h.game.world.event(64).unwrap().state, 0);
    assert_eq!(h.game.world.event(65).unwrap().state, 0);

    // 止血草 heals +50 and is consumed; wasted neither at full HP nor on script_fail.
    h.game.world.add_item(99, 2);
    let r = &mut h.game.world.roles[0];
    r.max_hp = 200;
    r.hp = 120;
    assert!(h.use_item(99, 0));
    assert_eq!(h.game.world.roles[0].hp, 170);
    assert_eq!(h.game.world.item_count(99), 1);
    h.game.world.roles[0].hp = 200;
    assert!(!h.use_item(99, 0));
    assert_eq!(h.game.world.item_count(99), 1);
    h.game.world.add_item(285, 1);
    assert!(!h.use_item(285, 0), "捕兽夹 ends in script_fail");
    assert_eq!(h.game.world.item_count(285), 1);
}

#[test]
fn walking_npcs_respect_walls() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);
    h.game.world.set_party_tile_pos(41, 18, 0);
    let s135d = h.game.prog.label_id("s_135d").unwrap();
    let set = |h: &mut NativeHost, id: u16, state: i32, x: i32, y: i32, auto: pal_core::script::LabelId| {
        let e = h.game.world.event_mut(id).unwrap();
        e.state = state;
        e.x = x;
        e.y = y;
        e.direction = 0;
        e.current_frame = 0;
        e.auto = ScriptPos::new(auto, 0);
    };
    set(&mut h, 27, 2, 1360, 1064, s135d);
    for t in 0..20 {
        h.auto_tick();
        assert!(!h.event_on_blocked_tile(27), "Ev#27 entered a wall at tick {t}");
    }
    let e = h.game.world.event(27).unwrap();
    assert_eq!((e.x, e.y), (1296, 1032));

    // Solid NPCs facing a wall turn and animate but do not move.
    for (d, sx, sy) in [(0u8, 1360, 960), (1, 1360, 960), (1, 1256, 1012), (2, 1312, 984), (3, 1296, 968)] {
        let l = h.game.prog.load_block("_t_walk", &serde_json::json!([{"op": format!("npc_walk_dir_{d}")}, {"op": "end"}])).unwrap();
        set(&mut h, 27, 2, sx, sy, l);
        h.auto_tick();
        let e = h.game.world.event(27).unwrap();
        assert_eq!((e.x, e.y, e.direction, e.current_frame), (sx, sy, d as i32, 1), "dir {d} from ({sx},{sy})");
    }
    // Non-solid ambient sprites are not clamped to walkable tiles.
    set(&mut h, 17, 1, 1256, 1012, s135d);
    h.auto_tick();
    let e = h.game.world.event(17).unwrap();
    assert_eq!((e.x, e.y), (1248, 1008));
}

/// Put event `ev` at `(x, y)` running a one-instruction auto script and tick until it completes.
fn auto_walk_case(scene: u16, ev: u16, op: &str, args: serde_json::Value, x: i32, y: i32, max: u32) -> (u32, u32, i32, i32) {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(scene, false);
    h.game.world.set_party_tile_pos(10, 10, 0);
    let l = h.game.prog.load_block("_t_case", &serde_json::json!([{"op": op, "args": args}, {"op": "end"}])).unwrap();
    let e = h.game.world.event_mut(ev).unwrap();
    e.state = 2;
    e.x = x;
    e.y = y;
    e.current_frame = 0;
    e.auto = ScriptPos::new(l, 0);
    let mut ticks = 0;
    while ticks < max && h.game.world.event(ev).unwrap().auto.idx == 0 {
        h.auto_tick();
        ticks += 1;
        assert!(!h.event_on_blocked_tile(ev), "{op} tick {ticks} stepped onto a blocked tile");
    }
    let e = h.game.world.event(ev).unwrap();
    (ticks, e.auto.idx, e.x, e.y)
}

/// Same, but as a foreground script (legacy `runForegroundScript`).
fn fg_walk_case(scene: u16, ev: u16, op: &str, args: serde_json::Value, x: i32, y: i32) -> (i32, i32) {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(scene, false);
    h.game.world.set_party_tile_pos(10, 10, 0);
    h.game.prog.load_block("_t_fg", &serde_json::json!([{"op": op, "args": args}, {"op": "end"}])).unwrap();
    let e = h.game.world.event_mut(ev).unwrap();
    e.state = 2;
    e.x = x;
    e.y = y;
    e.current_frame = 0;
    e.auto = ScriptPos::NONE;
    assert!(h.game.run_script("_t_fg", ev));
    assert!(h.settle(200_000));
    let e = h.game.world.event(ev).unwrap();
    (e.x, e.y)
}

#[test]
fn npc_walk_to_detours_and_converges() {
    use serde_json::json;
    let (_, done, x, y) = auto_walk_case(1, 27, "npc_walk_to", json!([36, 71, 0]), 1152, 960, 100);
    assert_eq!((done, x, y), (1, 1152, 1136), "around-corner walk");
    for (op, args) in [("npc_walk_to_2", json!([36, 71])), ("npc_walk_to_2", json!([36, 71, 0])), ("npc_walk_to_fast_2", json!([36, 71])), ("npc_walk_to_fast_2", json!([36, 71, 0]))] {
        let (_, done, x, y) = auto_walk_case(1, 27, op, args, 1152, 960, 100);
        assert_eq!((done, x, y), (1, 1152, 1136), "{op} must move the calling event");
    }
    // Unreachable goal across a wall must give up instead of oscillating forever in auto mode.
    for op in ["npc_walk_to", "npc_walk_to_slow", "npc_walk_to_fast_2"] {
        let (ticks, done, ..) = auto_walk_case(1, 27, op, json!([43, 64, 0]), 1152, 960, 150);
        assert!(done == 1 && ticks < 150, "{op}: done={done} after {ticks} ticks");
    }
    // Foreground scripted `npc_walk_to` snaps to exact `(gx, gy)` on stall/budget exhaustion
    // (SDLPAL `PAL_NPCWalkTo` convergence guarantee, #200).
    assert_eq!(
        fg_walk_case(1, 27, "npc_walk_to", json!([43, 64, 0]), 1152, 960),
        (43 * 32, 64 * 16),
        "foreground scripted npc_walk_to must converge to exact goal (gx, gy)"
    );

    // Long detours (legacy "Challenger 1" repros): (scene, ev, start, goal, args, ticks, ops, fg).
    let cases: [(u16, u16, (i32, i32), (i32, i32), [i32; 3], u32, &[&str], bool); 7] = [
        (4, 77, (1472, 1600), (1280, 1584), [40, 99, 0], 120, &["npc_walk_to", "npc_walk_to_slow"], true),
        (1, 27, (1152, 752), (1696, 784), [53, 49, 0], 200, &["npc_walk_to_slow"], false),
        (1, 27, (640, 1168), (1024, 1152), [32, 72, 0], 150, &["npc_walk_to", "npc_walk_to_slow"], true),
        (4, 77, (928, 1040), (896, 800), [28, 50, 0], 150, &["npc_walk_to", "npc_walk_to_slow"], false),
        (4, 77, (1344, 1632), (768, 1472), [24, 92, 0], 200, &["npc_walk_to_slow"], false),
        (1, 27, (1648, 1608), (576, 1152), [18, 72, 0], 350, &["npc_walk_to", "npc_walk_to_slow", "npc_walk_to_fast_2"], false),
        (1, 27, (1696, 528), (592, 1352), [18, 84, 1], 450, &["npc_walk_to_slow"], false),
    ];
    for (scene, ev, s, g, args, max, ops, fg) in cases {
        for op in ops {
            let (ticks, done, x, y) = auto_walk_case(scene, ev, op, json!(args), s.0, s.1, max);
            assert_eq!((done, x, y), (1, g.0, g.1), "scene {scene} {op} {s:?}->{g:?} after {ticks} ticks");
        }
        if fg {
            assert_eq!(fg_walk_case(scene, ev, "npc_walk_to", json!(args), s.0, s.1), g, "scene {scene} foreground {s:?}->{g:?}");
        }
    }
}

fn place_auto(h: &mut NativeHost, ev: u16, x: i32, y: i32, script: Option<(&str, serde_json::Value)>) {
    let auto = match script {
        Some((name, steps)) => ScriptPos::new(h.game.prog.load_block(name, &steps).unwrap(), 0),
        None => ScriptPos::NONE,
    };
    let e = h.game.world.event_mut(ev).unwrap();
    e.state = 2;
    e.x = x;
    e.y = y;
    e.current_frame = 0;
    e.auto = auto;
}

fn walk(op: &str, args: serde_json::Value) -> serde_json::Value {
    serde_json::json!([{"op": op, "args": args}, {"op": "end"}])
}

fn fresh(scene: u16) -> NativeHost {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(scene, false);
    h.game.world.set_party_tile_pos(10, 10, 0);
    h
}

#[test]
fn scene4_patrols_complete_full_loops_and_scene5_patrol() {
    let mut h = fresh(4);
    h.game.world.set_party_tile_pos(12, 70, 0);
    let (l77, l84) = (h.game.prog.label_id("s_14a8").unwrap(), h.game.prog.label_id("s_1657").unwrap());
    let mut seen = [BTreeSet::new(), BTreeSet::new()];
    let mut loops = [0, 0];
    let mut prev = [0u32, 0];
    for t in 0..400 {
        h.auto_tick();
        for (k, (id, lbl)) in [(77u16, l77), (84, l84)].into_iter().enumerate() {
            assert!(!h.event_on_blocked_tile(id), "Ev#{id} blocked at tick {t}");
            let e = h.game.world.event(id).unwrap();
            seen[k].insert((e.x, e.y));
            if e.auto.label == lbl && e.auto.idx == 0 && prev[k] > 0 && seen[k].len() >= 15 {
                loops[k] += 1;
            }
            prev[k] = e.auto.idx;
        }
    }
    assert!(loops[0] >= 1 && seen[0].len() >= 15, "Ev#77 loops={} seen={}", loops[0], seen[0].len());
    assert!(loops[1] >= 1 && seen[1].len() >= 15, "Ev#84 loops={} seen={}", loops[1], seen[1].len());

    let mut h = fresh(5);
    assert_eq!(h.game.prog.label_name(h.game.world.event(122).unwrap().auto.label), "s_0067");
    let mut pos = BTreeSet::new();
    for t in 0..80 {
        h.auto_tick();
        assert!(!h.event_on_blocked_tile(122), "Ev#122 blocked at tick {t}");
        let e = h.game.world.event(122).unwrap();
        pos.insert((e.x, e.y));
    }
    assert!(pos.len() >= 8, "Ev#122 patrolled {} positions", pos.len());
}

#[test]
fn npcs_pass_each_other_and_repath_around_blockers() {
    use serde_json::json;
    // Two NPCs walking head-on both finish without standing on each other.
    let mut h = fresh(1);
    place_auto(&mut h, 27, 1280, 1024, Some(("_t_a", walk("npc_walk_to", json!([42, 66, 0])))));
    place_auto(&mut h, 26, 1344, 1056, Some(("_t_b", walk("npc_walk_to", json!([40, 64, 0])))));
    for t in 0..80 {
        if h.game.world.event(27).unwrap().auto.idx == 1 && h.game.world.event(26).unwrap().auto.idx == 1 {
            break;
        }
        h.auto_tick();
        assert!(!h.event_on_blocked_tile(27) && !h.event_on_blocked_tile(26), "head-on blocked at tick {t}");
    }
    assert_eq!((h.game.world.event(27).unwrap().auto.idx, h.game.world.event(26).unwrap().auto.idx), (1, 1));

    // A blocker parked on the 10th waypoint forces a repath; the walker still reaches its goal.
    let mut h = fresh(1);
    h.game.sync_scene_obstacles_excluding(27);
    let path = pal_core::isometric::find_path_world(&h.game.grid, 1152, 752, 1696, 784, 16384);
    assert!(path.len() > 12);
    place_auto(&mut h, 26, path[10].0, path[10].1, None);
    place_auto(&mut h, 27, 1152, 752, Some(("_t_r", walk("npc_walk_to_slow", json!([53, 49, 0])))));
    for t in 0..220 {
        if h.game.world.event(27).unwrap().auto.idx == 1 {
            break;
        }
        h.auto_tick();
        assert!(!h.event_on_blocked_tile(27), "repath blocked at tick {t}");
    }
    let e = h.game.world.event(27).unwrap();
    assert_eq!((e.auto.idx, e.x, e.y), (1, 1696, 784));

    // A goal blocked by an NPC that later walks away is reached exactly.
    for (op, vacate) in [("npc_walk_to", 3), ("npc_walk_to_slow", 6)] {
        let mut h = fresh(1);
        place_auto(&mut h, 27, 1232, 1032, Some(("_t_v", walk(op, json!([40, 62, 1])))));
        place_auto(&mut h, 26, 1296, 1000, None);
        for t in 0..40 {
            if h.game.world.event(27).unwrap().auto.idx == 1 {
                break;
            }
            if t == vacate {
                let b = h.game.world.event_mut(26).unwrap();
                b.x = 32;
                b.y = 32;
            }
            h.auto_tick();
            assert!(!h.event_on_blocked_tile(27));
        }
        let e = h.game.world.event(27).unwrap();
        assert_eq!((e.auto.idx, e.x, e.y), (1, 1296, 1000), "{op} vacated goal");
    }

    // A doorway blocked for 5 ticks is passed once it clears.
    let mut h = fresh(1);
    place_auto(&mut h, 27, 688, 584, Some(("_t_d", walk("npc_walk_to", json!([24, 39, 0])))));
    place_auto(&mut h, 26, 736, 608, None);
    for t in 0..40 {
        if h.game.world.event(27).unwrap().auto.idx == 1 {
            break;
        }
        if t == 5 {
            let b = h.game.world.event_mut(26).unwrap();
            b.x = 32;
            b.y = 32;
        }
        h.auto_tick();
        assert!(!h.event_on_blocked_tile(27));
    }
    let e = h.game.world.event(27).unwrap();
    assert_eq!((e.auto.idx, e.x, e.y), (1, 768, 624));
}

#[test]
fn thin_walls_are_never_crossed() {
    use serde_json::json;
    // Scene 48: the goal is behind a 1px wall seam; the walker must take the A* detour.
    let (ticks, done, x, y) = auto_walk_case(48, 790, "npc_walk_to_fast_2", json!([15, 38, 1]), 496, 600, 400);
    assert!(done == 1 && ticks > 10 && (x, y) == (496, 616), "thin detour: {ticks} ticks -> ({x},{y})");
    // Scene 1: goal across a wall with no route -> give up in place.
    let (_, done, x, y) = auto_walk_case(1, 27, "npc_walk_to_fast_2", json!([21, 36, 0]), 672, 560, 50);
    assert_eq!((done, x, y), (1, 672, 560));

    // Scene 48 maze seam at y=304 near x=1296, both as auto script and in the foreground.
    for (sx, sy, args, goal) in [(1312, 272, [40, 21, 0], (1280, 336)), (1280, 272, [41, 21, 0], (1312, 336))] {
        for fg in [false, true] {
            let mut h = fresh(48);
            for id in h.game.world.current_event_ids() {
                let e = h.game.world.event_mut(id).unwrap();
                if id != 790 && e.state >= 2 {
                    e.x = 32;
                    e.y = 32;
                }
            }
            let steps = walk("npc_walk_to", json!(args));
            if fg {
                place_auto(&mut h, 790, sx, sy, None);
                h.game.prog.load_block("_t_seam_fg", &steps).unwrap();
                assert!(h.game.run_script("_t_seam_fg", 790));
            } else {
                place_auto(&mut h, 790, sx, sy, Some(("_t_seam", steps)));
            }
            let mut ticks = 0;
            let mut prev_y = sy;
            loop {
                let done = if fg { !h.game.is_busy() } else { h.game.world.event(790).unwrap().auto.idx == 1 };
                if done || ticks >= 3000 {
                    break;
                }
                if fg {
                    h.tick(pal_core::game::TickInput::IDLE);
                } else {
                    h.auto_tick();
                }
                ticks += 1;
                assert!(!h.event_on_blocked_tile(790), "seam fg={fg} tick {ticks} blocked");
                let e = h.game.world.event(790).unwrap();
                let crossed = (prev_y < 304) != (e.y < 304);
                assert!(!(crossed && (e.x - 1296).abs() < 40), "seam fg={fg} crossed the wall at ({}, {})", e.x, e.y);
                prev_y = e.y;
            }
            let e = h.game.world.event(790).unwrap();
            assert!(ticks > 100, "seam fg={fg} must take the full maze detour, took {ticks}");
            assert_eq!((e.x, e.y), goal, "seam fg={fg}");
        }
    }
}

#[test]
fn aborted_foreground_walk_does_not_corrupt_the_next_one() {
    use serde_json::json;
    let mut h = fresh(1);
    place_auto(&mut h, 27, 640, 1168, None);
    h.game.prog.load_block("_t_ab1", &walk("npc_walk_to_slow", json!([32, 72, 0]))).unwrap();
    h.game.prog.load_block("_t_ab2", &walk("npc_walk_to", json!([32, 72, 0]))).unwrap();
    assert!(h.game.run_script("_t_ab1", 27));
    for _ in 0..4 {
        h.tick(pal_core::game::TickInput::IDLE);
    }
    h.game.abort_foreground();
    assert!(h.game.run_script("_t_ab2", 27));
    assert!(h.settle(50_000));
    let e = h.game.world.event(27).unwrap();
    assert_eq!((e.x, e.y), (1024, 1152));
}
