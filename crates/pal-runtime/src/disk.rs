//! Local filesystem asset & YAML data loader shared by `pal-headless` and `pal-desktop`.

use crate::PalRuntime;
use pal_core::game::data::GameData;
use pal_core::game::Game;
use pal_render::cache::{AssetCache, AssetKind};
use pal_render::PalCompositor;
use std::fs;
use std::path::{Path, PathBuf};

/// Locate the `assets/` directory from an explicit CLI path, `./site/assets`, `./assets`, or
/// relative to the running executable.
pub fn resolve_assets_dir(explicit: Option<&str>) -> PathBuf {
    if let Some(p) = explicit {
        return PathBuf::from(p);
    }
    for candidate in ["site/assets", "assets"] {
        let p = PathBuf::from(candidate);
        if p.join("data/scenes.yaml").is_file() {
            return p;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for rel in ["assets", "site/assets", "../assets", "../site/assets"] {
                let p = dir.join(rel);
                if p.join("data/scenes.yaml").is_file() {
                    return p;
                }
            }
        }
    }
    PathBuf::from("site/assets")
}

/// Load bundled CJK/Latin TTF fonts and `WOR16.ASC` bitmap fallback from `assets_root` (or OS paths).
pub fn load_fonts_from_dir(assets_root: &Path, compositor: &mut PalCompositor) {
    let cjk_candidates = [
        assets_root.join("fonts/pal-cjk.ttf"),
        assets_root.join("fonts/NotoSansTC-Regular.ttf"),
        PathBuf::from("/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf"),
        PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
    ];
    for p in &cjk_candidates {
        if let Ok(bytes) = fs::read(p) {
            if compositor.fonts.load_primary_ttf(&bytes) {
                break;
            }
        }
    }
    let latin_candidates = [
        assets_root.join("fonts/pal-latin.ttf"),
        PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
        PathBuf::from("/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf"),
    ];
    for p in &latin_candidates {
        if let Ok(bytes) = fs::read(p) {
            if compositor.fonts.load_fallback_ttf(&bytes) {
                break;
            }
        }
    }
    let wor16_candidates = [
        assets_root.join("fonts/WOR16.ASC"),
        PathBuf::from("raw_data/WOR16.ASC"),
    ];
    for p in &wor16_candidates {
        if let Ok(asc) = fs::read(p) {
            compositor.fonts.load_wor16_asc(asc);
            break;
        }
    }
}

/// Load all 7 static YAML tables from `<assets_root>/data/*.yaml`.
pub fn load_game_data_from_dir(assets_root: &Path) -> Result<GameData, String> {
    let mut data = GameData::default();
    for table in ["scenes", "events", "roles", "items", "magic", "enemies", "shops"] {
        let path = assets_root.join(format!("data/{table}.yaml"));
        let text = fs::read_to_string(&path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        let val: serde_json::Value = serde_yaml::from_str(&text)
            .map_err(|e| format!("parse {}: {e}", path.display()))?;
        data.load_table(table, &val);
    }
    // Load optional decoupled English translation overlays (`data/i18n/en/*.json`).
    let i18n_dir = assets_root.join("data/i18n/en");
    if let Ok(entries) = fs::read_dir(&i18n_dir) {
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        paths.sort();
        for p in paths {
            if let Ok(text) = fs::read_to_string(&p) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                    data.load_i18n_en(&val);
                }
            }
        }
    }
    // Load optional visual-only sprite descriptions for `--obs fair` mode (#278).
    // Priority: `data/sprite_visuals.{yaml,json}` inside `assets_root` (shipped with releases),
    // then dev-checkout fallback `tools/mgo_catalog.yaml` (strictly reading ONLY `visual*` &
    // `visual_kind`, never `name` or `speaker_hints`).
    for (rel, is_json, overwrite) in [
        ("data/sprite_visuals.yaml", false, true),
        ("data/sprite_visuals.json", true, true),
    ] {
        let p = assets_root.join(rel);
        if let Ok(text) = fs::read_to_string(&p) {
            let parsed = if is_json {
                serde_json::from_str::<serde_json::Value>(&text).ok()
            } else {
                serde_yaml::from_str::<serde_json::Value>(&text).ok()
            };
            if let Some(val) = parsed {
                data.load_sprite_visuals(&val, overwrite);
            }
        }
    }
    let mut catalog_candidates = vec![PathBuf::from("tools/mgo_catalog.yaml")];
    if let Some(parent2) = assets_root.parent().and_then(Path::parent) {
        catalog_candidates.insert(0, parent2.join("tools/mgo_catalog.yaml"));
    }
    for p in catalog_candidates {
        if let Ok(text) = fs::read_to_string(&p) {
            if let Ok(val) = serde_yaml::from_str::<serde_json::Value>(&text) {
                data.load_sprite_visuals(&val, false);
                break;
            }
        }
    }
    Ok(data)
}

/// Load all 11 YAML script chapters (`scripts_00.yaml`..`scripts_10.yaml`) into `game`.
pub fn load_all_chapters_from_dir(assets_root: &Path, game: &mut Game) -> Result<(), String> {
    for ch in 0..=10u32 {
        let path = assets_root.join(format!("data/scripts/scripts_{ch:02}.yaml"));
        if !path.is_file() {
            continue;
        }
        let text = fs::read_to_string(&path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        let val: serde_json::Value = serde_yaml::from_str(&text)
            .map_err(|e| format!("parse {}: {e}", path.display()))?;
        game.load_chapter(ch, &val)
            .map_err(|e| format!("load chapter {ch}: {e}"))?;
    }
    Ok(())
}

/// Boot a `PalRuntime` directly from local disk: fonts + all 7 YAML tables + all 11 script chapters.
pub fn boot_runtime_from_dir(assets_root: &Path, hd_mode: bool) -> Result<PalRuntime, String> {
    let mut rt = PalRuntime::new(hd_mode);
    load_fonts_from_dir(assets_root, &mut rt.compositor);
    rt.data = load_game_data_from_dir(assets_root)?;
    rt.init_game();
    if let Some(g) = rt.game.as_mut() {
        load_all_chapters_from_dir(assets_root, g)?;
    }
    Ok(rt)
}

/// Compute per-map GOP visual flags from `gop_XXX.json` + `gop_XXX_1x.webp` bytes:
/// - `frame0_is_void`: true when GOP tile #0 is pure black / fully transparent (interior exterior void).
/// - `opaque_frames[i]`: true when tile `i` has `>= 90%` (`>= 230/256` px) opaque diamond coverage,
///   distinguishing genuine overhead roofs/canopies from partial railings, door lintels, and foliage (#234).
fn compute_gop_visual_flags_from_bytes(
    meta_text: &str,
    img_bytes: &[u8],
) -> Option<pal_core::isometric::GopVisualFlags> {
    let meta_val: serde_json::Value = serde_json::from_str(meta_text).ok()?;
    let frames = meta_val["frames"].as_array()?;
    let meta_w = meta_val["w"].as_u64().unwrap_or(0) as u32;
    let rgba = image::load_from_memory(img_bytes).ok()?.to_rgba8();
    let tex_w = rgba.width();
    let tex_h = rgba.height();
    let tex_scale = if meta_w > 0 && tex_w >= meta_w * 2 {
        (tex_w / meta_w).max(1)
    } else {
        1
    };
    let opaque_threshold = 230 * tex_scale * tex_scale;
    let mut frame0_is_void = false;
    let mut opaque_frames = Vec::with_capacity(frames.len());

    for (idx, f) in frames.iter().enumerate() {
        let fx = f.get(0).and_then(|v| v.as_i64()).unwrap_or(0).max(0) as u32 * tex_scale;
        let fy = f.get(1).and_then(|v| v.as_i64()).unwrap_or(0).max(0) as u32 * tex_scale;
        let fw = f.get(2).and_then(|v| v.as_i64()).unwrap_or(0).max(0) as u32 * tex_scale;
        let fh = f.get(3).and_then(|v| v.as_i64()).unwrap_or(0).max(0) as u32 * tex_scale;
        if fw == 0 || fh == 0 || fx >= tex_w || fy >= tex_h {
            if idx == 0 {
                frame0_is_void = true;
            }
            opaque_frames.push(false);
            continue;
        }
        let x_end = (fx + fw).min(tex_w);
        let y_end = (fy + fh).min(tex_h);
        let mut opaque_px = 0u32;
        let mut non_black_px = 0u32;
        for y in fy..y_end {
            for x in fx..x_end {
                let p = rgba.get_pixel(x, y).0;
                if p[3] > 0 {
                    opaque_px += 1;
                    if p[0] > 4 || p[1] > 4 || p[2] > 4 {
                        non_black_px += 1;
                    }
                }
            }
        }
        if idx == 0 {
            frame0_is_void = non_black_px == 0;
        }
        opaque_frames.push(opaque_px >= opaque_threshold);
    }

    Some(pal_core::isometric::GopVisualFlags {
        frame0_is_void,
        opaque_frames,
    })
}

fn compute_gop_visual_flags(
    assets_root: &Path,
    map_id: u16,
) -> Option<pal_core::isometric::GopVisualFlags> {
    let meta_path = assets_root.join(format!("graphics/gop/gop_{map_id:03}.json"));
    let tex_path = assets_root.join(format!("graphics/gop/gop_{map_id:03}_1x.webp"));
    let meta_text = fs::read_to_string(meta_path).ok()?;
    let img_bytes = fs::read(tex_path).ok()?;
    compute_gop_visual_flags_from_bytes(&meta_text, &img_bytes)
}

/// Upload a scene's 64 KiB collision map (`maps/map_XXX.bin`) and all active `mgo_XXX.json`
/// sprite frame counts into `game` (fulfilling the `HostCmd::LoadScene` contract).
pub fn load_scene_assets_from_dir(assets_root: &Path, game: &mut Game, map_id: u16) {
    if map_id > 0 {
        let path = assets_root.join(format!("maps/map_{map_id:03}.bin"));
        if let Ok(bytes) = fs::read(&path) {
            if !game.gop_visual_flags.contains_key(&map_id) {
                if let Some(flags) = compute_gop_visual_flags(assets_root, map_id) {
                    game.gop_visual_flags.insert(map_id, flags);
                }
            }
            if let Some(flags) = game.gop_visual_flags.get(&map_id).cloned() {
                let _ = game.set_map_collision_with_visuals(
                    &bytes,
                    flags.frame0_is_void,
                    &flags.opaque_frames,
                );
            } else {
                let _ = game.set_map_collision(&bytes);
            }
        }
    }
    let ids: Vec<u16> = game
        .world
        .current_event_ids()
        .into_iter()
        .filter_map(|id| game.world.event(id).map(|e| e.sprite_id))
        .filter(|&s| s > 0)
        .collect();
    for sid in ids {
        if game.sprite_frame_dims.contains_key(&sid) {
            continue;
        }
        let p = assets_root.join(format!("graphics/mgo/mgo_{sid:03}.json"));
        if let Ok(text) = fs::read_to_string(p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(arr) = v["frames"].as_array() {
                    let dims: Vec<(i32, i32)> = arr
                        .iter()
                        .map(|f| {
                            let w = f.get(2).and_then(|x| x.as_i64()).unwrap_or(24) as i32;
                            let h = f.get(3).and_then(|x| x.as_i64()).unwrap_or(40) as i32;
                            (w, h)
                        })
                        .collect();
                    if !dims.is_empty() {
                        game.set_sprite_frame_dims(sid, dims);
                    }
                }
            }
        }
    }
}

/// Resolve a relative asset key safely inside `assets_root`, rejecting path traversal (`..`),
/// absolute paths, and OS prefixes.
fn resolve_asset_path(assets_root: &Path, key: &str) -> Option<PathBuf> {
    let rel = Path::new(key);
    if rel.is_absolute() {
        return None;
    }
    let mut out = assets_root.to_path_buf();
    for comp in rel.components() {
        match comp {
            std::path::Component::Normal(seg) => out.push(seg),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// Synchronously fulfill all pending render `AssetCache` requests from local disk (`assets_root`).
pub fn pump_render_assets_from_dir(assets_root: &Path, cache: &mut AssetCache) -> usize {
    let mut loaded = 0usize;
    let requests = cache.drain_requests();
    for req in requests {
        let Some(full_path) = resolve_asset_path(assets_root, &req.key) else {
            cache.mark_missing_permanent(req.kind, &req.key);
            continue;
        };
        match req.kind {
            AssetKind::Texture => {
                if !full_path.exists() {
                    cache.mark_missing_permanent(AssetKind::Texture, &req.key);
                    continue;
                }
                match image::open(&full_path) {
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        let pinned = req.key.contains("/ui/");
                        cache.upload_texture(req.key, w, h, rgba.into_raw(), pinned);
                        loaded += 1;
                    }
                    Err(_) => {
                        cache.mark_failed(AssetKind::Texture, &req.key);
                    }
                }
            }
            AssetKind::AtlasMeta => match fs::read_to_string(&full_path) {
                Ok(text) => {
                    if cache.upload_atlas_json(req.key.clone(), &text) {
                        loaded += 1;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    cache.mark_missing_permanent(AssetKind::AtlasMeta, &req.key);
                }
                Err(_) => {
                    cache.mark_failed(AssetKind::AtlasMeta, &req.key);
                }
            },
            AssetKind::MapBinary => match fs::read(&full_path) {
                Ok(bytes) => {
                    cache.upload_map_binary(req.key, bytes);
                    loaded += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    cache.mark_missing_permanent(AssetKind::MapBinary, &req.key);
                }
                Err(_) => {
                    cache.mark_failed(AssetKind::MapBinary, &req.key);
                }
            },
            AssetKind::HdMapMeta => match fs::read_to_string(&full_path) {
                Ok(text) => {
                    if cache.upload_hd_map_json(req.key.clone(), &text) {
                        loaded += 1;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    cache.mark_missing_permanent(AssetKind::HdMapMeta, &req.key);
                }
                Err(_) => {
                    cache.mark_failed(AssetKind::HdMapMeta, &req.key);
                }
            },
            _ => {
                cache.mark_failed(req.kind, &req.key);
            }
        }
    }
    loaded
}

/// Populate `rt.ui.save_slots` from local `slot_{i}.json` files in `saves_dir`
/// (with slot 0 falling back to `assets_root/orig_saves/10.rpg`, matching web & desktop, #233).
pub fn refresh_save_slots_from_dir(saves_dir: &Path, assets_root: &Path, rt: &mut crate::PalRuntime) {
    let count = rt.ui.save_slots.len();
    for i in 0..count {
        let p = saves_dir.join(format!("slot_{i}.json"));
        let mut exists = false;
        let mut detail = match rt.ui.lang {
            pal_core::Language::En => "Empty".to_string(),
            _ => "空".to_string(),
        };
        if let Ok(raw) = fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                exists = true;
                let sid = v["sceneId"].as_u64().unwrap_or(1);
                let lv = v["roles"][0]["level"].as_u64().unwrap_or(1);
                detail = match rt.ui.lang {
                    pal_core::Language::En => format!("Scene #{sid}  Lv.{lv}"),
                    pal_core::Language::ZhHans => format!("场景#{sid} Lv.{lv}"),
                    pal_core::Language::ZhHant => format!("場景#{sid} Lv.{lv}"),
                };
            }
        } else if i == 0 && assets_root.join("orig_saves/10.rpg").is_file() {
            exists = true;
            detail = match rt.ui.lang {
                pal_core::Language::En => "Classic Save #10".to_string(),
                pal_core::Language::ZhHans => "经典存档 #10 (余杭客栈)".to_string(),
                pal_core::Language::ZhHant => "經典存檔 #10 (餘杭客棧)".to_string(),
            };
        }
        if let Some(s) = rt.ui.save_slots.get_mut(i) {
            s.exists = exists;
            s.detail = detail;
        }
    }
    rt.compositor.mark_ui_dirty();
}

/// Populate `rt.ui.save_slots` from local `slot_{i}.json` files in `saves_dir`
/// (with slot 0 falling back to `orig_saves/10.rpg` in `vfs`, #237).
pub fn refresh_save_slots_from_vfs(
    saves_dir: &Path,
    vfs: &crate::pack_vfs::OverlayVfs,
    rt: &mut crate::PalRuntime,
) {
    let count = rt.ui.save_slots.len();
    let has_orig_save = vfs.exists("orig_saves/10.rpg");
    for i in 0..count {
        let p = saves_dir.join(format!("slot_{i}.json"));
        let mut exists = false;
        let mut detail = match rt.ui.lang {
            pal_core::Language::En => "Empty".to_string(),
            _ => "空".to_string(),
        };
        if let Ok(raw) = fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                exists = true;
                let sid = v["sceneId"].as_u64().unwrap_or(1);
                let lv = v["roles"][0]["level"].as_u64().unwrap_or(1);
                detail = match rt.ui.lang {
                    pal_core::Language::En => format!("Scene #{sid}  Lv.{lv}"),
                    pal_core::Language::ZhHans => format!("场景#{sid} Lv.{lv}"),
                    pal_core::Language::ZhHant => format!("場景#{sid} Lv.{lv}"),
                };
            }
        } else if i == 0 && has_orig_save {
            exists = true;
            detail = match rt.ui.lang {
                pal_core::Language::En => "Classic Save #10".to_string(),
                pal_core::Language::ZhHans => "经典存档 #10 (余杭客栈)".to_string(),
                pal_core::Language::ZhHant => "經典存檔 #10 (餘杭客棧)".to_string(),
            };
        }
        if let Some(s) = rt.ui.save_slots.get_mut(i) {
            s.exists = exists;
            s.detail = detail;
        }
    }
    rt.compositor.mark_ui_dirty();
}

// ============================================================================
// OverlayVfs-backed Runtime Loaders (#237 — works identically with loose dirs & `.rpk` packs)
// ============================================================================

/// Load bundled TTF / `WOR16.ASC` fonts via [`crate::pack_vfs::OverlayVfs`] (with OS fallbacks).
pub fn load_fonts_from_vfs(vfs: &mut crate::pack_vfs::OverlayVfs, compositor: &mut PalCompositor) {
    for key in ["fonts/pal-cjk.ttf", "fonts/NotoSansTC-Regular.ttf"] {
        if let Ok(bytes) = vfs.read_bytes(key) {
            if compositor.fonts.load_primary_ttf(&bytes) {
                break;
            }
        }
    }
    if let Ok(bytes) = vfs.read_bytes("fonts/pal-latin.ttf") {
        let _ = compositor.fonts.load_fallback_ttf(&bytes);
    }
    if let Ok(asc) = vfs.read_bytes("fonts/WOR16.ASC") {
        compositor.fonts.load_wor16_asc(asc);
    }
}

/// Load all 7 static YAML tables, English i18n overlays, and optional `sprite_visuals` via [`OverlayVfs`].
pub fn load_game_data_from_vfs(vfs: &mut crate::pack_vfs::OverlayVfs) -> Result<GameData, String> {
    let mut data = GameData::default();
    for table in ["scenes", "events", "roles", "items", "magic", "enemies", "shops"] {
        let key = format!("data/{table}.yaml");
        let text = vfs.read_to_string(&key)?;
        let val: serde_json::Value =
            serde_yaml::from_str(&text).map_err(|e| format!("parse {key}: {e}"))?;
        data.load_table(table, &val);
    }
    for key in vfs.list_prefix("data/i18n/en/") {
        if key.ends_with(".json") {
            if let Ok(text) = vfs.read_to_string(&key) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                    data.load_i18n_en(&val);
                }
            }
        }
    }
    for (key, is_json) in [
        ("data/sprite_visuals.yaml", false),
        ("data/sprite_visuals.json", true),
    ] {
        if let Ok(text) = vfs.read_to_string(key) {
            let parsed = if is_json {
                serde_json::from_str::<serde_json::Value>(&text).ok()
            } else {
                serde_yaml::from_str::<serde_json::Value>(&text).ok()
            };
            if let Some(val) = parsed {
                data.load_sprite_visuals(&val, true);
            }
        }
    }
    Ok(data)
}

/// Load all 11 YAML script chapters via [`OverlayVfs`].
pub fn load_all_chapters_from_vfs(
    vfs: &mut crate::pack_vfs::OverlayVfs,
    game: &mut Game,
) -> Result<(), String> {
    for ch in 0..=10u32 {
        let key = format!("data/scripts/scripts_{ch:02}.yaml");
        if !vfs.exists(&key) {
            continue;
        }
        let text = vfs.read_to_string(&key)?;
        let val: serde_json::Value =
            serde_yaml::from_str(&text).map_err(|e| format!("parse {key}: {e}"))?;
        game.load_chapter(ch, &val)
            .map_err(|e| format!("load chapter {ch}: {e}"))?;
    }
    Ok(())
}

/// Boot a full [`PalRuntime`] directly from [`OverlayVfs`] (supports `--packs-dir` plain or encrypted!).
pub fn boot_runtime_from_vfs(
    vfs: &mut crate::pack_vfs::OverlayVfs,
    hd_mode: bool,
) -> Result<PalRuntime, String> {
    let mut rt = PalRuntime::new(hd_mode);
    load_fonts_from_vfs(vfs, &mut rt.compositor);
    rt.data = load_game_data_from_vfs(vfs)?;
    rt.init_game();
    if let Some(g) = rt.game.as_mut() {
        load_all_chapters_from_vfs(vfs, g)?;
    }
    Ok(rt)
}

fn compute_gop_visual_flags_from_vfs(
    vfs: &mut crate::pack_vfs::OverlayVfs,
    map_id: u16,
) -> Option<pal_core::isometric::GopVisualFlags> {
    let json_key = format!("graphics/gop/gop_{map_id:03}.json");
    let tex_key = format!("graphics/gop/gop_{map_id:03}_1x.webp");
    let text = vfs.read_to_string(&json_key).ok()?;
    let img_bytes = vfs.read_bytes(&tex_key).ok()?;
    compute_gop_visual_flags_from_bytes(&text, &img_bytes)
}

/// Load a scene's collision map and event sprite frame dimensions via [`OverlayVfs`] (auto-mounting scene packs).
pub fn load_scene_assets_from_vfs(
    vfs: &mut crate::pack_vfs::OverlayVfs,
    game: &mut Game,
    map_id: u16,
) {
    let _ = vfs.ensure_scene_packs(game.world.scene_id, (map_id > 0).then_some(map_id));
    if map_id > 0 {
        let key = format!("maps/map_{map_id:03}.bin");
        if let Ok(bytes) = vfs.read_bytes(&key) {
            if !game.gop_visual_flags.contains_key(&map_id) {
                if let Some(flags) = compute_gop_visual_flags_from_vfs(vfs, map_id) {
                    game.gop_visual_flags.insert(map_id, flags);
                }
            }
            if let Some(flags) = game.gop_visual_flags.get(&map_id).cloned() {
                let _ = game.set_map_collision_with_visuals(
                    &bytes,
                    flags.frame0_is_void,
                    &flags.opaque_frames,
                );
            } else {
                let _ = game.set_map_collision(&bytes);
            }
        }
    }
    let ids: Vec<u16> = game
        .world
        .current_event_ids()
        .into_iter()
        .filter_map(|id| game.world.event(id).map(|e| e.sprite_id))
        .filter(|&s| s > 0)
        .collect();
    for sid in ids {
        if game.sprite_frame_dims.contains_key(&sid) {
            continue;
        }
        let key = format!("graphics/mgo/mgo_{sid:03}.json");
        if let Ok(text) = vfs.read_to_string(&key) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(arr) = v["frames"].as_array() {
                    let dims: Vec<(i32, i32)> = arr
                        .iter()
                        .map(|f| {
                            let w = f.get(2).and_then(|x| x.as_i64()).unwrap_or(24) as i32;
                            let h = f.get(3).and_then(|x| x.as_i64()).unwrap_or(40) as i32;
                            (w, h)
                        })
                        .collect();
                    if !dims.is_empty() {
                        game.set_sprite_frame_dims(sid, dims);
                    }
                }
            }
        }
    }
}

/// Fulfill pending render `AssetCache` requests via [`OverlayVfs`] (zero file extraction!).
pub fn pump_render_assets_from_vfs(
    vfs: &mut crate::pack_vfs::OverlayVfs,
    cache: &mut AssetCache,
) -> usize {
    let mut loaded = 0usize;
    let requests = cache.drain_requests();
    for req in requests {
        match req.kind {
            AssetKind::Texture => match vfs.read_bytes(&req.key) {
                Ok(bytes) => match image::load_from_memory(&bytes) {
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        let pinned = req.key.contains("/ui/");
                        cache.upload_texture(req.key, w, h, rgba.into_raw(), pinned);
                        loaded += 1;
                    }
                    Err(_) => cache.mark_failed(AssetKind::Texture, &req.key),
                },
                Err(_) => cache.mark_missing_permanent(AssetKind::Texture, &req.key),
            },
            AssetKind::AtlasMeta => match vfs.read_to_string(&req.key) {
                Ok(text) => {
                    if cache.upload_atlas_json(req.key.clone(), &text) {
                        loaded += 1;
                    }
                }
                Err(_) => cache.mark_missing_permanent(AssetKind::AtlasMeta, &req.key),
            },
            AssetKind::MapBinary => match vfs.read_bytes(&req.key) {
                Ok(bytes) => {
                    cache.upload_map_binary(req.key, bytes);
                    loaded += 1;
                }
                Err(_) => cache.mark_missing_permanent(AssetKind::MapBinary, &req.key),
            },
            AssetKind::HdMapMeta => match vfs.read_to_string(&req.key) {
                Ok(text) => {
                    if cache.upload_hd_map_json(req.key.clone(), &text) {
                        loaded += 1;
                    }
                }
                Err(_) => cache.mark_missing_permanent(AssetKind::HdMapMeta, &req.key),
            },
            _ => cache.mark_failed(req.kind, &req.key),
        }
    }
    loaded
}
