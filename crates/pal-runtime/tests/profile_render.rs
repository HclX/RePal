#![cfg(all(unix, feature = "disk"))]
//! Opt-in CPU profile of the desktop-equivalent frame loop (not run by default):
//!
//!   RUSTFLAGS="-C force-frame-pointers=yes" CARGO_TARGET_DIR=target/profile \
//!   CARGO_PROFILE_RELEASE_DEBUG=true CARGO_PROFILE_RELEASE_STRIP=false \
//!   cargo test --release -p pal-runtime --features disk \
//!     --test profile_render -- --ignored --nocapture
//!
//! Plays an HD replay at 50 Hz ticks, draws every 4th tick (the 12.5 fps cap) unless the frame is
//! unchanged, loads assets on demand, and samples at ~1 kHz with `pprof`. Writes
//! `.tmp/profile/flamegraph.svg` and prints self / inclusive time per function.
//! `PAL_PROFILE_REPLAY` / `PAL_PROFILE_TICKS` override the replay and length.

use pal_core::game::{HostCmd, HostReply};
use pal_runtime::disk::{boot_runtime_from_dir, load_scene_assets_from_dir, pump_render_assets_from_vfs};
use pal_runtime::pack_vfs::OverlayVfs;
use pal_runtime::repal_archive::read_repal_file;
use pal_runtime::replay_player::{ReplayPlayer, ReplayStepOutcome};
use pal_runtime::PalRuntime;
use std::collections::HashMap;
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

fn draw(rt: &mut PalRuntime, vfs: &mut OverlayVfs) {
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
#[ignore]
fn profile_hd_replay_frame_loop() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let assets = root.join("site/assets");
    let replay = std::env::var("PAL_PROFILE_REPLAY")
        .unwrap_or_else(|_| "site/replays/v0.3.8/06_ch2_suzhou_full_to_hidden_dragon_cave.repal.gz".into());
    let ticks: usize = std::env::var("PAL_PROFILE_TICKS").ok().and_then(|s| s.parse().ok()).unwrap_or(12000);

    let mut rt = boot_runtime_from_dir(&assets, true).expect("boot");
    rt.set_in_title(false);
    if let Some(mb) = std::env::var("PAL_PROFILE_CACHE_MB").ok().and_then(|s| s.parse::<usize>().ok()) {
        rt.compositor.cache.max_texture_bytes = mb * 1024 * 1024;
    }
    let mut vfs = OverlayVfs::from_loose_dir(assets.clone());
    let mut player = ReplayPlayer::new(read_repal_file(&root.join(&replay)).expect("replay")).expect("player");
    assert!(player.load_segment(0, &mut rt));
    service_host(&mut rt, &assets);

    let guard = pprof::ProfilerGuardBuilder::default()
        .frequency(997)
        .build()
        .expect("profiler");
    let t0 = std::time::Instant::now();
    let (mut drawn, mut skipped) = (0usize, 0usize);
    for tick in 0..ticks {
        let out = player.step_one_tick(&mut rt, |_, _| {}, |rt, _| service_host(rt, &assets));
        service_host(&mut rt, &assets);
        match out {
            ReplayStepOutcome::SegmentCompleted { next_idx } => {
                player.load_segment(next_idx, &mut rt);
                service_host(&mut rt, &assets);
            }
            ReplayStepOutcome::Finished => break,
            ReplayStepOutcome::Stepped => {}
        }
        if tick % 4 == 0 {
            if rt.frame_unchanged() {
                skipped += 1;
            } else {
                draw(&mut rt, &mut vfs);
                drawn += 1;
            }
        }
    }
    let wall = t0.elapsed();
    let c = &rt.compositor.cache;
    eprintln!(
        "[cache] uploads={} evictions={} re-uploads after eviction={} resident={} MB / {} MB",
        c.stats_uploads, c.stats_evictions, c.stats_reuploads,
        c.total_texture_bytes() / (1024 * 1024), c.max_texture_bytes / (1024 * 1024)
    );
    let report = guard.report().build().expect("report");

    let out_dir = root.join(".tmp/profile");
    std::fs::create_dir_all(&out_dir).unwrap();
    let svg = out_dir.join("flamegraph.svg");
    report.flamegraph(std::fs::File::create(&svg).unwrap()).expect("flamegraph");

    // Self (leaf) and inclusive sample counts per function name.
    let (mut self_t, mut incl_t, mut total) = (HashMap::<String, usize>::new(), HashMap::<String, usize>::new(), 0usize);
    for (frames, count) in &report.data {
        let count = (*count).max(0) as usize;
        total += count;
        let names: Vec<String> = frames
            .frames
            .iter()
            .flat_map(|f| f.iter().map(|s| s.name()))
            .map(|n| {
                // Drop the trailing ::h<hash> and generic noise for readability.
                let n = n.rsplit_once("::h").map(|(a, _)| a.to_string()).unwrap_or(n);
                n.replace("pal_render::", "R::").replace("pal_runtime::", "RT::").replace("pal_core::", "C::")
            })
            .collect();
        if let Some(leaf) = names.first() {
            *self_t.entry(leaf.clone()).or_default() += count;
        }
        let mut seen = std::collections::HashSet::new();
        for n in names {
            if seen.insert(n.clone()) {
                *incl_t.entry(n).or_default() += count;
            }
        }
    }
    if std::env::var_os("PAL_PROFILE_DEBUG").is_some() {
        let mut v: Vec<_> = report.data.iter().collect();
        v.sort_by_key(|(f, _)| std::cmp::Reverse(f.frames.len()));
        eprintln!("[stack] {} distinct stacks; longest has {} frames", v.len(), v.first().map(|(f, _)| f.frames.len()).unwrap_or(0));
        for (frames, count) in v.into_iter().take(2) {
            let names: Vec<String> = frames.frames.iter().flat_map(|f| f.iter().map(|s| s.name())).collect();
            eprintln!("[stack x{count}] {} frames: {:?}", names.len(), names.iter().take(8).collect::<Vec<_>>());
        }
    }
    let pct = |c: usize| 100.0 * c as f64 / total.max(1) as f64;
    let top = |m: &HashMap<String, usize>, n: usize, filter: &dyn Fn(&str) -> bool| {
        let mut v: Vec<_> = m.iter().filter(|(k, _)| filter(k)).collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        v.into_iter().take(n).map(|(k, c)| format!("  {:5.1}%  {k}", pct(*c))).collect::<Vec<_>>().join("\n")
    };
    eprintln!(
        "[profile] {replay}: {ticks} ticks in {:.1}s wall, {drawn} frames drawn, {skipped} skipped, {total} samples\n\
         (R:: = pal_render, RT:: = pal_runtime, C:: = pal_core)\n\
         -- self time (top 25) --\n{}\n-- inclusive, crate functions (top 30) --\n{}\nflamegraph: {}",
        wall.as_secs_f64(),
        top(&self_t, 25, &|_| true),
        top(&incl_t, 30, &|k| k.contains("R::") || k.contains("RT::") || k.contains("C::") || k.contains("image::") || k.contains("serde_json::")),
        svg.display()
    );
}
