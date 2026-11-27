//! #273 A2 + C10 + D11 (`pal-runtime` disk-backed fuzzing):
//! - **A2 Record → Replay Equivalence**: drives `PalRuntime` through randomized exploration,
//!   battles (full command space), field/menu actions, and manual-save segment splits, exports
//!   the `.repal` bundle, round-trips it through gzip+tar (`write_repal_archive` → `read_repal_bytes`),
//!   replays it through `pal_runtime::replay_player::ReplayPlayer`, and asserts bit-exact final
//!   `canonical_state()` and `world.rng.state`.
//! - **C10 Story-Rail Anchors**: also seeds A2 sessions from segment anchors extracted out of
//!   bundled `test_data/replays/` `.repal` archives (in addition to New Game & DOS `.rpg` saves).
//! - **D11 `.repal` Parser Fuzzing**: structure-aware gzip + USTAR tar + JSON mutator seeded
//!   from real `.repal` archives under a watchdog deadline, dumping any crash input to
//!   `.tmp/fuzz_parser_repal_<hash>.bin`.

#![cfg(feature = "disk")]

use flate2::write::GzEncoder;
use flate2::Compression;
use pal_core::game::replay_rec::ReplayAnchor;
use pal_core::game::{BattleCmd, HostCmd, HostReply, TickInput};
use pal_runtime::disk::{boot_runtime_from_dir, load_scene_assets_from_dir};
use pal_runtime::repal_archive::{read_repal_bytes, read_repal_file, write_repal_archive};
use pal_runtime::replay_player::{ReplayPlayer, ReplayStepOutcome};
use pal_runtime::PalRuntime;
use std::io::Write;
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

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn assets_root() -> PathBuf {
    workspace_root().join("site/assets")
}

fn service_host_fuzz(
    rt: &mut PalRuntime,
    assets: &Path,
    rng: &mut FuzzRng,
    stop_flag: &mut bool,
) {
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
            HostCmd::ShowDialogue { .. } => {
                rt.reply(HostReply::DialogueDone);
            }
            HostCmd::AskYesNo => {
                rt.reply(HostReply::Choice(rng.range(2) == 0));
            }
            HostCmd::OpenShop(_) | HostCmd::OpenPawnshop | HostCmd::BattleVictory { .. } => {
                rt.reply(HostReply::ModalDone);
            }
            HostCmd::BattlePrompt { hero } => {
                let bcmd = pick_battle_cmd(rt, hero, rng);
                if let Some(g) = rt.game.as_mut() {
                    let _ = g.battle_command(bcmd);
                }
            }
            HostCmd::ReturnToTitle | HostCmd::GameOver => {
                *stop_flag = true;
            }
            _ => {}
        }
    }
}

fn service_host_replay(rt: &mut PalRuntime, assets: &Path) {
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

fn pick_battle_cmd(rt: &PalRuntime, hero_idx: usize, rng: &mut FuzzRng) -> BattleCmd {
    let Some(g) = rt.game.as_ref() else {
        return BattleCmd::Auto;
    };
    let Some(b) = g.battle.as_ref() else {
        return BattleCmd::Auto;
    };
    let alive_e: Vec<usize> = b
        .enemies
        .iter()
        .enumerate()
        .filter(|(_, e)| e.hp > 0)
        .map(|(i, _)| i)
        .collect();
    let te = if alive_e.is_empty() {
        0
    } else {
        alive_e[rng.range(alive_e.len())]
    };
    let alive_h: Vec<usize> = b
        .heroes
        .iter()
        .enumerate()
        .filter(|(_, h)| g.world.roles.get(h.role).is_some_and(|r| r.hp > 0))
        .map(|(i, _)| i)
        .collect();
    let role_id = b.heroes.get(hero_idx).map(|h| h.role).unwrap_or(0);
    match rng.range(8) {
        0..=1 => BattleCmd::Attack { target: te },
        2..=3 => {
            if let Some(r) = g.world.roles.get(role_id) {
                let castable: Vec<u16> = r
                    .magics
                    .iter()
                    .copied()
                    .filter(|mid| g.data.magics.get(mid).is_some_and(|m| m.cost_mp <= r.mp))
                    .collect();
                if !castable.is_empty() {
                    let mid = castable[rng.range(castable.len())];
                    let target = if g.data.magics.get(&mid).map(|m| m.target_enemy).unwrap_or(true) {
                        te
                    } else if !alive_h.is_empty() {
                        alive_h[rng.range(alive_h.len())]
                    } else {
                        0
                    };
                    return BattleCmd::Magic { magic: mid, target };
                }
            }
            BattleCmd::Attack { target: te }
        }
        4 => {
            let mut usable: Vec<u16> = g
                .world
                .inventory
                .iter()
                .filter(|(&id, &q)| {
                    q > 0 && g.data.items.get(&id).is_some_and(|it| it.usable || it.throwable)
                })
                .map(|(&id, _)| id)
                .collect();
            usable.sort();
            if !usable.is_empty() {
                let item = usable[rng.range(usable.len())];
                return BattleCmd::ItemTarget { item, target: te };
            }
            BattleCmd::Defend
        }
        5 if alive_h.len() >= 2 => BattleCmd::Coop { target: te },
        6 => BattleCmd::Defend,
        _ => BattleCmd::Flee,
    }
}

fn collect_repal_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_repal_files(&p, out);
        } else if p.file_name().and_then(|s| s.to_str()).is_some_and(pal_runtime::repal_archive::is_repal_file_name) {
            out.push(p);
        }
    }
}

fn verify_record_replay_session(
    assets: &Path,
    label: &str,
    setup: impl FnOnce(&mut PalRuntime),
    seed: u64,
    ticks: u32,
) {
    let mut rt = boot_runtime_from_dir(assets, false).expect("boot live runtime");
    rt.set_in_title(false);
    setup(&mut rt);
    let mut rng = FuzzRng::new(seed);
    let mut stopped = false;
    service_host_fuzz(&mut rt, assets, &mut rng, &mut stopped);

    // Ensure recorder is anchored cleanly at the start of the fuzz session
    if let Some(g) = rt.game.as_mut() {
        g.recorder.enabled = true;
    }

    let mut cur_dir = rng.range(4) as u8;
    let mut hold = 0u32;

    for t in 0..ticks {
        if stopped {
            break;
        }
        // Periodic field/menu actions & manual-save splits (B6 + #272)
        if t > 0 && t % 55 == 0 {
            let idle = rt.game.as_ref().is_some_and(|g| {
                !g.is_busy() && g.battle.is_none() && !g.is_loading_scene()
            });
            if idle {
                match rng.range(5) {
                    0 => {
                        if let Some(g) = rt.game.as_mut() {
                            let mut eqs: Vec<u16> = g
                                .world
                                .inventory
                                .iter()
                                .filter(|(&id, &q)| {
                                    q > 0 && g.data.items.get(&id).is_some_and(|it| it.equipable)
                                })
                                .map(|(&id, _)| id)
                                .collect();
                            eqs.sort();
                            if !eqs.is_empty() {
                                let item = eqs[rng.range(eqs.len())];
                                let party = g.world.party_role_ids();
                                if !party.is_empty() {
                                    let role = party[rng.range(party.len())];
                                    let _ = g.equip_from_menu(role, item);
                                }
                            }
                        }
                    }
                    1 => {
                        if let Some(g) = rt.game.as_mut() {
                            let mut items: Vec<u16> = g
                                .world
                                .inventory
                                .iter()
                                .filter(|(&id, &q)| {
                                    q > 0 && g.data.items.get(&id).is_some_and(|it| it.usable)
                                })
                                .map(|(&id, _)| id)
                                .collect();
                            items.sort();
                            if !items.is_empty() {
                                let item = items[rng.range(items.len())];
                                let party = g.world.party_role_ids();
                                if !party.is_empty() {
                                    let role = party[rng.range(party.len())];
                                    let _ = g.use_item(item, Some(role));
                                }
                            }
                        }
                    }
                    2 => {
                        if let Some(g) = rt.game.as_mut() {
                            let party = g.world.party_role_ids();
                            if !party.is_empty() {
                                let caster = party[rng.range(party.len())];
                                let mags: Vec<u16> = g
                                    .world
                                    .roles
                                    .get(caster)
                                    .map(|r| {
                                        r.magics
                                            .iter()
                                            .copied()
                                            .filter(|mid| {
                                                g.data.magics.get(mid).is_some_and(|m| {
                                                    m.usable_outside_battle && m.cost_mp <= r.mp
                                                })
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                if !mags.is_empty() {
                                    let mid = mags[rng.range(mags.len())];
                                    let _ = g.cast_field_magic(caster, mid, Some(caster));
                                }
                            }
                        }
                    }
                    3 => {
                        if let Some(g) = rt.game.as_mut() {
                            if g.world.cash >= 100 {
                                let _ = g.buy_item(49, 0);
                            }
                        }
                    }
                    _ => {
                        if let Some(g) = rt.game.as_mut() {
                            g.split_segment_on_manual_save();
                        }
                    }
                }
                service_host_fuzz(&mut rt, assets, &mut rng, &mut stopped);
            }
        }

        if rt.game.as_ref().is_some_and(|g| g.battle.is_some()) && rng.range(30) == 0 {
            if let Some(g) = rt.game.as_mut() {
                let _ = g.battle_cancel_auto();
            }
        }

        if hold == 0 {
            cur_dir = rng.range(4) as u8;
            hold = 6 + rng.range(16) as u32;
        } else {
            hold -= 1;
        }
        // Leave occasional idle gaps so deferred manual splits (#272) fire during the session!
        let idle_gap = t % 60 >= 55;
        let click = if !idle_gap
            && rt.game.as_ref().is_some_and(|g| !g.is_busy())
            && rng.range(24) == 0
        {
            rt.game.as_ref().and_then(|g| {
                let ids = g.world.current_event_ids();
                if ids.is_empty() {
                    return None;
                }
                let id = ids[rng.range(ids.len())];
                g.world
                    .event(id)
                    .filter(|e| e.state > 0 && e.vanish_time <= 0)
                    .map(|e| (e.x, e.y))
            })
        } else {
            None
        };
        let input = if idle_gap {
            TickInput::IDLE
        } else {
            TickInput {
                held_dir: if click.is_some() { -1 } else { cur_dir as i8 },
                interact: click.is_none() && rng.range(14) == 0,
                click,
                steer: None,
            }
        };
        rt.tick(input);
        service_host_fuzz(&mut rt, assets, &mut rng, &mut stopped);
    }

    let (expected_state, expected_rng, bundle) = {
        let g = rt.game.as_ref().unwrap();
        (g.canonical_state(), g.world.rng.state, g.export_replay_bundle())
    };
    if bundle.segments.is_empty() || bundle.total_ticks == 0 {
        return;
    }

    // Round-trip through real `.repal` (.tar.gz) archive serialization & deserialization!
    let tmp_path = std::env::temp_dir().join(format!(
        "repal_fuzz_a2_{}_{}.repal",
        std::process::id(),
        seed
    ));
    write_repal_archive(&tmp_path, &bundle).expect("write .repal");
    let raw = std::fs::read(&tmp_path).expect("read .repal bytes");
    let _ = std::fs::remove_file(&tmp_path);
    let parsed = read_repal_bytes(&raw).expect("parse .repal bytes");

    // Replay end-to-end through the unified `ReplayPlayer`
    let mut rep_rt = boot_runtime_from_dir(assets, false).expect("boot replay runtime");
    rep_rt.set_in_title(false);
    let mut player = ReplayPlayer::new(parsed).expect("new ReplayPlayer");
    assert!(player.load_segment(0, &mut rep_rt), "{label}: failed to load seg 0");
    service_host_replay(&mut rep_rt, assets);

    let mut steps = 0usize;
    loop {
        steps += 1;
        assert!(steps < 50_000, "{label}: replay player did not finish within budget");
        let outcome = player.step_one_tick(
            &mut rep_rt,
            |_, _| {},
            |r, _| service_host_replay(r, assets),
        );
        service_host_replay(&mut rep_rt, assets);
        match outcome {
            ReplayStepOutcome::Stepped => {}
            ReplayStepOutcome::SegmentCompleted { next_idx } => {
                // Verify zero drift at EVERY segment boundary too!
                if let Some(ReplayAnchor::SaveJson(exp_seg)) =
                    player.bundle.segments.get(next_idx).map(|s| &s.anchor)
                {
                    let g = rep_rt.game.as_ref().unwrap();
                    let exp_rng = exp_seg["rng"].as_u64().unwrap_or(0) as u32;
                    let exp_sc = exp_seg["sceneId"].as_u64().unwrap_or(0) as u16;
                    assert_eq!(
                        (g.world.scene_id, g.world.rng.state),
                        (exp_sc, exp_rng),
                        "{label}: segment boundary {next_idx} drifted!"
                    );
                }
                assert!(player.load_segment(next_idx, &mut rep_rt));
                service_host_replay(&mut rep_rt, assets);
            }
            ReplayStepOutcome::Finished => break,
        }
    }

    let got_g = rep_rt.game.as_ref().unwrap();
    assert_eq!(
        (got_g.canonical_state(), got_g.world.rng.state),
        (expected_state, expected_rng),
        "{label}: A2 Record -> ReplayPlayer equivalence diverged!"
    );
}

#[test]
fn fuzz_a2_record_replay_equivalence_and_c10_story_rail_anchors() {
    let assets = assets_root();
    if !assets.join("data/scenes.yaml").is_file() {
        return;
    }
    let ticks: u32 = std::env::var("PAL_FUZZ_TICKS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(|t: u32| t.min(600))
        .unwrap_or(240);

    // 1. New Game
    verify_record_replay_session(
        &assets,
        "new_game",
        |rt| rt.new_game(0x95_1995),
        0xA2_0001,
        ticks,
    );

    // 2. Diverse DOS `.rpg` saves across chapters
    for (idx, file) in ["0.rpg", "5.rpg", "10.rpg", "25.rpg", "45.rpg"].iter().enumerate() {
        let path = assets.join("orig_saves").join(file);
        if let Ok(bytes) = std::fs::read(&path) {
            verify_record_replay_session(
                &assets,
                file,
                |rt| {
                    assert!(rt.load_dos_rpg(&bytes), "load {file}");
                },
                0xA2_1000 + idx as u64,
                ticks,
            );
        }
    }

    // 3. C10: Story-rail anchors extracted from bundled `test_data/replays/` `.repal` archives
    let mut repals = Vec::new();
    collect_repal_files(&workspace_root().join("test_data/replays"), &mut repals);
    repals.sort();
    let mut story_anchors_tested = 0usize;
    for (ri, rp) in repals.iter().take(4).enumerate() {
        let Ok(bundle) = read_repal_file(rp) else {
            continue;
        };
        let Some(save_anchor) = bundle.segments.iter().find_map(|s| match &s.anchor {
            ReplayAnchor::SaveJson(v) => Some(v.clone()),
            _ => None,
        }) else {
            continue;
        };
        let lbl = format!("story_rail_{}", rp.file_name().unwrap().to_string_lossy());
        verify_record_replay_session(
            &assets,
            &lbl,
            |rt| {
                assert!(rt.load_save_json_str(&save_anchor.to_string()));
            },
            0xC10_0000 + ri as u64,
            ticks,
        );
        story_anchors_tested += 1;
    }
    eprintln!(
        "[fuzz_runtime] A2 record->replay equivalence OK (NewGame + 5 DOS saves + {story_anchors_tested} .repal story-rail anchors)"
    );
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
    let out_dir = workspace_root().join(".tmp");
    let _ = std::fs::create_dir_all(&out_dir);
    let p = out_dir.join(format!("fuzz_parser_{target}_{:016x}.bin", fnv1a64(bytes)));
    let _ = std::fs::write(&p, bytes);
    p
}

fn make_tar_header(name: &str, size_octal: &str, bad_cksum: bool) -> [u8; 512] {
    let mut hdr = [0u8; 512];
    let nb = name.as_bytes();
    let nlen = nb.len().min(100);
    hdr[..nlen].copy_from_slice(&nb[..nlen]);
    hdr[100..108].copy_from_slice(b"0000644\0");
    let sb = size_octal.as_bytes();
    let slen = sb.len().min(12);
    hdr[124..124 + slen].copy_from_slice(&sb[..slen]);
    hdr[148..156].copy_from_slice(b"        ");
    hdr[156] = b'0';
    hdr[257..263].copy_from_slice(b"ustar\0");
    hdr[263..265].copy_from_slice(b"00");
    let mut cksum: u32 = hdr.iter().map(|&b| b as u32).sum();
    if bad_cksum {
        cksum = cksum.wrapping_add(1);
    }
    let cs = format!("{cksum:06o}\0 ");
    hdr[148..156].copy_from_slice(cs.as_bytes());
    hdr
}

fn gzip_bytes(raw: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    let _ = enc.write_all(raw);
    enc.finish().unwrap_or_default()
}

/// #273 D11: Structure-aware parser fuzzing for `read_repal_bytes` (gzip + USTAR tar + JSON)
/// with watchdog deadline and real `.repal` seed corpus.
#[test]
fn fuzz_parser_repal_archive_structure_aware() {
    let iters: usize = std::env::var("PAL_PARSER_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(250);

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let res = catch_unwind(AssertUnwindSafe(move || {
            let mut repals = Vec::new();
            collect_repal_files(&workspace_root().join("test_data/replays"), &mut repals);
            repals.sort();
            let mut seeds: Vec<Vec<u8>> = repals
                .iter()
                .take(4)
                .filter_map(|p| std::fs::read(p).ok())
                .collect();
            if seeds.is_empty() {
                seeds.push(b"{\"kind\":\"RePal_CoreReplay_v3\",\"segments\":[]}".to_vec());
            }

            let mut rng = FuzzRng::new(0xAE9A_1001);
            for i in 0..iters {
                let candidate: Vec<u8> = match i % 5 {
                    0 => {
                        // Corrupt tar octal size / huge size / duplicate names inside valid gzip
                        let mut tar = Vec::new();
                        let manifest = format!(
                            "{{\"kind\":\"RePal_CoreReplay_v3\",\"engine_version\":\"0.4.0\",\"total_ticks\":{},\"segments\":[{{\"index\":0,\"start_tick\":0,\"duration_ticks\":{},\"scene_id\":1,\"label\":\"x\",\"anchor_type\":\"new_game\",\"save_file\":\"a.json\",\"events_file\":\"e.json\"}}]}}",
                            if rng.range(2) == 0 { u64::MAX } else { 100 },
                            if rng.range(2) == 0 { u64::MAX } else { 99_999_999 }
                        );
                        let sz = if rng.range(3) == 0 {
                            "77777777777\0".to_string()
                        } else {
                            format!("{:011o}\0", manifest.len())
                        };
                        tar.extend_from_slice(&make_tar_header("manifest.json", &sz, rng.range(4) == 0));
                        tar.extend_from_slice(manifest.as_bytes());
                        let pad = (512 - (manifest.len() % 512)) % 512;
                        tar.resize(tar.len() + pad, 0);
                        // Duplicate manifest entry with broken JSON
                        let dup = b"{\"kind\":\"RePal_CoreReplay_v3\",\"segments\":[[[[[";
                        let dsz = format!("{:011o}\0", dup.len());
                        tar.extend_from_slice(&make_tar_header("manifest.json", &dsz, false));
                        tar.extend_from_slice(dup);
                        tar.resize(tar.len() + 1024, 0);
                        gzip_bytes(&tar)
                    }
                    1 => {
                        // Many tiny tar entries to stress entry-count caps
                        let mut tar = Vec::new();
                        let count = 32 + rng.range(128);
                        for k in 0..count {
                            let name = format!("f_{k}.json");
                            tar.extend_from_slice(&make_tar_header(&name, "00000000002\0", false));
                            tar.extend_from_slice(b"{}");
                            tar.resize(tar.len() + 510, 0);
                        }
                        gzip_bytes(&tar)
                    }
                    _ => {
                        // Mutate a real `.repal` seed (bit flips, truncation, splice)
                        let mut b = seeds[rng.range(seeds.len())].clone();
                        for _ in 0..(1 + rng.range(4)) {
                            if b.is_empty() {
                                break;
                            }
                            match rng.range(4) {
                                0 => {
                                    let idx = rng.range(b.len());
                                    b[idx] ^= 1u8 << (rng.next_u64() & 7);
                                }
                                1 => {
                                    let n = rng.range(b.len());
                                    b.truncate(n);
                                }
                                2 => {
                                    let idx = rng.range(b.len());
                                    b[idx] = 0xFF;
                                }
                                _ => {
                                    if b.len() < 128 * 1024 {
                                        let idx = rng.range(b.len());
                                        b.insert(idx, rng.next_u64() as u8);
                                    }
                                }
                            }
                        }
                        b
                    }
                };

                let r = catch_unwind(AssertUnwindSafe(|| {
                    let _ = read_repal_bytes(&candidate);
                }));
                if r.is_err() {
                    let p = dump_failure("repal", &candidate);
                    panic!("read_repal_bytes panicked at iter {i}; dumped to {}", p.display());
                }
            }
        }));
        let _ = tx.send(res);
    });

    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(payload)) => {
            let msg = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("non-string panic");
            panic!("[fuzz_runtime:repal_parser] panicked: {msg}");
        }
        Err(_) => panic!("[fuzz_runtime:repal_parser] WATCHDOG TIMEOUT after 30s"),
    }
}

/// Structured fuzzer for `RpkReader` (#237): mutates valid plain & encrypted `RPK1` packs
/// (header fields, TOC offsets, entry_count bombs, DEFLATE streams, AES-GCM tags) and asserts
/// zero panics and zero OOM/hangs.
#[test]
fn fuzz_rpk_reader_malformed_inputs() {
    use pal_runtime::rpk::{RpkBuilder, RpkReader, RpkWriteOptions};
    use std::io::Cursor;

    let key = [0x42u8; 32];
    let mut b1 = RpkBuilder::new(RpkWriteOptions {
        pack_id: "fuzz".into(),
        ..Default::default()
    });
    b1.add("data/test.yaml", b"scenes:\n  1:\n    name: yuhang\n".to_vec())
        .unwrap();
    b1.add("img/tile.webp", vec![0x52, 0x49, 0x46, 0x46, 0, 1, 2, 3, 4, 5, 6, 7])
        .unwrap();
    let plain_buf = b1.finish().unwrap();

    let mut enc_opts = RpkWriteOptions {
        pack_id: "fuzz".into(),
        content_key: Some(key),
        encrypt_toc: true,
        ..Default::default()
    };
    enc_opts.compress_toc = true;
    let mut b2 = RpkBuilder::new(enc_opts);
    b2.add("data/test.yaml", b"scenes:\n  1:\n    name: yuhang\n".to_vec())
        .unwrap();
    b2.add("img/tile.webp", vec![0x52, 0x49, 0x46, 0x46, 0, 1, 2, 3, 4, 5, 6, 7])
        .unwrap();
    let enc_buf = b2.finish().unwrap();

    let seeds = [plain_buf, enc_buf];
    let mut rng = FuzzRng::new(0x5250_4B31_F022);

    for i in 0..1000usize {
        let candidate = if i % 5 == 0 {
            let len = rng.range(512);
            (0..len).map(|_| rng.next_u64() as u8).collect::<Vec<u8>>()
        } else {
            let mut b = seeds[i % seeds.len()].clone();
            let mutations = 1 + rng.range(6);
            for _ in 0..mutations {
                if b.is_empty() {
                    break;
                }
                match rng.range(4) {
                    0 => {
                        let idx = rng.range(b.len());
                        b[idx] ^= 1u8 << (rng.next_u64() & 7);
                    }
                    1 => {
                        let n = rng.range(b.len());
                        b.truncate(n);
                    }
                    2 => {
                        let idx = rng.range(b.len());
                        b[idx] = 0xFF;
                    }
                    _ => {
                        let idx = rng.range(b.len());
                        b[idx] = 0x00;
                    }
                }
            }
            b
        };

        let r = catch_unwind(AssertUnwindSafe(|| {
            if let Ok(mut reader) = RpkReader::open(Cursor::new(candidate.as_slice()), "fuzz", Some(key)) {
                let keys: Vec<String> = reader.keys().map(str::to_string).collect();
                for k in keys {
                    let _ = reader.read_entry(&k);
                }
            }
            if let Ok(mut reader) = RpkReader::open(Cursor::new(candidate.as_slice()), "fuzz", None) {
                let keys: Vec<String> = reader.keys().map(str::to_string).collect();
                for k in keys {
                    let _ = reader.read_entry(&k);
                }
            }
        }));
        assert!(r.is_ok(), "RpkReader panicked at fuzz iteration {i}");
    }
}
