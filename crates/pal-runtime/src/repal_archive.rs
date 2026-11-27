//! `.repal` (`.tar.gz`) self-contained segmented replay container reader & writer.
//!
//! Standard USTAR `.tar.gz` layout (extractable by any OS archive tool):
//! ```text
//! xxx.repal
//! ├── manifest.json              # format metadata, engine_version, total_ticks, segment index
//! ├── end_screenshot.png         # optional full-res ending screenshot
//! ├── state.json                 # optional canonical state snapshot (F9 bug report)
//! └── segments/
//!     ├── 000_sc1/
//!     │   ├── save.json          # standard RePal save file (or anchor.json for NewGame seg 0)
//!     │   ├── thumb.png          # optional 160x100 PNG thumbnail
//!     │   └── events.json        # local segment input event stream (ticks from 0)
//!     └── 001_sc2/ ...
//! ```
//!
//! [`read_repal_file`] also transparently loads plain `.json` and gzipped-JSON replays
//! (`RePal_CoreReplay_v3` / `v2` / `PalRemake_CoreReplay_v1` / `CoreDiag_v1`).

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use pal_core::game::replay_rec::{
    normalize_annotations, ReplayAnchor, ReplayAnnotation, ReplayBundle, ReplayEntry, ReplayEvent,
    ReplaySegment, REPLAY_FORMAT_KIND,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_AUTO_REPLAY_FILES: usize = 20;
/// Maximum decompressed size accepted by [`read_repal_bytes`] to defend against gzip bombs (#210).
pub const MAX_REPAL_BYTES: usize = 256 * 1024 * 1024;
/// Maximum tar entries accepted in a `.repal` archive (#210).
const MAX_TAR_ENTRIES: usize = 8192;

static ARCHIVE_SEQ: AtomicU64 = AtomicU64::new(0);

fn write_tar_entry<W: Write>(w: &mut W, name: &str, data: &[u8]) -> Result<(), String> {
    let mut header = [0u8; 512];
    let name_bytes = name.as_bytes();
    if name_bytes.len() > 100 {
        return Err(format!("tar entry name too long: {name}"));
    }
    header[..name_bytes.len()].copy_from_slice(name_bytes);
    header[100..108].copy_from_slice(b"0000644\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    let size_oct = format!("{:011o}\0", data.len());
    header[124..136].copy_from_slice(size_oct.as_bytes());
    let mtime = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mtime_oct = format!("{:011o}\0", mtime);
    header[136..148].copy_from_slice(mtime_oct.as_bytes());
    // Checksum field filled with spaces while computing checksum
    header[148..156].copy_from_slice(b"        ");
    header[156] = b'0'; // regular file
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");

    let cksum: u32 = header.iter().map(|&b| b as u32).sum();
    let cksum_oct = format!("{:06o}\0 ", cksum);
    header[148..156].copy_from_slice(cksum_oct.as_bytes());

    w.write_all(&header).map_err(|e| e.to_string())?;
    w.write_all(data).map_err(|e| e.to_string())?;
    let pad = (512 - (data.len() % 512)) % 512;
    if pad > 0 {
        w.write_all(&vec![0u8; pad]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn read_tar_entries<'a>(bytes: &'a [u8]) -> Option<HashMap<String, &'a [u8]>> {
    let mut out = HashMap::new();
    let mut off = 0usize;
    while off + 512 <= bytes.len() {
        if out.len() >= MAX_TAR_ENTRIES {
            return None;
        }
        let hdr = &bytes[off..off + 512];
        if hdr.iter().all(|&b| b == 0) {
            break;
        }
        // Validate USTAR magic or tar checksum
        let stored_cksum_str = std::str::from_utf8(&hdr[148..156])
            .ok()?
            .trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace());
        let stored_cksum = u32::from_str_radix(stored_cksum_str, 8).ok()?;
        let computed: u32 = hdr[..148].iter().map(|&b| b as u32).sum::<u32>()
            + 8 * (b' ' as u32)
            + hdr[156..].iter().map(|&b| b as u32).sum::<u32>();
        if stored_cksum != computed {
            return None;
        }
        let name_end = hdr[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = std::str::from_utf8(&hdr[..name_end])
            .ok()?
            .trim_start_matches("./")
            .to_string();
        let size_str = std::str::from_utf8(&hdr[124..136])
            .ok()?
            .trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace());
        let size = usize::from_str_radix(size_str, 8).ok()?;
        off = off.checked_add(512)?;
        let end = off.checked_add(size)?;
        if end > bytes.len() {
            return None;
        }
        let typeflag = hdr[156];
        if typeflag == b'0' || typeflag == 0 {
            out.insert(name, &bytes[off..end]);
        }
        let padded = size.checked_add(511)? & !511;
        off = off.checked_add(padded)?;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// File extension for new replay archives (`.repal.gz`: a tar.gz that GitHub issue attachments,
/// file managers and archive tools all accept). Readers detect the format by content.
pub const REPAL_FILE_EXT: &str = "repal.gz";

/// `true` for replay archive file names: current `*.repal.gz` and legacy `*.repal`.
pub fn is_repal_file_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".repal.gz") || n.ends_with(".repal")
}

/// Strip a replay archive / JSON extension (`.repal.gz`, `.repal`, `.json`).
fn strip_replay_ext(name: &str) -> &str {
    for ext in [".repal.gz", ".repal", ".json"] {
        if name.len() > ext.len() && name.to_ascii_lowercase().ends_with(ext) {
            return &name[..name.len() - ext.len()];
        }
    }
    name
}

/// Write a [`ReplayBundle`] into a standard `.tar.gz` `.repal` archive atomically via a temp sibling (#209).
pub fn write_repal_archive(path: &Path, bundle: &ReplayBundle) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp_seq = ARCHIVE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.{}.{tmp_seq}.tmp",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("repal"),
        std::process::id()
    );
    let tmp_path = path.with_file_name(tmp_name);
    let write_res = (|| -> Result<(), String> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| e.to_string())?;
        let mut gz = GzEncoder::new(file, Compression::default());

        let mut seg_manifest = Vec::with_capacity(bundle.segments.len());
        for seg in &bundle.segments {
            let dir = format!("segments/{:03}_sc{}", seg.index, seg.scene_id);
            let (anchor_type, save_file) = match &seg.anchor {
                ReplayAnchor::NewGame { .. } => ("new_game", format!("{dir}/anchor.json")),
                ReplayAnchor::SaveJson(_) => ("save_json", format!("{dir}/save.json")),
            };
            let events_file = format!("{dir}/events.json");
            let annotations_file =
                (!seg.annotations.is_empty()).then(|| format!("{dir}/annotations.json"));
            let thumb_file = seg.thumb_png.as_ref().map(|_| format!("{dir}/thumb.png"));
            seg_manifest.push(json!({
                "index": seg.index,
                "start_tick": seg.start_tick,
                "duration_ticks": seg.duration_ticks,
                "scene_id": seg.scene_id,
                "label": seg.label,
                "anchor_type": anchor_type,
                "save_file": save_file,
                "thumb_file": thumb_file,
                "events_file": events_file,
                "annotations_file": annotations_file,
            }));
        }

        let manifest = json!({
            "kind": REPLAY_FORMAT_KIND,
            "engine_version": bundle.engine_version,
            "total_ticks": bundle.total_ticks,
            "note": bundle.note,
            "tainted_by_debug": bundle.tainted_by_debug,
            "debug_mode": bundle.debug_mode,
            "end_screenshot": bundle.end_screenshot_png.as_ref().map(|_| "end_screenshot.png"),
            "state_file": bundle.state_json.as_ref().map(|_| "state.json"),
            "segments": seg_manifest,
        });
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
        write_tar_entry(&mut gz, "manifest.json", &manifest_bytes)?;

        if let Some(png) = &bundle.end_screenshot_png {
            write_tar_entry(&mut gz, "end_screenshot.png", png)?;
        }
        if let Some(st) = &bundle.state_json {
            let st_bytes = serde_json::to_vec_pretty(st).map_err(|e| e.to_string())?;
            write_tar_entry(&mut gz, "state.json", &st_bytes)?;
        }

        for seg in &bundle.segments {
            let dir = format!("segments/{:03}_sc{}", seg.index, seg.scene_id);
            match &seg.anchor {
                ReplayAnchor::NewGame { seed } => {
                    let anchor_bytes =
                        serde_json::to_vec(&json!({ "type": "new_game", "seed": seed }))
                            .map_err(|e| e.to_string())?;
                    write_tar_entry(&mut gz, &format!("{dir}/anchor.json"), &anchor_bytes)?;
                }
                ReplayAnchor::SaveJson(save) => {
                    // Standalone valid RePal save JSON!
                    let save_bytes = serde_json::to_vec(save).map_err(|e| e.to_string())?;
                    write_tar_entry(&mut gz, &format!("{dir}/save.json"), &save_bytes)?;
                }
            }
            if let Some(png) = &seg.thumb_png {
                write_tar_entry(&mut gz, &format!("{dir}/thumb.png"), png)?;
            }
            let events_val: Vec<Value> = seg.events.iter().map(|e| e.ev.to_json(e.tick)).collect();
            let events_bytes = serde_json::to_vec(&events_val).map_err(|e| e.to_string())?;
            write_tar_entry(&mut gz, &format!("{dir}/events.json"), &events_bytes)?;
            if !seg.annotations.is_empty() {
                let anns_val: Vec<Value> = seg
                    .annotations
                    .iter()
                    .map(ReplayAnnotation::to_json)
                    .collect();
                let anns_bytes = serde_json::to_vec(&anns_val).map_err(|e| e.to_string())?;
                write_tar_entry(&mut gz, &format!("{dir}/annotations.json"), &anns_bytes)?;
            }
        }

        // Two 512-byte zero blocks terminate a valid tar stream.
        gz.write_all(&[0u8; 1024]).map_err(|e| e.to_string())?;
        let file = gz.finish().map_err(|e| e.to_string())?;
        let _ = file.sync_all();
        Ok(())
    })();
    if let Err(e) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    fs::rename(&tmp_path, path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        e.to_string()
    })
}

/// Read a `.repal` (`.tar.gz`), gzipped JSON, or plain `.json` replay file into a [`ReplayBundle`].
pub fn read_repal_file(path: &Path) -> Result<ReplayBundle, String> {
    let raw = fs::read(path).map_err(|e| e.to_string())?;
    read_repal_bytes(&raw)
}

pub fn read_repal_bytes(raw: &[u8]) -> Result<ReplayBundle, String> {
    read_repal_bytes_with_limit(raw, MAX_REPAL_BYTES)
}

fn read_repal_bytes_with_limit(raw: &[u8], max_bytes: usize) -> Result<ReplayBundle, String> {
    let payload = if raw.len() >= 2 && raw[0] == 0x1F && raw[1] == 0x8B {
        let dec = GzDecoder::new(raw);
        let mut out = Vec::new();
        dec.take(max_bytes as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|e| e.to_string())?;
        if out.len() > max_bytes {
            return Err(format!(
                "replay archive exceeds maximum decompressed size ({max_bytes} bytes)"
            ));
        }
        out
    } else {
        if raw.len() > max_bytes {
            return Err(format!(
                "replay payload exceeds maximum size ({max_bytes} bytes)"
            ));
        }
        raw.to_vec()
    };

    // Try `.tar` container first (zero-copy slice indexing into `payload`, #269)
    if let Some(files) = read_tar_entries(&payload) {
        let manifest_bytes = *files
            .get("manifest.json")
            .ok_or("tar archive missing manifest.json")?;
        let manifest: Value =
            serde_json::from_slice(manifest_bytes).map_err(|e| e.to_string())?;
        let engine_version = manifest["engine_version"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        let total_ticks = manifest["total_ticks"].as_u64().unwrap_or(0);
        let note = manifest["note"].as_str().map(String::from);
        let tainted_by_debug = manifest["tainted_by_debug"].as_bool().unwrap_or(false);
        let debug_mode = manifest["debug_mode"].as_bool().unwrap_or(false);
        let end_screenshot_png = files.get("end_screenshot.png").map(|s| s.to_vec());
        let state_json = files
            .get("state.json")
            .and_then(|b| serde_json::from_slice::<Value>(b).ok());

        let seg_arr = manifest["segments"]
            .as_array()
            .ok_or("manifest missing segments array")?;
        let mut segments = Vec::with_capacity(seg_arr.len());
        for (idx, s) in seg_arr.iter().enumerate() {
            let save_file = s["save_file"].as_str().unwrap_or("");
            let anchor_type = s["anchor_type"].as_str().unwrap_or("save_json");
            let anchor_bytes = *files
                .get(save_file)
                .ok_or_else(|| format!("missing segment file {save_file}"))?;
            let anchor_val: Value =
                serde_json::from_slice(anchor_bytes).map_err(|e| e.to_string())?;
            let anchor = if anchor_type == "new_game" {
                ReplayAnchor::NewGame {
                    seed: anchor_val["seed"].as_u64().unwrap_or(1) as u32,
                }
            } else {
                ReplayAnchor::SaveJson(anchor_val)
            };

            let events_file = s["events_file"].as_str().unwrap_or("");
            let mut events = Vec::new();
            if let Some(&ev_bytes) = files.get(events_file) {
                if let Ok(Value::Array(arr)) = serde_json::from_slice::<Value>(ev_bytes) {
                    for item in &arr {
                        if let Some((tick, ev)) = ReplayEvent::from_json(item) {
                            events.push(ReplayEntry { tick, ev });
                        }
                    }
                }
            }
            let mut annotations = Vec::new();
            if let Some(ann_file) = s["annotations_file"].as_str() {
                if let Some(&ann_bytes) = files.get(ann_file) {
                    if let Ok(Value::Array(arr)) = serde_json::from_slice::<Value>(ann_bytes) {
                        for item in &arr {
                            if let Some(ann) = ReplayAnnotation::from_json(item) {
                                annotations.push(ann);
                            }
                        }
                    }
                }
            }
            normalize_annotations(&mut annotations);
            let thumb_png = s["thumb_file"]
                .as_str()
                .and_then(|tf| files.get(tf).map(|b| b.to_vec()));

            segments.push(ReplaySegment {
                index: s["index"].as_u64().unwrap_or(idx as u64) as usize,
                start_tick: s["start_tick"].as_u64().unwrap_or(0),
                duration_ticks: s["duration_ticks"]
                    .as_u64()
                    .unwrap_or_else(|| events.last().map(|e| e.tick + 1).unwrap_or(0)),
                scene_id: s["scene_id"].as_u64().unwrap_or(0) as u16,
                label: s["label"].as_str().unwrap_or("Chapter").to_string(),
                anchor,
                events,
                annotations,
                thumb_png,
            });
        }
        let bundle = ReplayBundle {
            engine_version,
            total_ticks,
            segments,
            end_screenshot_png,
            state_json,
            note,
            tainted_by_debug,
            debug_mode,
        };
        bundle.validate()?;
        return Ok(bundle);
    }

    // Fallback: plain JSON (or gzipped JSON)
    let doc: Value = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
    ReplayBundle::from_json(&doc)
}

/// Parse a normalized `(timestamp_ms, seq, filename)` sort key for `auto_*.repal` / `auto_*.json`
/// files so legacy `auto_{unix_secs}_sc{S}.repal` and current `auto_{unix_ms}_{seq:04}_sc{S}.repal`
/// filenames sort chronologically oldest-first (#232).
fn auto_replay_sort_key(path: &Path) -> (u128, u32, String) {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let stem = strip_replay_ext(&name);
    if let Some(rest) = stem.strip_prefix("auto_") {
        let mut parts = rest.split('_');
        if let Some(ts_str) = parts.next() {
            if let Ok(raw_ts) = ts_str.parse::<u128>() {
                // New format uses Unix milliseconds (>= 10^12) + 4-digit seq;
                // legacy format used Unix seconds (< 10^12) followed directly by `sc{S}`.
                let (ts_ms, seq) = if raw_ts >= 1_000_000_000_000 {
                    let seq = parts
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0);
                    (raw_ts, seq)
                } else {
                    (raw_ts.saturating_mul(1000), 0)
                };
                return (ts_ms, seq, name);
            }
        }
    }
    let mtime_ms = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);
    (mtime_ms, 0, name)
}

/// Write a `.repal` file to `<replays_dir>/<prefix>_<unix_ms>_<seq>_sc<S>.repal`.
/// When `auto_rotate` is true, keeps only the newest `MAX_AUTO_REPLAY_FILES` `auto_*.repal` files.
pub fn write_repal_to_dir(
    replays_dir: &Path,
    bundle: &ReplayBundle,
    prefix: &str,
    scene_id: u16,
    auto_rotate: bool,
) -> Option<PathBuf> {
    let _ = fs::create_dir_all(replays_dir);
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = ARCHIVE_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = replays_dir.join(format!("{prefix}_{ts_ms}_{seq:04}_sc{scene_id}.{REPAL_FILE_EXT}"));
    write_repal_archive(&path, bundle).ok()?;

    if auto_rotate {
        if let Ok(entries) = fs::read_dir(replays_dir) {
            let mut autos: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|s| s.to_str())
                        .map(|n| n.starts_with("auto_") && (is_repal_file_name(n) || n.ends_with(".json")))
                        .unwrap_or(false)
                })
                .collect();
            autos.sort_by_key(|p| auto_replay_sort_key(p));
            while autos.len() > MAX_AUTO_REPLAY_FILES {
                let oldest = autos.remove(0);
                let _ = fs::remove_file(oldest);
            }
        }
    }
    Some(path)
}

fn format_srt_timestamp(tick: u64) -> String {
    let total_ms = tick.saturating_mul(20); // 50 Hz -> 20 ms per tick
    let ms = total_ms % 1000;
    let total_secs = total_ms / 1000;
    let secs = total_secs % 60;
    let mins = (total_secs / 60) % 60;
    let hours = total_secs / 3600;
    format!("{hours:02}:{mins:02}:{secs:02},{ms:03}")
}

/// Export all per-segment AI thought annotations from a [`ReplayBundle`] into a standard
/// SubRip (`.srt`) subtitle file (50 Hz ticks -> `HH:MM:SS,mmm`).
/// Empty annotations (`think clear`) act as explicit cue end markers without emitting blank cues.
pub fn export_repal_to_srt(bundle: &ReplayBundle) -> String {
    const DEFAULT_MAX_CUE_TICKS: u64 = 500; // 10 seconds at 50 Hz
    let mut flat: Vec<(u64, u64, u16, usize, &str)> = Vec::new();
    for (seg_idx, seg) in bundle.segments.iter().enumerate() {
        let seg_end = seg.start_tick.saturating_add(seg.duration_ticks);
        for ann in &seg.annotations {
            flat.push((
                seg.start_tick.saturating_add(ann.tick),
                seg_end,
                seg.scene_id,
                seg_idx,
                ann.text.as_str(),
            ));
        }
    }
    let mut out = String::new();
    let mut cue_no = 1usize;
    for (i, &(start_tick, seg_end, scene_id, seg_idx, text)) in flat.iter().enumerate() {
        if text.is_empty() {
            continue;
        }
        // Clamp cue end at the scene boundary when the following segment switches scenes (#250).
        let scene_end = {
            let mut end = seg_end;
            for next_seg in bundle.segments.iter().skip(seg_idx + 1) {
                if next_seg.scene_id != scene_id {
                    break;
                }
                end = next_seg.start_tick.saturating_add(next_seg.duration_ticks);
            }
            end
        };
        let cap_end = if scene_end > start_tick {
            scene_end.min(bundle.total_ticks.max(scene_end))
        } else {
            bundle.total_ticks.max(seg_end).max(start_tick + 50)
        };
        let default_end = start_tick
            .saturating_add(DEFAULT_MAX_CUE_TICKS)
            .min(cap_end.max(start_tick + 1));
        let end_tick = match flat.get(i + 1) {
            Some(&(next_tick, _, _, _, _)) => next_tick.min(default_end).max(start_tick + 1),
            None => default_end.max(start_tick + 1),
        };
        out.push_str(&format!(
            "{cue_no}\n{} --> {}\n{text}\n\n",
            format_srt_timestamp(start_tick),
            format_srt_timestamp(end_tick)
        ));
        cue_no += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repal_tar_gz_roundtrip_and_standalone_saves() {
        let save1 = json!({"version": 1, "sceneId": 2, "cash": 500, "party": [], "roles": []});
        let bundle = ReplayBundle {
            engine_version: "0.1.0".into(),
            total_ticks: 240,
            segments: vec![
                ReplaySegment {
                    index: 0,
                    start_tick: 0,
                    duration_ticks: 100,
                    scene_id: 1,
                    label: "余杭客栈".into(),
                    anchor: ReplayAnchor::NewGame { seed: 42 },
                    events: vec![ReplayEntry {
                        tick: 10,
                        ev: ReplayEvent::DialogueDone,
                    }],
                    annotations: vec![
                        ReplayAnnotation {
                            tick: 5,
                            text: "先去客栈大堂看看".into(),
                        },
                        ReplayAnnotation {
                            tick: 60,
                            text: String::new(),
                        },
                    ],
                    thumb_png: Some(vec![0x89, b'P', b'N', b'G', 1, 2, 3]),
                },
                ReplaySegment {
                    index: 1,
                    start_tick: 100,
                    duration_ticks: 140,
                    scene_id: 2,
                    label: "余杭镇".into(),
                    anchor: ReplayAnchor::SaveJson(save1.clone()),
                    events: vec![ReplayEntry {
                        tick: 5,
                        ev: ReplayEvent::HeldDir(2),
                    }],
                    annotations: vec![ReplayAnnotation {
                        tick: 25,
                        text: "去买几株止血草".into(),
                    }],
                    thumb_png: Some(vec![0x89, b'P', b'N', b'G', 4, 5, 6]),
                },
            ],
            end_screenshot_png: Some(vec![0x89, b'P', b'N', b'G', 9, 9, 9]),
            state_json: Some(json!({"sceneId": 2})),
            note: Some("unit test".into()),
            tainted_by_debug: true,
            debug_mode: true,
        };

        let tmp = std::env::temp_dir().join("repal_archive_test.repal");
        write_repal_archive(&tmp, &bundle).expect("write .repal");
        let parsed = read_repal_file(&tmp).expect("read .repal");
        let _ = fs::remove_file(&tmp);

        assert_eq!(parsed.segments.len(), 2);
        assert_eq!(parsed.total_ticks, 240);
        assert!(parsed.tainted_by_debug, "tainted_by_debug flag must survive .repal roundtrip");
        assert!(parsed.debug_mode, "debug_mode flag must survive .repal roundtrip");
        assert_eq!(parsed.segments[0].label, "余杭客栈");
        assert_eq!(parsed.segments[1].label, "余杭镇");
        assert_eq!(parsed.segments[1].events.len(), 1);
        assert_eq!(parsed.segments[0].annotations.len(), 2);
        assert_eq!(parsed.segments[0].annotations[0].text, "先去客栈大堂看看");
        assert_eq!(parsed.segments[0].annotations[1].text, "");
        assert_eq!(parsed.segments[1].annotations.len(), 1);
        assert_eq!(parsed.segments[1].annotations[0].text, "去买几株止血草");
        assert_eq!(parsed.segments[1].thumb_png.as_deref(), Some(&[0x89, b'P', b'N', b'G', 4, 5, 6][..]));
        assert_eq!(parsed.end_screenshot_png.as_deref(), Some(&[0x89, b'P', b'N', b'G', 9, 9, 9][..]));
        match &parsed.segments[1].anchor {
            ReplayAnchor::SaveJson(v) => assert_eq!(v["cash"], 500),
            _ => panic!("expected SaveJson"),
        }

        // Verify SRT subtitle export: cue 1 ends at explicit clear tick 60 (1.200s),
        // cue 2 starts at abs tick 100+25=125 (2.500s) and ends at bundle total_ticks 240 (4.800s).
        let srt = export_repal_to_srt(&parsed);
        assert_eq!(
            srt,
            "1\n00:00:00,100 --> 00:00:01,200\n先去客栈大堂看看\n\n2\n00:00:02,500 --> 00:00:04,800\n去买几株止血草\n\n"
        );
    }

    #[test]
    fn repal_rejects_decompression_bomb() {
        let mut gz = GzEncoder::new(Vec::new(), Compression::best());
        gz.write_all(&vec![b'A'; 64 * 1024]).unwrap();
        let compressed = gz.finish().unwrap();
        assert!(compressed.len() < 1024);
        let err = read_repal_bytes_with_limit(&compressed, 4096).unwrap_err();
        assert!(err.contains("maximum decompressed size"), "unexpected err: {err}");
    }

    #[test]
    fn write_repal_to_dir_generates_unique_files_in_same_millisecond() {
        let dir = std::env::temp_dir().join(format!("repal_uniq_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let bundle = ReplayBundle {
            engine_version: "0.1.0".into(),
            total_ticks: 10,
            segments: vec![ReplaySegment {
                index: 0,
                start_tick: 0,
                duration_ticks: 10,
                scene_id: 1,
                label: "t".into(),
                anchor: ReplayAnchor::NewGame { seed: 1 },
                events: vec![],
                annotations: vec![],
                thumb_png: None,
            }],
            end_screenshot_png: None,
            state_json: None,
            note: None,
            tainted_by_debug: false,
            debug_mode: false,
        };
        let p1 = write_repal_to_dir(&dir, &bundle, "auto", 1, false).unwrap();
        let p2 = write_repal_to_dir(&dir, &bundle, "auto", 1, false).unwrap();
        assert_ne!(p1, p2, "rapid writes must not collide");
        assert!(p1.is_file() && p2.is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_replay_rotation_sorts_mixed_old_and_new_filenames_oldest_first() {
        let dir = std::env::temp_dir().join(format!("repal_rot_mixed_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Create 20 legacy-format archives (`auto_{unix_secs}_sc5.repal`, older)
        // plus 2 new-format archives with newer timestamps (`auto_{unix_ms}_{seq}_sc5.repal`).
        // Plain ASCII sort would put the newer ms filenames BEFORE the older secs filenames
        // because `'0'..'9'` < `'_'`, deleting the newest archives!
        for i in 0..20u64 {
            let legacy = dir.join(format!("auto_{}_sc5.repal", 1_700_000_000 + i));
            fs::write(&legacy, b"legacy").unwrap();
        }
        let newer1 = dir.join("auto_1795000000123_0001_sc5.repal");
        fs::write(&newer1, b"newer1").unwrap();

        let bundle = ReplayBundle {
            engine_version: "0.1.0".into(),
            total_ticks: 10,
            segments: vec![ReplaySegment {
                index: 0,
                start_tick: 0,
                duration_ticks: 10,
                scene_id: 5,
                label: "t".into(),
                anchor: ReplayAnchor::NewGame { seed: 1 },
                events: vec![],
                annotations: vec![],
                thumb_png: None,
            }],
            end_screenshot_png: None,
            state_json: None,
            note: None,
            tainted_by_debug: false,
            debug_mode: false,
        };
        let newest = write_repal_to_dir(&dir, &bundle, "auto", 5, true).unwrap();

        assert!(newest.is_file(), "newly written archive must NOT be rotated out");
        assert!(newer1.is_file(), "newer ms-format archive must survive rotation");
        // The two oldest legacy files (1700000000, 1700000001) should have been deleted.
        assert!(!dir.join("auto_1700000000_sc5.repal").exists());
        assert!(!dir.join("auto_1700000001_sc5.repal").exists());
        assert!(dir.join("auto_1700000019_sc5.repal").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn repal_validates_bounds_and_normalizes_annotations_and_srt_boundaries() {
        // #265: Excessive total_ticks rejected.
        let mut bad = ReplayBundle {
            engine_version: "0.3.11".into(),
            total_ticks: pal_core::game::replay_rec::MAX_REPLAY_DURATION_TICKS + 1,
            segments: vec![ReplaySegment {
                index: 0,
                start_tick: 0,
                duration_ticks: 10,
                scene_id: 1,
                label: "t".into(),
                anchor: ReplayAnchor::NewGame { seed: 1 },
                events: vec![],
                annotations: vec![],
                thumb_png: None,
            }],
            end_screenshot_png: None,
            state_json: None,
            note: None,
            tainted_by_debug: false,
            debug_mode: false,
        };
        assert!(bad.validate().is_err(), "#265: excessive total_ticks must fail validation");

        // #250: Unsorted/duplicate annotations normalized and SRT clamped at scene boundary.
        bad.total_ticks = 600;
        bad.segments = vec![
            ReplaySegment {
                index: 0,
                start_tick: 0,
                duration_ticks: 200,
                scene_id: 1,
                label: "s1".into(),
                anchor: ReplayAnchor::NewGame { seed: 1 },
                events: vec![],
                annotations: vec![
                    ReplayAnnotation { tick: 150, text: "先去问问".into() },
                    ReplayAnnotation { tick: 150, text: "先去问问".into() },
                    ReplayAnnotation { tick: 10, text: "第一步".into() },
                ],
                thumb_png: None,
            },
            ReplaySegment {
                index: 1,
                start_tick: 200,
                duration_ticks: 400,
                scene_id: 2,
                label: "s2".into(),
                anchor: ReplayAnchor::NewGame { seed: 1 },
                events: vec![],
                annotations: vec![],
                thumb_png: None,
            },
        ];
        normalize_annotations(&mut bad.segments[0].annotations);
        assert_eq!(bad.segments[0].annotations.len(), 2);
        assert_eq!(bad.segments[0].annotations[0].tick, 10);
        assert_eq!(bad.segments[0].annotations[1].tick, 150);
        let srt = export_repal_to_srt(&bad);
        // Second cue starts at 150 (3.000s) and must clamp at segment 0 end tick 200 (4.000s), not 150+400=550.
        assert!(srt.contains("00:00:03,000 --> 00:00:04,000"), "unexpected srt:\n{srt}");
    }

    #[test]
    fn repal_gz_names_are_recognised_and_sorted_like_legacy() {
        assert!(is_repal_file_name("auto_1795000000123_0001_sc5.repal.gz"));
        assert!(is_repal_file_name("OLD.REPAL"));
        assert!(!is_repal_file_name("notes.gz"));
        let a = auto_replay_sort_key(Path::new("auto_1795000000123_0001_sc5.repal.gz"));
        let b = auto_replay_sort_key(Path::new("auto_1795000000123_0001_sc5.repal"));
        assert_eq!((a.0, a.1), (b.0, b.1), "new and legacy names must share the same sort key");
        let old = auto_replay_sort_key(Path::new("auto_1700000000_sc5.repal"));
        assert!(old.0 < a.0, "legacy seconds-based names sort before newer ms-based ones");
    }
}
