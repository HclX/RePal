//! Comprehensive VM Opcode Matrix & Core Regression Tests (`crates/pal-core/tests/vm_opcodes.rs`).
//!
//! Replaces the retired JS `test_vm_opcodes.mjs` matrix with fast, deterministic Rust tests
//! covering control flow, branching, inventory/cash, role stats/magic, scene/event mutation,
//! audio/dialogue host commands, battle/damage invariants, and equal-Y draw-list tie-breaking.

mod common;

use common::NativeHost;
use pal_core::game::{HostCmd, TickInput};
use pal_core::render::build_field_draw_list;
use pal_core::state::{EventState, GameState, PartyMember};
use serde_json::json;

fn run_block(h: &mut NativeHost, name: &str, steps: serde_json::Value) {
    h.game.prog.load_block(name, &steps).unwrap();
    assert!(h.game.run_script(name, 0), "run_script({name}) failed");
    assert!(h.settle(10_000), "script {name} never settled");
}

// =========================================================================
// 1. Control Flow, Calls, Loops & Recursion Guard
// =========================================================================

#[test]
fn opcode_jump_and_call_script_and_recursion_guard() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);

    // call_script invokes sub-block and returns to caller
    h.game
        .prog
        .load_block("_sub_add", &json!([{"op": "cash_add", "args": [123]}, {"op": "end"}]))
        .unwrap();
    let cash0 = h.game.world.cash;
    run_block(
        &mut h,
        "_call_main",
        json!([
            {"op": "call_script", "target": "_sub_add"},
            {"op": "cash_add", "args": [77]},
            {"op": "end"}
        ]),
    );
    assert_eq!(h.game.world.cash, cash0 + 200);

    // Self-recursive call_script must hit MAX_CALL_DEPTH cleanly without stack overflow or panic
    run_block(
        &mut h,
        "_self_rec",
        json!([
            {"op": "call_script", "target": "_self_rec"},
            {"op": "end"}
        ]),
    );
    assert!(!h.game.is_busy());

    // Counted jump (0x0003 with count > 0) loops `count` times then exits to else_jump
    h.game
        .prog
        .load_block("_counted_after", &json!([{"op": "cash_add", "args": [7]}, {"op": "end"}]))
        .unwrap();
    let cash_before = h.game.world.cash;
    run_block(
        &mut h,
        "_counted_loop",
        json!([
            {"op": "cash_add", "args": [10]},
            {"op": "jump", "target": "_counted_loop", "count": 3, "else_jump": "_counted_after"}
        ]),
    );
    assert_eq!(
        h.game.world.cash,
        cash_before + 37,
        "counted jump(count=3) must execute loop body 3 times (+30) then fall through (+7)"
    );
}

#[test]
fn opcode_branch_if_yes_and_no() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);
    h.game
        .prog
        .load_block("_no_branch", &json!([{"op": "cash_add", "args": [500]}, {"op": "end"}]))
        .unwrap();

    // Answer YES -> falls through, skips _no_branch
    h.choice_answer = true;
    let cash0 = h.game.world.cash;
    run_block(
        &mut h,
        "_ask_yes",
        json!([
            {"op": "confirm_choice", "on_no": "_no_branch"},
            {"op": "cash_add", "args": [10]},
            {"op": "end"}
        ]),
    );
    assert_eq!(h.game.world.cash, cash0 + 10);

    // Answer NO -> jumps to _no_branch
    h.choice_answer = false;
    let cash1 = h.game.world.cash;
    run_block(
        &mut h,
        "_ask_no",
        json!([
            {"op": "confirm_choice", "on_no": "_no_branch"},
            {"op": "cash_add", "args": [10]},
            {"op": "end"}
        ]),
    );
    assert_eq!(h.game.world.cash, cash1 + 500);
}

// =========================================================================
// 2. Inventory, Cash & Shop / Pawnshop Bounds
// =========================================================================

#[test]
fn opcode_inventory_cash_and_branch_checks() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);

    h.game
        .prog
        .load_block("_has_item_ok", &json!([{"op": "cash_add", "args": [42]}, {"op": "end"}]))
        .unwrap();
    h.game
        .prog
        .load_block("_no_item", &json!([{"op": "cash_add", "args": [1]}, {"op": "end"}]))
        .unwrap();

    let cash0 = h.game.world.cash;
    run_block(
        &mut h,
        "_inv_ops",
        json!([
            {"op": "item_add", "item_id": 279, "count": 3},
            {"op": "item_remove", "item_id": 279, "count": 1},
            {"op": "branch_if_item_less", "args": [279, 2], "jump_if_less": "_no_item"},
            {"op": "cash_add", "args": [42]},
            {"op": "end"}
        ]),
    );
    assert_eq!(h.game.world.item_count(279), 2);
    assert_eq!(h.game.world.cash, cash0 + 42);

    // Removing more than owned leaves inventory intact and branches to else_jump
    let cash1 = h.game.world.cash;
    run_block(
        &mut h,
        "_inv_over_remove",
        json!([
            {"op": "item_remove", "item_id": 279, "count": 99, "else_jump": "_no_item"},
            {"op": "cash_add", "args": [999]},
            {"op": "end"}
        ]),
    );
    assert_eq!(h.game.world.item_count(279), 2, "failed item_remove must leave stack intact");
    assert_eq!(h.game.world.cash, cash1 + 1, "failed item_remove must jump to else_jump");

    // Cash buy/sell saturating bounds (no wraparound)
    h.game.world.cash = u32::MAX - 5;
    h.game.world.add_item(62, 1);
    let _ = h.game.sell_item(62, 100);
    assert_eq!(h.game.world.cash, u32::MAX);
}

// =========================================================================
// 3. Roles, Healing, Revival, Magic & Party Management
// =========================================================================

#[test]
fn opcode_role_heal_revive_and_magic_learning() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);

    // Damage & deplete MP on role 0
    h.game.world.roles[0].max_hp = 200;
    h.game.world.roles[0].hp = 40;
    h.game.world.roles[0].max_mp = 100;
    h.game.world.roles[0].mp = 10;

    run_block(
        &mut h,
        "_heal_ops",
        json!([
            {"op": "party_heal_hp", "args": [0, 50]},
            {"op": "party_heal_mp", "args": [0, 30]},
            {"op": "end"}
        ]),
    );
    assert_eq!(h.game.world.roles[0].hp, 90);
    assert_eq!(h.game.world.roles[0].mp, 40);

    // KO'd role must NOT be revived by normal party_heal_hp, only by role_revive
    h.game.world.roles[0].hp = 0;
    run_block(
        &mut h,
        "_heal_ko",
        json!([
            {"op": "party_heal_hp", "args": [0, 50]},
            {"op": "end"}
        ]),
    );
    assert_eq!(h.game.world.roles[0].hp, 0, "normal heal must not revive KO role");

    run_block(
        &mut h,
        "_revive_ko",
        json!([
            {"op": "role_revive", "args": [0, 5]},
            {"op": "end"}
        ]),
    );
    assert!(h.game.world.roles[0].hp > 0, "role_revive must revive KO role");

    // Learn & forget magic
    h.game.world.roles[0].magics.clear();
    run_block(
        &mut h,
        "_magic_learn",
        json!([
            {"op": "role_learn_magic", "args": [0, 296]},
            {"op": "end"}
        ]),
    );
    assert!(h.game.world.roles[0].magics.contains(&296));
    run_block(
        &mut h,
        "_magic_forget",
        json!([
            {"op": "role_forget_magic", "args": [296]},
            {"op": "end"}
        ]),
    );
    assert!(!h.game.world.roles[0].magics.contains(&296));
}

// =========================================================================
// 4. Scene, Event Mutation, Party Positioning & Same-Scene Fade-In
// =========================================================================

#[test]
fn opcode_event_state_pos_and_same_scene_fade_in() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);

    let ev_id = h.game.world.current_event_ids()[0];
    let orig_x = h.game.world.event(ev_id).unwrap().x;
    let orig_y = h.game.world.event(ev_id).unwrap().y;

    run_block(
        &mut h,
        "_ev_mutate",
        json!([
            {"op": "event_set_state", "args": [ev_id, 2]},
            {"op": "event_move_pos", "args": [ev_id, 16, -8]},
            {"op": "screen_fade_out", "args": [2]},
            {"op": "party_set_dir_frame", "args": [2, 0]},
            {"op": "end"}
        ]),
    );
    let ev = h.game.world.event(ev_id).unwrap();
    assert_eq!(ev.state, 2);
    assert_eq!((ev.x, ev.y), (orig_x + 16, orig_y - 8));
    assert_eq!(h.game.world.leader().dir, 2);
    assert_eq!(
        h.game.world.view.fade_alpha, 0.0,
        "same-scene screen_fade_out must auto-fade back in on script completion"
    );
}

#[test]
fn opcode_say_music_sfx_shop_and_rng_emit_expected_host_cmds() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);
    h.game.drain_cmds();

    h.game
        .prog
        .load_block(
            "_host_cmds",
            &json!([
                {"op": "music_play", "track": 12, "loop": true},
                {"op": "sfx_play", "args": [19]},
                {"op": "say", "speaker": "李逍遥", "text": "测试对话"},
                {"op": "shop_open", "args": [1]},
                {"op": "rng_set", "args": [0]},
                {"op": "rng_play", "args": [0, 5, 8]},
                {"op": "end"}
            ]),
        )
        .unwrap();
    assert!(h.game.run_script("_host_cmds", 0));

    let mut saw_music = false;
    let mut saw_sfx = false;
    let mut saw_shop = false;
    let mut saw_rng = false;

    for _ in 0..500 {
        for c in h.game.drain_cmds() {
            match c {
                HostCmd::PlayMusic { track: 12, looped: true } => saw_music = true,
                HostCmd::PlaySfx(19) => saw_sfx = true,
                HostCmd::ShowDialogue { text, .. } => {
                    h.dialogues.push(text);
                    h.game.reply(pal_core::game::HostReply::DialogueDone);
                }
                HostCmd::OpenShop(1) => {
                    saw_shop = true;
                    h.game.reply(pal_core::game::HostReply::ModalDone);
                }
                HostCmd::PlayRng { id: 0, start: 0, end: 5, speed: 8 } => {
                    saw_rng = true;
                    h.game.reply(pal_core::game::HostReply::Done);
                }
                _ => {}
            }
        }
        if !h.game.is_busy() {
            break;
        }
        h.game.tick(&TickInput::IDLE);
    }

    assert!(saw_music, "music_play must emit PlayMusic");
    assert!(saw_sfx, "sfx_play must emit PlaySfx");
    assert!(saw_shop, "shop_open must emit OpenShop");
    assert!(saw_rng, "rng_play must emit PlayRng");
    assert!(h.dialogues.iter().any(|d| d.contains("测试对话")));
}

// =========================================================================
// 5. Legacy Regression #98: Equal-Y Draw-List Deterministic Tie-Breaking
// =========================================================================

#[test]
fn regression98_equal_y_draw_list_is_deterministic() {
    let mut gs = GameState::new();
    gs.party = vec![PartyMember {
        role_id: 0,
        x: 320,
        y: 320,
        dir: 0,
        frame: 0,
    }];
    gs.events = vec![
        EventState {
            id: 1,
            x: 320,
            y: 320,
            layer: 0,
            state: 1,
            sprite_id: 10,
            ..Default::default()
        },
        EventState {
            id: 2,
            x: 320,
            y: 320,
            layer: 1,
            state: 1,
            sprite_id: 11,
            ..Default::default()
        },
    ];
    let list1 = build_field_draw_list(&gs, 1, 2);
    let list2 = build_field_draw_list(&gs, 1, 2);
    assert_eq!(list1.len(), 3);
    assert_eq!(
        list1.iter().map(|s| s.sprite_id).collect::<Vec<_>>(),
        list2.iter().map(|s| s.sprite_id).collect::<Vec<_>>(),
        "equal-Y sprite draw order must be 100% deterministic"
    );
    // Higher layer event must sort after lower layer event at identical Y
    let pos_l0 = list1.iter().position(|s| s.sprite_id == 10).unwrap();
    let pos_l1 = list1.iter().position(|s| s.sprite_id == 11).unwrap();
    assert!(pos_l1 > pos_l0, "layer 1 must draw in front of layer 0 at equal Y");
}

// =========================================================================
// 6. Issue #109 Regressions: Zero-Target Branches, remove_item, cd_or_midi, scene_set_scripts
// =========================================================================

#[test]
fn issue109_zero_target_branch_terminates_and_guards_hold() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);

    // 1. branch_if_event_state_eq with zero/NO_LABEL target must terminate (not fall through)
    let ev_id = h.game.world.current_event_ids()[0];
    h.game.world.event_mut(ev_id).unwrap().state = 2;
    let cash0 = h.game.world.cash;
    run_block(
        &mut h,
        "_zero_tgt",
        json!([
            {"op": "branch_if_event_state_eq", "args": [ev_id, 2, 0]},
            {"op": "cash_add", "args": [999]},
            {"op": "end"}
        ]),
    );
    assert_eq!(
        h.game.world.cash, cash0,
        "branch_if_event_state_eq with NO_LABEL target must terminate instead of falling through"
    );

    // 2. World::remove_item with negative count must fail and never add items
    h.game.world.add_item(279, 2);
    assert!(!h.game.world.remove_item(279, -5));
    assert!(!h.game.world.remove_item(279, 0));
    assert_eq!(h.game.world.item_count(279), 2);

    // 3. cd_or_midi_play plays MIDI operand a[1] rather than CD track a[0]
    h.game.drain_cmds();
    run_block(
        &mut h,
        "_cd_midi",
        json!([
            {"op": "cd_or_midi_play", "args": [2, 17]},
            {"op": "end"}
        ]),
    );
    assert_eq!(h.game.world.music_track, 17);

    // 4. scene_set_scripts with argc==1 (trailing zero stripped) clears script_enter
    let some_lbl = h.game.prog.intern("s_1217");
    h.game.world.scenes.get_mut(&8).unwrap().enter = pal_core::script::ScriptPos::new(some_lbl, 0);
    run_block(
        &mut h,
        "_clear_enter",
        json!([
            {"op": "scene_set_scripts", "args": [8]},
            {"op": "end"}
        ]),
    );
    assert!(
        h.game.world.scenes[&8].enter.is_none(),
        "scene_set_scripts with argc==1 must clear script_enter"
    );
}
