//! Play the real game scripts natively through the Rust core (no browser, no JS).

mod common;

use common::NativeHost;

#[test]
fn new_game_intro_runs_to_player_control_in_scene_1() {
    let mut h = NativeHost::new();
    h.game.start_new_game(0x95_1995);
    assert!(h.settle(20_000), "intro never finished; waiting on {:?}", h.game.waiting_on());
    println!("dialogues: {}", h.dialogues.len());
    for d in h.dialogues.iter().take(8) {
        println!("  {d}");
    }
    println!("trace: {:?}", h.game.trace);
    let w = &h.game.world;
    println!("scene {} leader ({}, {}) cash {} ticks {}", w.scene_id, w.leader().x, w.leader().y, w.cash, h.game.tick_count);
    assert_eq!(w.scene_id, 1);
    assert!(h.dialogues.len() > 5);
}

#[test]
fn save_roundtrip_preserves_canonical_state_and_rng() {
    let mut h = NativeHost::new();
    h.game.start_new_game(12345);
    assert!(h.settle(20_000));
    h.game.world.add_item(61, 3);
    h.game.world.rng.next_u32();
    let before = h.game.canonical_state();
    let rng = h.game.world.rng;
    let save = h.game.save_json("t", "2026-11-20T00:00:00Z");
    let text = serde_json::to_string(&save).unwrap();

    let mut h2 = NativeHost::new();
    h2.game.load_save_json(&serde_json::from_str(&text).unwrap()).unwrap();
    assert!(h2.settle(1000));
    assert_eq!(h2.game.canonical_state(), before);
    assert_eq!(h2.game.world.rng, rng);
    assert_eq!(h2.game.save_json("t", "2026-11-20T00:00:00Z"), save);
}

#[test]
fn all_51_dos_saves_load_and_tick() {
    let mut h = NativeHost::new();
    for i in 0..=50 {
        let buf = std::fs::read(common::assets_dir().join(format!("orig_saves/{i}.rpg"))).unwrap();
        h.game.load_dos_rpg(&buf).unwrap();
        assert!(h.settle(100), "{i}.rpg: scene load");
        for _ in 0..30 {
            h.tick(pal_core::game::TickInput::IDLE);
        }
        assert!(!h.game.world.party.is_empty(), "{i}.rpg");
    }
    // Review B1: EXP lives at offset 124 (ALLEXPERIENCE), not in the poison table at 1408.
    let buf = std::fs::read(common::assets_dir().join("orig_saves/27.rpg")).unwrap();
    h.game.load_dos_rpg(&buf).unwrap();
    assert_eq!(h.game.world.roles[0].exp, 21198);
    assert_eq!(h.game.world.roles[0].level, 85);
}

#[test]
fn quick_battle_runs_to_victory_with_rewards() {
    let mut h = NativeHost::new();
    h.game.start_new_game(777);
    assert!(h.settle(20_000));
    {
        let r = &mut h.game.world.roles[0];
        r.max_hp = 9999;
        r.hp = 9999;
        r.stats[0] = 400;
    }
    let exp0 = h.game.world.roles[0].exp;
    let cash0 = h.game.world.cash;
    assert!(h.game.start_test_battle(5));
    assert!(h.game.battle.is_some());
    assert!(h.settle(50_000), "battle never ended: {:?}", h.game.waiting_on());
    assert!(h.game.battle.is_none());
    assert_eq!(h.victories.len(), 1, "toasts: {:?}", h.toasts);
    let (exp, cash, _) = &h.victories[0];
    assert!(*exp > 0 && *cash > 0);
    // Review B2: the per-level requirement is consumed on every level-up.
    let (mut e, mut lv) = (exp0 + exp, 1);
    while e >= h.game.data.levelup_exp[lv as usize] {
        e -= h.game.data.levelup_exp[lv as usize];
        lv += 1;
    }
    assert_eq!((h.game.world.roles[0].exp, h.game.world.roles[0].level), (e, lv));
    assert_eq!(h.game.world.cash, cash0 + *cash as u32);
}

#[test]
fn battles_are_deterministic_for_a_seed() {
    let run = |seed| {
        let mut h = NativeHost::new();
        h.game.start_new_game(seed);
        h.settle(20_000);
        h.game.start_test_battle(12);
        h.settle(50_000);
        (h.game.canonical_state(), h.game.tick_count)
    };
    assert_eq!(run(99), run(99));
}

#[test]
fn menu_equip_and_field_magic() {
    let mut h = NativeHost::new();
    h.game.start_new_game(1);
    assert!(h.settle(20_000));
    let g = &mut h.game;
    // Equip a different weapon from the inventory: old one returns to the bag, stats follow.
    let old = g.world.roles[0].equipment[3];
    let atk0 = g.world.roles[0].stats[0];
    g.world.add_item(168, 1);
    let delta = g.item_stat_bonuses(168)[0] - g.item_stat_bonuses(old)[0];
    assert!(g.equip_from_menu(0, 168));
    assert_eq!(g.world.roles[0].equipment[3], 168);
    assert_eq!(g.world.item_count(old), 1);
    assert_eq!(g.world.roles[0].stats[0], (atk0 + delta).max(1));
    assert!(!g.equip_from_menu(0, 168), "no second copy in the bag");
    g.world.add_item(165, 1);
    assert!(!g.equip_from_menu(0, 165), "金蛇鞭 is Lin Yueru only");
    assert!(g.equip_from_menu(2, 165));

    // 气疗术 (#296) heals the injured caster and costs MP.
    let r = &mut g.world.roles[0];
    r.hp = 10;
    let mp0 = r.mp;
    assert!(g.cast_field_magic(0, 296, None));
    assert!(g.world.roles[0].hp > 10);
    assert!(g.world.roles[0].mp < mp0);
}

#[test]
fn losing_a_battle_without_defeat_branch_is_game_over() {
    let mut h = NativeHost::new();
    h.choice_answer = false; // Decline pre-battle retry -> GameOver
    h.game.start_new_game(5);
    assert!(h.settle(20_000));
    // A one-instruction script battle with no on_defeat label, against a team that wins.
    h.game.prog.load_block("s_ffe0", &serde_json::json!([{"op": "battle_start", "team_id": 5}, {"op": "end"}])).unwrap();
    let ev = h.game.world.current_event_ids()[0];
    h.game.world.event_mut(ev).unwrap().trigger = pal_core::script::ScriptPos::new(h.game.prog.label_id("s_ffe0").unwrap(), 0);
    h.game.world.roles[0].hp = 1;
    assert!(h.game.trigger_event(ev));
    assert!(h.settle(50_000));
    assert!(h.game_over, "expected HostCmd::GameOver when declining retry");
}

#[test]
fn losing_a_battle_and_retrying_reloads_pre_battle_autosave() {
    let mut h = NativeHost::new();
    h.choice_answer = true; // Accept pre-battle retry -> reload hidden pre-battle autosave
    h.game.start_new_game(5);
    assert!(h.settle(20_000));
    let scene_before = h.game.world.scene_id;
    h.game.prog.load_block("s_ffe1", &serde_json::json!([{"op": "battle_start", "team_id": 5}, {"op": "end"}])).unwrap();
    let ev = h.game.world.current_event_ids()[0];
    h.game.world.event_mut(ev).unwrap().trigger = pal_core::script::ScriptPos::new(h.game.prog.label_id("s_ffe1").unwrap(), 0);
    h.game.world.roles[0].hp = 1;
    assert!(h.game.trigger_event(ev));
    assert!(h.settle(50_000));
    assert!(!h.game_over, "retrying should not trigger GameOver");
    assert_eq!(h.game.world.scene_id, scene_before);
    assert_eq!(h.game.world.roles[0].hp, 1, "should restore exact pre-battle HP snapshot");
}

#[test]
fn battle_healing_spell_targets_selected_ally() {
    let mut h = NativeHost::new();
    h.game.start_new_game(7);
    assert!(h.settle(20_000));
    // Add Zhao Ling'er (role 1) to party, injure her while Li Xiaoyao (role 0) is full HP.
    let lead = h.game.world.party[0].clone();
    h.game.world.party.push(pal_core::game::world::PartyMember {
        role_id: 1,
        x: lead.x,
        y: lead.y,
        dir: lead.dir,
        frame: 0,
    });
    h.game.world.roles[0].hp = h.game.world.roles[0].max_hp;
    h.game.world.roles[1].max_hp = 240;
    h.game.world.roles[1].hp = 40;
    h.game.world.roles[1].mp = 100;
    if !h.game.world.roles[1].magics.contains(&297) {
        h.game.world.roles[1].magics.push(297);
    }
    assert!(h.game.start_test_battle(1));
    // Step until hero 1 (Zhao Ling'er) is prompted, then cast 观音咒 (#297) targeting hero slot 1.
    use pal_core::game::{BattleCmd, HostCmd, HostReply, TickInput};
    let mut cast = false;
    for _ in 0..2000 {
        for cmd in h.game.drain_cmds() {
            match cmd {
                HostCmd::BattlePrompt { hero } => {
                    if hero == 1 && !cast {
                        h.game.battle_command(BattleCmd::Magic { magic: 297, target: 1 });
                        cast = true;
                    } else {
                        h.game.battle_command(BattleCmd::Defend);
                    }
                }
                HostCmd::BattleVictory { .. } => h.game.reply(HostReply::ModalDone),
                HostCmd::ShowDialogue { .. } => h.game.reply(HostReply::DialogueDone),
                _ => {}
            }
        }
        if cast && h.game.world.roles[1].hp > 40 {
            break;
        }
        h.game.tick(&TickInput::IDLE);
    }
    assert!(cast, "Zhao Ling'er should have been prompted to cast 观音咒");
    assert!(
        h.game.world.roles[1].hp > 40,
        "观音咒 (#297) must heal targeted ally (role 1), got hp={}",
        h.game.world.roles[1].hp
    );
}

#[test]
fn scripted_boss_defeat_with_load_last_save_offers_pre_event_retry() {
    let mut h = NativeHost::new();
    h.choice_answer = true;
    h.game.start_new_game(5);
    assert!(h.settle(20_000));
    h.game
        .prog
        .load_block("s_ffd0", &serde_json::json!([{"op": "load_last_save"}, {"op": "end"}]))
        .unwrap();
    h.game
        .prog
        .load_block(
            "s_ffd1",
            &serde_json::json!([
                {"op": "battle_start", "team_id": 5, "on_defeat": "s_ffd0"},
                {"op": "end"}
            ]),
        )
        .unwrap();
    let ev = h.game.world.current_event_ids()[0];
    h.game.world.event_mut(ev).unwrap().trigger =
        pal_core::script::ScriptPos::new(h.game.prog.label_id("s_ffd1").unwrap(), 0);
    h.game.world.roles[0].hp = 1;
    assert!(h.game.trigger_event(ev));
    assert!(h.settle(50_000));
    assert!(!h.game_over, "retrying from load_last_save should reload pre-event autosave");
    assert_eq!(h.game.world.roles[0].hp, 1, "should restore exact pre-event HP instead of 25% revive");
}

#[test]
fn scripted_loss_battle_followed_by_cutscene_continues_story() {
    // Regression test for BUG #2: Scene #67 Stone Elder (`s_42a6`, team 34) has no `on_defeat`
    // and is followed immediately by `music_play` + `battle_end_return` + cutscene.
    let mut h = NativeHost::new();
    h.choice_answer = false; // If wrongly treated as GameOver, this would fail!
    h.game.start_new_game(5);
    assert!(h.settle(20_000));
    h.game
        .prog
        .load_block(
            "s_ffc0",
            &serde_json::json!([
                {"op": "battle_start", "team_id": 5},
                {"op": "battle_end_return"},
                {"op": "cash_add", "args": [777]},
                {"op": "end"}
            ]),
        )
        .unwrap();
    let cash_before = h.game.world.cash;
    let ev = h.game.world.current_event_ids()[0];
    h.game.world.event_mut(ev).unwrap().trigger =
        pal_core::script::ScriptPos::new(h.game.prog.label_id("s_ffc0").unwrap(), 0);
    h.game.world.roles[0].hp = 1;
    assert!(h.game.trigger_event(ev));
    assert!(h.settle(50_000));
    assert!(!h.game_over, "scripted-loss battle with post-battle cutscene must NOT GameOver");
    assert_eq!(
        h.game.world.cash,
        cash_before + 777,
        "post-battle cutscene after scripted loss must execute"
    );
    assert!(h.game.world.roles[0].hp > 0, "fallen heroes revive after scripted loss");
}
