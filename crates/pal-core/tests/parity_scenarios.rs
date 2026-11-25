//! Differential parity: replay `tools/parity/scenarios/*.json` through the Rust core and compare
//! every checkpoint with `tools/parity/golden/*.json`. The goldens were recorded from the legacy
//! JavaScript runtime (removed after the Rust core became authoritative) and are now frozen
//! regression baselines: an intentional behaviour change must update them deliberately.

mod common;

use common::NativeHost;
use pal_core::isometric::{direction_between, find_path_world};
use serde_json::Value;
use std::path::Path;

fn parity_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/parity")
}

/// Run foreground work to completion without advancing exploration (mirrors the JS oracle).
fn settle(h: &mut NativeHost) {
    for _ in 0..200_000 {
        h.pump();
        if !h.game.is_busy() && !h.game.is_loading_scene() {
            return;
        }
        h.game.tick(&pal_core::game::TickInput::IDLE);
    }
    panic!("settle timeout; waiting on {:?}", h.game.waiting_on());
}

fn walk_to(h: &mut NativeHost, tx: i32, ty: i32) {
    h.game.sync_scene_obstacles();
    let lead = h.game.world.leader().clone();
    let path = find_path_world(&h.game.grid, lead.x, lead.y, tx, ty, 1200);
    for (px, py) in path {
        for _ in 0..24 {
            let l = h.game.world.leader().clone();
            if (l.x - px).abs() <= 8 && (l.y - py).abs() <= 4 {
                break;
            }
            let moved = h.game.try_manual_step(direction_between(l.x, l.y, px, py));
            if h.game.is_busy() {
                settle(h);
                return;
            }
            if !moved {
                break;
            }
        }
    }
}

fn run_scenario(name: &str) -> Vec<(String, Value)> {
    let scen: Value = serde_json::from_str(&std::fs::read_to_string(parity_dir().join(format!("scenarios/{name}.json"))).unwrap()).unwrap();
    let mut h = NativeHost::new();
    if let Some(c) = scen.get("choice").and_then(Value::as_bool) {
        h.choice_answer = c;
    }
    let mut out = Vec::new();
    for step in scen["steps"].as_array().unwrap() {
        let num = |k: &str| step.get(k).and_then(Value::as_i64);
        if let Some(seed) = num("new_game") {
            h.game.start_new_game(seed as u32);
            settle(&mut h);
        } else if let Some(xy) = step.get("walk_to").and_then(Value::as_array) {
            walk_to(&mut h, xy[0].as_i64().unwrap() as i32, xy[1].as_i64().unwrap() as i32);
        } else if let Some(id) = num("walk_to_event") {
            let e = h.game.world.event(id as u16).unwrap().clone();
            walk_to(&mut h, e.x, e.y);
        } else if let Some(id) = num("face_interact") {
            let e = h.game.world.event(id as u16).unwrap().clone();
            let l = h.game.world.leader().clone();
            h.game.world.leader_mut().dir = direction_between(l.x, l.y, e.x, e.y);
            h.game.interact_in_front();
            settle(&mut h);
        } else if let Some(id) = num("face_event") {
            let e = h.game.world.event(id as u16).unwrap().clone();
            let l = h.game.world.leader().clone();
            h.game.world.leader_mut().dir = direction_between(l.x, l.y, e.x, e.y);
        } else if let Some(id) = num("trigger") {
            h.game.trigger_event(id as u16);
            settle(&mut h);
        } else if let Some(item) = num("use_item") {
            let target = num("target").unwrap_or(0) as usize;
            h.game.use_item(item as u16, Some(target));
            settle(&mut h);
        } else if let Some(n) = num("auto_ticks") {
            for _ in 0..n {
                h.game.run_auto_tick();
                if h.game.is_busy() {
                    settle(&mut h);
                }
            }
        } else if let Some(cp) = step.get("checkpoint").and_then(Value::as_str) {
            out.push((cp.to_string(), h.game.canonical_state()));
        }
    }
    out
}

fn diff(path: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                diff(&format!("{path}.{k}"), x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null), out);
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (p, q)) in x.iter().zip(y).enumerate() {
                diff(&format!("{path}[{i}]"), p, q, out);
            }
        }
        _ if a == b => {}
        _ => out.push(format!("{path}: js={a} rust={b}")),
    }
}

fn check(name: &str) {
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(parity_dir().join(format!("golden/{name}.json"))).unwrap()).unwrap();
    let got = run_scenario(name);
    let want = golden["checkpoints"].as_array().unwrap();
    assert_eq!(want.len(), got.len(), "checkpoint count");
    let mut diffs = Vec::new();
    for (w, (cp, state)) in want.iter().zip(&got) {
        assert_eq!(w["checkpoint"], *cp);
        diff(cp, &w["state"], state, &mut diffs);
    }
    assert!(diffs.is_empty(), "{name}: {} differences vs legacy JS:\n{}", diffs.len(), diffs.join("\n"));
}

#[test]
fn ch1_opening_matches_legacy_js() {
    check("ch1_opening");
}

#[test]
fn ch1_inn_matches_legacy_js() {
    check("ch1_inn");
}
