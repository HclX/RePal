#![cfg(feature = "disk")]

use pal_core::game::replay_rec::ReplayAnchor;
use pal_core::game::{HostCmd, HostReply};
use pal_runtime::disk::{boot_runtime_from_dir, load_scene_assets_from_dir};
use pal_runtime::repal_archive::read_repal_file;
use pal_runtime::replay_player::{ReplayPlayer, ReplayStepOutcome};
use pal_runtime::PalRuntime;
use std::path::PathBuf;

fn service_host(rt: &mut PalRuntime, assets_root: &std::path::Path) {
    loop {
        rt.route_cmds();
        let Some(cmd) = rt.out_cmds.pop_front() else {
            break;
        };
        match cmd {
            HostCmd::LoadScene { map_id, .. } => {
                if let Some(g) = rt.game.as_mut() {
                    load_scene_assets_from_dir(assets_root, g, map_id);
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

fn collect_repal_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
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

#[test]
fn verify_real_repal_archives_playback() {
    let Ok(dir) = std::env::var("REPAL_VERIFY_DIR") else {
        return;
    };
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let assets_root = workspace_root.join("site/assets");
    let raw_dir = PathBuf::from(&dir);
    let verify_root = if raw_dir.is_dir() {
        raw_dir
    } else {
        workspace_root.join(&raw_dir)
    };

    let mut files = Vec::new();
    collect_repal_files(&verify_root, &mut files);
    files.sort();
    assert!(!files.is_empty(), "no .repal files in {} (resolved: {})", dir, verify_root.display());

    let mut rt = boot_runtime_from_dir(&assets_root, false).expect("boot runtime");
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bundle = read_repal_file(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        rt.set_in_title(false);

        let mut seg_matches = 0usize;
        let mut seg_drifts = 0usize;
        let mut total_events_played = 0usize;

        let mut player = ReplayPlayer::new(bundle).unwrap_or_else(|e| panic!("{name}: {e}"));
        let n_segs = player.bundle.segments.len();

        println!(
            "\n=== Replaying {} ({} segments, {} ticks) ===",
            name, n_segs, player.bundle.total_ticks
        );

        assert!(player.load_segment(0, &mut rt), "{name} seg 0: failed to load anchor");
        service_host(&mut rt, &assets_root);

        let mut seg_ev_count = 0usize;
        loop {
            let cur_seg = player.seg_idx;
            let outcome = player.step_one_tick(
                &mut rt,
                |_, _| {},
                |rt, _ev| {
                    total_events_played += 1;
                    seg_ev_count += 1;
                    service_host(rt, &assets_root);
                },
            );
            service_host(&mut rt, &assets_root);

            match outcome {
                ReplayStepOutcome::Stepped => {}
                ReplayStepOutcome::SegmentCompleted { next_idx } => {
                    let seg_tick = player.seg_tick;
                    if let Some(next_seg) = player.bundle.segments.get(next_idx) {
                        if let ReplayAnchor::SaveJson(exp_save) = &next_seg.anchor {
                            let g = rt.game.as_ref().unwrap();
                            let exp_sc = exp_save["sceneId"].as_u64().unwrap_or(0) as u16;
                            let exp_cash = exp_save["cash"].as_u64().unwrap_or(0) as u32;
                            let exp_x = exp_save["party"][0]["x"].as_i64().unwrap_or(0) as i32;
                            let exp_y = exp_save["party"][0]["y"].as_i64().unwrap_or(0) as i32;
                            let exp_rng = exp_save["rng"].as_u64().unwrap_or(0) as u32;
                            let lead = g.world.leader();
                            let exact = g.world.scene_id == exp_sc
                                && g.world.cash == exp_cash
                                && lead.x == exp_x
                                && lead.y == exp_y
                                && g.world.rng.state == exp_rng;
                            if exact {
                                seg_matches += 1;
                                println!(
                                    "  [OK] seg {:02} ({}t, {}ev) -> sc#{} ({},{}) cash={} rng={}",
                                    cur_seg, seg_tick, seg_ev_count, exp_sc, exp_x, exp_y, exp_cash, exp_rng
                                );
                            } else {
                                seg_drifts += 1;
                                println!(
                                    "  [DRIFT] seg {:02} ({}t, {}ev): got sc#{} ({},{}) cash={} rng={} | exp sc#{} ({},{}) cash={} rng={}",
                                    cur_seg,
                                    seg_tick,
                                    seg_ev_count,
                                    g.world.scene_id,
                                    lead.x,
                                    lead.y,
                                    g.world.cash,
                                    g.world.rng.state,
                                    exp_sc,
                                    exp_x,
                                    exp_y,
                                    exp_cash,
                                    exp_rng
                                );
                            }
                        }
                    }
                    seg_ev_count = 0;
                    assert!(
                        player.load_segment(next_idx, &mut rt),
                        "{name} seg {next_idx}: failed to load anchor"
                    );
                    service_host(&mut rt, &assets_root);
                }
                ReplayStepOutcome::Finished => {
                    if let Some(g) = rt.game.as_ref() {
                        let lead = g.world.leader();
                        println!(
                            "  [END] seg {:02} ({}t, {}ev) -> sc#{} ({},{}) cash={} lv={} rng={}",
                            cur_seg,
                            player.seg_tick,
                            seg_ev_count,
                            g.world.scene_id,
                            lead.x,
                            lead.y,
                            g.world.cash,
                            g.world.roles.first().map(|r| r.level).unwrap_or(0),
                            g.world.rng.state,
                        );
                    }
                    break;
                }
            }
        }
        println!(
            "  => Summary {}: {} events played, {} exact boundary matches, {} drifts",
            name, total_events_played, seg_matches, seg_drifts
        );
    }
}

#[test]
fn boat_ride_lockstep_and_frames() {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let assets_root = workspace_root.join("site/assets");
    if !assets_root.join("data/scenes.yaml").is_file() {
        return;
    }

    let mut rt = boot_runtime_from_dir(&assets_root, true).expect("boot runtime");
    rt.set_in_title(false);
    rt.ui.set_view(pal_ui::UiView::None);
    rt.world.fbp_override = None;
    service_host(&mut rt, &assets_root);

    // Enter Scene #5 (盛渔村·菜市场与码头), enable Zhang Sige rowing ev#118, and trigger boat ev#117 (`s_1725`).
    if let Some(g) = rt.game.as_mut() {
        g.enter_scene(5, false);
        g.world.party[0].x = 1136;
        g.world.party[0].y = 1368;
        if let Some(e118) = g.world.event_mut(118) {
            e118.state = 2;
        }
        let lbl = g.prog.intern("s_1725");
        if let Some(e117) = g.world.event_mut(117) {
            e117.trigger = pal_core::script::ScriptPos::new(lbl, 0);
        }
    }
    service_host(&mut rt, &assets_root);
    if let Some(g) = rt.game.as_mut() {
        g.trigger_event(117);
    }
    service_host(&mut rt, &assets_root);

    let dump_dir = std::env::var("BOAT_DUMP_DIR").ok().map(PathBuf::from);
    if let Some(d) = &dump_dir {
        let _ = std::fs::create_dir_all(d);
    }

    let mut initial_delta: Option<(i32, i32)> = None;
    let mut observed_ride_steps = 0usize;
    let mut frames_seen = std::collections::HashSet::new();
    let mut dumped = 0usize;

    for tick in 0..400 {
        rt.tick(pal_core::game::TickInput::default());
        service_host(&mut rt, &assets_root);
        let (sc, lead_x, lead_y, e117, e118) = {
            let g = rt.game.as_ref().unwrap();
            let l = g.world.leader();
            let a = g.world.event(117).map(|e| (e.x, e.y, e.current_frame));
            let b = g.world.event(118).map(|e| (e.x, e.y, e.current_frame, !e.auto.is_none()));
            (g.world.scene_id, l.x, l.y, a, b)
        };
        if sc != 5 {
            break;
        }
        if let (Some((x117, y117, _)), Some((x118, y118, f118, has_auto))) = (e117, e118) {
            if has_auto {
                let delta = (x117 - x118, y117 - y118);
                if let Some(d0) = initial_delta {
                    assert_eq!(
                        delta, d0,
                        "tick {tick}: Zhang Sige ev#118 drifted from boat ev#117! delta={delta:?} vs initial {d0:?} (party=({lead_x},{lead_y}), 117=({x117},{y117}), 118=({x118},{y118}))"
                    );
                } else {
                    initial_delta = Some(delta);
                }
                observed_ride_steps += 1;
                frames_seen.insert(f118);
                if let Some(d) = &dump_dir {
                    if dumped < 5 && observed_ride_steps % 8 == 1 {
                        // Pump render assets and draw one frame to PNG
                        rt.render_frame();
                        let _ = pal_runtime::disk::pump_render_assets_from_dir(&assets_root, &mut rt.compositor.cache);
                        rt.render_frame();
                        let _ = pal_runtime::disk::pump_render_assets_from_dir(&assets_root, &mut rt.compositor.cache);
                        rt.render_frame();
                        let fb = &rt.compositor.fb;
                        let out = d.join(format!("boat_f{dumped}.png"));
                        let img = image::RgbaImage::from_raw(fb.width, fb.height, fb.pixels.clone())
                            .expect("rgba image");
                        img.save(&out).expect("save png");
                        println!(
                            "  dumped {} (tick={} party=({},{}) boat117=({},{}) zhang118=({},{}) f={})",
                            out.display(),
                            tick,
                            lead_x,
                            lead_y,
                            x117,
                            y117,
                            x118,
                            y118,
                            f118
                        );
                        dumped += 1;
                    }
                }
            }
        }
    }

    assert!(
        observed_ride_steps >= 20,
        "expected >=20 boat ride steps, got {observed_ride_steps}"
    );
    assert!(
        frames_seen.len() >= 2,
        "expected Zhang Sige rowing animation frames to advance, saw {:?}",
        frames_seen
    );
}
