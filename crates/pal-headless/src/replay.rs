//! Scripted Input Replay & Golden-Image Pixel Diff Regression Helpers for `pal-headless`.

use crate::HeadlessApp;
use pal_ui::{PointerEvent, PointerKind, UiKey, UiView};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayStep {
    Key(UiKey),
    Walk(u8), // 0=SW, 1=NW, 2=NE, 3=SE (matches pal_core::isometric::step_half_tile)
    Click { x: i32, y: i32 },
    SetView(String),
    SetHd(bool),
    LoadSave(String),
    Shot(String),
    Tick(usize),
}

#[derive(Clone, Debug)]
pub struct GoldenDiffReport {
    pub width: u32,
    pub height: u32,
    pub total_pixels: usize,
    pub diff_pixels: usize,
    pub diff_pct: f64,
    pub mean_abs_error: f64,
}

pub fn parse_step_token(raw: &str) -> Result<Option<ReplayStep>, String> {
    let tok = raw.trim();
    if tok.is_empty() || tok.starts_with('#') || tok.starts_with("//") {
        return Ok(None);
    }
    let lower = tok.to_ascii_lowercase();
    match lower.as_str() {
        "up" => Ok(Some(ReplayStep::Key(UiKey::Up))),
        "down" => Ok(Some(ReplayStep::Key(UiKey::Down))),
        "left" => Ok(Some(ReplayStep::Key(UiKey::Left))),
        "right" => Ok(Some(ReplayStep::Key(UiKey::Right))),
        "confirm" | "enter" | "space" => Ok(Some(ReplayStep::Key(UiKey::Confirm))),
        "cancel" | "esc" | "escape" => Ok(Some(ReplayStep::Key(UiKey::Cancel))),
        "menu" | "alt" => Ok(Some(ReplayStep::Key(UiKey::MenuToggle))),
        "tab" => Ok(Some(ReplayStep::Key(UiKey::Tab))),
        "pgup" | "pageup" => Ok(Some(ReplayStep::Key(UiKey::PageUp))),
        "pgdn" | "pagedown" => Ok(Some(ReplayStep::Key(UiKey::PageDown))),
        "home" => Ok(Some(ReplayStep::Key(UiKey::Home))),
        "end" => Ok(Some(ReplayStep::Key(UiKey::End))),
        "hd" => Ok(Some(ReplayStep::SetHd(true))),
        "retro" => Ok(Some(ReplayStep::SetHd(false))),
        _ => {
            if let Some(dir_s) = lower.strip_prefix("walk:") {
                let dir = match dir_s {
                    "sw" | "0" => 0,
                    "nw" | "1" => 1,
                    "ne" | "2" => 2,
                    "se" | "3" => 3,
                    _ => return Err(format!("unknown walk direction '{}'", dir_s)),
                };
                return Ok(Some(ReplayStep::Walk(dir)));
            }
            if let Some(coords) = lower.strip_prefix("click:") {
                let mut parts = coords.split(',');
                let x = parts
                    .next()
                    .and_then(|s| s.trim().parse::<i32>().ok())
                    .ok_or_else(|| format!("invalid click x in '{}'", tok))?;
                let y = parts
                    .next()
                    .and_then(|s| s.trim().parse::<i32>().ok())
                    .ok_or_else(|| format!("invalid click y in '{}'", tok))?;
                return Ok(Some(ReplayStep::Click { x, y }));
            }
            if let Some(v) = tok.strip_prefix("view:").or_else(|| tok.strip_prefix("VIEW:")) {
                return Ok(Some(ReplayStep::SetView(v.trim().to_ascii_lowercase())));
            }
            if let Some(p) = tok.strip_prefix("shot:").or_else(|| tok.strip_prefix("SHOT:")) {
                return Ok(Some(ReplayStep::Shot(p.trim().to_string())));
            }
            if let Some(p) = tok.strip_prefix("save:").or_else(|| tok.strip_prefix("SAVE:")) {
                return Ok(Some(ReplayStep::LoadSave(p.trim().to_string())));
            }
            if let Some(n) = lower.strip_prefix("tick:") {
                let count = n
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| format!("invalid tick count '{}'", tok))?;
                return Ok(Some(ReplayStep::Tick(count.max(1))));
            }
            Err(format!("unknown replay step '{}'", tok))
        }
    }
}

/// Parse comma- or newline-separated replay script tokens.
pub fn parse_script_str(input: &str) -> Result<Vec<ReplayStep>, String> {
    let mut steps = Vec::new();
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        // Allow comma- or semicolon-separated tokens on a single line, keeping `click:x,y` together.
        let mut parts = trimmed.split([',', ';']).peekable();
        while let Some(part) = parts.next() {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }
            if p.to_ascii_lowercase().starts_with("click:") && !p.contains(',') {
                if let Some(y_part) = parts.next() {
                    let combined = format!("{},{}", p, y_part.trim());
                    if let Some(step) = parse_step_token(&combined)? {
                        steps.push(step);
                    }
                    continue;
                }
            }
            if let Some(step) = parse_step_token(p)? {
                steps.push(step);
            }
        }
    }
    Ok(steps)
}

/// Parse a `.rec` / `.txt` script file or a minimal FlightRecorder `.json` file.
pub fn parse_recording_file(path: impl AsRef<Path>) -> Result<Vec<ReplayStep>, String> {
    let text = fs::read_to_string(path.as_ref())
        .map_err(|e| format!("read recording {:?}: {}", path.as_ref(), e))?;
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        // Lightweight JSON extraction for FlightRecorder-style action lists:
        // looks for `"type": "step"` / `"dir": N` or `"key": "..."` entries
        let mut steps = Vec::new();
        for chunk in trimmed.split('{').skip(1) {
            let body = chunk.split('}').next().unwrap_or("");
            if body.contains("\"walk\"") || body.contains("\"step\"") {
                if let Some(dir) = extract_json_int(body, "\"dir\"") {
                    steps.push(ReplayStep::Walk((dir & 3) as u8));
                }
            } else if let Some(k) = extract_json_str(body, "\"key\"") {
                if let Ok(Some(step)) = parse_step_token(&k) {
                    steps.push(step);
                }
            } else if let Some(s) = extract_json_str(body, "\"shot\"") {
                steps.push(ReplayStep::Shot(s));
            }
        }
        return Ok(steps);
    }
    parse_script_str(&text)
}

fn extract_json_int(text: &str, key: &str) -> Option<i32> {
    let pos = text.find(key)?;
    let rest = &text[pos + key.len()..];
    let colon = rest.find(':')?;
    let s = rest[colon + 1..].trim_start();
    let end = s
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(s.len());
    s[..end].parse().ok()
}

fn extract_json_str(text: &str, key: &str) -> Option<String> {
    let pos = text.find(key)?;
    let rest = &text[pos + key.len()..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let inner = after.strip_prefix('"')?;
    let end = inner.find('"')?;
    Some(inner[..end].to_string())
}

/// Apply a named view shortcut on any `PalRuntime` (`title`, `field`, `menu`, `status`, `equip`,
/// `magic`, `items`, `system`, `pawnshop`, `battle`, `dialog`).
pub fn apply_named_view_rt(rt: &mut pal_runtime::PalRuntime, view: &str) -> Result<(), String> {
    if view != "title" {
        if let Some(g) = rt.game.as_mut() {
            g.in_title = false;
        }
    }
    match view {
        "title" => {
            rt.set_in_title(true);
        }
        "field" => {
            rt.world.battle.active = false;
            rt.world.fbp_override = None;
            rt.ui.set_view(UiView::None);
        }
        "menu" => {
            rt.world.battle.active = false;
            rt.world.fbp_override = None;
            rt.ui.set_view(UiView::RootMenu);
        }
        "status" => {
            rt.world.battle.active = false;
            rt.world.fbp_override = None;
            rt.ui.set_view(UiView::StatusFbp000);
        }
        "equip" => {
            rt.world.battle.active = false;
            rt.world.fbp_override = None;
            rt.ui.set_view(UiView::EquipFbp001);
        }
        "magic" => {
            rt.world.battle.active = false;
            rt.world.fbp_override = None;
            rt.ui.set_view(UiView::MagicGrid);
        }
        "items" => {
            rt.world.battle.active = false;
            rt.world.fbp_override = None;
            rt.ui.set_view(UiView::ItemGrid);
        }
        "system" => {
            rt.world.battle.active = false;
            rt.world.fbp_override = None;
            rt.ui.set_view(UiView::SystemSubmenu);
        }
        "pawnshop" => {
            rt.world.battle.active = false;
            rt.world.fbp_override = None;
            let st = rt.state.clone();
            rt.ui.open_pawnshop_from_inventory(&st);
        }
        "battle" => {
            crate::populate_sample_battle(rt, 10);
            let _ = rt.on_key(UiKey::Confirm);
        }
        "dialog" => {
            rt.world.battle.active = false;
            rt.world.fbp_override = None;
            rt.ui.open_dialog(
                "李逍遙",
                "十年了！老夫已經隱忍十年了\n要不是顧忌巫後還留個孽種在\n人世，我也不必等到現在！",
                Some(1),
                false,
            );
        }
        other => return Err(format!("unknown view '{}'", other)),
    }
    rt.compositor.mark_ui_dirty();
    Ok(())
}

/// Apply a named view shortcut (`title`, `field`, `menu`, `status`, `equip`, `magic`, `items`, `system`, `pawnshop`, `battle`, `dialog`).
pub fn apply_named_view(app: &mut HeadlessApp, view: &str) -> Result<(), String> {
    apply_named_view_rt(&mut app.rt, view)?;
    app.pump_host_cmds();
    Ok(())
}

/// Execute a sequence of `ReplayStep`s against `app`, returning the list of exported screenshot paths.
pub fn execute_steps(
    app: &mut HeadlessApp,
    steps: &[ReplayStep],
    shot_dir: &Path,
) -> Result<Vec<PathBuf>, String> {
    let mut shots = Vec::new();
    for step in steps {
        match step {
            ReplayStep::Key(k) => {
                app.press_key(*k);
            }
            ReplayStep::Walk(dir) => {
                app.step_walk(*dir);
            }
            ReplayStep::Click { x, y } => {
                // Ensure hit_rects are up to date before clicking
                app.render_settled_frame();
                app.send_pointer(PointerEvent {
                    kind: PointerKind::Down,
                    x: *x,
                    y: *y,
                });
            }
            ReplayStep::SetView(v) => {
                apply_named_view(app, v)?;
            }
            ReplayStep::SetHd(hd) => {
                app.set_hd_mode(*hd);
            }
            ReplayStep::LoadSave(p) => {
                let candidate = Path::new(p);
                let resolved = if candidate.exists() {
                    candidate.to_path_buf()
                } else {
                    app.assets_root.join("orig_saves").join(p)
                };
                app.load_rpg_file(&resolved)?;
            }
            ReplayStep::Tick(n) => {
                for _ in 0..*n {
                    app.render_settled_frame();
                }
            }
            ReplayStep::Shot(filename) => {
                app.render_settled_frame();
                let out_path = if Path::new(filename).is_absolute() {
                    PathBuf::from(filename)
                } else {
                    shot_dir.join(filename)
                };
                app.export_png(&out_path)?;
                shots.push(out_path);
            }
        }
    }
    Ok(shots)
}

/// Compare a framebuffer against a golden PNG on disk.
/// Optionally writes a red-highlighted diff PNG to `diff_out`.
pub fn compare_with_golden_png_fb(
    fb: &pal_render::framebuffer::Framebuffer,
    golden_path: impl AsRef<Path>,
    diff_out: Option<&Path>,
    channel_tolerance: u8,
) -> Result<GoldenDiffReport, String> {
    let golden = image::open(golden_path.as_ref())
        .map_err(|e| format!("open golden {:?}: {}", golden_path.as_ref(), e))?
        .to_rgba8();
    let (gw, gh) = golden.dimensions();
    if gw != fb.width || gh != fb.height {
        return Err(format!(
            "dimension mismatch: rendered {}x{} vs golden {}x{}",
            fb.width, fb.height, gw, gh
        ));
    }

    let total_pixels = (gw as usize) * (gh as usize);
    let mut diff_pixels = 0usize;
    let mut abs_err_sum = 0u64;
    let mut diff_buf = if diff_out.is_some() {
        vec![0u8; fb.pixels.len()]
    } else {
        Vec::new()
    };

    let g_raw = golden.as_raw();
    for i in 0..total_pixels {
        let base = i * 4;
        let dr = (fb.pixels[base] as i32 - g_raw[base] as i32).abs();
        let dg = (fb.pixels[base + 1] as i32 - g_raw[base + 1] as i32).abs();
        let db = (fb.pixels[base + 2] as i32 - g_raw[base + 2] as i32).abs();
        let max_c = dr.max(dg).max(db);
        abs_err_sum += (dr + dg + db) as u64;

        let changed = max_c > channel_tolerance as i32;
        if changed {
            diff_pixels += 1;
        }
        if !diff_buf.is_empty() {
            if changed {
                diff_buf[base] = 255;
                diff_buf[base + 1] = 32;
                diff_buf[base + 2] = 32;
                diff_buf[base + 3] = 255;
            } else {
                // Dim matching pixels so diffs pop clearly
                diff_buf[base] = fb.pixels[base] / 3;
                diff_buf[base + 1] = fb.pixels[base + 1] / 3;
                diff_buf[base + 2] = fb.pixels[base + 2] / 3;
                diff_buf[base + 3] = 255;
            }
        }
    }

    if let Some(out) = diff_out {
        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = fs::create_dir_all(parent);
            }
        }
        let img = image::RgbaImage::from_raw(gw, gh, diff_buf)
            .ok_or_else(|| "diff image buffer mismatch".to_string())?;
        img.save(out).map_err(|e| format!("save diff png: {}", e))?;
    }

    Ok(GoldenDiffReport {
        width: gw,
        height: gh,
        total_pixels,
        diff_pixels,
        diff_pct: if total_pixels > 0 {
            (diff_pixels as f64) * 100.0 / (total_pixels as f64)
        } else {
            0.0
        },
        mean_abs_error: if total_pixels > 0 {
            (abs_err_sum as f64) / ((total_pixels * 3) as f64)
        } else {
            0.0
        },
    })
}

/// Compare `app`'s current framebuffer against a golden PNG on disk.
pub fn compare_with_golden_png(
    app: &HeadlessApp,
    golden_path: impl AsRef<Path>,
    diff_out: Option<&Path>,
    channel_tolerance: u8,
) -> Result<GoldenDiffReport, String> {
    compare_with_golden_png_fb(&app.compositor.fb, golden_path, diff_out, channel_tolerance)
}

fn decode_base64_str(input: &str) -> Result<Vec<u8>, String> {
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

fn map_ui_key_code(code: u64) -> Option<UiKey> {
    UiKey::from_u32(code as u32)
}

fn service_core_replay_cmds_rt(rt: &mut pal_runtime::PalRuntime, assets_root: &Path) {
    use pal_core::game::{HostCmd, HostReply};
    use pal_runtime::disk::load_scene_assets_from_dir;

    while let Some(cmd) = rt.out_cmds.pop_front() {
        match cmd {
            HostCmd::LoadScene { map_id, .. } => {
                if let Some(g) = rt.game.as_mut() {
                    load_scene_assets_from_dir(assets_root, g, map_id);
                }
                rt.reply(HostReply::SceneLoaded);
            }
            HostCmd::PlayRng { .. } | HostCmd::LoadChapter(_) => {
                rt.reply(HostReply::Done);
            }
            _ => {}
        }
    }
}

/// Replay a `.repal` (`.tar.gz`), `RePal_CoreReplay_v3`/`v2`, or legacy `PalRemake_CoreDiag_v1`
/// / `PalRemake_CoreReplay_v1` file natively on any `PalRuntime` (#235).
pub fn run_core_replay_rt(
    rt: &mut pal_runtime::PalRuntime,
    assets_root: &Path,
    path: impl AsRef<Path>,
    until_tick: Option<u64>,
) -> Result<serde_json::Value, String> {
    use pal_core::game::TickInput;
    use pal_runtime::repal_archive::read_repal_file;
    use pal_runtime::replay_player::{ReplayPlayer, ReplayStepOutcome};
    use serde_json::Value;

    // First try unified `.repal` / `RePal_CoreReplay_v3` archive reader via `ReplayPlayer` (#235).
    // Propagate hard validation/bomb errors instead of falling back to an unbounded JSON reader (#265).
    let path_ref = path.as_ref();
    let is_repal_ext = path_ref
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("repal") || e.eq_ignore_ascii_case("gz"));
    match read_repal_file(path_ref) {
        Ok(bundle) => {
            if !bundle.segments.is_empty() {
                let mut player = ReplayPlayer::new(bundle)?;
                if !player.load_segment(0, rt) {
                    return Err("failed to load segment 0 anchor".into());
                }
                service_core_replay_cmds_rt(rt, assets_root);
                loop {
                    if let Some(lim) = until_tick {
                        if player.current_abs_tick() >= lim {
                            break;
                        }
                    }
                    let outcome = player.step_one_tick(
                        rt,
                        |_, _| {},
                        |rt, _ev| {
                            service_core_replay_cmds_rt(rt, assets_root);
                        },
                    );
                    service_core_replay_cmds_rt(rt, assets_root);
                    match outcome {
                        ReplayStepOutcome::Stepped => {}
                        ReplayStepOutcome::SegmentCompleted { next_idx } => {
                            if !player.load_segment(next_idx, rt) {
                                return Err(format!("failed to load segment {next_idx} anchor"));
                            }
                            service_core_replay_cmds_rt(rt, assets_root);
                        }
                        ReplayStepOutcome::Finished => break,
                    }
                }
                rt.flush_projection();
                return Ok(rt
                    .game
                    .as_ref()
                    .map(|g| g.canonical_state())
                    .unwrap_or(Value::Null));
            }
            return Err("replay contains no segments".into());
        }
        Err(e) => {
            // Only fall back to legacy `PalRemake_CoreDiag_v1` JSON when the file is plain non-CoreReplay JSON.
            if is_repal_ext || e != "not a CoreReplay JSON" {
                return Err(format!("read replay {}: {e}", path_ref.display()));
            }
        }
    }

    let text = fs::read_to_string(path.as_ref())
        .map_err(|e| format!("read {}: {e}", path.as_ref().display()))?;
    let doc: Value = serde_json::from_str(&text)
        .map_err(|e| format!("parse {}: {e}", path.as_ref().display()))?;
    let replay = doc.get("replay").unwrap_or(&doc);
    let log = replay["log"].as_array().cloned().unwrap_or_default();

    // Restore anchor (`new_game`, `save_json`, `rpg_base64`, `orig_save`, or fallback `seed`).
    if let Some(anchor) = replay.get("anchor").filter(|v| !v.is_null()) {
        match anchor["type"].as_str().unwrap_or("") {
            "save_json" => {
                let s = if let Some(raw) = anchor["data"].as_str() {
                    raw.to_string()
                } else {
                    anchor["data"].to_string()
                };
                if !rt.load_save_json_str(&s) {
                    return Err("failed to load save_json anchor".into());
                }
            }
            "rpg_base64" => {
                let b64 = anchor["data"]
                    .as_str()
                    .ok_or_else(|| "rpg_base64 anchor missing data".to_string())?;
                let bytes = decode_base64_str(b64)?;
                if !rt.load_dos_rpg(&bytes) {
                    return Err("failed to load rpg_base64 anchor".into());
                }
            }
            "orig_save" => {
                let file = anchor["file"]
                    .as_str()
                    .ok_or_else(|| "orig_save anchor missing file".to_string())?;
                let p = assets_root.join("orig_saves").join(file);
                let bytes = fs::read(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
                if !rt.load_dos_rpg(&bytes) {
                    return Err(format!("failed to load orig_save {file}"));
                }
            }
            _ => {
                let seed = anchor["seed"]
                    .as_u64()
                    .or_else(|| replay["seed"].as_u64())
                    .unwrap_or(1) as u32;
                rt.new_game(seed);
            }
        }
    } else {
        let seed = replay["seed"].as_u64().unwrap_or(1) as u32;
        rt.new_game(seed);
    }
    service_core_replay_cmds_rt(rt, assets_root);

    let last_t = log.last().and_then(|e| e["t"].as_u64()).unwrap_or(0);
    let target_tick = until_tick.unwrap_or(last_t.saturating_add(1));
    let mut idx = 0usize;
    let mut guard = 0u64;
    let max_iters = target_tick.saturating_add(20_000);

    loop {
        service_core_replay_cmds_rt(rt, assets_root);
        let (now, blocked, busy) = rt
            .game
            .as_ref()
            .map(|g| (g.tick_count, g.is_blocked_on_host(), g.is_busy()))
            .unwrap_or((0, false, false));

        if let Some(lim) = until_tick {
            if now >= lim {
                break;
            }
        } else if idx >= log.len() && !busy && !blocked {
            break;
        }
        guard += 1;
        if guard > max_iters {
            break;
        }

        if blocked {
            rt.tick(TickInput::IDLE);
            service_core_replay_cmds_rt(rt, assets_root);
            continue;
        }

        let mut input = TickInput::IDLE;
        while idx < log.len() && log[idx]["t"].as_u64().unwrap_or(u64::MAX) <= now {
            let e = &log[idx];
            idx += 1;
            if let Some(m) = e.get("m").and_then(Value::as_i64) {
                if m != 0 {
                    rt.ui.set_view(UiView::RootMenu);
                } else {
                    rt.ui.set_view(UiView::None);
                }
                rt.compositor.mark_ui_dirty();
            } else if let Some(k) = e.get("k").and_then(Value::as_u64).and_then(map_ui_key_code) {
                let _ = rt.on_key(k);
                service_core_replay_cmds_rt(rt, assets_root);
            } else if let Some(p) = e.get("p").and_then(Value::as_array) {
                let kind = match p.first().and_then(Value::as_u64).unwrap_or(0) {
                    1 => PointerKind::Down,
                    2 => PointerKind::Up,
                    _ => PointerKind::Move,
                };
                let x = p.get(1).and_then(Value::as_i64).unwrap_or(0) as i32;
                let y = p.get(2).and_then(Value::as_i64).unwrap_or(0) as i32;
                let _ = rt.on_pointer(PointerEvent { kind, x, y });
                service_core_replay_cmds_rt(rt, assets_root);
            } else if let Some(arr) = e.get("i").and_then(Value::as_array) {
                if e["t"].as_u64() == Some(now) {
                    let held_dir = arr.first().and_then(Value::as_i64).unwrap_or(-1) as i8;
                    let flags = arr.get(1).and_then(Value::as_u64).unwrap_or(0) as u32;
                    let cx = arr.get(2).and_then(Value::as_i64).unwrap_or(0) as i32;
                    let cy = arr.get(3).and_then(Value::as_i64).unwrap_or(0) as i32;
                    let sx = arr.get(4).and_then(Value::as_i64).unwrap_or(0) as i32;
                    let sy = arr.get(5).and_then(Value::as_i64).unwrap_or(0) as i32;
                    input = TickInput {
                        held_dir: if held_dir >= 0 { held_dir & 3 } else { -1 },
                        interact: (flags & 1) != 0,
                        click: ((flags & 2) != 0).then_some((cx, cy)),
                        steer: ((flags & 4) != 0).then_some((sx, sy)),
                    };
                }
            }
        }

        rt.tick(input);
        service_core_replay_cmds_rt(rt, assets_root);
    }

    rt.flush_projection();
    Ok(rt
        .game
        .as_ref()
        .map(|g| g.canonical_state())
        .unwrap_or(Value::Null))
}

/// Replay a browser F9 `PalRemake_CoreDiag_v1` or `PalRemake_CoreReplay_v1` JSON file natively
/// on `HeadlessApp`, matching `site/js/host/core-host.js::_replayStep` semantics.
pub fn run_core_replay_file(
    app: &mut HeadlessApp,
    path: impl AsRef<Path>,
    until_tick: Option<u64>,
) -> Result<serde_json::Value, String> {
    app.ensure_chapters_loaded()?;
    run_core_replay_rt(&mut app.rt, &app.assets_root, path, until_tick)
}
