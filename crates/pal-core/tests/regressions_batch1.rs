//! Regression tests for Batch 1 Core Battle & Script VM fixes:
//! - #212: Enemy `script_pre_turn` / `script_in_turn` `ScriptPos` persistence across rounds
//!   (Lin Tiannan 7-turn duel `s_a3c3`, Mingwang multi-turn AI).
//! - #213: Dual-script magics (`script_use` + `script_success`), specifically `酒神 #370`
//!   (wine check, MP drain -> massive damage, 9-use forget counter).
//! - #214: Equipment `script_equip` effects (`role_set_attr_equip` elemental/poison resist &
//!   `role_set_attr(65, ...)` coop override — 雷灵珠 + 五毒珠).
//! - #215: `battle_hide_role` (`隐蛊 #141`) hides party from enemy attacks for `a[0]` rounds.
//! - #185: `object_set_script` runtime override respected by `battle_enemy_template_with_obj`
//!   and live battle enemies (`胖苗` obj_id 485).
//! - #188: `BattleCmd::Coop` deducts MP and commits the remaining party turns for the round.
//! - #216: Explicit mismatched speaker clears stale portrait so unportraited NPC continuation
//!   lines do not misattribute to the previous portrait's speaker.
//! - #191: Fleeing battle revives KO'd party members to 1 HP.
//! - #150: Scene #1 secret passage `(640, 608)` refuses stepping `NW` into the `(624, 600)` wall-top void.
//! - #179: Scene #3 `s_1cb2` night-crying cutscene hides `ev#57` and `ev#69` before transitioning to Scene #1.

mod common;

use common::NativeHost;
use pal_core::game::{BattleCmd, HostCmd, HostReply, TickInput};
use pal_core::script::ScriptPos;
use serde_json::json;

/// Advance a battle until `BattlePrompt`, `BattleEnded`, or tick budget exhausted, auto-answering
/// any in-battle dialogues/modals while recording spoken lines.
fn step_battle_until_prompt_or_end(h: &mut NativeHost, max_ticks: usize) -> (bool, Vec<(String, String)>) {
    let mut lines = Vec::new();
    for _ in 0..max_ticks {
        h.game.tick(&TickInput::IDLE);
        let cmds = h.game.drain_cmds();
        let mut prompted = false;
        let mut ended = false;
        for c in cmds {
            match c {
                HostCmd::LoadChapter(_) => {
                    h.game.reply(HostReply::Done);
                }
                HostCmd::LoadScene { .. } => {
                    h.game.reply(HostReply::SceneLoaded);
                }
                HostCmd::ShowDialogue { speaker, text, .. } => {
                    lines.push((speaker, text));
                    h.game.reply(HostReply::DialogueDone);
                }
                HostCmd::AskYesNo => h.game.reply(HostReply::Choice(true)),
                HostCmd::BattleVictory { .. } => h.game.reply(HostReply::ModalDone),
                HostCmd::BattlePrompt { .. } => prompted = true,
                HostCmd::BattleEnded => ended = true,
                _ => {}
            }
        }
        if ended {
            return (false, lines);
        }
        if prompted {
            return (true, lines);
        }
    }
    (false, lines)
}

/// #212: Multi-turn `script_pre_turn` with `pause_yield` persists across rounds (e.g. Lin Tiannan
/// 7-turn duel), rather than being wiped on round 1.
#[test]
fn issue212_enemy_pre_turn_persists_across_rounds() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);
    h.game.world.roles[0].hp = 9999;
    h.game.world.roles[0].max_hp = 9999;

    // Register a synthetic 3-turn duel script: 2 rounds of pause_yield, then enemy escapes on round 3.
    h.game
        .prog
        .load_block(
            "_t212_pre",
            &json!([
                {"op": "dialogue_pos_center"},
                {"op": "say", "speaker": "林天南", "text": "第一回合"},
                {"op": "pause_yield"},
                {"op": "pause_yield"},
                {"op": "dialogue_pos_center"},
                {"op": "say", "speaker": "林天南", "text": "第三回合结束"},
                {"op": "battle_enemies_escape"},
                {"op": "end"}
            ]),
        )
        .unwrap();
    let lbl = h.game.prog.label_id("_t212_pre").unwrap();

    assert!(h.game.start_test_battle(1));
    {
        let b = h.game.battle.as_mut().unwrap();
        b.enemies[0].hp = 9999;
        b.enemies[0].max_hp = 9999;
        b.enemies[0].script_pre_turn = ScriptPos::new(lbl, 0);
        b.enemies[0].script_in_turn = ScriptPos::NONE;
    }

    // Round 0 is already at Command(0) after start_test_battle; defend once to enter our custom
    // pre-turn script on the next round, then verify rounds 1 -> 2 -> 3 advance across pause_yield.
    let (prompted0, _) = step_battle_until_prompt_or_end(&mut h, 2000);
    assert!(prompted0);
    assert!(h.game.battle_command(BattleCmd::Defend));

    let mut all_lines = Vec::new();
    for round in 0..4 {
        let (prompted, lines) = step_battle_until_prompt_or_end(&mut h, 2000);
        all_lines.extend(lines);
        if !prompted {
            assert_eq!(round, 2, "battle must end on round 3 pre-turn escape, ended on round {round}");
            break;
        }
        assert!(h.game.battle_command(BattleCmd::Defend));
    }
    assert!(h.game.battle.is_none(), "battle should have ended via battle_enemies_escape");
    assert!(
        all_lines.iter().any(|(_, t)| t.contains("第一回合")),
        "round 1 dialogue missing: {all_lines:?}"
    );
    assert!(
        all_lines.iter().any(|(_, t)| t.contains("第三回合结束")),
        "round 3 dialogue missing (script_pre_turn was wiped too early): {all_lines:?}"
    );
}

/// #213: `酒神 #370` runs `script_use` (`s_a46b`: consumes 1 酒, drains caster MP -> huge AoE dmg)
/// AND persists `script_success` (`s_a49e`: 9-use counter -> `role_forget_magic(370)`).
#[test]
fn issue213_jiushen_drains_mp_consumes_wine_and_forgets_after_9_uses() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);

    // Give Xiaoyao 酒神 (#370), plenty of HP/MP, and NO wine first.
    h.game.world.roles[0].hp = 9999;
    h.game.world.roles[0].max_hp = 9999;
    h.game.world.roles[0].mp = 200;
    h.game.world.roles[0].max_mp = 200;
    h.game.world.roles[0].magics.push(370);
    h.game.world.inventory.remove(&86);

    assert!(h.game.start_test_battle(1));
    h.game.battle.as_mut().unwrap().enemies[0].hp = 20_000;
    h.game.battle.as_mut().unwrap().enemies[0].max_hp = 20_000;

    // 1) Cast without wine -> must fail with "酒不足" and keep MP intact + enemy unharmed.
    let (prompted, _) = step_battle_until_prompt_or_end(&mut h, 2000);
    assert!(prompted);
    assert!(h.game.battle_command(BattleCmd::Magic { magic: 370, target: 0 }));
    let (prompted, lines) = step_battle_until_prompt_or_end(&mut h, 2000);
    assert!(prompted);
    assert!(
        lines.iter().any(|(_, t)| t.contains("酒不足")),
        "Expected '酒不足' dialogue when casting 酒神 without wine, got {lines:?}"
    );
    assert_eq!(h.game.world.roles[0].mp, 200, "MP must not be drained when wine check fails");
    assert_eq!(h.game.battle.as_ref().unwrap().enemies[0].hp, 20_000);

    // 2) Give 9 bottles of wine (#86) and cast 酒神 9 times.
    h.game.world.add_item(86, 9);
    let mut saw_exhausted = false;
    for use_idx in 1..=9 {
        h.game.world.roles[0].mp = 100;
        let hp_before = h.game.battle.as_ref().unwrap().enemies[0].hp;
        assert!(h.game.battle_command(BattleCmd::Magic { magic: 370, target: 0 }));
        let (prompted, lines) = step_battle_until_prompt_or_end(&mut h, 2000);
        assert!(prompted, "battle should stay alive on use {use_idx}");
        let hp_after = h.game.battle.as_ref().unwrap().enemies[0].hp;
        let dealt = hp_before - hp_after;
        assert!(dealt >= 500, "酒神 use {use_idx} with 100 MP should deal huge damage (>=500), got {dealt}");
        assert_eq!(h.game.world.roles[0].mp, 0, "酒神 must drain caster MP to 0");
        assert_eq!(h.game.world.item_count(86), 9 - use_idx, "酒神 must consume 1 酒 per cast");
        if lines.iter().any(|(_, t)| t.contains("酒神咒") && t.contains("用尽")) {
            saw_exhausted = true;
        }
        if use_idx < 9 {
            assert!(h.game.world.roles[0].magics.contains(&370), "酒神 must still be known after use {use_idx}");
        } else {
            assert!(!h.game.world.roles[0].magics.contains(&370), "酒神 must be forgotten after 9th use");
        }
    }
    assert!(saw_exhausted, "Expected 9th use exhaustion message for 酒神咒");
}

/// #214: Equipping 雷灵珠 (#264) updates coop_magic to 狂雷 (#320) & grants 50% thunder resist;
/// equipping 五毒珠 (#262) grants 100% poison immunity.
#[test]
fn issue214_equip_scripts_apply_resistances_and_coop_magic() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);

    let orig_coop = h.game.world.roles[0].coop_magic;
    assert_eq!(h.game.world.roles[0].elem_resist(1), 0);
    assert_eq!(h.game.world.roles[0].poison_resist(), 0);

    // Equip 雷灵珠 (#264) -> attr 24 (thunder = elem 1) +50, coop_magic = 320 (狂雷)
    h.game.world.add_item(264, 1);
    assert!(h.game.equip_from_menu(0, 264));
    assert_eq!(h.game.world.roles[0].elem_resist(1), 50, "雷灵珠 must grant 50% thunder resist");
    assert_eq!(h.game.world.roles[0].coop_magic, 320, "雷灵珠 must override coop_magic to 狂雷 (320)");

    // Swap ornament to 五毒珠 (#262) -> thunder resist cleared, coop_magic restored, poison resist = 100%
    h.game.world.add_item(262, 1);
    assert!(h.game.equip_from_menu(0, 262));
    assert_eq!(h.game.world.roles[0].elem_resist(1), 0, "unequipping 雷灵珠 must clear thunder resist");
    assert_eq!(h.game.world.roles[0].coop_magic, orig_coop, "unequipping 雷灵珠 must restore base coop_magic");
    assert_eq!(h.game.world.roles[0].poison_resist(), 100, "五毒珠 must grant 100% poison resist");

    // Verify poison immunity in battle against an enemy with 100% poison-on-hit rate.
    h.game.world.roles[0].hp = 9999;
    h.game.world.roles[0].max_hp = 9999;
    assert!(h.game.start_test_battle(1));
    {
        let b = h.game.battle.as_mut().unwrap();
        b.enemies[0].hp = 9999;
        b.enemies[0].attack_equiv_item = 555;
        b.enemies[0].attack_equiv_rate = 10; // 10/10 = 100% poison attempt
        b.enemies[0].magic_rate = 0;
        b.enemies[0].script_pre_turn = ScriptPos::NONE;
        b.enemies[0].script_in_turn = ScriptPos::NONE;
    }
    let (prompted, _) = step_battle_until_prompt_or_end(&mut h, 2000);
    assert!(prompted);
    assert!(h.game.battle_command(BattleCmd::Defend));
    let (prompted, _) = step_battle_until_prompt_or_end(&mut h, 2000);
    assert!(prompted);
    assert!(
        h.game.world.roles[0].poisons.is_empty(),
        "五毒珠 wearer must not be poisoned by enemy attack_equiv_item"
    );
}

/// #215: Using `隐蛊 #141` (`battle_hide_role(3)`) prevents enemies from damaging the party for 3 rounds.
#[test]
fn issue215_yingu_battle_hide_role_blocks_enemy_attacks() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);

    h.game.world.roles[0].hp = 500;
    h.game.world.roles[0].max_hp = 500;
    h.game.world.add_item(141, 1);

    assert!(h.game.start_test_battle(1));
    {
        let b = h.game.battle.as_mut().unwrap();
        b.enemies[0].hp = 9999;
        b.enemies[0].attack = 200;
        b.enemies[0].magic_rate = 0;
        b.enemies[0].script_pre_turn = ScriptPos::NONE;
        b.enemies[0].script_in_turn = ScriptPos::NONE;
    }

    let (prompted, _) = step_battle_until_prompt_or_end(&mut h, 2000);
    assert!(prompted);
    assert!(h.game.battle_command(BattleCmd::Item { item: 141 }));
    let (prompted, _) = step_battle_until_prompt_or_end(&mut h, 2000);
    assert!(prompted);
    assert_eq!(h.game.world.roles[0].hp, 500, "隐蛊 must prevent enemy damage on the round it is used");
    assert!(
        h.game.battle.as_ref().unwrap().party_hidden_turns >= 2,
        "party_hidden_turns should remain active for subsequent rounds"
    );
}

/// #185: `object_set_script(obj_id, 0)` suppresses the enemy's static `script_pre_turn` in future
/// battles against that `obj_id` (e.g. `胖苗` obj_id 485 after Ch.1 intro).
#[test]
fn issue185_object_set_script_overrides_enemy_pre_turn() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);

    // Find a team containing an enemy with a non-empty `script_pre_turn` and `obj_id > 0`.
    let (team_id, obj_id) = h
        .game
        .data
        .teams
        .iter()
        .find_map(|(&tid, t)| {
            let members = t.get("members")?.as_array()?;
            for m in members {
                let eid = m.get("enemy_id")?.as_u64()? as u16;
                let oid = m.get("obj_id")?.as_i64()? as i32;
                if oid > 0 {
                    if let Some(ed) = h.game.data.enemies.get(&eid) {
                        if !ed.script_pre_turn.is_empty() {
                            return Some((tid as i32, oid));
                        }
                    }
                }
            }
            None
        })
        .expect("expected at least one team with an enemy having obj_id and script_pre_turn");

    // Clear that object's script via `object_set_script(obj_id, 0)`.
    h.game
        .prog
        .load_block(
            "_t185",
            &json!([
                {"op": "object_set_script", "args": [obj_id, 0]},
                {"op": "end"}
            ]),
        )
        .unwrap();
    assert!(h.game.run_script("_t185", 0));
    assert!(h.settle(500));

    assert!(h.game.start_test_battle(team_id));
    let b = h.game.battle.as_ref().unwrap();
    let en = b.enemies.iter().find(|e| e.obj_id == obj_id).unwrap();
    assert!(
        en.script_pre_turn.is_none(),
        "object_set_script({obj_id}, 0) must clear script_pre_turn on newly spawned enemy"
    );
}

/// #188: `BattleCmd::Coop` deducts MP and ends command collection for the round (no free multi-coop).
#[test]
fn issue188_coop_deducts_mp_and_consumes_party_round() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);

    // 2-member party (Xiaoyao + Ling'er)
    h.game.world.party = vec![
        pal_core::game::world::PartyMember { role_id: 0, x: 100, y: 100, dir: 0, frame: 0 },
        pal_core::game::world::PartyMember { role_id: 1, x: 116, y: 108, dir: 0, frame: 0 },
    ];
    h.game.world.roles[0].hp = 500;
    h.game.world.roles[0].max_hp = 500;
    h.game.world.roles[0].mp = 100;
    h.game.world.roles[0].max_mp = 100;
    h.game.world.roles[1].hp = 500;
    h.game.world.roles[1].max_hp = 500;
    h.game.world.roles[1].mp = 100;
    h.game.world.roles[1].max_mp = 100;

    assert!(h.game.start_test_battle(1));
    h.game.battle.as_mut().unwrap().enemies[0].hp = 9999;
    h.game.battle.as_mut().unwrap().enemies[0].script_pre_turn = ScriptPos::NONE;

    let (prompted, _) = step_battle_until_prompt_or_end(&mut h, 2000);
    assert!(prompted);
    assert_eq!(h.game.battle.as_ref().unwrap().prompting, Some(0));

    // Hero 0 chooses Coop -> must immediately execute (NOT prompt Hero 1) and deduct MP from Hero 0.
    assert!(h.game.battle_command(BattleCmd::Coop { target: 0 }));
    let (prompted_next, _) = step_battle_until_prompt_or_end(&mut h, 2000);
    assert!(prompted_next);
    assert_eq!(
        h.game.battle.as_ref().unwrap().prompting,
        Some(0),
        "Coop must consume the whole party's round and return to Hero 0 on the next round"
    );
    assert!(
        h.game.world.roles[0].mp < 100,
        "Coop must deduct MP from the initiating hero, mp={}",
        h.game.world.roles[0].mp
    );
}

/// #216: When an unportraited NPC speaks with an explicit speaker name after a portraited speaker,
/// the stale portrait is cleared so continuation lines without an explicit speaker inherit the NPC's
/// name instead of the previous portrait's owner.
#[test]
fn issue216_unportraited_npc_clears_stale_portrait_for_continuation() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);

    h.game
        .prog
        .load_block(
            "_t216",
            &json!([
                {"op": "dialogue_pos_upper", "args": [22, 0]},
                {"op": "say", "speaker": "林月如", "text": "前一句月如说话"},
                {"op": "say", "speaker": "道士", "text": "第一句道士说话"},
                {"op": "say", "text": "第二句道士续行"},
                {"op": "end"}
            ]),
        )
        .unwrap();
    assert!(h.game.run_script("_t216", 0));

    let mut seen = Vec::new();
    for _ in 0..500 {
        h.game.tick(&TickInput::IDLE);
        for c in h.game.drain_cmds() {
            if let HostCmd::ShowDialogue { speaker, text, portrait, .. } = c {
                seen.push((speaker, text, portrait));
                h.game.reply(HostReply::DialogueDone);
            }
        }
        if !h.game.is_busy() {
            break;
        }
    }
    assert_eq!(seen.len(), 3, "expected 3 dialogue lines: {seen:?}");
    assert_eq!(seen[0].0, "林月如");
    assert_eq!(seen[0].2, 22);
    assert_eq!(seen[1].0, "道士");
    assert_eq!(seen[1].2, 0, "stale Yueru portrait must be cleared for 道士");
    assert_eq!(seen[2].0, "道士", "continuation line must attribute to 道士, not 林月如");
    assert_eq!(seen[2].2, 0);
}

/// #191: Fleeing a battle revives KO'd party members to 1 HP.
#[test]
fn issue191_flee_revives_ko_party_member_to_1_hp() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);

    h.game.world.party = vec![
        pal_core::game::world::PartyMember { role_id: 0, x: 100, y: 100, dir: 0, frame: 0 },
        pal_core::game::world::PartyMember { role_id: 1, x: 116, y: 108, dir: 0, frame: 0 },
    ];
    h.game.world.roles[0].hp = 0; // Leader KO'd
    h.game.world.roles[1].hp = 200;
    h.game.world.roles[1].max_hp = 200;
    h.game.world.roles[1].mp = 100;
    h.game.world.roles[1].max_mp = 100;
    h.game.world.roles[1].magics.push(392); // 金蝉脱壳 (guaranteed non-boss flee)

    assert!(h.game.start_test_battle(1));
    h.game.battle.as_mut().unwrap().enemies[0].script_pre_turn = ScriptPos::NONE;
    h.game.battle.as_mut().unwrap().enemies[0].is_boss = false;

    let (prompted, _) = step_battle_until_prompt_or_end(&mut h, 2000);
    assert!(prompted);
    assert!(h.game.battle_command(BattleCmd::Magic { magic: 392, target: 0 }));
    let (prompted, _) = step_battle_until_prompt_or_end(&mut h, 2000);
    assert!(!prompted, "金蝉脱壳 must end the non-boss battle via Flee");
    assert!(h.game.battle.is_none());
    assert_eq!(h.game.world.roles[0].hp, 1, "KO'd leader must be revived to 1 HP on flee");
}

/// #150: Scene #1 secret passage landing `(640, 608)` refuses stepping `NW` onto `(624, 600)`
/// wall-top void, while still allowing `SE` onto `(656, 616)` toward the basement stairs.
#[test]
fn issue150_scene1_secret_passage_blocks_walltop_void() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);
    h.game.world.set_party_world_pos(640, 608);

    // NW (dir = 1) toward (624, 600) must be blocked!
    assert!(!h.game.try_manual_step(1), "stepping NW from (640, 608) onto wall-top void must be refused");
    assert_eq!((h.game.world.leader().x, h.game.world.leader().y), (640, 608));

    // SE (dir = 3) toward (656, 616) must succeed!
    assert!(h.game.try_manual_step(3), "stepping SE from (640, 608) onto tunnel floor must succeed");
    assert_eq!((h.game.world.leader().x, h.game.world.leader().y), (656, 616));
}

/// #179: Scene #3 `s_1cb2` night-crying cutscene hides both Auntie Li (`ev#57`) and Ling'er (`ev#69`)
/// before transitioning to Scene #1 so they never replay the night cutscene the next morning.
#[test]
fn issue179_scene3_night_crying_hides_npcs_before_scene_change() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(3, false);

    // Position party & ev#69 at upstairs hallway right before triggering s_1cb2.
    h.game.world.set_party_world_pos(1600, 1520);
    if let Some(ev69) = h.game.world.event_mut(69) {
        ev69.x = 1600;
        ev69.y = 1520;
        ev69.state = 2;
        ev69.trigger_mode = 7;
        let lbl = h.game.prog.intern("s_1cb2");
        ev69.trigger = ScriptPos::new(lbl, 0);
    }

    assert!(h.game.trigger_event(69));
    assert!(h.settle(50_000), "s_1cb2 failed to settle");
    assert_eq!(h.game.world.scene_id, 1, "s_1cb2 must transition to Scene #1 next morning");
    assert_eq!(
        h.game.world.event(57).unwrap().state,
        0,
        "ev#57 (Auntie Li) must be hidden before leaving Scene #3"
    );
    assert_eq!(
        h.game.world.event(69).unwrap().state,
        0,
        "ev#69 (Zhao Ling'er) must be hidden before leaving Scene #3"
    );
}

/// #170: Loading DOS `.rpg` saves must restore `rgTrail[5]` so followers advance step-by-step
/// through doorways instead of teleporting across walls (`45.rpg`), and `set_party_world_pos`
/// must place followers on the guaranteed-walkable leader tile instead of blindly offsetting into
/// adjacent cliff walls (`35.rpg` / scene entry).
#[test]
fn issue170_follower_trail_restored_from_dos_rpg_and_no_wall_clip_on_spawn() {
    let mut h = NativeHost::new();
    let rpg45 = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../site/assets/orig_saves/45.rpg");
    let bytes = std::fs::read(&rpg45).expect("read 45.rpg");
    h.game.load_dos_rpg(&bytes).expect("load 45.rpg");
    assert!(h.settle(50), "45.rpg load must settle");

    assert_eq!(h.game.world.party.len(), 2);
    assert_eq!((h.game.world.party[0].x, h.game.world.party[0].y), (624, 504));
    assert_eq!((h.game.world.party[1].x, h.game.world.party[1].y), (672, 528));
    assert!(
        h.game.world.trail.len() >= 3,
        "45.rpg rgTrail must be restored on load: {:?}",
        h.game.world.trail
    );

    // Step 1 SW: follower must step 1 half-tile NW through the doorway (672,528) -> (656,520),
    // NOT teleport 3 half-tiles across the bamboo wall straight to (624,504)!
    assert!(h.game.try_manual_step(0));
    assert_eq!(
        (h.game.world.party[1].x, h.game.world.party[1].y),
        (656, 520),
        "follower must advance 1 trail slot through the doorway on step 1"
    );
    // Step 2 SW: (656,520) -> (640,512)
    assert!(h.game.try_manual_step(0));
    assert_eq!((h.game.world.party[1].x, h.game.world.party[1].y), (640, 512));

    // Scene entry / `set_party_world_pos` must never offset followers blindly off the walkable tile.
    h.game.world.set_party_world_pos(256, 1600);
    assert_eq!((h.game.world.party[0].x, h.game.world.party[0].y), (256, 1600));
    assert_eq!((h.game.world.party[1].x, h.game.world.party[1].y), (256, 1600));
}

/// Audit regressions (#241, #246, #248, #253, #254, #268):
/// - Debug mode & god mode reset across session boundaries and replay takeover while preserving taint.
/// - `validate_debug_action` rejects invalid targets without enabling debug_mode.
/// - Language survives `start_new_game` and `load_save_json`.
/// - Base stats vs equip bonuses prevent negative-bonus stat inflation and restore base coop magic.
/// - `buy_item`/`sell_item` always enforce canonical prices & `sellable`; `load_save_json` validates sceneId & clamps HP/MP.
#[test]
fn audit_batch_core_security_stats_and_lifecycle_regressions() {
    use pal_core::game::replay_rec::DebugAction;
    use pal_core::i18n::Language;

    let mut h = NativeHost::new();
    h.game.world.set_lang(Language::En);
    h.reset_new_game();
    h.enter_scene(1, false);
    assert_eq!(h.game.world.lang, Language::En, "#248: start_new_game must preserve lang");
    assert_eq!(h.game.recorder.seg_start_abs_tick, 0, "#254: seg_start_abs_tick reset");

    // #241/#246: Invalid debug actions fail validation without enabling debug_mode.
    assert!(h.game.validate_debug_action(&DebugAction::Tp { scene_id: 9999, coords: None }).is_err());
    assert!(h.game.validate_debug_action(&DebugAction::Battle(65000)).is_err());
    assert!(h.game.validate_debug_action(&DebugAction::Script("nonexistent_label_xyz".into())).is_err());
    assert!(!h.game.debug_mode);

    // #241: Takeover from tainted replay clears active debug/god mode while keeping taint.
    h.game.set_debug_mode(true);
    h.game.god_mode = true;
    h.game.take_over_from_replay(true);
    assert!(!h.game.debug_mode);
    assert!(!h.game.god_mode);
    assert!(h.game.recorder.tainted_by_debug);

    // #241: New game clears debug/god mode and taint.
    h.game.set_debug_mode(true);
    h.game.god_mode = true;
    h.game.start_new_game(7);
    assert!(!h.game.debug_mode);
    assert!(!h.game.god_mode);
    assert!(!h.game.recorder.tainted_by_debug);
    assert_eq!(h.game.world.lang, Language::En);

    // #253: Negative equip stat bonus clamping cannot inflate base stats on unequip.
    h.game.world.roles[0].base_stats[2] = 5; // low defense
    h.game.world.roles[0].equip_stat_bonuses[2] = -20;
    h.game.world.roles[0].recompute_effective_stats();
    assert_eq!(h.game.world.roles[0].stats[2], 1);
    h.game.world.roles[0].equip_stat_bonuses[2] = 0;
    h.game.world.roles[0].recompute_effective_stats();
    assert_eq!(h.game.world.roles[0].stats[2], 5, "#253: unequip must restore exact base stat without inflation");

    // #268: Canonical shop/pawnshop pricing ignores caller-tampered price & enforces sellable.
    h.game.world.cash = 10_000;
    let cash_before_buy = h.game.world.cash;
    assert!(h.game.buy_item(62, 0), "buy_item must succeed using canonical price even if caller passes 0");
    assert_eq!(h.game.world.cash, cash_before_buy - 2000, "#268: must deduct canonical 2000, not 0");
    let cash_before_sell = h.game.world.cash;
    assert!(h.game.sell_item(62, u32::MAX), "sell_item must pay canonical half price");
    assert_eq!(h.game.world.cash, cash_before_sell + 1000, "#268: must credit canonical 1000, not u32::MAX");
    if let Some((&unsellable_id, _)) = h.game.data.items.iter().find(|(_, it)| !it.sellable) {
        h.game.world.add_item(unsellable_id, 1);
        assert!(!h.game.sell_item(unsellable_id, 500), "#268: unsellable items must be rejected");
    }

    // #268 + #248: Save load rejects unknown sceneId, clamps negative HP, and preserves lang.
    let mut bad_save = h.game.save_json("test", "2026-11-24T00:00:00Z");
    bad_save["sceneId"] = json!(9999);
    assert!(h.game.load_save_json(&bad_save).is_err(), "#268: unknown sceneId must be rejected");

    let mut neg_hp_save = h.game.save_json("test", "2026-11-24T00:00:00Z");
    neg_hp_save["roles"][0]["hp"] = json!(-50);
    assert!(h.game.load_save_json(&neg_hp_save).is_ok());
    assert_eq!(h.game.world.roles[0].hp, 0, "#268: negative hp must clamp to 0");
    assert_eq!(h.game.world.lang, Language::En, "#248: load_save_json must preserve lang");
}

/// #271 item 1: Starting equipment stat bonuses must apply on top of `roles.yaml` / DOS `.rpg`
/// base stats without double-subtracting (`start_new_game` & `load_dos_rpg`).
#[test]
fn issue271_starting_gear_bonuses_preserve_base_stats_on_new_game_and_unequip() {
    let mut h = NativeHost::new();
    h.reset_new_game();

    // Li Xiaoyao's `roles.yaml` base stats: atk=33, mag=20, def=30, dex=25, flee=26.
    // Starting gear (hat 196, cloak 225, armor 208, weapon 166 木剑 +2 atk, boots 235, ornament 249)
    // adds +2 atk, +11 def, +6 dex -> effective atk=35, def=41, dex=31.
    let lxy = &h.game.world.roles[0];
    assert_eq!(lxy.base_stats[0], 33, "New Game base attack must come verbatim from roles.yaml");
    assert_eq!(lxy.equip_stat_bonuses[0], 2, "木剑 #166 must contribute +2 attack bonus");
    assert_eq!(lxy.stats[0], 35, "effective attack must be base(33) + 木剑(2) = 35");
    assert_eq!(lxy.stats[2], 41, "effective defense must include starting armor/cloak/hat/boots bonuses");
    assert_eq!(lxy.stats[3], 31, "effective dexterity must include starting boots/ornament bonuses");

    // Swapping 木剑 (#166, +2) -> 铁剑 (#167, +15) must yield base(33) + 15 = 48 (not 46),
    // and unequipping slot 3 via `role_unequip` must return atk to exact base 33 (not 31).
    let sword_bonus = h.game.item_stat_bonuses(167)[0];
    assert!(sword_bonus > 2, "铁剑 #167 should have higher atk bonus than 木剑");
    h.game.world.add_item(167, 1);
    assert!(h.game.equip_from_menu(0, 167));
    assert_eq!(h.game.world.roles[0].base_stats[0], 33);
    assert_eq!(h.game.world.roles[0].stats[0], 33 + sword_bonus);

    h.game
        .prog
        .load_block("_t271_unequip", &json!([{"op": "role_unequip", "args": [0, 4]}, {"op": "end"}]))
        .unwrap();
    assert!(h.game.run_script("_t271_unequip", 0));
    assert!(h.settle(50));
    assert_eq!(h.game.world.roles[0].equipment[3], 0);
    assert_eq!(h.game.world.roles[0].base_stats[0], 33);
    assert_eq!(h.game.world.roles[0].stats[0], 33, "unequipping weapon must restore exact base atk 33");

    // Loading a DOS `.rpg` save must also preserve base stats from the binary and add equip bonuses on top.
    let rpg0 = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../site/assets/orig_saves/0.rpg");
    let bytes = std::fs::read(&rpg0).expect("read 0.rpg");
    h.game.load_dos_rpg(&bytes).expect("load 0.rpg");
    let lxy0 = &h.game.world.roles[0];
    assert_eq!(
        lxy0.stats[0],
        lxy0.base_stats[0] + lxy0.equip_stat_bonuses[0],
        "DOS .rpg effective stats must equal base_stats + equip_stat_bonuses"
    );
    assert!(
        lxy0.equip_stat_bonuses[0] > 0 || lxy0.equip_stat_bonuses[2] > 0,
        "0.rpg starting gear should contribute non-zero equip bonuses"
    );
}

/// #255: Verbatim SDLPAL `PAL_CLASSIC` interleaved `ActionQueue` turn order & `PAL_PlayerLevelUp`
/// stat growth bounds verified against live `Game` state.
#[test]
fn issue255_interleaved_action_queue_and_sdlpal_level_up_bounds() {
    let mut h = NativeHost::new();
    h.reset_new_game();
    h.enter_scene(1, false);

    // 1. Interleaved dexterity ordering: fast enemy (raw_dex = 300) acts BEFORE slow hero (dex = 10)
    //    when hero chooses Attack, whereas Defend (5x speed) lets hero act BEFORE a slower enemy!
    h.game.world.roles[0].hp = 500;
    h.game.world.roles[0].max_hp = 500;
    h.game.world.roles[0].base_stats[3] = 10;
    h.game.world.roles[0].equip_stat_bonuses[3] = 0;
    h.game.world.roles[0].recompute_effective_stats();

    assert!(h.game.start_test_battle(1));
    {
        let b = h.game.battle.as_mut().unwrap();
        b.enemies[0].hp = 500;
        b.enemies[0].max_hp = 500;
        b.enemies[0].level = 5;
        b.enemies[0].raw_dexterity = 300;
        b.enemies[0].script_pre_turn = ScriptPos::NONE;
        b.enemies[0].script_in_turn = ScriptPos::NONE;
    }
    let (prompted, _) = step_battle_until_prompt_or_end(&mut h, 2000);
    assert!(prompted);
    assert!(h.game.battle_command(BattleCmd::Attack { target: 0 }));
    // Tick until either enemy acts (`is_enemy_turn == true`) or hero damages enemy (`enemies[0].hp < 500`).
    let mut enemy_acted_first = false;
    for _ in 0..100 {
        h.game.tick(&TickInput::IDLE);
        let _ = h.game.drain_cmds();
        let b = h.game.battle.as_ref().unwrap();
        if b.is_enemy_turn {
            enemy_acted_first = true;
            assert_eq!(b.enemies[0].hp, 500, "fast enemy must act BEFORE slow hero deals damage");
            break;
        }
        if b.enemies[0].hp < 500 {
            break;
        }
    }
    assert!(
        enemy_acted_first,
        "fast enemy (dex ~333) must precede slow attacking hero (dex ~10) in interleaved ActionQueue"
    );

    // Finish round 1, then pick Defend (5x speed -> hero dex 10*5=50 vs enemy dex 30 -> hero first).
    let (prompted2, _) = step_battle_until_prompt_or_end(&mut h, 2000);
    assert!(prompted2);
    {
        let b = h.game.battle.as_mut().unwrap();
        b.enemies[0].level = 1;
        b.enemies[0].raw_dexterity = 9; // (1+6)*3 + 9 = 30
    }
    assert!(h.game.battle_command(BattleCmd::Defend));
    let mut hero_defended_first = false;
    for _ in 0..100 {
        h.game.tick(&TickInput::IDLE);
        let _ = h.game.drain_cmds();
        let b = h.game.battle.as_ref().unwrap();
        if b.heroes[0].defending {
            hero_defended_first = !b.is_enemy_turn;
            break;
        }
        if b.is_enemy_turn {
            break;
        }
    }
    assert!(
        hero_defended_first,
        "Defend (5x speed -> ~50) must activate before slower enemy (dex 30) acts"
    );

    // 2. PAL_PlayerLevelUp stat increments (`global.c:2347-2409`) stay within verbatim SDLPAL ranges.
    let before_hp = h.game.world.roles[0].max_hp;
    let before_mp = h.game.world.roles[0].max_mp;
    let before_base = h.game.world.roles[0].base_stats;
    let _ = h.game.world.apply_level_up(&h.game.data, 0, 1, false);
    let after = &h.game.world.roles[0];
    assert!((10..=17).contains(&(after.max_hp - before_hp)), "MaxHP delta out of 10..=17");
    assert!((8..=13).contains(&(after.max_mp - before_mp)), "MaxMP delta out of 8..=13");
    assert!((4..=5).contains(&(after.base_stats[0] - before_base[0])), "Atk delta out of 4..=5");
    assert!((4..=5).contains(&(after.base_stats[1] - before_base[1])), "Mag delta out of 4..=5");
    assert!((2..=3).contains(&(after.base_stats[2] - before_base[2])), "Def delta out of 2..=3");
    assert!((2..=3).contains(&(after.base_stats[3] - before_base[3])), "Dex delta out of 2..=3");
    assert_eq!(after.base_stats[4] - before_base[4], 2, "Flee delta must be +2");
}
