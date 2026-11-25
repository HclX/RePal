//! Browserless Visual & Interactive E2E Tests for PalRemake Rust Renderer + UI.

use pal_headless::replay::{
    apply_named_view, compare_with_golden_png, execute_steps, parse_recording_file,
};
use pal_headless::HeadlessApp;
use pal_ui::{UiKey, UiView};
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is .../crates/pal-headless
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn e2e_title_hd_and_retro_export() {
    let root = repo_root();
    let assets = root.join("site/assets");
    let out_dir = root.join("target/headless_shots");

    // HD Title
    let mut app_hd = HeadlessApp::new(&assets, true);
    app_hd.render_settled_frame();
    assert_eq!((app_hd.compositor.fb.width, app_hd.compositor.fb.height), (1280, 800));
    assert!(app_hd.non_black_pixel_ratio() > 0.35, "HD title should render fbp_060");
    app_hd.export_png(out_dir.join("01_title_hd.png")).unwrap();

    // Retro Title
    let mut app_retro = HeadlessApp::new(&assets, false);
    app_retro.render_settled_frame();
    assert_eq!((app_retro.compositor.fb.width, app_retro.compositor.fb.height), (320, 200));
    assert!(app_retro.non_black_pixel_ratio() > 0.35, "Retro title should render fbp_060");
    app_retro.export_png(out_dir.join("02_title_retro.png")).unwrap();
}

#[test]
fn e2e_field_menus_status_equip_magic_items_system_pawnshop_battle() {
    let root = repo_root();
    let assets = root.join("site/assets");
    let save10 = assets.join("orig_saves/10.rpg");
    let out_dir = root.join("target/headless_shots");

    let mut app = HeadlessApp::new(&assets, true);
    app.load_rpg_file(&save10).expect("load 10.rpg");

    // 1. Field screen
    app.render_settled_frame();
    assert!(app.non_black_pixel_ratio() > 0.40, "Field map should render");
    app.export_png(out_dir.join("03_field_hd.png")).unwrap();

    // 2. Open Root Menu via MenuToggle (Alt/Esc)
    app.press_key(UiKey::MenuToggle);
    assert_eq!(app.ui.view, UiView::RootMenu);
    app.render_settled_frame();
    app.export_png(out_dir.join("04_root_menu.png")).unwrap();

    // 3. Status screen (FBP_000)
    app.press_key(UiKey::Confirm);
    if matches!(app.ui.view, UiView::RoleSelect(_)) {
        app.press_key(UiKey::Confirm);
    }
    assert_eq!(app.ui.view, UiView::StatusFbp000);
    app.render_settled_frame();
    assert!(app.non_black_pixel_ratio() > 0.50);
    app.export_png(out_dir.join("05_status_fbp000.png")).unwrap();

    // Back to RootMenu -> 物品 -> 裝備 (FBP_001)
    app.press_key(UiKey::Cancel);
    assert_eq!(app.ui.view, UiView::RootMenu);
    app.press_key(UiKey::Down); // 仙術
    app.press_key(UiKey::Down); // 物品
    app.press_key(UiKey::Confirm);
    assert_eq!(app.ui.view, UiView::ItemSubmenu);
    app.press_key(UiKey::Down); // 裝備
    app.press_key(UiKey::Confirm);
    if matches!(app.ui.view, UiView::RoleSelect(_)) {
        app.press_key(UiKey::Confirm);
    }
    assert_eq!(app.ui.view, UiView::EquipFbp001);
    app.render_settled_frame();
    app.export_png(out_dir.join("06_equip_fbp001.png")).unwrap();

    // Back to ItemSubmenu -> 使用 (3-col ItemGrid)
    app.press_key(UiKey::Cancel);
    assert_eq!(app.ui.view, UiView::ItemSubmenu);
    app.press_key(UiKey::Up); // 使用
    app.press_key(UiKey::Confirm);
    assert_eq!(app.ui.view, UiView::ItemGrid);
    app.render_settled_frame();
    app.export_png(out_dir.join("07_item_grid.png")).unwrap();

    // Back to RootMenu -> 仙術 (3-col MagicGrid)
    app.press_key(UiKey::Cancel);
    app.press_key(UiKey::Cancel);
    assert_eq!(app.ui.view, UiView::RootMenu);
    app.press_key(UiKey::Up); // 仙術
    app.press_key(UiKey::Confirm);
    if matches!(app.ui.view, UiView::RoleSelect(_)) {
        app.press_key(UiKey::Confirm);
    }
    assert_eq!(app.ui.view, UiView::MagicGrid);
    app.render_settled_frame();
    app.export_png(out_dir.join("08_magic_grid.png")).unwrap();

    // Back to RootMenu -> 系統 submenu
    app.press_key(UiKey::Cancel);
    app.press_key(UiKey::Down); // 物品
    app.press_key(UiKey::Down); // 系統
    app.press_key(UiKey::Confirm);
    assert_eq!(app.ui.view, UiView::SystemSubmenu);
    app.render_settled_frame();
    app.export_png(out_dir.join("09_system_submenu.png")).unwrap();

    // Pawnshop sell view
    let st_clone = app.state.clone();
    app.ui.open_pawnshop_from_inventory(&st_clone);
    assert_eq!(app.ui.view, UiView::PawnshopSell);
    app.render_settled_frame();
    app.export_png(out_dir.join("10_pawnshop_sell.png")).unwrap();

    // Battle scene with target cursor
    app.enter_sample_battle(10);
    app.press_key(UiKey::Confirm); // Select enemy target
    app.render_settled_frame();
    assert!(app.non_black_pixel_ratio() > 0.50);
    app.export_png(out_dir.join("11_battle_hd.png")).unwrap();
}

#[test]
fn regression_scene1_hero_and_walking_changes_framebuffer() {
    let root = repo_root();
    let assets = root.join("site/assets");
    let save0 = assets.join("orig_saves/0.rpg");

    let mut app = HeadlessApp::new(&assets, true);
    app.load_rpg_file(&save0).expect("load 0.rpg");
    assert_eq!(app.active_map_id(), 12, "Scene 1 must resolve to map_012");
    assert_eq!(app.world.ev_start, 1);
    assert_eq!(app.world.ev_end, 32);

    let start_pos = (app.state.party[0].x, app.state.party[0].y);
    app.render_settled_frame();
    let fb_before = app.compositor.fb.pixels.clone();

    let mut moved_steps = 0;
    for _ in 0..3 {
        if app.step_walk(0) {
            moved_steps += 1;
        }
    }
    assert!(moved_steps >= 2, "Li Xiaoyao should walk freely SW in bedroom");
    let end_pos = (app.state.party[0].x, app.state.party[0].y);
    assert_ne!(start_pos, end_pos);

    app.render_settled_frame();
    let changed_bytes = app
        .compositor
        .fb
        .pixels
        .iter()
        .zip(fb_before.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        changed_bytes > 50_000,
        "Walking must visibly update the rendered framebuffer"
    );
}

#[test]
fn regression_recording_replay_and_deterministic_golden_diff() {
    let root = repo_root();
    let assets = root.join("site/assets");
    let rec_path = root.join("crates/pal-headless/recordings/smoke_walk_and_menus.rec");
    let shot_dir = root.join("target/headless_shots/replay");

    let steps = parse_recording_file(&rec_path).expect("parse smoke_walk_and_menus.rec");
    assert!(steps.len() >= 15);

    // Run 1: produce checkpoint PNGs
    let mut app1 = HeadlessApp::new(&assets, true);
    let shots = execute_steps(&mut app1, &steps, &shot_dir).expect("execute replay run 1");
    assert_eq!(shots.len(), 7, "recording should export 7 checkpoint PNGs");
    app1.render_settled_frame();
    let golden_final = shot_dir.join("rec_07_field_retro.png");
    assert!(golden_final.exists());
    // Free Run 1's AssetCache and framebuffers before allocating Run 2 so peak RAM stays halved.
    drop(app1);

    // Run 2: replay identically into a separate output directory and verify 0.000% pixel diff
    // against Run 1's golden PNG (never overwriting Run 1's output file).
    let shot_dir_2 = root.join("target/headless_shots/replay_run2");
    let mut app2 = HeadlessApp::new(&assets, true);
    let _ = execute_steps(&mut app2, &steps, &shot_dir_2).expect("execute replay run 2");
    app2.render_settled_frame();
    let report = compare_with_golden_png(&app2, &golden_final, None, 0).expect("golden diff");
    assert_eq!(report.diff_pixels, 0, "Replay rendering must be 100% deterministic");
    assert_eq!(report.diff_pct, 0.0);

    // Negative check: different view must trigger a measurable diff
    apply_named_view(&mut app2, "status").unwrap();
    app2.render_settled_frame();
    let diff_report = compare_with_golden_png(
        &app2,
        &golden_final,
        Some(&shot_dir.join("diff_status_vs_field.png")),
        2,
    )
    .expect("negative golden diff");
    assert!(
        diff_report.diff_pct > 25.0,
        "Status screen vs field must report >25% pixel diff (got {:.2}%)",
        diff_report.diff_pct
    );
}

#[test]
fn regression_retro_mode_full_coverage() {
    let root = repo_root();
    let assets = root.join("site/assets");
    let save10 = assets.join("orig_saves/10.rpg");

    let mut app = HeadlessApp::new(&assets, false);
    app.load_rpg_file(&save10).expect("load 10.rpg");
    for view in ["title", "field", "menu", "status", "equip", "battle"] {
        apply_named_view(&mut app, view).unwrap();
        app.render_settled_frame();
        assert_eq!((app.compositor.fb.width, app.compositor.fb.height), (320, 200));
        assert!(
            app.non_black_pixel_ratio() > 0.35,
            "Retro view '{}' rendered too dark ({:.1}%)",
            view,
            app.non_black_pixel_ratio() * 100.0
        );
    }
}

#[test]
fn regression_track_b_hd_maps_spatial_occluders_and_rng_cutscene() {
    let root = repo_root();
    let assets = root.join("site/assets");

    let mut app = HeadlessApp::new(&assets, true);
    assert!(app.compositor.fonts.has_ttf(), "Bundled pal-cjk.ttf should load in pal-headless");

    // 1. Scene 60 -> Map 055 (densest HD map: 4,966 strips across 25 spatial <=2048x2048 pages!)
    app.set_scene(60);
    app.state.party[0].x = 1024;
    app.state.party[0].y = 1024;
    app.render_settled_frame();
    let meta = app
        .compositor
        .cache
        .get_hd_map("graphics/maps_hd/map_055/meta.json")
        .expect("Map 055 Track-B HD meta.json must load in pal-render");
    assert!(!meta.rooms.is_empty());
    assert!(meta.occluder_pages.len() >= 10, "Map 055 should use spatial occluder pages");
    // Viewport-only streaming at the worst-case 4-quadrant corner (1024,1024) must stay well below the 256 MB LRU budget
    // (previously a single monolithic occluders_4x.webp decoded to 281.5 MB RGBA by itself!)
    assert!(
        app.compositor.cache.total_texture_bytes() < 220 * 1024 * 1024,
        "Map 055 viewport streaming exceeded 220 MB: {} bytes",
        app.compositor.cache.total_texture_bytes()
    );
    assert!(
        app.non_black_pixel_ratio() > 0.40,
        "Map 055 HD field rendered too dark ({:.1}%)",
        app.non_black_pixel_ratio() * 100.0
    );

    // 2. RNG opening cutscene playback in HD and 1x Retro modes
    app.world.active_rng = Some((0, 0));
    app.render_settled_frame();
    assert!(app.non_black_pixel_ratio() > 0.15, "HD RNG frame 0 should render");
    app.set_hd_mode(false);
    app.render_settled_frame();
    assert_eq!((app.compositor.fb.width, app.compositor.fb.height), (320, 200));
    assert!(app.non_black_pixel_ratio() > 0.15, "Retro 1x RNG frame 0 should render");
}

fn mean_rgb(pixels: &[u8]) -> [f64; 3] {
    let mut sum = [0u64; 3];
    let n = (pixels.len() / 4).max(1) as f64;
    for px in pixels.chunks_exact(4) {
        sum[0] += px[0] as u64;
        sum[1] += px[1] as u64;
        sum[2] += px[2] as u64;
    }
    [sum[0] as f64 / n, sum[1] as f64 / n, sum[2] as f64 / n]
}

#[test]
fn regression_issue_35_night_palette_retro_and_hd_parity() {
    let root = repo_root();
    let assets = root.join("site/assets");
    let save3 = assets.join("orig_saves/3.rpg");

    let mut app = HeadlessApp::new(&assets, false);
    app.load_rpg_file(&save3).expect("load 3.rpg");
    assert!(app.state.night_palette, "3.rpg should have pal_off != 0");
    assert!(
        app.world.night_palette,
        "load_rpg_file must forward night_palette to world"
    );

    app.render_settled_frame();
    let retro_night = mean_rgb(&app.compositor.fb.pixels);

    app.world.night_palette = false;
    app.render_settled_frame();
    let retro_day = mean_rgb(&app.compositor.fb.pixels);

    let retro_dr = retro_day[0] - retro_night[0];
    let retro_db = retro_day[2] - retro_night[2];
    assert!(
        retro_dr > 15.0,
        "Retro night tint must darken red channel significantly (day={:.1}, night={:.1})",
        retro_day[0],
        retro_night[0]
    );
    assert!(
        retro_dr > retro_db,
        "Night tint (18,32,68) must attenuate red ({:.1}) more than blue ({:.1})",
        retro_dr,
        retro_db
    );

    // Switch live to HD and verify parity
    app.world.night_palette = true;
    app.set_hd_mode(true);
    app.render_settled_frame();
    let hd_night = mean_rgb(&app.compositor.fb.pixels);

    app.world.night_palette = false;
    app.render_settled_frame();
    let hd_day = mean_rgb(&app.compositor.fb.pixels);

    let hd_dr = hd_day[0] - hd_night[0];
    assert!(
        hd_dr > 15.0,
        "HD night tint must darken red channel significantly (day={:.1}, night={:.1})",
        hd_day[0],
        hd_night[0]
    );
}

#[test]
fn regression_issue_35_screen_wave_hd_and_retro_parity() {
    let root = repo_root();
    let assets = root.join("site/assets");
    let save14 = assets.join("orig_saves/14.rpg");

    let mut app = HeadlessApp::new(&assets, true);
    app.load_rpg_file(&save14).expect("load 14.rpg");
    assert_eq!(app.state.screen_wave, 2, "14.rpg should have screen_wave=2");
    assert!(
        (app.world.wave_amplitude - 3.6).abs() < 0.01,
        "load_rpg_file must forward wave_amplitude=3.6, got {}",
        app.world.wave_amplitude
    );

    // Settle assets first, then compare unwaved vs waved HD frame at exact frame_count
    app.render_settled_frame();
    app.world.wave_amplitude = 0.0;
    app.render_settled_frame();
    let unwaved_hd = app.compositor.fb.pixels.clone();

    app.world.wave_amplitude = 3.6;
    app.world.frame_count = 9;
    app.render_settled_frame();
    let fc_hd = app.world.frame_count;
    let waved_hd = app.compositor.fb.pixels.clone();

    let row_shift_mae = |unwaved: &[u8], waved: &[u8], w: usize, h: usize, amp: f32, freq: f32, phase: f32| -> f64 {
        let mut diff_sum = 0u64;
        for y in 0..h {
            let shift = ((y as f32 * freq + phase).sin() * amp).round() as i32;
            let row_off = y * w * 4;
            for x in 0..w {
                let sx = (x as i32 - shift).clamp(0, w as i32 - 1) as usize;
                let wi = row_off + x * 4;
                let ui = row_off + sx * 4;
                for c in 0..3 {
                    diff_sum += unwaved[ui + c].abs_diff(waved[wi + c]) as u64;
                }
            }
        }
        diff_sum as f64 / (w * h * 3) as f64
    };

    let phase_hd = (fc_hd as f32) * 0.15;
    let mae_expected_hd = row_shift_mae(&unwaved_hd, &waved_hd, 1280, 800, 3.6 * 4.0, 0.02, phase_hd);
    let mae_unscaled_hd = row_shift_mae(&unwaved_hd, &waved_hd, 1280, 800, 3.6, 0.08, phase_hd);
    assert!(
        mae_expected_hd < 1.0,
        "HD waved frame must match 4x scaled wave (MAE={:.3})",
        mae_expected_hd
    );
    assert!(
        mae_unscaled_hd > 5.0,
        "HD waved frame must not match unscaled 1x wave (MAE={:.3})",
        mae_unscaled_hd
    );

    // Switch live to Retro and verify 1x wave parity
    app.set_hd_mode(false);
    app.world.wave_amplitude = 0.0;
    app.render_settled_frame();
    let unwaved_retro = app.compositor.fb.pixels.clone();

    app.world.wave_amplitude = 3.6;
    app.world.frame_count = 9;
    app.render_settled_frame();
    let fc_retro = app.world.frame_count;
    let waved_retro = app.compositor.fb.pixels.clone();
    let mae_expected_retro = row_shift_mae(
        &unwaved_retro,
        &waved_retro,
        320,
        200,
        3.6,
        0.08,
        (fc_retro as f32) * 0.15,
    );
    assert!(
        mae_expected_retro < 1.0,
        "Retro waved frame must match 1x wave (MAE={:.3})",
        mae_expected_retro
    );
}

#[test]
fn regression_issue_35_battle_live_hd_retro_switch() {
    let root = repo_root();
    let assets = root.join("site/assets");

    let mut app = HeadlessApp::new(&assets, true);
    app.enter_sample_battle(10);
    app.render_settled_frame();
    assert_eq!((app.compositor.fb.width, app.compositor.fb.height), (1280, 800));
    assert!(app.non_black_pixel_ratio() > 0.50);

    app.set_hd_mode(false);
    app.render_settled_frame();
    assert_eq!((app.compositor.fb.width, app.compositor.fb.height), (320, 200));
    assert!(app.non_black_pixel_ratio() > 0.50);
    assert!(
        app.compositor
            .cache
            .get_texture("graphics/fbp/fbp_010_1x.webp")
            .is_some(),
        "Switching battle to Retro must load fbp_010_1x.webp"
    );

    app.set_hd_mode(true);
    app.render_settled_frame();
    assert_eq!((app.compositor.fb.width, app.compositor.fb.height), (1280, 800));
    assert!(app.non_black_pixel_ratio() > 0.50);
}
