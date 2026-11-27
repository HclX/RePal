#![cfg(feature = "disk")]
//! `PalRuntime::frame_unchanged` must never report "unchanged" when a fresh render would differ:
//! play real replays tick by tick, render every tick anyway (pumping assets like the desktop
//! host), and require a byte-identical framebuffer whenever the check said "unchanged".
//! `PAL_FRAME_SKIP_TICKS` sets ticks per replay (default 2500).

use pal_core::game::{HostCmd, HostReply};
use pal_runtime::disk::{boot_runtime_from_dir, load_scene_assets_from_dir, pump_render_assets_from_vfs};
use pal_runtime::pack_vfs::OverlayVfs;
use pal_runtime::repal_archive::read_repal_file;
use pal_runtime::replay_player::{ReplayPlayer, ReplayStepOutcome};
use pal_runtime::PalRuntime;
use std::path::{Path, PathBuf};

fn service_host(rt: &mut PalRuntime, assets_root: &Path) {
    loop {
        rt.route_cmds();
        let Some(cmd) = rt.out_cmds.pop_front() else { break };
        match cmd {
            HostCmd::LoadScene { map_id, .. } => {
                if let Some(g) = rt.game.as_mut() {
                    load_scene_assets_from_dir(assets_root, g, map_id);
                }
                rt.reply(HostReply::SceneLoaded);
            }
            HostCmd::LoadChapter(_) | HostCmd::PlayRng { .. } => rt.reply(HostReply::Done),
            _ => {}
        }
    }
}

fn render_like_desktop(rt: &mut PalRuntime, vfs: &mut OverlayVfs) {
    for _ in 0..6 {
        if rt.ui.dirty {
            rt.compositor.mark_ui_dirty();
            rt.ui.dirty = false;
        }
        rt.render_frame();
        if !rt.compositor.cache.has_pending_requests() {
            break;
        }
        pump_render_assets_from_vfs(vfs, &mut rt.compositor.cache);
        rt.compositor.mark_ui_dirty();
    }
}

#[test]
fn skipped_frames_are_identical_to_fresh_renders() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let assets = root.join("site/assets");
    if !assets.join("data/scenes.yaml").is_file() {
        eprintln!("skip: assets not found");
        return;
    }
    let ticks: usize = std::env::var("PAL_FRAME_SKIP_TICKS").ok().and_then(|s| s.parse().ok()).unwrap_or(2500);
    let replays = [
        "site/replays/v0.3.8/01_ch1_yuhang_opening.repal.gz",
        "site/replays/v0.3.8/06_ch2_suzhou_full_to_hidden_dragon_cave.repal.gz",
    ];
    for (hd, rel) in [(true, replays[0]), (true, replays[1]), (false, replays[1])] {
        let mut rt = boot_runtime_from_dir(&assets, hd).expect("boot");
        rt.set_in_title(false);
        let mut vfs = OverlayVfs::from_loose_dir(assets.clone());
        let bundle = read_repal_file(&root.join(rel)).expect("read replay");
        let mut player = ReplayPlayer::new(bundle).expect("player");
        assert!(player.load_segment(0, &mut rt));
        service_host(&mut rt, &assets);

        let (mut unchanged, mut rendered) = (0usize, 0usize);
        let mut prev: Vec<u8> = Vec::new();
        for tick in 0..ticks {
            let out = player.step_one_tick(&mut rt, |_, _| {}, |rt, _| service_host(rt, &assets));
            service_host(&mut rt, &assets);
            if let ReplayStepOutcome::SegmentCompleted { next_idx } = out {
                player.load_segment(next_idx, &mut rt);
                service_host(&mut rt, &assets);
            } else if matches!(out, ReplayStepOutcome::Finished) {
                break;
            }
            let same = rt.frame_unchanged();
            render_like_desktop(&mut rt, &mut vfs);
            if same {
                unchanged += 1;
                assert!(
                    rt.compositor.fb.pixels == prev,
                    "{rel} hd={hd}: tick {tick} reported unchanged but the fresh render differs"
                );
            }
            rendered += 1;
            prev.clone_from(&rt.compositor.fb.pixels);
        }
        eprintln!("[frame_skip] {rel} hd={hd}: {unchanged}/{rendered} ticks unchanged ({:.0}%)", 100.0 * unchanged as f64 / rendered.max(1) as f64);
    }
}
