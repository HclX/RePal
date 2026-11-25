//! Integration tests for the unified RePal Text Mode (`TextEngine` + `HeadlessTextHost` + `ChannelServer`).

use pal_headless::text_mode::{
    ChannelServer, HeadlessTextHost, TextEngine, TextHost, TextModeConfig,
};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn assets_root() -> PathBuf {
    repo_root().join("site/assets")
}

fn assert_unified_envelope(v: &serde_json::Value) {
    assert_eq!(v["protocol"], pal_headless::text_mode::TEXT_PROTOCOL_VERSION);
    for key in [
        "protocol",
        "obs_mode",
        "ok",
        "cmd",
        "mode",
        "lang",
        "ui_view",
        "view",
        "tick",
        "scene",
        "party",
        "cash",
        "walkable",
        "walkable_dist",
        "minimap",
        "events",
        "prompt",
        "battle",
        "menu",
        "rng",
        "notices",
        "artifacts",
        "log",
        "interrupted_by",
        "error",
        "error_code",
    ] {
        assert!(
            v.get(key).is_some(),
            "missing required envelope field '{key}' in {v}"
        );
    }
    assert!(v["view"]["camera"]["x"].is_i64());
    assert!(v["view"]["camera"]["y"].is_i64());
    assert_eq!(v["view"]["camera"]["w"], 320);
    assert_eq!(v["view"]["camera"]["h"], 200);
    assert!(v["view"]["hd"].is_boolean());
    assert!(v["notices"].is_array());
}

#[test]
fn text_mode_default_boots_to_title_and_new_starts_scene1() {
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot headless text host");
    let mut engine = TextEngine::new(false, 0);

    let look = engine.execute_sync(&mut host, "look", false);
    assert_unified_envelope(&look.response);
    assert_eq!(look.response["ok"], true);
    assert_eq!(look.response["mode"], "title");
    assert_eq!(look.response["menu"]["screen"], "title");

    let started = engine.execute_sync(&mut host, "new 12345", false);
    assert_unified_envelope(&started.response);
    assert_eq!(started.response["ok"], true);
    assert_ne!(started.response["mode"], "title");
    assert_eq!(started.response["scene"]["id"], 1);
}

#[test]
fn text_mode_walk_multistep_not_stuck_at_one_step() {
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        save_path: Some(PathBuf::from("0.rpg")),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 0.rpg");
    let mut engine = TextEngine::new(false, 0);
    engine.settle_sync(&mut host, 60);

    let before = engine.execute_sync(&mut host, "look", false);
    let x0 = before.response["party"][0]["x"].as_i64().unwrap();
    let y0 = before.response["party"][0]["y"].as_i64().unwrap();

    // Regression test for P0 Bug #1: `sw 3` must actually walk 3 half-tile steps across MOVE_TICKS cooldowns.
    let walked = engine.execute_sync(&mut host, "sw 3", false);
    assert_unified_envelope(&walked.response);
    assert_eq!(walked.response["ok"], true);
    let x1 = walked.response["party"][0]["x"].as_i64().unwrap();
    let y1 = walked.response["party"][0]["y"].as_i64().unwrap();
    let iso_steps = ((x1 - x0).abs() + (y1 - y0).abs() * 2) / 32;
    assert!(
        iso_steps >= 2,
        "expected sw 3 to walk multiple steps, moved from ({x0},{y0}) to ({x1},{y1}) (iso_steps={iso_steps}, log={:?})",
        walked.response["log"]
    );
}

#[test]
fn text_mode_goto_multistep_and_rejects_out_of_scene_event() {
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        save_path: Some(PathBuf::from("0.rpg")),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 0.rpg");
    let mut engine = TextEngine::new(false, 0);
    engine.settle_sync(&mut host, 60);

    // Event #500 does not belong to Scene 1 -> must fail cleanly instead of walking to stale coords.
    let bad = engine.execute_sync(&mut host, "goto 500", false);
    assert_unified_envelope(&bad.response);
    assert_eq!(bad.response["ok"], false);
    assert!(
        bad.response["error"]
            .as_str()
            .unwrap_or("")
            .contains("not active in current scene"),
        "unexpected error: {}",
        bad.response["error"]
    );

    // Goto a reachable coordinate 3 SW steps away -> must actually walk multiple steps.
    let before = engine.execute_sync(&mut host, "look", false);
    let x0 = before.response["party"][0]["x"].as_i64().unwrap() as i32;
    let y0 = before.response["party"][0]["y"].as_i64().unwrap() as i32;
    let tx = x0 - 48;
    let ty = y0 + 24;
    let moved = engine.execute_sync(&mut host, &format!("goto {tx} {ty}"), false);
    assert_unified_envelope(&moved.response);
    assert_eq!(moved.response["ok"], true);
    let x1 = moved.response["party"][0]["x"].as_i64().unwrap() as i32;
    let y1 = moved.response["party"][0]["y"].as_i64().unwrap() as i32;
    let dist_moved = ((x1 - x0).abs() + (y1 - y0).abs() * 2) / 32;
    assert!(
        dist_moved >= 2,
        "expected goto to move multiple steps from ({x0},{y0}) toward ({tx},{ty}), ended at ({x1},{y1})"
    );
}

#[test]
fn text_mode_equip_reports_real_failure_and_validates_party_idx() {
    let out_dir = std::env::temp_dir().join(format!(
        "repal_proto_v1_tests_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        save_path: Some(PathBuf::from("10.rpg")),
        out_dir: out_dir.clone(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 10.rpg");
    let mut engine = TextEngine::new(false, 0);
    engine.settle_sync(&mut host, 60);

    // Invalid item must return ok=false (regresses P0 Bug #3 where equip always returned ok=true).
    let bad_item = engine.execute_sync(&mut host, "equip 9999", false);
    assert_unified_envelope(&bad_item.response);
    assert_eq!(bad_item.response["ok"], false);

    // Out-of-range party_idx must return ok=false.
    let bad_party = engine.execute_sync(&mut host, "equip 163 9", false);
    assert_eq!(bad_party.response["ok"], false);
    assert!(bad_party.response["error"]
        .as_str()
        .unwrap_or("")
        .contains("out of range"));

    let bad_use_party = engine.execute_sync(&mut host, "use 100 9", false);
    assert_eq!(bad_use_party.response["ok"], false);
}

#[test]
fn text_mode_pawnshop_sell_stops_at_item_exhaustion_without_selling_neighbor() {
    let out_dir = std::env::temp_dir().join(format!(
        "repal_proto_v1_tests_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        save_path: Some(PathBuf::from("10.rpg")),
        out_dir: out_dir.clone(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 10.rpg");
    let mut engine = TextEngine::new(false, 0);
    engine.settle_sync(&mut host, 60);

    // Open pawnshop using real runtime inventory
    host.runtime_mut().refresh_pawnshop();
    host.runtime_mut().ui.set_view(pal_ui::UiView::PawnshopSell);
    assert!(
        host.runtime().ui.shop_items.len() >= 2,
        "10.rpg should have at least 2 sellable items"
    );
    let item_a = host.runtime().ui.shop_items[0].item_id;
    let item_b = host.runtime().ui.shop_items[1].item_id;
    let qty_a = host.runtime().game.as_ref().unwrap().world.item_count(item_a);
    let qty_b = host.runtime().game.as_ref().unwrap().world.item_count(item_b);
    assert!(qty_a > 0 && qty_b > 0);

    // Request selling more of item_a than owned: must exhaust item_a and NEVER touch item_b.
    let over_qty = (qty_a as usize) + 5;
    let res = engine.execute_sync(&mut host, &format!("sell {item_a} {over_qty}"), false);
    assert_unified_envelope(&res.response);
    assert_eq!(res.response["ok"], true);
    assert_eq!(res.response["artifacts"]["sold"], qty_a as u64);

    let g = host.runtime().game.as_ref().unwrap();
    assert_eq!(g.world.item_count(item_a), 0, "item_a should be fully sold");
    assert_eq!(
        g.world.item_count(item_b),
        qty_b,
        "item_b must NOT be touched when item_a runs out"
    );
}

#[test]
fn text_mode_json_envelope_and_menu_and_shot_consistency() {
    let out_dir = repo_root().join("target/text_mode_test_shots");
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        save_path: Some(PathBuf::from("10.rpg")),
        out_dir: out_dir.clone(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 10.rpg");
    let mut engine = TextEngine::new(false, 0);
    engine.settle_sync(&mut host, 60);

    for cmd in ["look", "status", "bag", "menu", "ui down", "ui back", "shot smoke_test.png", "unknown_xyz"] {
        let reply = engine.execute_sync(&mut host, cmd, false);
        assert_unified_envelope(&reply.response);
        match cmd {
            "status" => assert!(reply.response["artifacts"]["status"].is_array()),
            "bag" => assert!(reply.response["artifacts"]["bag"].is_array()),
            "menu" => {
                assert_eq!(reply.response["mode"], "menu");
                assert_eq!(reply.response["menu"]["screen"], "root");
            }
            "shot smoke_test.png" => {
                assert_eq!(reply.response["ok"], true);
                assert!(out_dir.join("smoke_test.png").is_file());
                assert!(reply.response["artifacts"]["shot"]["non_black_pct"].as_f64().unwrap_or(0.0) > 20.0);
            }
            "unknown_xyz" => assert_eq!(reply.response["ok"], false),
            _ => assert_eq!(reply.response["ok"], true),
        }
    }
}

#[test]
fn text_mode_tcp_channel_server_handles_comments_and_multiple_clients() {
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        save_path: Some(PathBuf::from("0.rpg")),
        json_mode: true,
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 0.rpg");
    let mut engine = TextEngine::new(false, 0);
    engine.settle_sync(&mut host, 60);

    let server = ChannelServer::start_tcp("127.0.0.1:0", true, None).expect("bind ephemeral tcp");
    let addr = server.local_addr().expect("local_addr");

    // Drive the server on a worker thread for 2 clients
    let driver = thread::spawn(move || {
        let mut handled = 0usize;
        while handled < 6 {
            match server.rx.recv_timeout(Duration::from_secs(5)) {
                Ok(req) => {
                    let reply = engine.execute_sync(&mut host, &req.raw, req.interactive_blank);
                    let _ = req.reply_tx.send(reply);
                    handled += 1;
                }
                Err(_) => break,
            }
        }
    });

    // Client 1: connect, read initial look, send comments + look + quit
    {
        let stream = TcpStream::connect(addr).expect("client 1 connect");
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);

        let mut line = String::new();
        reader.read_line(&mut line).expect("initial look");
        let v: serde_json::Value = serde_json::from_str(line.trim()).expect("valid json");
        assert_unified_envelope(&v);
        assert_eq!(v["ok"], true);

        writeln!(writer, "# comment ignored").unwrap();
        writeln!(writer, "// comment ignored").unwrap();
        writeln!(writer, "look").unwrap();
        writer.flush().unwrap();

        line.clear();
        reader.read_line(&mut line).expect("look reply");
        let v2: serde_json::Value = serde_json::from_str(line.trim()).expect("valid json");
        assert_eq!(v2["ok"], true);
        assert_eq!(v2["cmd"], "look");

        writeln!(writer, "quit").unwrap();
        writer.flush().unwrap();

        line.clear();
        reader.read_line(&mut line).expect("bye reply");
        let bye: serde_json::Value = serde_json::from_str(line.trim()).expect("valid bye json");
        assert_eq!(bye["bye"], true);
    }

    // Client 2: server must still be alive after Client 1 quit!
    {
        let stream = TcpStream::connect(addr).expect("client 2 connect");
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);

        let mut line = String::new();
        reader.read_line(&mut line).expect("client 2 initial look");
        let v: serde_json::Value = serde_json::from_str(line.trim()).expect("valid json");
        assert_eq!(v["ok"], true);

        writeln!(writer, "sw 2").unwrap();
        writer.flush().unwrap();

        line.clear();
        reader.read_line(&mut line).expect("client 2 sw 2 reply");
        let v2: serde_json::Value = serde_json::from_str(line.trim()).expect("valid json");
        assert_eq!(v2["ok"], true);
        assert_eq!(v2["cmd"], "sw 2");

        writeln!(writer, "quit").unwrap();
        writer.flush().unwrap();
    }

    driver.join().expect("driver join");
}

#[test]
fn text_mode_battle_magic_default_target_picks_ally_for_heal_and_enemy_for_attack() {
    let out_dir = std::env::temp_dir().join(format!(
        "repal_proto_v1_tests_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        save_path: Some(PathBuf::from("10.rpg")),
        out_dir: out_dir.clone(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 10.rpg");
    let mut engine = TextEngine::new(false, 0);
    engine.settle_sync(&mut host, 60);

    // Start a real battle and settle until prompting for hero 0.
    let started = engine.execute_sync(&mut host, "/battle 1", false);
    assert_eq!(started.response["ok"], true);
    assert_eq!(started.response["mode"], "battle");

    // Pick a real enemy-targeted spell and a real ally-targeted heal spell from game data.
    let (atk_magic, heal_magic) = {
        let g = host.runtime().game.as_ref().unwrap();
        let atk = g
            .data
            .magics
            .iter()
            .find(|(_, m)| m.target_enemy && m.base_damage > 0)
            .map(|(&id, _)| id)
            .expect("offensive magic in data");
        let heal = g
            .data
            .magics
            .iter()
            .find(|(_, m)| !m.target_enemy && m.base_damage != 0)
            .map(|(&id, _)| id)
            .expect("healing magic in data");
        (atk, heal)
    };

    // Simulate enemy 0 dead (if multiple enemies) and hero 1 injured, hero 0 full HP.
    {
        let g = host.runtime_mut().game.as_mut().unwrap();
        let b = g.battle.as_mut().unwrap();
        if b.enemies.len() >= 2 {
            b.enemies[0].hp = 0;
        }
        let hero_roles: Vec<usize> = b.heroes.iter().map(|h| h.role).collect();
        if hero_roles.len() >= 2 {
            g.world.roles[hero_roles[0]].hp = g.world.roles[hero_roles[0]].max_hp;
            g.world.roles[hero_roles[1]].hp = 10;
            g.world.roles[hero_roles[1]].max_hp = 200;
        }
    }

    let atk_target = pal_headless::text_mode::engine::default_magic_target(host.runtime(), atk_magic);
    let heal_target = pal_headless::text_mode::engine::default_magic_target(host.runtime(), heal_magic);

    let g = host.runtime().game.as_ref().unwrap();
    let b = g.battle.as_ref().unwrap();
    assert!(
        b.enemies[atk_target].hp > 0,
        "offensive magic should target an alive enemy (got slot {atk_target})"
    );
    if b.heroes.len() >= 2 {
        assert_eq!(
            heal_target, 1,
            "healing magic with no explicit slot must target the injured ally (slot 1), not an enemy slot"
        );
    }
}

#[test]
fn text_mode_click_in_explore_mode_moves_party_and_respects_out_dir() {
    let out_dir = repo_root().join("target/text_mode_test_click_out");
    let _ = std::fs::remove_dir_all(&out_dir);
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        save_path: Some(PathBuf::from("0.rpg")),
        out_dir: out_dir.clone(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 0.rpg");
    let mut engine = TextEngine::new(false, 0);
    engine.settle_sync(&mut host, 60);

    let before = engine.execute_sync(&mut host, "look", false);
    let x0 = before.response["party"][0]["x"].as_i64().unwrap();
    let y0 = before.response["party"][0]["y"].as_i64().unwrap();

    // Leader is centered near logical (160, 100); click SW at (112, 124) in explore mode.
    let clicked = engine.execute_sync(&mut host, "click 112 124", false);
    assert_unified_envelope(&clicked.response);
    assert_eq!(clicked.response["ok"], true);
    let x1 = clicked.response["party"][0]["x"].as_i64().unwrap();
    let y1 = clicked.response["party"][0]["y"].as_i64().unwrap();
    assert!(
        (x1, y1) != (x0, y0),
        "expected explore click to move party away from ({x0},{y0}), stayed at ({x1},{y1})"
    );

    // `shot` without explicit path and `dump-state` / `save` with relative path must respect `out_dir`.
    let shot = engine.execute_sync(&mut host, "shot", false);
    assert_eq!(shot.response["ok"], true);
    let shot_path = PathBuf::from(shot.response["artifacts"]["shot"]["path"].as_str().unwrap());
    assert!(shot_path.starts_with(&out_dir) && shot_path.is_file());

    let dump = engine.execute_sync(&mut host, "dump-state sub/state.json", false);
    assert_eq!(dump.response["ok"], true);
    assert!(out_dir.join("sub/state.json").is_file());

    let saved = engine.execute_sync(&mut host, "save sub/slot.json", false);
    assert_eq!(saved.response["ok"], true);
    assert!(out_dir.join("sub/slot.json").is_file());
}

#[test]
fn text_mode_tick_driven_continue_and_plain_tcp_blank_prompt() {
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        save_path: Some(PathBuf::from("0.rpg")),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 0.rpg");
    let mut engine = TextEngine::new(false, 2);
    engine.settle_sync(&mut host, 60);

    // Simulate GUI 50 Hz tick-by-tick stepping of `ContinueDialog` when idle: must terminate cleanly.
    let outcome = engine.submit(&mut host, "continue", false);
    assert!(matches!(outcome, pal_headless::text_mode::ExecOutcome::Pending));
    let mut reply = None;
    for _ in 0..20 {
        let (input, done) = engine.step_tick(&mut host);
        if let Some(r) = done {
            reply = Some(r);
            break;
        }
        host.runtime_mut().tick(input);
        host.service_host_cmds();
    }
    let reply = reply.expect("tick-driven continue must finish");
    assert_eq!(reply.response["ok"], true);

    // Plain-text TCP server must reprint `[<mode>]> ` after blank / comment lines.
    let server = ChannelServer::start_tcp("127.0.0.1:0", false, None).expect("bind plain tcp");
    let addr = server.local_addr().unwrap();
    let driver = thread::spawn(move || {
        let mut handled = 0usize;
        while handled < 2 {
            match server.rx.recv_timeout(Duration::from_secs(5)) {
                Ok(req) => {
                    let r = engine.execute_sync(&mut host, &req.raw, req.interactive_blank);
                    let _ = req.reply_tx.send(r);
                    handled += 1;
                }
                Err(_) => break,
            }
        }
    });

    {
        use std::io::Read;
        let mut stream = TcpStream::connect(addr).expect("connect plain tcp");
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = [0u8; 4096];
        let mut acc = String::new();
        while !acc.contains("]> ") {
            let n = stream.read(&mut buf).expect("read initial prompt");
            assert!(n > 0);
            acc.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        // Send blank line + comment -> server must reprint prompt without blocking.
        writeln!(stream).unwrap();
        writeln!(stream, "# comment").unwrap();
        stream.flush().unwrap();

        let mut after = String::new();
        while after.matches("]> ").count() < 2 {
            let n = stream.read(&mut buf).expect("read prompt after blank/comment");
            assert!(n > 0);
            after.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        assert!(after.contains("[explore]> "));

        writeln!(stream, "quit").unwrap();
        stream.flush().unwrap();
    }
    driver.join().expect("plain tcp driver join");
}

#[test]
fn text_mode_playtest_regressions_toast_battle_atb_and_dialog_sanitize() {
    let out_dir = std::env::temp_dir().join(format!(
        "repal_proto_v1_tests_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        save_path: Some(PathBuf::from("10.rpg")),
        out_dir: out_dir.clone(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 10.rpg");
    let mut engine = TextEngine::new(false, 0);
    engine.settle_sync(&mut host, 60);

    // 1. Toast ticks must decay during headless ticks (without render_frame) and clear on menu open.
    host.runtime_mut().show_toast("测试提示", 5);
    assert!(host.runtime().ui.toast_text.is_some());
    let _ = engine.execute_sync(&mut host, "wait 6", false);
    assert!(
        host.runtime().ui.toast_text.is_none(),
        "toast must expire during headless ticks without calling render_frame"
    );

    host.runtime_mut().show_toast("残留提示", 90);
    let _ = engine.execute_sync(&mut host, "menu", false);
    assert!(
        host.runtime().ui.toast_text.is_none(),
        "opening menu from explore must clear stale overworld toast"
    );
    let _ = engine.execute_sync(&mut host, "close", false);

    // 2. Dialog trailing unmatched '(' (DOS PAL wait marker) must be stripped cleanly.
    host.runtime_mut().ui.open_dialog("李逍遥", "啊．．(", None, false);
    assert_eq!(host.runtime().ui.dialog.text, "啊．．");
    host.runtime_mut().ui.open_dialog("旁白", "测试(保留括号)", None, false);
    assert_eq!(host.runtime().ui.dialog.text, "测试(保留括号)");
    host.runtime_mut().ui.set_view(pal_ui::UiView::None);

    // 3. Entering battle must auto-settle ATB so `battle.prompting` is immediately ready (`defend` succeeds without manual `wait`).
    let started = engine.execute_sync(&mut host, "/battle 1", false);
    assert_eq!(started.response["ok"], true);
    assert_eq!(started.response["mode"], "battle");
    assert!(
        started.response["battle"]["prompting"].is_u64(),
        "battle entry must settle until prompting is ready, got: {}",
        started.response["battle"]
    );
    let defended = engine.execute_sync(&mut host, "defend", false);
    assert_eq!(defended.response["ok"], true);

    // 4. Plain-text output includes party HP/MP, walkable dirs, and localized menu items.
    let _ = engine.execute_sync(&mut host, "load 0.rpg", false);
    let look = engine.execute_sync(&mut host, "look", false);
    let plain = pal_headless::text_mode::format_plain_response(&look.response, true);
    assert!(plain.contains("[队伍]"), "plain look should show party HP/MP:\n{plain}");
    assert!(plain.contains("[可走]"), "plain look should show walkable dirs:\n{plain}");
}

#[test]
fn text_mode_batch3_regressions_validation_use_goto_lang_hd_and_layer() {
    let out_dir = std::env::temp_dir().join(format!(
        "repal_batch3_tests_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        save_path: Some(PathBuf::from("10.rpg")),
        out_dir: out_dir.clone(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 10.rpg");
    let mut engine = TextEngine::new(false, 0);
    engine.settle_sync(&mut host, 60);

    // 1. #175: `save` + `load` symmetry under `--out-dir` & EN plain-text labels.
    let saved = engine.execute_sync(&mut host, "save cp_sym.json", false);
    assert_eq!(saved.response["ok"], true);
    assert!(out_dir.join("cp_sym.json").is_file());
    let loaded = engine.execute_sync(&mut host, "load cp_sym.json", false);
    assert_eq!(loaded.response["ok"], true, "load must find file in out_dir: {}", loaded.response);

    let _ = engine.execute_sync(&mut host, "lang en", false);
    let look_en = engine.execute_sync(&mut host, "look", false);
    let plain_en = pal_headless::text_mode::format_plain_response(&look_en.response, false);
    assert!(plain_en.contains("Wen:") && plain_en.contains("[Party]"), "EN plain labels expected:\n{plain_en}");
    let back_zh = engine.execute_sync(&mut host, "lang hans", false);
    assert_eq!(back_zh.response["ok"], true);

    // 2. #192: Consecutive `use 101 0; use 101 1` both succeed back-to-back.
    {
        let g = host.runtime_mut().game.as_mut().unwrap();
        g.world.add_item(101, 5);
        for m in g.world.party.clone() {
            if let Some(r) = g.world.roles.get_mut(m.role_id as usize) {
                r.hp = (r.max_hp / 2).max(1);
            }
        }
    }
    let u0 = engine.execute_sync(&mut host, "use 101 0", false);
    assert_eq!(u0.response["ok"], true, "first use failed: {}", u0.response);
    let u1 = engine.execute_sync(&mut host, "use 101 0", false);
    assert_eq!(u1.response["ok"], true, "second consecutive use failed: {}", u1.response);

    // 3. #168: `view items` + `key confirm` must not hang on chapter load.
    let v_items = engine.execute_sync(&mut host, "view items", false);
    assert_eq!(v_items.response["ok"], true);
    let k_confirm = engine.execute_sync(&mut host, "key confirm", false);
    assert_eq!(k_confirm.response["ok"], true);
    let _ = engine.execute_sync(&mut host, "close", false);

    // 4. #203: Unreachable `goto` returns `ok: false` and `artifacts.goto.reached == false`.
    let unreachable = engine.execute_sync(&mut host, "goto 0 0", false);
    assert_eq!(unreachable.response["ok"], false, "unreachable goto must fail: {}", unreachable.response);
    assert_eq!(unreachable.response["artifacts"]["goto"]["reached"], false);

    // 5. #176: Runtime `set_lang` refreshes active Shop item names immediately.
    host.runtime_mut().ui.shop_items = vec![pal_ui::ShopEntry {
        item_id: 101,
        name: host.runtime().ui.catalog.item_name(101),
        price: 280,
        count: 1,
    }];
    host.runtime_mut().ui.set_view(pal_ui::UiView::Shop);
    let zh_name = host.runtime().ui.shop_items[0].name.clone();
    host.runtime_mut().set_lang(pal_core::Language::En);
    let en_name = host.runtime().ui.shop_items[0].name.clone();
    assert_ne!(zh_name, en_name, "active shop item name must refresh on set_lang");
    host.runtime_mut().ui.set_view(pal_ui::UiView::None);
    host.runtime_mut().set_lang(pal_core::Language::ZhHans);

    // 6. #175 & #174: Battle `magic <id>` rejects unlearned spells, `save` rejected in battle,
    // and MiscMenu UI-key flee settles cleanly out of battle.
    let started = engine.execute_sync(&mut host, "/battle 1", false);
    assert_eq!(started.response["mode"], "battle");
    let unlearned = engine.execute_sync(&mut host, "magic 370 0", false);
    assert_eq!(unlearned.response["ok"], false, "unlearned magic must be rejected: {}", unlearned.response);
    let mid_battle_save = engine.execute_sync(&mut host, "save illegal.json", false);
    assert_eq!(mid_battle_save.response["ok"], false, "save outside explore must be rejected");
    // Open MiscMenu (Down -> Confirm) -> select Flee (Up wraps to index 3 -> Confirm) (#174).
    let _ = engine.execute_sync(&mut host, "key down", false);
    let _ = engine.execute_sync(&mut host, "key confirm", false);
    let _ = engine.execute_sync(&mut host, "key up", false);
    let fled = engine.execute_sync(&mut host, "key confirm", false);
    assert_eq!(fled.response["ok"], true);
    assert_ne!(
        fled.response["ui_view"], "battle_misc_menu",
        "MiscMenu flee must exit BattleHud cleanly"
    );
    if fled.response["mode"] == "battle" {
        let _ = engine.execute_sync(&mut host, "flee", false);
    }

    // 7. #178/#189 & #186: Scene #59 (1x fallback map 76 + Blood Pool party_layer) renders non-black in HD
    // and `party_layer` persists through save/load and resets on scene change.
    let _ = engine.execute_sync(&mut host, "/tp 59", false);
    {
        let g = host.runtime_mut().game.as_mut().unwrap();
        g.world.party_layer = 6;
        let saved_json = g.save_json("layer_test", "test");
        assert_eq!(saved_json["partyLayer"], 6);
        g.world.party_layer = 0;
        g.load_save_json(&saved_json).expect("reload layer save");
        assert_eq!(g.world.party_layer, 6, "party_layer must round-trip through save JSON (#186)");
    }
    host.runtime_mut().project();
    assert_eq!(host.runtime().state.party_layer, 6);
    let _ = engine.execute_sync(&mut host, "hd on", false);
    let shot = engine.execute_sync(&mut host, "shot sc59_hd.png", false);
    assert_eq!(shot.response["ok"], true);
    let non_black = shot.response["artifacts"]["shot"]["non_black_pct"].as_f64().unwrap_or(0.0);
    assert!(
        non_black > 30.0,
        "Scene #59 HD 1x fallback map must render non-black on first shot (got {non_black}%)"
    );
    let _ = engine.execute_sync(&mut host, "/tp 1", false);
    assert_eq!(
        host.runtime().state.party_layer,
        0,
        "party_layer must reset to 0 on scene change (#186)"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn text_mode_protocol_v1_pagination_notices_battle_feed_metadata_menus_and_fair_mode() {
    use pal_headless::text_mode::{ObsMode, TEXT_PROTOCOL_VERSION};

    let out_dir = std::env::temp_dir().join(format!(
        "repal_proto_v1_tests_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        save_path: Some(PathBuf::from("10.rpg")),
        out_dir: out_dir.clone(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 10.rpg");
    let mut engine = TextEngine::with_obs(ObsMode::Debug, 0);
    engine.settle_sync(&mut host, 60);

    // 0. #225: Verify JSON schema file is valid JSON and covers all envelope keys returned by `look`.
    let schema_text = std::fs::read_to_string(repo_root().join("docs/text_mode.schema.json"))
        .expect("read docs/text_mode.schema.json");
    let schema: serde_json::Value = serde_json::from_str(&schema_text).expect("parse schema json");
    let required_keys: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    let look = engine.execute_sync(&mut host, "look", false);
    assert_unified_envelope(&look.response);
    assert_eq!(look.response["protocol"], TEXT_PROTOCOL_VERSION);
    for k in &required_keys {
        assert!(
            look.response.get(*k).is_some(),
            "response missing schema-required key '{k}'"
        );
    }
    // #225: `events[].kind` is a stable English machine enum and `ui_view` is snake_case.
    assert_eq!(look.response["ui_view"], "none");
    for ev in look.response["events"].as_array().unwrap() {
        let kind = ev["kind"].as_str().unwrap();
        assert!(
            matches!(kind, "portal" | "trigger" | "npc" | "object"),
            "unexpected event kind enum: {kind}"
        );
        assert!(ev["kind_label"].is_string());
    }

    // 1. #217: Long multi-page English dialog must have identical `next` count headless WITHOUT
    // `shot` vs WITH `shot`, and a 2-page choice prompt must be answered by a single `yes`.
    let long_en = "This is a very long English dialogue line designed specifically to wrap across \
                   more than three lines inside the authentic 320x200 dialogue viewport so that \
                   pagination produces at least two pages consistently in both headless and GUI.";
    let _ = engine.execute_sync(&mut host, "lang en", false);
    let count_nexts = |host: &mut HeadlessTextHost, engine: &mut TextEngine, take_shot: bool| -> usize {
        {
            let rt = host.runtime_mut();
            rt.ui.open_dialog("Li Xiaoyao", long_en, Some(1), false);
            rt.ui.sync_dialog_layout(&mut rt.compositor.fonts, rt.ui.hd_mode);
        }
        if take_shot {
            let _ = engine.execute_sync(host, "shot", false);
        }
        let obs = engine.execute_sync(host, "look", false);
        let tp = obs.response["prompt"]["total_pages"].as_u64().unwrap();
        assert!(tp >= 2, "expected multi-page dialog, got total_pages={tp}");
        assert!(obs.response["prompt"]["page_text"].is_string());
        let mut presses = 0usize;
        while host.runtime().ui.view == pal_ui::UiView::Dialog && presses < 10 {
            let _ = engine.execute_sync(host, "next", false);
            presses += 1;
        }
        presses
    };
    let n_no_shot = count_nexts(&mut host, &mut engine, false);
    let n_with_shot = count_nexts(&mut host, &mut engine, true);
    assert_eq!(
        n_no_shot, n_with_shot,
        "headless dialog pagination must be identical with and without shot"
    );
    assert!(n_no_shot >= 2);

    // 2-page choice dialog answered in a single `yes` (#217)
    {
        let rt = host.runtime_mut();
        rt.ui.open_dialog("Elder", long_en, Some(1), false);
        rt.ui.dialog.choices = vec!["Yes".into(), "No".into()];
        rt.ui.sync_dialog_layout(&mut rt.compositor.fonts, rt.ui.hd_mode);
        assert!(rt.ui.dialog.total_pages >= 2);
    }
    let yes_res = engine.execute_sync(&mut host, "yes", false);
    assert_eq!(yes_res.response["ok"], true, "single `yes` must answer multi-page choice: {}", yes_res.response);
    assert_ne!(yes_res.response["mode"], "choice");

    // 2. #218: Toasts appear in `notices[]`, and using a heal item at full HP returns `ok: false` + `item_refused` + notice.
    let _ = engine.execute_sync(&mut host, "lang hans", false);
    {
        let g = host.runtime_mut().game.as_mut().unwrap();
        g.world.add_item(101, 2);
        for r in &mut g.world.roles {
            r.hp = r.max_hp;
            r.mp = r.max_mp;
        }
    }
    let refused = engine.execute_sync(&mut host, "use 101 0", false);
    assert_eq!(refused.response["ok"], false, "full-HP potion use must return ok:false: {}", refused.response);
    assert_eq!(refused.response["error_code"], "item_refused");
    let notices = refused.response["notices"].as_array().unwrap();
    assert!(
        notices.iter().any(|n| n["kind"] == "toast" && n["text"].as_str().unwrap_or("").contains("已满")),
        "refusal toast must appear in notices: {notices:?}"
    );

    // 3. #220 & #221: Rich `bag` / `status` metadata and complete menu snapshots (`RemakeSubmenu`, `EquipFbp001`, `ItemGrid`, `SaveLoadSlots`).
    let bag = engine.execute_sync(&mut host, "bag", false);
    let first_item = &bag.response["artifacts"]["bag"][0];
    assert!(first_item["desc"].is_string());
    assert!(first_item["usable_field"].is_boolean());
    assert!(first_item["target"].is_string());

    let st = engine.execute_sync(&mut host, "status", false);
    let first_magic = &st.response["artifacts"]["status"][0]["magics"][0];
    assert!(first_magic["target"].is_string());
    assert!(first_magic["affordable"].is_boolean());
    assert!(first_magic["usable_field"].is_boolean());

    let _ = engine.execute_sync(&mut host, "view equip", false);
    let eq_obs = engine.execute_sync(&mut host, "look", false);
    assert_eq!(eq_obs.response["ui_view"], "equip");
    assert_eq!(eq_obs.response["menu"]["slots"].as_array().unwrap().len(), 6);
    assert!(eq_obs.response["menu"]["slots"][0]["item_name"].is_string());
    assert!(eq_obs.response["menu"]["candidates"].is_array());

    host.runtime_mut().ui.set_view(pal_ui::UiView::RemakeSubmenu);
    let rem_obs = engine.execute_sync(&mut host, "look", false);
    assert_eq!(rem_obs.response["ui_view"], "remake_submenu");
    assert_eq!(rem_obs.response["menu"]["items"].as_array().unwrap().len(), 10);

    host.runtime_mut().ui.set_view(pal_ui::UiView::SaveLoadSlots(pal_ui::SaveSlotMode::SaveInGame));
    let slot_obs = engine.execute_sync(&mut host, "look", false);
    assert_eq!(slot_obs.response["ui_view"], "save_slots");
    assert_eq!(slot_obs.response["menu"]["items"].as_array().unwrap().len(), 5);
    assert!(slot_obs.response["menu"]["total_pages"].as_u64().unwrap() >= 1);

    let _ = engine.execute_sync(&mut host, "view items", false);
    let item_grid_obs = engine.execute_sync(&mut host, "look", false);
    assert_eq!(item_grid_obs.response["menu"]["cols"], 3);
    assert_eq!(item_grid_obs.response["menu"]["rows"], 6);
    host.runtime_mut().ui.set_view(pal_ui::UiView::None);

    // 4. #226: `close` with nothing open returns ok:false + wrong_mode; blocked walk returns ok:false + blocked.
    let noop_close = engine.execute_sync(&mut host, "close", false);
    assert_eq!(noop_close.response["ok"], false);
    assert_eq!(noop_close.response["error_code"], "wrong_mode");

    // 5. #219: Battle observation includes `heroes[]`, `statuses`, `poisons`, `spell_banner`, and `feed[]`.
    let bstart = engine.execute_sync(&mut host, "/battle 1", false);
    assert_eq!(bstart.response["mode"], "battle");
    assert!(!bstart.response["battle"]["heroes"].as_array().unwrap().is_empty());
    assert!(bstart.response["battle"]["heroes"][0]["atb_pct"].is_u64());
    assert!(bstart.response["battle"]["enemies"][0]["statuses"].is_array());
    // Submit commands for all heroes in the party so the round executes and populates `battle.feed[]`.
    let hero_count = bstart.response["battle"]["heroes"].as_array().unwrap().len();
    let mut saw_feed = false;
    for _ in 0..hero_count {
        let act = engine.execute_sync(&mut host, "attack 0", false);
        assert_eq!(act.response["ok"], true);
        if act.response["battle"].is_object()
            && !act.response["battle"]["feed"].as_array().unwrap().is_empty()
        {
            saw_feed = true;
            break;
        }
        if act.response["mode"] != "battle" {
            // Battle ended in victory (e.g. 1-shot kill) — verify victory notice was emitted instead.
            saw_feed = true;
            break;
        }
    }
    assert!(saw_feed, "completing a battle round must produce battle.feed entries");
    if pal_headless::text_mode::current_mode(host.runtime()) == "battle_victory" {
        let _ = engine.execute_sync(&mut host, "continue", false);
    } else if pal_headless::text_mode::current_mode(host.runtime()) == "battle" {
        let _ = engine.execute_sync(&mut host, "flee", false);
    }

    // 6. #224: Fair observation mode (`ObsMode::Fair`) enforces viewport visibility, hides enemy HP,
    // disables `/debug` & `dump-state`, and blocks off-screen `goto`/`talk` maze-solving cheat!
    let mut fair_engine = TextEngine::with_obs(ObsMode::Fair, 0);
    let fair_look = fair_engine.execute_sync(&mut host, "look", false);
    assert_eq!(fair_look.response["obs_mode"], "fair");
    for ev in fair_look.response["events"].as_array().unwrap() {
        assert!(ev.get("trigger_mode").is_none(), "fair mode must not leak trigger_mode");
        assert!(ev.get("state").is_none(), "fair mode must not leak internal state");
        assert!(ev["screen"].is_object(), "fair mode must only include on-screen events");
    }
    // Debug & dump-state blocked in fair mode
    let dbg_tp = fair_engine.execute_sync(&mut host, "/tp 1", false);
    assert_eq!(dbg_tp.response["ok"], false);
    assert_eq!(dbg_tp.response["error_code"], "forbidden_in_fair_mode");
    let dump_st = fair_engine.execute_sync(&mut host, "dump-state", false);
    assert_eq!(dump_st.response["ok"], false);
    assert_eq!(dump_st.response["error_code"], "forbidden_in_fair_mode");

    // Off-screen `goto` blocked in fair mode (anti-maze-cheat!)
    let offscreen_goto = fair_engine.execute_sync(&mut host, "goto 0 0", false);
    assert_eq!(offscreen_goto.response["ok"], false);
    assert_eq!(offscreen_goto.response["error_code"], "out_of_view");

    // Enter battle via debug engine, then inspect with fair engine -> enemy HP must be null!
    let _ = engine.execute_sync(&mut host, "/battle 1", false);
    let fair_battle = fair_engine.execute_sync(&mut host, "look", false);
    assert_eq!(fair_battle.response["mode"], "battle");
    assert!(
        fair_battle.response["battle"]["enemies"][0]["hp"].is_null(),
        "fair mode must hide exact enemy HP"
    );
    assert!(fair_battle.response["battle"]["enemies"][0]["alive"].is_boolean());
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn text_mode_security_fair_and_followups_193_230_233() {
    use pal_headless::text_mode::{ClientOrigin, ObsMode};

    let assets = assets_root();
    if !assets.join("data/scenes.yaml").is_file() {
        eprintln!("skip: assets not found");
        return;
    }
    let out_dir = std::env::temp_dir().join(format!(
        "repal_sec_followups_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();

    let cfg = TextModeConfig {
        assets_root: assets.clone(),
        json_mode: true,
        hd_mode: true,
        lang: pal_core::Language::ZhHans,
        save_path: Some(assets.join("orig_saves/10.rpg")),
        out_dir: out_dir.clone(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot host");
    let mut dbg_engine = TextEngine::with_obs(ObsMode::Debug, 0);
    dbg_engine.settle_sync(&mut host, 80);

    // 1. #233 Item 3: Headless populates `ui.save_slots` on boot (slot 0 falls back to classic 10.rpg).
    assert!(
        host.runtime().ui.save_slots[0].exists,
        "headless boot must populate slot 0 metadata"
    );
    assert!(
        host.runtime().ui.save_slots[0].detail.contains("10"),
        "unexpected slot 0 detail: {}",
        host.runtime().ui.save_slots[0].detail
    );

    // 2. #230: Fair mode blocks `view` (e.g. `view pawnshop`) and `core-replay`.
    let mut fair_engine = TextEngine::with_obs(ObsMode::Fair, 0);
    let view_cheat = fair_engine.execute_sync(&mut host, "view pawnshop", false);
    assert_eq!(view_cheat.response["ok"], false);
    assert_eq!(view_cheat.response["error_code"], "forbidden_in_fair_mode");

    let replay_cheat = fair_engine.execute_sync(&mut host, "core-replay foo.repal", false);
    assert_eq!(replay_cheat.response["ok"], false);
    assert_eq!(replay_cheat.response["error_code"], "forbidden_in_fair_mode");

    // 3. #233 Item 2: Fair-mode `goto`/`talk` returns identical error_code for hidden vs missing events.
    let hidden_ev_id = host
        .runtime()
        .game
        .as_ref()
        .unwrap()
        .world
        .current_event_ids()
        .into_iter()
        .find(|&id| {
            host.runtime()
                .game
                .as_ref()
                .unwrap()
                .world
                .event(id)
                .is_some_and(|e| e.state > 0 && e.vanish_time <= 0 && e.sprite_id == 0)
        });
    if let Some(hid) = hidden_ev_id {
        let r_hidden = fair_engine.execute_sync(&mut host, &format!("goto {hid}"), false);
        let r_missing = fair_engine.execute_sync(&mut host, "goto 65000", false);
        assert_eq!(r_hidden.response["ok"], false);
        assert_eq!(r_missing.response["ok"], false);
        assert_eq!(r_hidden.response["error_code"], "no_target");
        assert_eq!(r_missing.response["error_code"], "no_target");
    }

    // 4. #233 Item 1: Per-client notice cursors — boot backlog not dumped, and two TCP clients
    // each receive a new toast independently without stealing each other's notices.
    host.runtime_mut().show_toast("Boot backlog toast", 60);
    let c1 = ClientOrigin::Tcp {
        client_id: 101,
        is_loopback: true,
    };
    let c2 = ClientOrigin::Tcp {
        client_id: 102,
        is_loopback: true,
    };
    let init1 = dbg_engine.execute_sync_from(&mut host, "look", false, c1);
    let init2 = dbg_engine.execute_sync_from(&mut host, "look", false, c2);
    assert!(
        init1.response["notices"].as_array().unwrap().is_empty(),
        "new TCP client must not receive pre-connect boot backlog"
    );
    assert!(init2.response["notices"].as_array().unwrap().is_empty());

    host.runtime_mut().show_toast("Live shared toast", 60);
    let after1 = dbg_engine.execute_sync_from(&mut host, "look", false, c1);
    let after2 = dbg_engine.execute_sync_from(&mut host, "look", false, c2);
    assert!(
        after1.response["notices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["text"] == "Live shared toast"),
        "client 1 must see live toast"
    );
    assert!(
        after2.response["notices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["text"] == "Live shared toast"),
        "client 2 must also see live toast independently of client 1"
    );

    // 5. #193 / #236: Socket path confinement + safe extension allowlist + bugreport disabled + remote gate.
    for bad_cmd in [
        "save /tmp/escape.json",
        "save ../escape.json",
        "save .bashrc",
        "save .ssh/authorized_keys",
        "shot /tmp/escape.png",
        "shot evil.sh",
        "dump-state ../escape.json",
        "dump-state .profile",
        "replay-export evil.exe",
    ] {
        let r = dbg_engine.execute_sync_from(&mut host, bad_cmd, false, c1);
        assert_eq!(r.response["ok"], false, "expected {bad_cmd} to be blocked");
        assert_eq!(
            r.response["error_code"], "path_forbidden",
            "unexpected error for {bad_cmd}: {}",
            r.response
        );
    }
    let safe_save = dbg_engine.execute_sync_from(&mut host, "save ok_local.json", false, c1);
    assert_eq!(safe_save.response["ok"], true, "relative save inside out_dir must succeed");
    assert!(out_dir.join("ok_local.json").is_file());

    let bug_sock = dbg_engine.execute_sync_from(&mut host, "bugreport spam", false, c1);
    assert_eq!(bug_sock.response["ok"], false);
    assert_eq!(bug_sock.response["error_code"], "forbidden_over_socket");

    let remote = ClientOrigin::Tcp {
        client_id: 201,
        is_loopback: false,
    };
    for rem_cmd in ["/tp 1", "load ok_local.json", "view pawnshop", "dump-state"] {
        let r = dbg_engine.execute_sync_from(&mut host, rem_cmd, false, remote);
        assert_eq!(r.response["ok"], false, "remote {rem_cmd} must be blocked");
        assert_eq!(r.response["error_code"], "forbidden_remote");
    }

    // 6. #193: Anti-HTTP cross-protocol guard drops browser `POST / HTTP/1.1` on loopback TCP!
    let server = ChannelServer::start_tcp("127.0.0.1:0", true, None).expect("bind tcp");
    let addr = server.local_addr().unwrap();
    let srv_thread = std::thread::spawn(move || {
        let mut h = HeadlessTextHost::from_config(&cfg).unwrap();
        let mut eng = TextEngine::with_obs(ObsMode::Debug, 0);
        if let Ok(req) = server.rx.recv() {
            let reply = eng.execute_sync_from(&mut h, &req.raw, req.interactive_blank, req.origin);
            let _ = req.reply_tx.send(reply);
        }
    });
    let mut stream = TcpStream::connect(addr).expect("connect tcp");
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).expect("initial look");
    stream
        .write_all(b"POST / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\nsave /tmp/pwn.json\r\n")
        .unwrap();
    stream.flush().unwrap();
    let mut err_line = String::new();
    reader.read_line(&mut err_line).expect("protocol_mismatch reply");
    let v: serde_json::Value = serde_json::from_str(err_line.trim()).expect("valid json err");
    assert_eq!(v["ok"], false);
    assert_eq!(v["error_code"], "protocol_mismatch");
    let mut after_eof = String::new();
    let n = reader.read_line(&mut after_eof).unwrap_or(0);
    assert_eq!(n, 0, "socket must be closed immediately after HTTP probe");
    let _ = srv_thread.join();

    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn text_mode_debug_commands_taint_and_split_replay_199() {
    use pal_core::game::replay_rec::{DebugAction, ReplayEvent};
    use pal_headless::text_mode::ObsMode;
    use pal_runtime::repal_archive::{read_repal_file, write_repal_archive};
    use pal_runtime::replay_player::{ReplayPlayer, ReplayStepOutcome};

    let assets = assets_root();
    if !assets.join("data/scenes.yaml").is_file() {
        eprintln!("skip: assets not found");
        return;
    }
    let out_dir = std::env::temp_dir().join(format!(
        "repal_taint_split_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();

    let cfg = TextModeConfig {
        assets_root: assets.clone(),
        json_mode: true,
        hd_mode: true,
        lang: pal_core::Language::ZhHans,
        save_path: Some(assets.join("orig_saves/10.rpg")),
        out_dir: out_dir.clone(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot host");
    let mut engine = TextEngine::with_obs(ObsMode::Debug, 0);
    engine.settle_sync(&mut host, 80);

    // 1. Initially untainted & `debug_mode == false`: direct `apply_debug_action` is strictly refused (#235).
    {
        let g = host.runtime_mut().game.as_mut().unwrap();
        assert!(!g.debug_mode);
        assert!(!g.export_replay_bundle().tainted_by_debug);
        assert!(!g.export_replay_bundle().debug_mode);
        let cash_before = g.world.cash;
        assert!(g.apply_debug_action(&DebugAction::Cash(9999)).is_err());
        assert_eq!(g.world.cash, cash_before, "debug action must be a no-op when debug_mode=false");
    }
    host.runtime_mut().render_frame();
    let clean_fb = host.runtime().compositor.fb.pixels.clone();

    // 2. Run mutating debug commands -> activates `debug_mode`, renders `DEBUG ENABLED` watermark,
    //    and records deterministic `ReplayEvent::Debug` entries directly (#235).
    let _ = engine.execute_sync(&mut host, "wait 10", false);
    let base_cash = host.runtime().game.as_ref().unwrap().world.cash;
    let _ = engine.execute_sync(&mut host, "/cash 5000", false);
    let _ = engine.execute_sync(&mut host, "/item 101 3", false);
    host.runtime_mut().render_frame();
    let debug_fb = host.runtime().compositor.fb.pixels.clone();
    assert!(clean_fb != debug_fb, "DEBUG ENABLED watermark must visibly render when debug_mode=true");

    let _ = engine.execute_sync(&mut host, "/tp 2 640 480", false);
    let _ = engine.execute_sync(&mut host, "/god", false);
    let _ = engine.execute_sync(&mut host, "wait 10", false);
    let bundle_after_tp = host.runtime().game.as_ref().unwrap().export_replay_bundle();
    assert!(bundle_after_tp.tainted_by_debug, "bundle must be marked tainted_by_debug");
    assert!(bundle_after_tp.debug_mode, "bundle must carry debug_mode=true");
    assert!(
        bundle_after_tp
            .segments
            .iter()
            .flat_map(|s| s.events.iter())
            .any(|e| matches!(e.ev, ReplayEvent::Debug(DebugAction::Tp { scene_id: 2, coords: Some((640, 480)) }))),
        "expected deterministic ReplayEvent::Debug(Tp) in recorded events"
    );

    // 3. Roundtrip `.repal` archive & replay deterministically through unified ReplayPlayer (#235).
    let repal_path = out_dir.join("debug_det.repal");
    write_repal_archive(&repal_path, &bundle_after_tp).expect("write debug .repal");
    let reloaded = read_repal_file(&repal_path).expect("read debug .repal");
    assert!(reloaded.tainted_by_debug && reloaded.debug_mode);

    let step_replay = |bundle: pal_core::game::replay_rec::ReplayBundle| -> pal_runtime::PalRuntime {
        let mut rt = pal_runtime::disk::boot_runtime_from_dir(&assets, false).expect("boot rt");
        let mut player = ReplayPlayer::new(bundle).expect("new ReplayPlayer");
        assert!(player.load_segment(0, &mut rt));
        let drain_host = |rt: &mut pal_runtime::PalRuntime| loop {
            rt.route_cmds();
            let Some(cmd) = rt.out_cmds.pop_front() else { break };
            match cmd {
                pal_core::game::HostCmd::LoadScene { map_id, .. } => {
                    if let Some(g) = rt.game.as_mut() {
                        pal_runtime::disk::load_scene_assets_from_dir(&assets, g, map_id);
                    }
                    rt.reply(pal_core::game::HostReply::SceneLoaded);
                }
                pal_core::game::HostCmd::LoadChapter(_) | pal_core::game::HostCmd::PlayRng { .. } => {
                    rt.reply(pal_core::game::HostReply::Done);
                }
                _ => {}
            }
        };
        drain_host(&mut rt);
        loop {
            let outcome = player.step_one_tick(&mut rt, |_, _| {}, |rt, _| drain_host(rt));
            drain_host(&mut rt);
            match outcome {
                ReplayStepOutcome::Stepped => {}
                ReplayStepOutcome::SegmentCompleted { next_idx } => {
                    assert!(player.load_segment(next_idx, &mut rt));
                    drain_host(&mut rt);
                }
                ReplayStepOutcome::Finished => break,
            }
        }
        rt
    };

    // 3a. With `debug_mode == true`, ReplayPlayer reproduces exact mutated state + watermark.
    let rt_dbg = step_replay(reloaded.clone());
    assert!(rt_dbg.ui.debug_mode, "replaying a debug_mode bundle must render DEBUG ENABLED watermark");
    {
        let g = rt_dbg.game.as_ref().unwrap();
        assert_eq!(g.world.scene_id, 2);
        assert_eq!(g.world.cash, base_cash + 5000);
        assert_eq!((g.world.leader().x, g.world.leader().y), (640, 480));
        assert!(g.god_mode);
    }

    // 3b. Untrusted bundle security gate: if `debug_mode == false`, embedded `ReplayEvent::Debug`
    //     events MUST be treated as strict no-ops!
    let mut forged = reloaded.clone();
    forged.debug_mode = false;
    forged.tainted_by_debug = false;
    let rt_safe = step_replay(forged);
    assert!(!rt_safe.ui.debug_mode);
    {
        let g = rt_safe.game.as_ref().unwrap();
        assert_eq!(g.world.cash, base_cash, "forged Debug events must be ignored when debug_mode=false");
        assert!(!g.god_mode);
    }

    // 4. `/god` persists across `load` while ON and keeps `tainted_by_debug` set (#236).
    let _ = engine.execute_sync(&mut host, "save god_check.json", false);
    let _ = engine.execute_sync(&mut host, "load god_check.json", false);
    assert!(
        host.runtime().game.as_ref().unwrap().god_mode,
        "god_mode must remain active across `load` while /god is ON (#235/#236)"
    );
    assert!(
        host.runtime().game.as_ref().unwrap().recorder.tainted_by_debug,
        "tainted_by_debug must remain set across `load` while /god is ON (#236)"
    );
    let _ = engine.execute_sync(&mut host, "/god", false);
    assert!(!host.runtime().game.as_ref().unwrap().god_mode);

    // 5. #236: Mutating debug commands must be refused with `busy` while a battle is active.
    let _ = engine.execute_sync(&mut host, "/battle 1", false);
    assert!(
        host.runtime()
            .game
            .as_ref()
            .unwrap()
            .battle
            .as_ref()
            .is_some_and(|b| b.active),
        "expected battle to be active for #236 busy-guard check"
    );
    for busy_cmd in ["/heal", "/cash 100", "/item 101 1", "/god"] {
        let r = engine.execute_sync(&mut host, busy_cmd, false);
        assert_eq!(r.response["ok"], false, "{busy_cmd} must be refused mid-battle");
        assert_eq!(r.response["error_code"], "busy", "unexpected code for {busy_cmd}: {}", r.response);
    }

    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn text_mode_issue_234_use_effects_field_magic_bbox_minimap_and_fair_zero_leaks() {
    use pal_headless::text_mode::{MinimapMode, ObsMode};

    let assets = assets_root();
    if !assets.join("data/scenes.yaml").is_file() {
        eprintln!("skip: assets not found");
        return;
    }

    // 0. Verify `docs/text_mode.schema.json` parses cleanly and includes all new #234/#228 fields.
    let schema_text =
        std::fs::read_to_string(repo_root().join("docs/text_mode.schema.json")).expect("read schema");
    let schema: serde_json::Value = serde_json::from_str(&schema_text).expect("valid schema json");
    let req_keys: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    for k in ["walkable_dist", "minimap", "events", "interrupted_by"] {
        assert!(req_keys.contains(&k), "schema missing required key {k}");
    }
    let kind_enum: Vec<&str> = schema["properties"]["events"]["items"]["properties"]["kind"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(kind_enum.contains(&"exit"));
    let intr_enum: Vec<&str> = schema["properties"]["interrupted_by"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(intr_enum.contains(&"human"));

    let out_dir = std::env::temp_dir().join(format!("repal_issue_234_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();

    let cfg = TextModeConfig {
        assets_root: assets.clone(),
        json_mode: true,
        hd_mode: true,
        lang: pal_core::Language::ZhHans,
        new_game: true,
        out_dir: out_dir.clone(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot host");
    let mut dbg_engine = TextEngine::with_obs(ObsMode::Debug, 0);
    dbg_engine.settle_sync(&mut host, 80);

    // 1. Item 1 (#234): `use 279` (破天锤) on non-first Asura statue #240 in Scene #17:
    // VM keeps SDLPAL-faithful sticky `last_item_result == Some(false)`, while Text Mode observes
    // the smashed statue (`effects: ["event_changed"]`) and returns `ok: true`!
    let _ = dbg_engine.execute_sync(&mut host, "/tp 17", false);
    let (s240_x, s240_y) = {
        let g = host.runtime_mut().game.as_mut().unwrap();
        g.world.add_item(279, 1);
        let ev = g.world.event(240).expect("Asura statue #240 in scene 17");
        assert!(ev.state > 0);
        (ev.x, ev.y)
    };
    // Place party 1 step SE of statue #240 facing NW toward it.
    {
        let g = host.runtime_mut().game.as_mut().unwrap();
        for m in &mut g.world.party {
            m.x = s240_x + 16;
            m.y = s240_y + 8;
            m.dir = 1; // NW
        }
    }
    let hammer_hit = dbg_engine.execute_sync(&mut host, "use 279", false);
    assert_eq!(
        hammer_hit.response["ok"], true,
        "破天锤 smashing statue #240 must return ok:true: {}",
        hammer_hit.response
    );
    assert_eq!(hammer_hit.response["artifacts"]["use"]["script_success"], false);
    let effects: Vec<&str> = hammer_hit.response["artifacts"]["use"]["effects"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        effects.contains(&"event_changed"),
        "expected event_changed in effects: {effects:?}"
    );
    assert_eq!(
        host.runtime().game.as_ref().unwrap().world.item_count(279),
        1,
        "non-consumable 破天锤 must not be consumed"
    );
    // Swinging 破天锤 in empty space jumps to `s_9789` and displays the "无任何效果" dialog.
    let hammer_miss = dbg_engine.execute_sync(&mut host, "use 279", false);
    assert_eq!(hammer_miss.response["mode"], "dialog");
    assert!(
        hammer_miss.response["prompt"]["text"]
            .as_str()
            .unwrap_or("")
            .contains("无任何效果"),
        "expected '无任何效果' dialog on empty-space hammer swing: {}",
        hammer_miss.response
    );
    let _ = dbg_engine.execute_sync(&mut host, "next", false);

    // 2. Item 2 (#234): `magic <id> [party_idx]` works in `explore` mode (气疗术 #296).
    {
        let g = host.runtime_mut().game.as_mut().unwrap();
        let r0 = &mut g.world.roles[0];
        if !r0.magics.contains(&296) {
            r0.magics.push(296);
        }
        r0.hp = (r0.max_hp / 2).max(1);
        r0.mp = r0.max_mp.max(50);
    }
    let hp_before = host.runtime().game.as_ref().unwrap().world.roles[0].hp;
    let mp_before = host.runtime().game.as_ref().unwrap().world.roles[0].mp;
    let cast_ok = dbg_engine.execute_sync(&mut host, "magic 296 0", false);
    assert_eq!(
        cast_ok.response["ok"], true,
        "explore-mode magic 296 0 must succeed: {}",
        cast_ok.response
    );
    assert_eq!(cast_ok.response["artifacts"]["field_magic"]["magic_id"], 296);
    assert!(host.runtime().game.as_ref().unwrap().world.roles[0].hp > hp_before);
    assert!(host.runtime().game.as_ref().unwrap().world.roles[0].mp < mp_before);
    // Unknown magic returns `magic_not_known`.
    let cast_unknown = dbg_engine.execute_sync(&mut host, "magic 999 0", false);
    assert_eq!(cast_unknown.response["ok"], false);
    assert_eq!(cast_unknown.response["error_code"], "magic_not_known");

    // 3. Item 3 (#234): Real MGO frame `bbox ∩ viewport`:
    // Tall statue #238 (mgo_066, h=88) with foot anchor at sy=230 is visible (`dy = 230-82 = 148 < 200`);
    // Short chest #264 (mgo_010, h=25) with foot anchor at sy=230 is NOT visible (`dy = 230-19 = 211 >= 200`)!
    let mut fair_engine = TextEngine::with_obs(ObsMode::Fair, 0);
    {
        let (cam_x, cam_y) = pal_headless::text_mode::runtime_camera(host.runtime());
        let g = host.runtime_mut().game.as_mut().unwrap();
        if let Some(st) = g.world.event_mut(238) {
            st.state = 2;
            st.vanish_time = 0;
            st.x = cam_x + 120;
            st.y = cam_y + 230;
        }
        if let Some(ch) = g.world.event_mut(264) {
            ch.state = 2;
            ch.vanish_time = 0;
            ch.x = cam_x + 180;
            ch.y = cam_y + 230;
        }
    }
    let fair_bbox = fair_engine.execute_sync(&mut host, "look", false);
    let ev_ids: Vec<u64> = fair_bbox.response["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["id"].as_u64())
        .collect();
    assert!(
        ev_ids.contains(&238),
        "tall 88px statue at sy=230 must be visible via real MGO bbox: {ev_ids:?}"
    );
    assert!(
        !ev_ids.contains(&264),
        "short 25px chest at sy=230 must NOT leak into fair events[]: {ev_ids:?}"
    );

    // 4. Item 4 & 5 (#234): Staggered 20x25 minimap format, `--minimap auto|always|off`, `map`,
    // `walkable_dist`, zero `interactable` leak in fair mode, occluded `?`, and solid hidden trigger zero-leak!
    assert_unified_envelope(&fair_bbox.response);
    let mm = fair_bbox.response["minimap"]
        .as_object()
        .expect("look must include minimap in Auto mode");
    assert_eq!(mm["cols"], 20);
    assert_eq!(mm["rows"], 25);
    assert_eq!(mm["staggered"], true);
    let ox = mm["origin"]["x"].as_i64().unwrap();
    let oy = mm["origin"]["y"].as_i64().unwrap();
    let grid = mm["grid"].as_array().unwrap();
    assert_eq!(grid.len(), 25);
    for (r, row_v) in grid.iter().enumerate() {
        let row = row_v.as_str().unwrap();
        assert_eq!(row.len(), 20, "row {r} must be exact length 20");
        let wy = oy + (r as i64) * 8;
        for (c, ch) in row.chars().enumerate() {
            let wx = ox + (c as i64) * 16;
            let even = ((wx.div_euclid(16) + wy.div_euclid(8)).rem_euclid(2)) == 0;
            if !even {
                assert_eq!(ch, ' ', "odd parity cell ({r},{c}) must be spacer");
            }
        }
    }
    // Fair mode must NEVER include `interactable`.
    for ev in fair_bbox.response["events"].as_array().unwrap() {
        assert!(
            ev.get("interactable").is_none(),
            "fair mode must omit interactable flag: {ev}"
        );
    }

    // MinimapMode::Off omits minimap on `look`, but explicit `map` still returns it!
    fair_engine.minimap_mode = MinimapMode::Off;
    let look_off = fair_engine.execute_sync(&mut host, "look", false);
    assert!(look_off.response["minimap"].is_null());
    let explicit_map = fair_engine.execute_sync(&mut host, "map", false);
    assert!(explicit_map.response["minimap"].is_object());
    fair_engine.minimap_mode = MinimapMode::Auto;

    // Zero-leak fairness test: inject an invisible solid trigger (`sprite_id == 0, state == 2`)
    // on a walkable tile right inside the viewport -> must NOT appear in `events[]` and must NOT
    // render as `#` on the fair minimap (renders as underlying `.`)!
    let _ = dbg_engine.execute_sync(&mut host, "/tp 1 1312 1008", false);
    let probe_xy = {
        let g = host.runtime_mut().game.as_mut().unwrap();
        // Find a nearby walkable unoccluded half-tile inside the viewport and place a solid sprite=0 event there.
        let lead = g.world.leader().clone();
        let (hx, hy, hh) = pal_core::isometric::world_to_half_tile(lead.x, lead.y);
        let mut chosen = None;
        for dir in 0..4u8 {
            let (nx, ny, nh) = pal_core::isometric::step_half_tile(hx, hy, hh, dir);
            if !g.base_grid().is_blocked(nx, ny, nh) && !g.base_grid().is_occluded(nx, ny, nh) {
                chosen = Some(pal_core::isometric::half_tile_to_world(nx, ny, nh));
                break;
            }
        }
        let (px, py) = chosen.expect("walkable neighbor in scene 1");
        if let Some(id) = g.world.current_event_ids().into_iter().next() {
            if let Some(ev) = g.world.event_mut(id) {
                ev.sprite_id = 0;
                ev.state = 2;
                ev.trigger_mode = 4;
                ev.trigger = pal_core::script::ScriptPos::NONE;
                ev.x = px;
                ev.y = py;
            }
            g.sync_scene_obstacles();
        }
        (px, py)
    };
    let fair_sc1 = fair_engine.execute_sync(&mut host, "map", false);
    let mm1 = fair_sc1.response["minimap"].as_object().unwrap();
    let ox1 = mm1["origin"]["x"].as_i64().unwrap() as i32;
    let oy1 = mm1["origin"]["y"].as_i64().unwrap() as i32;
    let c = ((probe_xy.0 - ox1) / 16) as usize;
    let r = ((probe_xy.1 - oy1) / 8) as usize;
    let ch = mm1["grid"][r].as_str().unwrap().as_bytes()[c] as char;
    assert_eq!(
        ch, '.',
        "solid invisible sprite_id=0 trigger must NOT leak as '#' on fair minimap (got '{ch}')"
    );

    // 5. GOP-visual void & partial-lintel occlusion fix (#234 follow-up):
    // - Scene #3 (Yuhang Inn lobby): black exterior void (1680,1504) renders as ' ' (not '.'),
    //   and Inn main exit ev#46 (1472,1520, top_layer=4 partial lintel) is visible as '>' & kind="exit"!
    let _ = dbg_engine.execute_sync(&mut host, "/tp 3 1568 1504", false);
    let fair_sc3 = fair_engine.execute_sync(&mut host, "look", false);
    let mm3 = fair_sc3.response["minimap"].as_object().unwrap();
    let ox3 = mm3["origin"]["x"].as_i64().unwrap() as i32;
    let oy3 = mm3["origin"]["y"].as_i64().unwrap() as i32;
    let void_c = ((1680 - ox3) / 16) as usize;
    let void_r = ((1504 - oy3) / 8) as usize;
    let void_ch = mm3["grid"][void_r].as_str().unwrap().as_bytes()[void_c] as char;
    assert_eq!(
        void_ch, ' ',
        "Scene #3 black exterior void (1680,1504) must render as ' ' on minimap (got '{void_ch}')"
    );
    let sc3_exit_visible = fair_sc3.response["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["id"] == 46 && e["kind"] == "exit");
    assert!(
        sc3_exit_visible,
        "Scene #3 main exit ev#46 under partial lintel must be visible as exit in fair mode: {}",
        fair_sc3.response["events"]
    );
    let ex3_c = ((1472 - ox3) / 16) as usize;
    let ex3_r = ((1520 - oy3) / 8) as usize;
    let ex3_ch = mm3["grid"][ex3_r].as_str().unwrap().as_bytes()[ex3_c] as char;
    assert_eq!(
        ex3_ch, '>',
        "Scene #3 main exit ev#46 must render as '>' on minimap (got '{ex3_ch}')"
    );

    // - Scene #4 (Shengyu Village): outdoor GOP tile #0 is real grass (NOT void!),
    //   and Yuhang Inn front door ev#95 (1296,872, top_layer=7 partial lintel) is visible as exit!
    let _ = dbg_engine.execute_sync(&mut host, "/tp 4 1248 896", false);
    let fair_sc4 = fair_engine.execute_sync(&mut host, "look", false);
    let sc4_exit_visible = fair_sc4.response["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["id"] == 95 && e["kind"] == "exit");
    assert!(
        sc4_exit_visible,
        "Scene #4 Inn front door ev#95 under partial lintel must be visible as exit: {}",
        fair_sc4.response["events"]
    );
    let mm4 = fair_sc4.response["minimap"].as_object().unwrap();
    let sc4_walk_dots: usize = mm4["grid"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row.as_str().unwrap().bytes().filter(|&b| b == b'.').count())
        .sum();
    assert!(
        sc4_walk_dots > 30,
        "Scene #4 outdoor walkable grass must remain '.' (got only {sc4_walk_dots} dots)"
    );

    // 6. `replay-export` writes a valid `.repal` archive in `--obs fair` with current engine version.
    let exp_res = fair_engine.execute_sync(&mut host, "replay-export exported_test.repal", false);
    assert_eq!(
        exp_res.response["ok"], true,
        "replay-export must succeed in fair mode: {}",
        exp_res.response
    );
    let exp_path = out_dir.join("exported_test.repal");
    assert!(exp_path.is_file(), "exported .repal file must exist");
    let exp_bundle = pal_runtime::repal_archive::read_repal_file(&exp_path)
        .expect("exported .repal must parse cleanly");
    assert_eq!(exp_bundle.engine_version, pal_core::game::replay_rec::ENGINE_VERSION);
    assert!(!exp_bundle.segments.is_empty());

    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn text_mode_ai_think_overlay_rate_limit_length_limit_and_repal_roundtrip() {
    use pal_core::game::replay_rec::{MAX_AI_THOUGHTS_PER_SEGMENT, MAX_AI_THOUGHT_CHARS};
    use pal_headless::text_mode::ObsMode;
    use pal_runtime::repal_archive::{export_repal_to_srt, read_repal_file};

    let assets = assets_root();
    if !assets.join("data/scenes.yaml").is_file() {
        eprintln!("skip: assets not found");
        return;
    }
    let out_dir = std::env::temp_dir().join(format!("repal_think_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();

    let cfg = TextModeConfig {
        assets_root: assets.clone(),
        json_mode: true,
        hd_mode: true,
        lang: pal_core::Language::ZhHans,
        save_path: Some(assets.join("orig_saves/10.rpg")),
        out_dir: out_dir.clone(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot host");
    let mut engine = TextEngine::with_obs(ObsMode::Fair, 0);
    engine.settle_sync(&mut host, 80);

    let events_before = host.runtime().game.as_ref().unwrap().recorder.total_events;

    // 1. Works in `--obs fair`, strips emoji + bidi controls & collapses newlines, preserves apostrophes.
    let r1 = engine.execute_sync(
        &mut host,
        "think 💭 \u{202E}先去客栈找Ling'er\n和醉道士！\u{2066}🚀",
        false,
    );
    assert_eq!(r1.response["ok"], true, "think must succeed in fair mode: {}", r1.response);
    assert_eq!(
        r1.response["artifacts"]["think"]["text"],
        "先去客栈找Ling'er 和醉道士！"
    );
    assert_eq!(r1.response["artifacts"]["think"]["truncated"], false);
    assert_eq!(r1.response["artifacts"]["think"]["shown"], true);
    assert_eq!(r1.response["artifacts"]["think"]["coalesced"], false);
    assert_eq!(r1.response["artifacts"]["think"]["recorded"], true);
    assert_eq!(
        host.runtime().ui.ai_thought.as_deref(),
        Some("先去客栈找Ling'er 和醉道士！")
    );
    // Cosmetic thoughts must NOT inflate `total_events`, enter `events`, or taint replay!
    assert_eq!(
        host.runtime().game.as_ref().unwrap().recorder.total_events,
        events_before
    );
    assert!(!host.runtime().game.as_ref().unwrap().recorder.tainted_by_debug);
    let first_ann_tick = host
        .runtime()
        .game
        .as_ref()
        .unwrap()
        .recorder
        .segments
        .last()
        .unwrap()
        .annotations[0]
        .tick;

    // 2. Second think within 50 ticks ALWAYS succeeds (`ok: true`), coalesces in-place, and
    //    preserves the original start tick!
    let _ = engine.execute_sync(&mut host, "wait 10", false);
    let r_coal = engine.execute_sync(&mut host, "think 还是先去买几株止血草", false);
    assert_eq!(r_coal.response["ok"], true, "coalesced think must never fail batches");
    assert_eq!(r_coal.response["artifacts"]["think"]["coalesced"], true);
    assert_eq!(r_coal.response["artifacts"]["think"]["recorded"], true);
    assert_eq!(r_coal.response["artifacts"]["think"]["shown"], true);
    {
        let anns = &host
            .runtime()
            .game
            .as_ref()
            .unwrap()
            .recorder
            .segments
            .last()
            .unwrap()
            .annotations;
        assert_eq!(anns.len(), 1, "within-50-tick think must coalesce in-place");
        assert_eq!(anns[0].tick, first_ann_tick, "coalescing must keep original start tick");
        assert_eq!(anns[0].text, "还是先去买几株止血草");
    }

    // 3. Duplicate think also returns `ok: true` with `coalesced: true, recorded: false`.
    let r_dup = engine.execute_sync(&mut host, "think 还是先去买几株止血草", false);
    assert_eq!(r_dup.response["ok"], true);
    assert_eq!(r_dup.response["artifacts"]["think"]["coalesced"], true);
    assert_eq!(r_dup.response["artifacts"]["think"]["recorded"], false);

    // 4. Friendly parse-error hint when the command immediately after `think` fails to parse
    //    (e.g. unquoted `;` inside a thought split by batch tokenizer).
    let r_hint = engine.execute_sync(&mut host, "没有这个命令", false);
    assert_eq!(r_hint.response["ok"], false);
    assert_eq!(r_hint.response["error_code"], "parse_error");
    assert!(
        r_hint.response["error"]
            .as_str()
            .unwrap_or("")
            .contains("wrap the thought in quotes"),
        "expected helpful quote hint, got: {}",
        r_hint.response["error"]
    );

    // 5. Structural capture exclusion: default `shot` is thought-free, while `shot --with-thought`
    //    renders the HD top subtitle bar!
    let clean_shot_path = out_dir.join("clean.png");
    let thought_shot_path = out_dir.join("with_thought.png");
    let r_s1 = engine.execute_sync(&mut host, "shot clean.png", false);
    let r_s2 = engine.execute_sync(&mut host, "shot --with-thought with_thought.png", false);
    assert_eq!(r_s1.response["ok"], true);
    assert_eq!(r_s2.response["ok"], true);
    assert_eq!(r_s1.response["artifacts"]["shot"]["with_thought"], false);
    assert_eq!(r_s2.response["artifacts"]["shot"]["with_thought"], true);
    let clean_bytes = std::fs::read(&clean_shot_path).expect("read clean shot");
    let thought_bytes = std::fs::read(&thought_shot_path).expect("read thought shot");
    assert_ne!(
        clean_bytes, thought_bytes,
        "shot --with-thought must include the HD subtitle bar while default shot excludes it"
    );

    // 6. Emoji-only think rejected as invalid_args.
    let r_emoji = engine.execute_sync(&mut host, "think 💭🚀🤖", false);
    assert_eq!(r_emoji.response["ok"], false);
    assert_eq!(r_emoji.response["error_code"], "invalid_args");

    // 7. After advancing 50 ticks, a >200-char think appends a new annotation and truncates to 200 chars with `…`.
    let _ = engine.execute_sync(&mut host, "wait 50", false);
    let long_msg = format!("think {}", "测".repeat(240));
    let r_long = engine.execute_sync(&mut host, &long_msg, false);
    assert_eq!(r_long.response["ok"], true, "post-cooldown think must succeed: {}", r_long.response);
    assert_eq!(r_long.response["artifacts"]["think"]["coalesced"], false);
    assert_eq!(r_long.response["artifacts"]["think"]["truncated"], true);
    let stored = r_long.response["artifacts"]["think"]["text"].as_str().unwrap();
    assert_eq!(stored.chars().count(), MAX_AI_THOUGHT_CHARS);
    assert!(stored.ends_with('…'));

    // 8. `think clear` always succeeds immediately and appends an empty clear annotation.
    let _ = engine.execute_sync(&mut host, "wait 10", false);
    let r_clear = engine.execute_sync(&mut host, "think clear", false);
    assert_eq!(r_clear.response["ok"], true);
    assert!(host.runtime().ui.ai_thought.is_none());

    // 9. Export `.repal` and verify separate `annotations.json` round-trip, `tainted_by_debug: false`,
    //    and `.srt` subtitle export.
    let exp = engine.execute_sync(&mut host, "replay-export think_test.repal", false);
    assert_eq!(exp.response["ok"], true);
    let bundle = read_repal_file(&out_dir.join("think_test.repal")).expect("read think .repal");
    assert!(!bundle.tainted_by_debug);
    let anns: Vec<&pal_core::game::replay_rec::ReplayAnnotation> = bundle
        .segments
        .iter()
        .flat_map(|s| s.annotations.iter())
        .collect();
    assert_eq!(anns.len(), 3, "expected 2 thoughts + 1 clear: {anns:?}");
    assert_eq!(anns[0].tick, first_ann_tick);
    assert_eq!(anns[0].text, "还是先去买几株止血草");
    assert!(anns[1].text.ends_with('…'));
    assert_eq!(anns[2].text, "");

    let srt = export_repal_to_srt(&bundle);
    assert!(srt.contains("还是先去买几株止血草"), "srt missing cue 1: {srt}");
    assert!(srt.contains(" --> "), "srt missing timestamps: {srt}");
    assert_eq!(srt.matches(" --> ").count(), 2, "clear marker must not emit a blank cue: {srt}");

    // 10. Ring cap of 200 annotations/segment drops oldest without failing.
    {
        let rec = &mut host.runtime_mut().game.as_mut().unwrap().recorder;
        let base_t = 10_000u64;
        for i in 0..(MAX_AI_THOUGHTS_PER_SEGMENT + 5) {
            let res = rec.push_annotation(base_t + (i as u64) * 60, format!("note #{i}"));
            assert!(res.recorded && !res.coalesced);
        }
        let seg_anns = &rec.segments.last().unwrap().annotations;
        assert_eq!(seg_anns.len(), MAX_AI_THOUGHTS_PER_SEGMENT);
        assert_eq!(seg_anns.last().unwrap().text, format!("note #{}", MAX_AI_THOUGHTS_PER_SEGMENT + 4));
    }

    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn text_mode_goto_and_talk_record_standard_player_click_events() {
    use pal_headless::text_mode::ObsMode;
    use pal_runtime::repal_archive::read_repal_file;
    use pal_runtime::replay_player::{ReplayPlayer, ReplayStepOutcome};

    let assets = assets_root();
    if !assets.join("data/scenes.yaml").is_file() {
        eprintln!("skip: assets not found");
        return;
    }
    let out_dir = std::env::temp_dir().join(format!(
        "repal_goto_det_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();

    let cfg = TextModeConfig {
        assets_root: assets.clone(),
        json_mode: true,
        hd_mode: true,
        lang: pal_core::Language::ZhHans,
        save_path: Some(assets.join("orig_saves/10.rpg")),
        out_dir: out_dir.clone(),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot host");
    let mut engine = TextEngine::with_obs(ObsMode::Fair, 0);
    engine.settle_sync(&mut host, 80);

    // Move using `goto` (which dispatches standard player `TickInput::click`) and export `.repal`.
    let start_pos = {
        let lead = host.runtime().game.as_ref().unwrap().world.leader();
        (lead.x, lead.y)
    };
    let r_goto = engine.execute_sync(
        &mut host,
        &format!("goto {} {}", start_pos.0 - 64, start_pos.1 + 32),
        false,
    );
    assert_eq!(r_goto.response["ok"], true, "goto failed: {}", r_goto.response);
    let r_wait = engine.execute_sync(&mut host, "wait 10", false);
    assert_eq!(r_wait.response["ok"], true);
    let (exp_sc, exp_x, exp_y, exp_dir, exp_rng) = {
        let g = host.runtime().game.as_ref().unwrap();
        let l = g.world.leader();
        (g.world.scene_id, l.x, l.y, l.dir, g.world.rng.state)
    };
    assert_ne!((exp_x, exp_y), start_pos, "goto must have moved party");

    let exp = engine.execute_sync(&mut host, "replay-export goto_det.repal", false);
    assert_eq!(exp.response["ok"], true);
    let bundle = read_repal_file(&out_dir.join("goto_det.repal")).expect("read .repal");
    assert!(
        bundle.segments.iter().any(|s| s
            .events
            .iter()
            .any(|e| matches!(e.ev, pal_core::game::replay_rec::ReplayEvent::Click(_, _)))),
        "expected standard player Click event recorded for goto"
    );

    // Replay into a fresh runtime (still in Title view before load_segment) and verify exact parity
    // plus clean UiView::None reset.
    let mut rt = pal_runtime::disk::boot_runtime_from_dir(&assets, false).expect("boot rt");
    assert_eq!(rt.ui.view, pal_ui::UiView::Title);
    let mut player = ReplayPlayer::new(bundle).expect("new ReplayPlayer");
    assert!(player.load_segment(0, &mut rt));
    assert_eq!(rt.ui.view, pal_ui::UiView::None, "load_segment must clear Title overlay");
    loop {
        rt.route_cmds();
        let Some(cmd) = rt.out_cmds.pop_front() else { break };
        if let pal_core::game::HostCmd::LoadScene { map_id, .. } = cmd {
            if let Some(g) = rt.game.as_mut() {
                pal_runtime::disk::load_scene_assets_from_dir(&assets, g, map_id);
            }
            rt.reply(pal_core::game::HostReply::SceneLoaded);
        }
    }
    loop {
        let outcome = player.step_one_tick(&mut rt, |_, _| {}, |rt, _| {
            loop {
                rt.route_cmds();
                let Some(cmd) = rt.out_cmds.pop_front() else { break };
                match cmd {
                    pal_core::game::HostCmd::LoadScene { map_id, .. } => {
                        if let Some(g) = rt.game.as_mut() {
                            pal_runtime::disk::load_scene_assets_from_dir(&assets, g, map_id);
                        }
                        rt.reply(pal_core::game::HostReply::SceneLoaded);
                    }
                    pal_core::game::HostCmd::LoadChapter(_) | pal_core::game::HostCmd::PlayRng { .. } => {
                        rt.reply(pal_core::game::HostReply::Done);
                    }
                    _ => {}
                }
            }
        });
        loop {
            rt.route_cmds();
            let Some(cmd) = rt.out_cmds.pop_front() else { break };
            match cmd {
                pal_core::game::HostCmd::LoadScene { map_id, .. } => {
                    if let Some(g) = rt.game.as_mut() {
                        pal_runtime::disk::load_scene_assets_from_dir(&assets, g, map_id);
                    }
                    rt.reply(pal_core::game::HostReply::SceneLoaded);
                }
                pal_core::game::HostCmd::LoadChapter(_) | pal_core::game::HostCmd::PlayRng { .. } => {
                    rt.reply(pal_core::game::HostReply::Done);
                }
                _ => {}
            }
        }
        match outcome {
            ReplayStepOutcome::Stepped => {}
            ReplayStepOutcome::SegmentCompleted { next_idx } => {
                assert!(player.load_segment(next_idx, &mut rt));
            }
            ReplayStepOutcome::Finished => break,
        }
    }
    let g = rt.game.as_ref().unwrap();
    let l = g.world.leader();
    assert_eq!((g.world.scene_id, l.x, l.y, l.dir, g.world.rng.state), (exp_sc, exp_x, exp_y, exp_dir, exp_rng));
    let _ = std::fs::remove_dir_all(&out_dir);
}

/// #271 item 2: `goto <npc>` offsets the click target outside the NPC's 32px interactive radius so
/// it does not auto-talk, and must return `ok: true, reached: true, adjacent: true` instead of a
/// false `unreachable` error.
#[test]
fn text_mode_goto_npc_returns_ok_and_adjacent_without_talking_271() {
    let cfg = TextModeConfig {
        assets_root: assets_root(),
        save_path: Some(PathBuf::from("10.rpg")),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 10.rpg");
    let mut engine = TextEngine::new(false, 0);
    engine.settle_sync(&mut host, 60);

    // Find any visible solid NPC in the current scene (`kind == "npc"`).
    let look = engine.execute_sync(&mut host, "look", false);
    assert_eq!(look.response["ok"], true);
    let npc_id = look.response["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "npc")
        .and_then(|e| e["id"].as_u64())
        .expect("10.rpg scene should have at least one visible NPC");

    let res = engine.execute_sync(&mut host, &format!("goto {npc_id}"), false);
    assert_unified_envelope(&res.response);
    assert_eq!(
        res.response["ok"], true,
        "goto <npc #{npc_id}> must succeed when arriving adjacent instead of false unreachable: {}",
        res.response
    );
    assert_eq!(res.response["mode"], "explore", "goto <npc> must not trigger dialogue");
    assert_eq!(res.response["artifacts"]["goto"]["reached"], true);
    assert_eq!(res.response["artifacts"]["goto"]["adjacent"], true);
}

/// #276: battle `use` auto-routing, spell/item name lookup, plain-text effect descriptions in
/// `bag`/`status`, and interactive localized fair-aware `help [cmd|topic]`.
#[test]
fn text_mode_issue_276_battle_use_name_lookup_effects_and_help() {
    use pal_headless::text_mode::ObsMode;

    let assets = assets_root();
    if !assets.join("data/scenes.yaml").is_file() {
        eprintln!("skip: assets not found");
        return;
    }

    let cfg = TextModeConfig {
        assets_root: assets,
        json_mode: true,
        lang: pal_core::Language::ZhHans,
        save_path: Some(PathBuf::from("10.rpg")),
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot 10.rpg");
    let mut dbg_engine = TextEngine::with_obs(ObsMode::Debug, 0);
    let mut fair_engine = TextEngine::with_obs(ObsMode::Fair, 0);
    dbg_engine.settle_sync(&mut host, 60);

    // 1. Interactive `help [cmd|topic]`: fair mode hides debug commands; topic lookup works.
    let help_dbg = dbg_engine.execute_sync(&mut host, "help", false);
    assert_eq!(help_dbg.response["ok"], true);
    let dbg_topics: Vec<&str> = help_dbg.response["artifacts"]["help"]["topics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(dbg_topics.contains(&"debug"));
    assert!(dbg_topics.contains(&"battle"));

    let help_fair = fair_engine.execute_sync(&mut host, "help", false);
    assert_eq!(help_fair.response["ok"], true);
    let fair_topics: Vec<&str> = help_fair.response["artifacts"]["help"]["topics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        !fair_topics.contains(&"debug"),
        "fair-mode help overview must hide debug commands: {fair_topics:?}"
    );
    let help_fair_log = help_fair.response["log"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!help_fair_log.contains("/tp"), "fair help must not advertise /tp");

    let help_use = fair_engine.execute_sync(&mut host, "help use", false);
    assert_eq!(help_use.response["ok"], true);
    assert_eq!(help_use.response["artifacts"]["help"]["topic"], "items");

    let help_debug_fair = fair_engine.execute_sync(&mut host, "help debug", false);
    assert_eq!(help_debug_fair.response["ok"], true);
    assert_eq!(help_debug_fair.response["artifacts"]["help"]["fair_disabled"], true);

    let help_bad = fair_engine.execute_sync(&mut host, "help nonexistent_xyz", false);
    assert_eq!(help_bad.response["ok"], false);
    assert_eq!(help_bad.response["error_code"], "invalid_args");

    // 2. Plain-text `status` and `bag` include #ID, tags, and effect descriptions.
    {
        let g = host.runtime_mut().game.as_mut().unwrap();
        g.world.add_item(61, 2); // 观音符 (usable in battle + field, HP+150)
        g.world.add_item(99, 2); // 止血草
        if !g.world.roles[0].magics.contains(&296) {
            g.world.roles[0].magics.push(296); // 气疗术
        }
        g.world.roles[0].hp = 50;
        g.world.roles[0].mp = 200;
    }
    let st_res = fair_engine.execute_sync(&mut host, "status", false);
    let st_plain = pal_headless::text_mode::format_plain_response(&st_res.response, false);
    assert!(
        st_plain.contains("#296") && st_plain.contains("气疗术") && st_plain.contains("6MP") && st_plain.contains("—"),
        "plain-text status must show spell #ID, MP cost, tags, and desc:\n{st_plain}"
    );
    let bag_res = fair_engine.execute_sync(&mut host, "bag", false);
    let bag_plain = pal_headless::text_mode::format_plain_response(&bag_res.response, false);
    assert!(
        bag_plain.contains("#61") && bag_plain.contains("观音符") && bag_plain.contains("HP+150"),
        "plain-text bag must show item #ID, name, tags, and effect desc:\n{bag_plain}"
    );

    // 3. Name lookup in explore mode: `magic 气疗术 0` and `use 止血草 0`.
    let hp_before = host.runtime().game.as_ref().unwrap().world.roles[0].hp;
    let cast_by_name = fair_engine.execute_sync(&mut host, "magic 气疗术 0", false);
    assert_eq!(
        cast_by_name.response["ok"], true,
        "explore `magic 气疗术 0` must resolve by name and heal: {}",
        cast_by_name.response
    );
    assert_eq!(cast_by_name.response["artifacts"]["field_magic"]["magic_id"], 296);
    assert!(host.runtime().game.as_ref().unwrap().world.roles[0].hp > hp_before);

    let hp_before_herb = host.runtime().game.as_ref().unwrap().world.roles[0].hp;
    let use_by_name = fair_engine.execute_sync(&mut host, "use 止血草 0", false);
    assert_eq!(
        use_by_name.response["ok"], true,
        "explore `use 止血草 0` must resolve by name: {}",
        use_by_name.response
    );
    assert!(host.runtime().game.as_ref().unwrap().world.roles[0].hp >= hp_before_herb);

    // 4. Battle mode: `use 观音符 0` auto-routes to BattleItem, invalid item fails fast, `magic 御剑术 0` works.
    let _ = dbg_engine.execute_sync(&mut host, "/battle 10", false);
    assert_eq!(
        pal_headless::text_mode::current_mode(host.runtime()),
        "battle",
        "must enter battle"
    );
    {
        let g = host.runtime_mut().game.as_mut().unwrap();
        g.world.roles[0].hp = 40;
        g.world.add_item(196, 1); // 头巾 (headgear: usable=false, throwable=false)
    }
    // Non-usable/non-throwable armor in battle must fail fast with `item_refused` (without losing the turn).
    let bad_use = fair_engine.execute_sync(&mut host, "use 头巾", false);
    assert_eq!(bad_use.response["ok"], false);
    assert_eq!(bad_use.response["error_code"], "item_refused");
    assert_eq!(bad_use.response["mode"], "battle");

    // `use 观音符 0` in battle must succeed and consume 1x item #61!
    let qty_before = host.runtime().game.as_ref().unwrap().world.item_count(61);
    let battle_use = fair_engine.execute_sync(&mut host, "use 观音符 0", false);
    assert_eq!(
        battle_use.response["ok"], true,
        "`use 观音符 0` in battle must auto-route to BattleItem and succeed: {}",
        battle_use.response
    );
    // Advance/settle if other heroes still need turns or round already executed
    let qty_after = host.runtime().game.as_ref().unwrap().world.item_count(61);
    // In PAL_CLASSIC multi-hero party, round executes after all heroes choose; let remaining heroes defend so item executes.
    let mut guard = 0;
    while host.runtime().game.as_ref().unwrap().world.item_count(61) == qty_before
        && pal_headless::text_mode::current_mode(host.runtime()) == "battle"
        && guard < 6
    {
        let _ = fair_engine.execute_sync(&mut host, "defend", false);
        guard += 1;
    }
    let qty_final = host.runtime().game.as_ref().unwrap().world.item_count(61);
    assert!(
        qty_final < qty_before || qty_after < qty_before,
        "battle `use 观音符 0` must consume item #61 during round execution (before={qty_before}, final={qty_final})"
    );

    // If still in battle, test `magic 御剑术 0` by name (if active hero knows it, or ensure known).
    if pal_headless::text_mode::current_mode(host.runtime()) == "battle" {
        let active_role = host
            .runtime()
            .game
            .as_ref()
            .and_then(|g| g.battle.as_ref().and_then(|b| b.prompting.and_then(|hi| b.heroes.get(hi).map(|h| h.role))))
            .unwrap_or(0);
        {
            let g = host.runtime_mut().game.as_mut().unwrap();
            if !g.world.roles[active_role].magics.contains(&345) {
                g.world.roles[active_role].magics.push(345);
            }
            g.world.roles[active_role].mp = 200;
        }
        let cast_battle = fair_engine.execute_sync(&mut host, "magic 御剑术 0", false);
        assert_eq!(
            cast_battle.response["ok"], true,
            "battle `magic 御剑术 0` must resolve by name and succeed: {}",
            cast_battle.response
        );
    }
}

/// #277: Fair mode surfaces unlocked unconditional `0x0059` doorways (Yuhang Inn bedroom door ev#4)
/// and pure initially-active stair-walk triggers (Yuhang Inn 1F<->2F stairs ev#47/ev#48) as visible
/// `exit` events & `>` minimap markers, while keeping secret passages (bed tunnel ev#10) & locked
/// doors (`ev.state == 0`) strictly hidden.
#[test]
fn text_mode_issue_277_fair_mode_indoor_doors_and_stairs() {
    use pal_headless::text_mode::ObsMode;

    let assets = assets_root();
    if !assets.join("data/scenes.yaml").is_file() {
        eprintln!("skip: assets not found");
        return;
    }

    let cfg = TextModeConfig {
        assets_root: assets,
        json_mode: true,
        lang: pal_core::Language::ZhHans,
        new_game: true,
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot new game");
    let mut dbg_engine = TextEngine::with_obs(ObsMode::Debug, 0);
    let mut fair_engine = TextEngine::with_obs(ObsMode::Fair, 0);
    dbg_engine.settle_sync(&mut host, 80);

    // Drain opening Aunt Li cutscene until explore mode in Xiaoyao's bedroom (Scene 1).
    let _ = fair_engine.execute_sync(&mut host, "continue", false);
    assert_eq!(pal_headless::text_mode::current_mode(host.runtime()), "explore");

    // Move close to Xiaoyao's bedroom door ev#4 (1152, 384) and verify it is visible as `exit` & `>`
    // while secret bed passage ev#10 (1376, 272, tm=2) stays strictly hidden!
    let _ = dbg_engine.execute_sync(&mut host, "/tp 1 1248 336", false);
    let fair_bed = fair_engine.execute_sync(&mut host, "look", false);
    let bed_evs = fair_bed.response["events"].as_array().unwrap();
    assert!(
        bed_evs.iter().any(|e| e["id"] == 4 && e["kind"] == "exit"),
        "unlocked bedroom door ev#4 must be surfaced as exit in fair mode: {bed_evs:?}"
    );
    assert!(
        !bed_evs.iter().any(|e| e["id"] == 10),
        "secret bed-to-woodshed passage ev#10 must NEVER leak in fair mode: {bed_evs:?}"
    );
    let mm_bed = fair_bed.response["minimap"].as_object().unwrap();
    let ox = mm_bed["origin"]["x"].as_i64().unwrap() as i32;
    let oy = mm_bed["origin"]["y"].as_i64().unwrap() as i32;
    let c4 = ((1152 - ox) / 16) as usize;
    let r4 = ((384 - oy) / 8) as usize;
    let ch4 = mm_bed["grid"][r4].as_str().unwrap().as_bytes()[c4] as char;
    assert_eq!(ch4, '>', "bedroom door ev#4 must render as '>' on fair minimap");

    // When ev#4 is locked (`state == 0`), it must remain strictly hidden!
    {
        let g = host.runtime_mut().game.as_mut().unwrap();
        if let Some(e4) = g.world.event_mut(4) {
            e4.state = 0;
        }
    }
    let fair_locked = fair_engine.execute_sync(&mut host, "look", false);
    assert!(
        !fair_locked.response["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"] == 4),
        "locked ev#4 (state=0) must NOT leak in fair mode"
    );

    // Scene 3 (Yuhang Inn lobby): 2F->1F stairs ev#48 (1168, 1368) & 1F->2F stairs ev#47 (1248, 1456)
    // must be surfaced as `kind == "exit"` and `>` on the fair minimap!
    let _ = dbg_engine.execute_sync(&mut host, "/tp 3 1216 1408", false);
    let fair_stairs = fair_engine.execute_sync(&mut host, "look", false);
    let stair_evs = fair_stairs.response["events"].as_array().unwrap();
    assert!(
        stair_evs.iter().any(|e| e["id"] == 47 && e["kind"] == "exit"),
        "Inn 1F->2F stair trigger ev#47 must be surfaced as exit in fair mode: {stair_evs:?}"
    );
    assert!(
        stair_evs.iter().any(|e| e["id"] == 48 && e["kind"] == "exit"),
        "Inn 2F->1F stair trigger ev#48 must be surfaced as exit in fair mode: {stair_evs:?}"
    );
    let mm_st = fair_stairs.response["minimap"].as_object().unwrap();
    let ox_s = mm_st["origin"]["x"].as_i64().unwrap() as i32;
    let oy_s = mm_st["origin"]["y"].as_i64().unwrap() as i32;
    for &(sx, sy, id) in &[(1248, 1456, 47), (1168, 1368, 48)] {
        let cs = ((sx - ox_s) / 16) as usize;
        let rs = ((sy - oy_s) / 8) as usize;
        let ch = mm_st["grid"][rs].as_str().unwrap().as_bytes()[cs] as char;
        assert_eq!(ch, '>', "Inn stair ev#{id} at ({sx},{sy}) must render as '>' on fair minimap (got '{ch}')");
    }
}

/// #278: Scene 5 blacksmith closed double door (`ev#138`/`ev#139` blocking `ev#140`),
/// fair-mode visual-only sprite labels (zero identity/script leaks), `goto` blocker diagnosis,
/// silent-effect `talk 138` door opening, and subsequent `goto 140` into Scene 12.
#[test]
fn text_mode_issue_278_closed_double_door_blockers_and_visual_sprite_labels() {
    use pal_headless::text_mode::ObsMode;

    let assets = assets_root();
    if !assets.join("data/scenes.yaml").is_file() {
        eprintln!("skip: assets not found");
        return;
    }

    // 1. Fairness guard on `GameData::load_sprite_visuals`: MUST ignore `name`/`speaker_hints`
    //    and only load visual-only `visual`/`visual_kind` fields!
    let mut gd = pal_core::game::data::GameData::default();
    gd.load_table(
        "sprite_visuals",
        &serde_json::json!({
            "sprites": [
                {
                    "id": 12,
                    "name": "张四 / 老板",
                    "speaker_hints": ["张四"],
                    "category": "npc"
                },
                {
                    "id": 62,
                    "name": "铁匠铺左门（不应泄露）",
                    "visual": "红色木门（关闭）",
                    "visual_en": "closed red wooden door",
                    "visual_kind": "door"
                },
                {
                    "id": 63,
                    "visual": "红色木门（关闭）",
                    "visual_en": "closed red wooden door",
                    "visual_kind": "door"
                }
            ]
        }),
    );
    assert!(
        !gd.sprite_visuals.contains_key(&12),
        "mgo_catalog entries without visual/visual_kind must NEVER be loaded into sprite_visuals"
    );
    assert_eq!(gd.sprite_visuals[&62].visual, "红色木门（关闭）");
    assert_eq!(gd.sprite_visuals[&62].visual_kind, "door");

    // 2. Live Scene 5 regression: start at (640, 1248) outside the closed blacksmith double door.
    let cfg = TextModeConfig {
        assets_root: assets,
        json_mode: true,
        lang: pal_core::Language::ZhHans,
        new_game: true,
        ..Default::default()
    };
    let mut host = HeadlessTextHost::from_config(&cfg).expect("boot new game");
    let mut dbg_engine = TextEngine::with_obs(ObsMode::Debug, 0);
    let mut fair_engine = TextEngine::with_obs(ObsMode::Fair, 0);
    dbg_engine.settle_sync(&mut host, 120);

    // Teleport to Scene 5 (640, 1248) and attach visual-only door labels for MGO 62 & 63.
    let _ = dbg_engine.execute_sync(&mut host, "/tp 5 640 1248", false);
    if let Some(g) = host.runtime_mut().game.as_mut() {
        g.data.load_table(
            "sprite_visuals",
            &serde_json::json!({
                "sprites": [
                    { "id": 62, "visual": "红色木门（关闭）", "visual_kind": "door" },
                    { "id": 63, "visual": "红色木门（关闭）", "visual_kind": "door" }
                ]
            }),
        );
    }

    // Fair `look` shows ev#138 & ev#139 as doors with visual description and ZERO script/destination leaks.
    let look = fair_engine.execute_sync(&mut host, "look", false);
    assert_unified_envelope(&look.response);
    let evs = look.response["events"].as_array().unwrap();
    for id in [138u64, 139u64] {
        let ev = evs
            .iter()
            .find(|e| e["id"] == id)
            .unwrap_or_else(|| panic!("ev#{id} must be visible in fair look: {evs:?}"));
        assert_eq!(ev["kind"], "object");
        assert_eq!(ev["visual_kind"], "door");
        assert_eq!(ev["visual"], "红色木门（关闭）");
        assert_eq!(ev["hint"], "红色木门（关闭）");
        assert!(ev.get("interactable").is_none());
        assert!(ev.get("trigger_mode").is_none());
    }
    assert!(
        evs.iter().any(|e| e["id"] == 140 && e["kind"] == "exit"),
        "exit ev#140 behind the double door must be visible in fair look: {evs:?}"
    );

    // `goto 140` while doors 138/139 are closed -> `unreachable` with blockers [138, 139]!
    let blocked = fair_engine.execute_sync(&mut host, "goto 140", false);
    assert_unified_envelope(&blocked.response);
    assert_eq!(
        blocked.response["ok"], false,
        "goto 140 behind closed double doors must fail unreachable: {}",
        blocked.response
    );
    assert_eq!(blocked.response["error_code"], "unreachable");
    let blockers: Vec<u64> = blocked.response["artifacts"]["goto"]["blockers"]
        .as_array()
        .expect("artifacts.goto.blockers must be present")
        .iter()
        .filter_map(|v| v.as_u64())
        .collect();
    assert!(
        blockers.contains(&138) && blockers.contains(&139),
        "blockers must include both door panels 138 & 139, got: {blockers:?} (resp: {})",
        blocked.response
    );
    let err_msg = blocked.response["error"].as_str().unwrap_or("");
    assert!(
        err_msg.contains("#138") && err_msg.contains("talk 138"),
        "unreachable error should diagnose blocker & suggest talk: {err_msg}"
    );

    // `talk 138` opens the double door silently (state 2 -> 0 for both 138 & 139) and returns ok=true!
    let opened = fair_engine.execute_sync(&mut host, "talk 138", false);
    assert_unified_envelope(&opened.response);
    assert_eq!(
        opened.response["ok"], true,
        "talk 138 must succeed when silently opening the double door: {}",
        opened.response
    );
    {
        let g = host.runtime().game.as_ref().unwrap();
        assert_eq!(g.world.event(138).unwrap().state, 0, "ev#138 must open (state 0)");
        assert_eq!(g.world.event(139).unwrap().state, 0, "ev#139 must sync-open (state 0)");
    }

    // `goto 140` now walks through the open doorway straight into Scene 12 (blacksmith)!
    let entered = fair_engine.execute_sync(&mut host, "goto 140", false);
    assert_unified_envelope(&entered.response);
    assert_eq!(
        entered.response["ok"], true,
        "goto 140 after opening doors must succeed: {}",
        entered.response
    );
    assert_eq!(
        entered.response["scene"]["id"], 12,
        "goto 140 must enter Scene 12 (盛渔村·铁匠装备铺), got: {}",
        entered.response["scene"]
    );
}
