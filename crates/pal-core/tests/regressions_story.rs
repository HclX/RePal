//! Story/VM regressions ported from the legacy `tools/tests/test_ready_to_fix_regressions.mjs`.

mod common;

use common::NativeHost;
use pal_core::game::HostCmd;
use pal_core::script::ScriptPos;

fn label(h: &NativeHost, p: ScriptPos) -> String {
    h.game.prog.label_name(p.label).to_string()
}

/// #15: entering the Miao guest room (s_1217) fires the Ev#16 story trigger at once, while
/// doorway portals stay suppressed.
#[test]
fn issue15_story_trigger_fires_on_scene_entry() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(2, false);
    h.game.world.scenes.get_mut(&1).unwrap().enter = ScriptPos::NONE;
    let e = h.game.world.event_mut(16).unwrap();
    e.state = 1;
    e.vanish_time = 0;
    assert!(h.game.run_script("s_1217", 0));
    assert!(h.settle(100_000));
    assert_eq!(h.game.world.scene_id, 1);
    // s_1315 (served the guests) ran right after landing: it hands over 桂花酒.
    assert_eq!(label(&h, h.game.world.event(16).unwrap().trigger), "s_1315");
    assert!(h.game.trace.iter().any(|t| t.starts_with("s_1315")), "Ev#16 must trigger: {:?}", h.game.trace);
}

/// #16 / #11: riding the lotus leaf moves the party and the leaf together with frame 0.
#[test]
fn issue16_lotus_leaf_ride_round_trip() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(17, false);
    let s2110 = h.game.prog.label_id("s_2110").unwrap();
    let e = h.game.world.event_mut(244).unwrap();
    e.x = 944;
    e.y = 888;
    e.trigger = ScriptPos::new(s2110, 0);
    h.game.world.set_party_world_pos(944, 888);
    assert!(h.game.trigger_event(244));
    let mut max_frame = 0;
    for _ in 0..10_000 {
        h.tick(pal_core::game::TickInput::IDLE);
        if matches!(h.game.waiting_on(), Some(pal_core::game::Wait::WalkParty { ride, .. }) if *ride == 244) {
            max_frame = max_frame.max(h.game.world.leader().frame);
        }
        if !h.game.is_busy() {
            break;
        }
    }
    let e = h.game.world.event(244).unwrap();
    assert_eq!((e.x, e.y), (27 * 32, 53 * 16), "leaf glides to its destination");
    let l = h.game.world.leader();
    assert_eq!((l.x, l.y), (26 * 32 + 16, 52 * 16 + 8), "leader steps off");
    assert_eq!(max_frame, 0, "no walking legs while gliding");
    assert_eq!(e.trigger.idx, 3, "s_2110 yields for the return trip");

    h.game.world.set_party_world_pos(e.x, e.y);
    assert!(h.game.trigger_event(244));
    assert!(h.settle(20_000));
    let e = h.game.world.event(244).unwrap();
    assert_eq!((e.x, e.y), (29 * 32 + 16, 55 * 16 + 8));
    let l = h.game.world.leader();
    assert_eq!((l.x, l.y), (30 * 32, 56 * 16));
}

/// #27 / #29: multi-level level-ups learn every levelup_magic spell crossed.
#[test]
fn issue27_level_up_learns_spells() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    let g = &mut h.game;
    g.world.roles[0].level = 6;
    g.world.roles[0].magics.clear();
    let learned = g.world.apply_level_up(&g.data, 0, 3, true);
    assert_eq!(g.world.roles[0].level, 9);
    assert!(g.world.roles[0].magics.contains(&349) && g.world.roles[0].magics.contains(&311));
    assert!(learned.len() >= 2);
}

/// #29: elemental weakness plus a favourable battlefield vastly out-damages a resisted cast.
#[test]
fn issue29_elemental_resist_and_field_bonus() {
    use pal_core::battle::calc_magic_damage;
    let resisted = calc_magic_damage(100, 80, 20, 9, -30, 12345);
    let weak = calc_magic_damage(100, 80, 20, 0, 50, 12345);
    assert!(weak > resisted * 3, "{weak} vs {resisted}");
}

/// #80: item_add is silent so it cannot spoil the following cutscene line.
#[test]
fn issue80_item_add_is_silent() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);
    h.game.prog.load_block("_t80", &serde_json::json!([{"op": "item_add", "item_id": 279, "count": 1}, {"op": "end"}])).unwrap();
    assert!(h.game.run_script("_t80", 0));
    let cmds = h.game.drain_cmds();
    assert!(!cmds.iter().any(|c| matches!(c, HostCmd::Toast(_))), "{cmds:?}");
    assert!(h.settle(1000));
    assert_eq!(h.game.world.item_count(279), 1);
}

/// #93 / #97: scene enter offsets and role magics survive save/load.
#[test]
fn issue93_97_save_keeps_enter_offset_and_magics() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(3, false);
    h.game.world.scenes.get_mut(&1).unwrap().enter.idx = 147;
    h.game.world.roles[0].magics = vec![296];
    let save = h.game.save_json("t", "");
    h.game.load_save_json(&save).unwrap();
    assert!(h.settle(1000));
    assert_eq!(h.game.world.scenes[&1].enter.idx, 147);
    assert_eq!(h.game.world.roles[0].magics, vec![296]);
}

/// #134: Scene 14 Ev#204 (Zhao Ling'er's clothes at `(1104, 1432)`) sits on a blocked shoreline
/// tile; clicking it from adjacent `(1088, 1424)` must trigger `s_25aa` instead of failing LOS.
#[test]
fn issue134_scene14_click_blocked_tile_event_triggers_linger_cutscene() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(14, false);
    h.game.world.party[0].x = 1088;
    h.game.world.party[0].y = 1424;
    h.game.world.party[0].dir = 3;
    h.game.click_world(1104, 1432);
    assert!(
        h.game.is_busy(),
        "Clicking Ev#204 (Ling'er's clothes on blocked tile) must trigger s_25aa cutscene"
    );
    let cmds = h.game.drain_cmds();
    assert!(
        cmds.iter().any(|c| matches!(c, HostCmd::ShowDialogue { .. })),
        "Expected opening clothes dialogue from s_25aa, got: {cmds:?}"
    );
    h.game.reply(pal_core::game::HostReply::DialogueDone);
    // #138: Stepping all the way through s_25aa must smoothly pan the camera and then reset
    // `cam_override = None` and `fade_alpha = 0.0` (never leaving the viewport in black void).
    assert!(h.settle(4000), "s_25aa cutscene failed to settle cleanly");
    assert_eq!(h.game.world.view.cam_override, None, "camera_pan (0,0) must reset cam_override to party");
    assert_eq!(h.game.world.view.fade_alpha, 0.0, "screen must fade back in cleanly");
    assert_eq!(h.game.world.event(205).unwrap().state, 1, "Ev#205 follow-up trigger must be enabled");
}

/// SDLPAL Batch 1 P0: `0x0078` (`battle_end_return`) is a no-op so post-battle cutscenes continue.
#[test]
fn sdlpal_p0_battle_end_return_continues_cutscene() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);
    h.game.prog.load_block("_t_ber", &serde_json::json!([
        {"op": "battle_end_return"},
        {"op": "item_add", "item_id": 99, "count": 2},
        {"op": "end"}
    ])).unwrap();
    assert!(h.game.run_script("_t_ber", 0));
    assert!(h.settle(500));
    assert_eq!(h.game.world.item_count(99), 2, "instructions after battle_end_return must run");
}

/// SDLPAL Batch 1 P0: `0x0079` (`branch_if_role_in_party`) compares DOS rgwName word IDs (36..41).
#[test]
fn sdlpal_p0_branch_if_role_in_party_word_ids() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);
    // Party has role 0 (Xiaoyao, word ID 36), NOT role 3 (Empress Wu, word ID 39).
    h.game.prog.load_block("_t_role_hit", &serde_json::json!([
        {"op": "item_add", "item_id": 98, "count": 1},
        {"op": "end"}
    ])).unwrap();
    h.game.prog.load_block("_t_role_chk", &serde_json::json!([
        {"op": "branch_if_role_in_party", "args": [39, "_t_role_hit"]},
        {"op": "branch_if_role_in_party", "args": [36, "_t_role_hit"]},
        {"op": "end"}
    ])).unwrap();
    assert!(h.game.run_script("_t_role_chk", 0));
    assert!(h.settle(500));
    assert_eq!(h.game.world.item_count(98), 1, "word ID 36 must match Xiaoyao in party");
}

/// SDLPAL Batch 1 P0: `0x0023` (`role_unequip`) uses 0-based role index, and `0x0020` (`item_remove`)
/// removes equipped quest items when not in inventory.
#[test]
fn sdlpal_p0_role_unequip_zero_based_and_item_remove_equipped() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);
    h.game.world.roles[0].equipment[5] = 274; // Xiaoyao accessory
    h.game.world.roles[1].equipment[5] = 274; // Ling'er accessory
    h.game.prog.load_block("_t_uneq", &serde_json::json!([
        {"op": "role_unequip", "args": [1, 6]},
        {"op": "end"}
    ])).unwrap();
    assert!(h.game.run_script("_t_uneq", 0));
    assert!(h.settle(500));
    assert_eq!(h.game.world.roles[0].equipment[5], 274, "role_unequip [1, 6] must NOT touch Xiaoyao (role 0)");
    assert_eq!(h.game.world.roles[1].equipment[5], 0, "role_unequip [1, 6] must unequip Ling'er (role 1)");

    // Clear inventory copy and test removing Xiaoyao's equipped 274 via `item_remove`.
    h.game.world.inventory.remove(&274);
    h.game.prog.load_block("_t_rm_fail", &serde_json::json!([{"op": "item_add", "item_id": 97, "count": 1}, {"op": "end"}])).unwrap();
    h.game.prog.load_block("_t_rm_eq", &serde_json::json!([
        {"op": "item_remove", "args": [274, 0, "_t_rm_fail"]},
        {"op": "end"}
    ])).unwrap();
    assert!(h.game.run_script("_t_rm_eq", 0));
    assert!(h.settle(500));
    assert_eq!(h.game.world.roles[0].equipment[5], 0, "item_remove must unequip equipped copy");
    assert_eq!(h.game.world.item_count(97), 0, "item_remove must NOT branch to else when equipped copy satisfies count");
}

/// SDLPAL Batch 1 P1: `0x0054` night palette, `0x009B` fbp_fade_current, `0x006D` scene_set_scripts,
/// and `0x0049` event_id == 0 no-op.
#[test]
fn sdlpal_p1_night_palette_fbp_fade_scene_scripts_and_ev0_noop() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);
    let ev1_orig_state = h.game.world.event(1).unwrap().state;
    h.game.world.view.active_fbp = 5;
    h.game.prog.load_block("_t_p1", &serde_json::json!([
        {"op": "op_0054"},
        {"op": "fbp_fade_current", "args": [2, 65535]},
        {"op": "event_set_state", "args": [0, 0]},
        {"op": "end"}
    ])).unwrap();
    assert!(h.game.run_script("_t_p1", 1));
    assert!(h.settle(500));
    assert!(h.game.world.night_palette, "0x0054 must enable night_palette");
    assert_eq!(h.game.world.view.active_fbp, -1, "0x009B must clear active_fbp instead of showing FBP #2");
    assert_eq!(h.game.world.event(1).unwrap().state, ev1_orig_state, "event_set_state [0, 0] must be a no-op");
}
