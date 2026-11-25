//! #273 B7 + D11 (`pal-headless` Text Mode fuzzing & command/path parser fuzzing):
//! - **B7 Text Mode Command Grammar Fuzzing**: runs randomized real command sequences through
//!   `TextEngine` + `HeadlessTextHost` across both `ObsMode::Fair` and `ObsMode::Debug`, validates
//!   EVERY response against `docs/text_mode.schema.json`, and verifies end-to-end record→replay
//!   equivalence via `pal_runtime::replay_player::ReplayPlayer`.
//! - **D11 Command & Path Parser Fuzzing**: structure-aware fuzzing of `parse_command`,
//!   `split_batch_commands`, and `confine_socket_path` under a watchdog deadline, dumping any
//!   crashing input to `.tmp/fuzz_parser_<target>_<hash>.bin`.

use pal_core::game::{HostCmd, HostReply};
use pal_headless::text_mode::engine::confine_socket_path;
use pal_headless::text_mode::{
    parse_command, split_batch_commands, HeadlessTextHost, ObsMode, TextEngine, TextHost,
    TextModeConfig, TEXT_PROTOCOL_VERSION,
};
use pal_runtime::disk::{boot_runtime_from_dir, load_scene_assets_from_dir};
use pal_runtime::replay_player::{ReplayPlayer, ReplayStepOutcome};
use serde_json::Value;
use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

struct FuzzRng(u64);

impl FuzzRng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() as usize) % n }
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn assets_root() -> PathBuf {
    repo_root().join("site/assets")
}

struct SchemaValidator {
    required_keys: Vec<String>,
    allowed_modes: HashSet<String>,
    allowed_langs: HashSet<String>,
    allowed_ui_views: HashSet<String>,
}

impl SchemaValidator {
    fn load() -> Self {
        let text = std::fs::read_to_string(repo_root().join("docs/text_mode.schema.json"))
            .expect("read docs/text_mode.schema.json");
        let v: Value = serde_json::from_str(&text).expect("parse text_mode.schema.json");
        let required_keys = v["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        let enum_set = |prop: &str| -> HashSet<String> {
            v["properties"][prop]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        };
        Self {
            required_keys,
            allowed_modes: enum_set("mode"),
            allowed_langs: enum_set("lang"),
            allowed_ui_views: enum_set("ui_view"),
        }
    }

    fn validate(&self, resp: &Value, cmd: &str) {
        assert_eq!(resp["protocol"], TEXT_PROTOCOL_VERSION, "cmd={cmd}");
        for k in &self.required_keys {
            assert!(resp.get(k).is_some(), "missing schema key '{k}' on cmd='{cmd}': {resp}");
        }
        assert!(resp["ok"].is_boolean(), "ok must be bool on cmd='{cmd}'");
        assert!(
            matches!(resp["obs_mode"].as_str(), Some("debug" | "fair")),
            "bad obs_mode on cmd='{cmd}'"
        );
        let mode = resp["mode"].as_str().expect("mode string");
        assert!(self.allowed_modes.contains(mode), "unknown mode '{mode}' on cmd='{cmd}'");
        let lang = resp["lang"].as_str().expect("lang string");
        assert!(self.allowed_langs.contains(lang), "unknown lang '{lang}' on cmd='{cmd}'");
        let view = resp["ui_view"].as_str().expect("ui_view string");
        assert!(
            self.allowed_ui_views.contains(view),
            "unknown ui_view '{view}' on cmd='{cmd}'"
        );
        assert_eq!(resp["view"]["camera"]["w"], 320);
        assert_eq!(resp["view"]["camera"]["h"], 200);
        assert!(resp["notices"].is_array());
        if let Some(evs) = resp["events"].as_array() {
            for e in evs {
                let kind = e["kind"].as_str().unwrap_or("");
                assert!(
                    matches!(kind, "portal" | "trigger" | "npc" | "object" | "exit"),
                    "invalid event kind '{kind}' on cmd='{cmd}'"
                );
            }
        }
    }
}

fn service_host_replay(rt: &mut pal_runtime::PalRuntime, assets: &Path) {
    loop {
        rt.route_cmds();
        let Some(cmd) = rt.out_cmds.pop_front() else {
            break;
        };
        match cmd {
            HostCmd::LoadScene { map_id, .. } => {
                if let Some(g) = rt.game.as_mut() {
                    load_scene_assets_from_dir(assets, g, map_id);
                }
                rt.reply(HostReply::SceneLoaded);
            }
            HostCmd::LoadChapter(_) | HostCmd::PlayRng { .. } => {
                rt.reply(HostReply::Done);
            }
            _ => {}
        }
    }
}

fn gen_text_command(last_resp: &Value, rng: &mut FuzzRng) -> String {
    let mode = last_resp["mode"].as_str().unwrap_or("explore");
    // 10% malformed / adversarial commands to ensure clean error envelopes
    if rng.range(10) == 0 {
        let bad = [
            "unknown_cmd_xyz",
            "goto 99999",
            "talk 99999",
            "use 99999 9",
            "equip 99999 9",
            "magic 99999 9",
            "attack 99",
            "walk nope 3",
            "think 💭🚀",
            "key bogus_key",
            "lang klingon",
        ];
        return bad[rng.range(bad.len())].to_string();
    }

    match mode {
        "dialog" | "battle_victory" => match rng.range(4) {
            0 => "next".into(),
            1 => "continue".into(),
            2 => "look".into(),
            _ => "key confirm".into(),
        },
        "choice" => match rng.range(3) {
            0 => "yes".into(),
            1 => "no".into(),
            _ => "look".into(),
        },
        "menu" | "shop" => match rng.range(6) {
            0 => "ui down".into(),
            1 => "ui up".into(),
            2 => "ui back".into(),
            3 => "close".into(),
            4 => "key cancel".into(),
            _ => "look".into(),
        },
        "battle" => match rng.range(7) {
            0 => "attack 0".into(),
            1 => "defend".into(),
            2 => "flee".into(),
            3 => "auto".into(),
            4 => "cancel-auto".into(),
            5 => "magic 296 0".into(),
            _ => "look".into(),
        },
        _ => {
            // Explore mode
            match rng.range(16) {
                0 => "look".into(),
                1 => "map".into(),
                2 => format!("sw {}", 1 + rng.range(3)),
                3 => format!("nw {}", 1 + rng.range(3)),
                4 => format!("ne {}", 1 + rng.range(3)),
                5 => format!("se {}", 1 + rng.range(3)),
                6 => {
                    if let Some(evs) = last_resp["events"].as_array() {
                        if !evs.is_empty() {
                            let id = evs[rng.range(evs.len())]["id"].as_u64().unwrap_or(1);
                            return format!("goto {id}");
                        }
                    }
                    "search".into()
                }
                7 => {
                    if let Some(evs) = last_resp["events"].as_array() {
                        if !evs.is_empty() {
                            let id = evs[rng.range(evs.len())]["id"].as_u64().unwrap_or(1);
                            return format!("talk {id}");
                        }
                    }
                    "search".into()
                }
                8 => "status".into(),
                9 => "bag".into(),
                10 => "menu".into(),
                11 => "wait 5".into(),
                12 => {
                    let thoughts = ["先看看周围情况", "去前面找找NPC", "clear"];
                    format!("think {}", thoughts[rng.range(thoughts.len())])
                }
                13 => match rng.range(3) {
                    0 => "lang hans".into(),
                    1 => "lang hant".into(),
                    _ => "lang en".into(),
                },
                14 => {
                    if rng.range(2) == 0 {
                        "hd on".into()
                    } else {
                        "hd off".into()
                    }
                }
                _ => "search".into(),
            }
        }
    }
}

/// #273 B7: Text Mode command grammar fuzzing across Fair & Debug modes, verifying
/// `docs/text_mode.schema.json` conformance on every response AND record→replay equivalence.
#[test]
fn fuzz_b7_text_mode_grammar_schema_and_replay_equivalence() {
    let assets = assets_root();
    if !assets.join("data/scenes.yaml").is_file() {
        return;
    }
    let schema = SchemaValidator::load();
    let cmds_per_session: usize = std::env::var("PAL_FUZZ_TICKS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(|t: usize| (t / 10).clamp(40, 200))
        .unwrap_or(50);

    let scenarios = [
        ("0.rpg", ObsMode::Fair, 0xB7_0001u64),
        ("10.rpg", ObsMode::Fair, 0xB7_0002u64),
        ("25.rpg", ObsMode::Debug, 0xB7_0003u64),
    ];

    for (save, obs, seed) in scenarios {
        let cfg = TextModeConfig {
            assets_root: assets.clone(),
            save_path: Some(PathBuf::from(save)),
            obs_mode: obs,
            ..Default::default()
        };
        let mut host = HeadlessTextHost::from_config(&cfg).expect("boot text host");
        let mut engine = TextEngine::with_obs(obs, 0);
        engine.settle_sync(&mut host, 60);
        if let Some(g) = host.runtime_mut().game.as_mut() {
            g.recorder.enabled = true;
            let _ = g.restart_recording_here();
        }

        let mut rng = FuzzRng::new(seed);
        let mut last = engine.execute_sync(&mut host, "look", false).response;
        schema.validate(&last, "look");

        for _ in 0..cmds_per_session {
            let cmd = gen_text_command(&last, &mut rng);
            let reply = engine.execute_sync(&mut host, &cmd, false);
            schema.validate(&reply.response, &cmd);
            last = reply.response;
        }

        // Verify record -> ReplayPlayer equivalence for the entire Text Mode session!
        let (exp_state, exp_rng, bundle) = {
            let g = host.runtime().game.as_ref().unwrap();
            (g.canonical_state(), g.world.rng.state, g.export_replay_bundle())
        };
        if bundle.segments.is_empty() || bundle.total_ticks == 0 {
            continue;
        }

        let mut rep_rt = boot_runtime_from_dir(&assets, false).expect("boot replay rt");
        rep_rt.set_in_title(false);
        let mut player = ReplayPlayer::new(bundle).expect("new ReplayPlayer");
        assert!(player.load_segment(0, &mut rep_rt));
        service_host_replay(&mut rep_rt, &assets);
        let mut steps = 0usize;
        loop {
            steps += 1;
            assert!(steps < 100_000, "{save} ({obs:?}): text-mode replay did not finish");
            let outcome = player.step_one_tick(
                &mut rep_rt,
                |_, _| {},
                |r, _| service_host_replay(r, &assets),
            );
            service_host_replay(&mut rep_rt, &assets);
            match outcome {
                ReplayStepOutcome::Stepped => {}
                ReplayStepOutcome::SegmentCompleted { next_idx } => {
                    assert!(player.load_segment(next_idx, &mut rep_rt));
                    service_host_replay(&mut rep_rt, &assets);
                }
                ReplayStepOutcome::Finished => break,
            }
        }
        let got_g = rep_rt.game.as_ref().unwrap();
        assert_eq!(
            (got_g.canonical_state(), got_g.world.rng.state),
            (exp_state, exp_rng),
            "{save} ({obs:?}): Text Mode session record->replay diverged!"
        );
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn dump_failure(target: &str, bytes: &[u8]) -> PathBuf {
    let out_dir = repo_root().join(".tmp");
    let _ = std::fs::create_dir_all(&out_dir);
    let p = out_dir.join(format!("fuzz_parser_{target}_{:016x}.bin", fnv1a64(bytes)));
    let _ = std::fs::write(&p, bytes);
    p
}

/// #273 D11: Structure-aware parser fuzzing for `parse_command`, `split_batch_commands`,
/// and `confine_socket_path` under a watchdog deadline.
#[test]
fn fuzz_d11_command_and_socket_path_parsers() {
    let iters: usize = std::env::var("PAL_PARSER_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(400);

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let res = catch_unwind(AssertUnwindSafe(move || {
            let mut rng = FuzzRng::new(0xC0AD_273);
            let tokens = [
                "look", "goto", "talk", "use", "equip", "magic", "buy", "sell", "think",
                "shot", "save", "load", "/tp", "/item", "/cash", "/script", "key", "ui",
                ";", "\"", "'", "\\\"", "#", "//", " ", "\t", "\r", "\n", "\0",
                "李逍遥", "💭", "\u{202E}", "-1", "99999999999999999999", "0", "ev#10",
            ];

            for i in 0..iters {
                let n = 1 + rng.range(18);
                let mut line = String::new();
                for _ in 0..n {
                    line.push_str(tokens[rng.range(tokens.len())]);
                    if rng.range(3) == 0 {
                        line.push(' ');
                    }
                }
                // Occasional very long line (64 KiB+) to test linear-time parsing
                if i % 50 == 0 {
                    line.push_str(&"a;\"b\" ".repeat(12_000));
                }
                let r = catch_unwind(AssertUnwindSafe(|| {
                    let parts = split_batch_commands(&line);
                    for p in &parts {
                        let _ = parse_command(p);
                    }
                    let _ = parse_command(&line);
                }));
                if r.is_err() {
                    let p = dump_failure("command", line.as_bytes());
                    panic!("command parser panicked at iter {i}; dumped to {}", p.display());
                }
            }

            // `confine_socket_path` traversal & confinement fuzzing
            let base = std::env::temp_dir().join(format!("repal_fuzz_confine_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).unwrap();
            let canonical_base = base.canonicalize().unwrap_or_else(|_| base.clone());

            let path_parts = [
                "..", ".", "/", "\\", "sub", "a.json", "\0", "C:", "etc", "passwd",
                "~", "%2e%2e", "中文", " ", "slot_1.json", "...",
            ];
            for i in 0..iters {
                let n = 1 + rng.range(8);
                let mut raw = String::new();
                for k in 0..n {
                    if k > 0 {
                        raw.push(if rng.range(4) == 0 { '\\' } else { '/' });
                    }
                    raw.push_str(path_parts[rng.range(path_parts.len())]);
                }
                let res = catch_unwind(AssertUnwindSafe(|| confine_socket_path(&base, &raw)));
                match res {
                    Err(_) => {
                        let p = dump_failure("confine_path", raw.as_bytes());
                        panic!(
                            "confine_socket_path panicked at iter {i}; dumped to {}",
                            p.display()
                        );
                    }
                    Ok(Ok(resolved)) => {
                        assert!(
                            resolved.starts_with(&canonical_base) || resolved.starts_with(&base),
                            "confine_socket_path ESCAPED base {:?} -> {:?} for input {:?}",
                            canonical_base,
                            resolved,
                            raw
                        );
                    }
                    Ok(Err(_)) => {}
                }
            }
            let _ = std::fs::remove_dir_all(&base);
        }));
        let _ = tx.send(res);
    });

    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(())) => {}
        Ok(Err(payload)) => {
            let msg = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("non-string panic");
            panic!("[fuzz_text_mode:d11] panicked: {msg}");
        }
        Err(_) => panic!("[fuzz_text_mode:d11] WATCHDOG TIMEOUT after 20s"),
    }
}
