//! Layer 1 Automated Testing: Data-driven `PalRemake_CoreReplay_v1` regression suite.
//!
//! Every `*.json` file in `crates/pal-core/tests/replays/` is automatically discovered,
//! anchored (`new_game`, `save_json`, `rpg_base64`, or `orig_save`), replayed frame-for-frame
//! against the deterministic Rust core, and checked against its `expect` assertions.

mod common;

use common::{assets_dir, NativeHost};
use pal_core::game::TickInput;
use serde_json::Value;
use std::path::Path;

fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    const TABLE: [i8; 128] = {
        let mut t = [-1i8; 128];
        let mut i = 0u8;
        while i < 26 {
            t[(b'A' + i) as usize] = i as i8;
            t[(b'a' + i) as usize] = (i + 26) as i8;
            i += 1;
        }
        let mut d = 0u8;
        while d < 10 {
            t[(b'0' + d) as usize] = (d + 52) as i8;
            d += 1;
        }
        t[b'+' as usize] = 62;
        t[b'/' as usize] = 63;
        t
    };
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for b in input.bytes() {
        if b.is_ascii_whitespace() || b == b'=' {
            continue;
        }
        if b >= 128 || TABLE[b as usize] < 0 {
            return Err(format!("invalid base64 byte: 0x{b:02x}"));
        }
        buf = (buf << 6) | (TABLE[b as usize] as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

fn apply_anchor(host: &mut NativeHost, replay: &Value) {
    if let Some(anchor) = replay.get("anchor").filter(|v| !v.is_null()) {
        match anchor["type"].as_str().unwrap_or("") {
            "new_game" => {
                let seed = anchor["seed"]
                    .as_u64()
                    .or_else(|| replay["seed"].as_u64())
                    .unwrap_or(1) as u32;
                host.game.start_new_game(seed);
                host.pump();
                return;
            }
            "save_json" => {
                let data = &anchor["data"];
                let val = if let Some(s) = data.as_str() {
                    serde_json::from_str::<Value>(s).expect("parse save_json string")
                } else {
                    data.clone()
                };
                host.game
                    .load_save_json(&val)
                    .expect("load_save_json anchor");
                host.pump();
                return;
            }
            "rpg_base64" => {
                let b64 = anchor["data"].as_str().expect("rpg_base64 data string");
                let bytes = decode_base64(b64).expect("decode rpg_base64");
                host.game
                    .load_dos_rpg(&bytes)
                    .expect("load_dos_rpg base64 anchor");
                host.pump();
                return;
            }
            "orig_save" => {
                let name = anchor["file"].as_str().expect("orig_save file name");
                let path = assets_dir().join("orig_saves").join(name);
                let bytes = std::fs::read(&path)
                    .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                host.game
                    .load_dos_rpg(&bytes)
                    .unwrap_or_else(|e| panic!("load_dos_rpg {}: {e}", path.display()));
                host.pump();
                return;
            }
            other => panic!("unsupported replay anchor type: '{other}'"),
        }
    }
    let seed = replay["seed"].as_u64().unwrap_or(1) as u32;
    host.game.start_new_game(seed);
    host.pump();
}

fn parse_input_entry(arr: &[Value]) -> TickInput {
    let held_dir = arr.first().and_then(Value::as_i64).unwrap_or(-1) as i8;
    let flags = arr.get(1).and_then(Value::as_u64).unwrap_or(0) as u32;
    let cx = arr.get(2).and_then(Value::as_i64).unwrap_or(0) as i32;
    let cy = arr.get(3).and_then(Value::as_i64).unwrap_or(0) as i32;
    let sx = arr.get(4).and_then(Value::as_i64).unwrap_or(0) as i32;
    let sy = arr.get(5).and_then(Value::as_i64).unwrap_or(0) as i32;
    TickInput {
        held_dir: if held_dir >= 0 { held_dir & 3 } else { -1 },
        interact: (flags & 1) != 0,
        click: ((flags & 2) != 0).then_some((cx, cy)),
        steer: ((flags & 4) != 0).then_some((sx, sy)),
    }
}

fn check_expect(host: &NativeHost, exp: &Value, ctx: &str) {
    let st = host.game.canonical_state();
    if let Some(sc) = exp.get("scene").and_then(Value::as_u64) {
        let actual = st.get("sceneId").or_else(|| st.get("scene")).and_then(Value::as_u64);
        assert_eq!(
            actual,
            Some(sc),
            "{ctx}: expected scene {sc}, got {:?}",
            actual
        );
    }
    if let Some(cash) = exp.get("cash").and_then(Value::as_i64) {
        assert_eq!(
            st["cash"].as_i64(),
            Some(cash),
            "{ctx}: expected cash {cash}, got {}",
            st["cash"]
        );
    }
    if let Some(busy) = exp.get("busy").and_then(Value::as_bool) {
        assert_eq!(
            host.game.is_busy(),
            busy,
            "{ctx}: expected busy={busy}, got {}",
            host.game.is_busy()
        );
    }
    if let Some(item_arr) = exp.get("item").and_then(Value::as_array) {
        let id = item_arr[0].as_u64().unwrap() as u16;
        let qty = item_arr[1].as_i64().unwrap() as i32;
        assert_eq!(
            host.game.world.item_count(id),
            qty,
            "{ctx}: expected item {id} qty {qty}"
        );
    }
    if let Some(ev_id) = exp.get("event").and_then(Value::as_u64) {
        let ev = host
            .game
            .world
            .event(ev_id as u16)
            .unwrap_or_else(|| panic!("{ctx}: missing event {ev_id}"));
        if let Some(state) = exp.get("state").and_then(Value::as_i64) {
            assert_eq!(ev.state as i64, state, "{ctx}: event {ev_id} state");
        }
        if let Some(trig) = exp.get("trig").and_then(Value::as_i64) {
            assert_eq!(ev.trigger_mode as i64, trig, "{ctx}: event {ev_id} trig");
        }
    }
    if let Some(min_ticks) = exp.get("min_ticks").and_then(Value::as_u64) {
        assert!(
            host.game.tick_count >= min_ticks,
            "{ctx}: expected tick_count >= {min_ticks}, got {}",
            host.game.tick_count
        );
    }
}

fn run_replay_file(path: &Path) {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let doc: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    let replay = doc.get("replay").unwrap_or(&doc);
    let log = replay["log"].as_array().cloned().unwrap_or_default();
    let expects = doc["expect"].as_array().cloned().unwrap_or_default();
    let settle_after = doc["settle_after"].as_bool().unwrap_or(true);
    let total_ticks = doc["run_ticks"]
        .as_u64()
        .or_else(|| log.last().and_then(|e| e["t"].as_u64()).map(|t| t + 1))
        .unwrap_or(1);

    let mut host = NativeHost::new();
    apply_anchor(&mut host, replay);

    let mut idx = 0usize;
    let mut checked = vec![false; expects.len()];
    for _ in 0..total_ticks {
        let now = host.game.tick_count;
        let mut input = TickInput::IDLE;
        while idx < log.len() && log[idx]["t"].as_u64().unwrap_or(u64::MAX) <= now {
            let e = &log[idx];
            idx += 1;
            if let Some(arr) = e.get("i").and_then(Value::as_array) {
                if e["t"].as_u64() == Some(now) {
                    input = parse_input_entry(arr);
                }
            }
        }
        host.tick(input);
        for (ei, exp) in expects.iter().enumerate() {
            if !checked[ei] && exp.get("at_tick").and_then(Value::as_u64) == Some(host.game.tick_count) {
                check_expect(&host, exp, &format!("{}@tick={}", path.display(), host.game.tick_count));
                checked[ei] = true;
            }
        }
    }

    if settle_after {
        assert!(
            host.settle(20_000),
            "{}: replay never settled after log",
            path.display()
        );
    }

    for (ei, exp) in expects.iter().enumerate() {
        if exp.get("at_end").and_then(Value::as_bool).unwrap_or(false) {
            check_expect(&host, exp, &format!("{}@end", path.display()));
            checked[ei] = true;
        }
    }
    for (ei, ok) in checked.iter().enumerate() {
        assert!(
            *ok,
            "{}: expect[{ei}] was never evaluated: {}",
            path.display(),
            expects[ei]
        );
    }
}

#[test]
fn replay_regression_suite() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/replays");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "expected at least one replay regression JSON in {}",
        dir.display()
    );
    for p in files {
        run_replay_file(&p);
    }
}

#[test]
fn save_json_anchor_replay_is_bit_exact() {
    // Run from New Game until settled, snapshot a save_json anchor, then record 120 ticks of input.
    let mut host_a = NativeHost::new();
    host_a.game.start_new_game(0x95_1995);
    host_a.pump();
    assert!(host_a.settle(20_000));
    let anchor_save = host_a.game.save_json("anchor", "2026-11-20T00:00:00Z");

    let mut log = Vec::new();
    for step in 0..120u64 {
        let dir = ((step / 20) % 4) as i8;
        let flags = if step % 30 == 15 { 1u32 } else { 0u32 };
        let t = host_a.game.tick_count;
        log.push(serde_json::json!({ "t": t, "i": [dir, flags, 0, 0, 0, 0] }));
        host_a.tick(TickInput {
            held_dir: dir,
            interact: flags != 0,
            click: None,
            steer: None,
        });
    }
    assert!(host_a.settle(5_000));
    let expected = host_a.game.canonical_state();

    // Replay from the save_json anchor on a completely fresh NativeHost.
    let mut host_b = NativeHost::new();
    host_b.game.load_save_json(&anchor_save).unwrap();
    host_b.pump();
    // Adjust timestamps relative to host_b's reset tick_count (0).
    let base_t = log[0]["t"].as_u64().unwrap();
    for entry in &log {
        let rel_t = entry["t"].as_u64().unwrap() - base_t;
        while host_b.game.tick_count < rel_t {
            host_b.tick(TickInput::IDLE);
        }
        let arr = entry["i"].as_array().unwrap();
        host_b.tick(parse_input_entry(arr));
    }
    assert!(host_b.settle(5_000));
    let actual = host_b.game.canonical_state();
    assert_eq!(
        actual, expected,
        "Replay from save_json anchor must match continuous execution bit-for-bit"
    );
}

#[test]
fn in_between_held_directions_alternate_adjacent_diagonals() {
    // Held 4..=7 (W, N, E, S) step only along the two adjacent diagonals `a = held - 4` and
    // `a + 1`: alternating when both are open (net straight line), sliding along one when the
    // other is blocked.
    const DIAG: [(i32, i32); 4] = [(-16, 8), (-16, -8), (16, -8), (16, 8)]; // SW, NW, NE, SE
    let mut alternated = false;
    for held in 4i8..=7 {
        let a = (held - 4) as usize;
        let allowed = [DIAG[a], DIAG[(a + 1) & 3]];
        let mut host = NativeHost::new();
        host.game.start_new_game(0x95_1995);
        host.pump();
        assert!(host.settle(20_000));
        let mut prev = (host.game.world.leader().x, host.game.world.leader().y);
        let mut used = [false; 2];
        for _ in 0..80 {
            host.tick(TickInput { held_dir: held, interact: false, click: None, steer: None });
            let cur = (host.game.world.leader().x, host.game.world.leader().y);
            let d = (cur.0 - prev.0, cur.1 - prev.1);
            if d != (0, 0) {
                let k = allowed.iter().position(|&v| v == d);
                assert!(k.is_some(), "held {held}: unexpected step {d:?}");
                used[k.unwrap()] = true;
            }
            prev = cur;
        }
        alternated |= used[0] && used[1];
    }
    assert!(alternated, "no in-between direction alternated its two diagonals");
}
