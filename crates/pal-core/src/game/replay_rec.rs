//! Engine-level segmented deterministic input recorder (`RePal_CoreReplay_v3`).
//!
//! Each recording is a sequence of self-contained [`ReplaySegment`]s:
//! - Segment 0 starts from either `NewGame { seed }` or a standard `SaveJson`.
//! - Subsequent segments split automatically ONLY in **idle overworld state** (no active script,
//!   battle, or modal) once at least [`SEGMENT_MIN_GAP_TICKS`] (30 s at 50 Hz) have elapsed
//!   (or immediately upon leaving the opening New Game intro).
//! - Every segment `1..` starts with a 100% valid standard RePal `save_json` + optional
//!   `thumb_png` thumbnail, and records its own local `events` starting from local tick `0`.
//! - During playback, transitioning to the next segment hard-loads its `save_json` to guarantee
//!   zero drift accumulation across segments.

use super::BattleCmd;
use serde_json::{json, Value};

pub const REPLAY_FORMAT_KIND: &str = "RePal_CoreReplay_v3";
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Minimum `engine_version` whose deterministic simulation (battle/RNG/VM) is compatible with the
/// current engine. Replays recorded on older versions (e.g. `v0.3.x` prior to the `v0.4.0` strict
/// SDLPAL `fight.c` battle formula overhaul) display an incompatibility warning during playback.
pub const MIN_COMPAT_REPLAY_VERSION: &str = "0.4.0";

/// Parse a `major.minor.patch` semver triple (ignoring any leading `v` or `-prerelease` suffix).
pub fn parse_semver_triple(raw: &str) -> Option<(u32, u32, u32)> {
    let clean = raw.trim().trim_start_matches(['v', 'V']);
    let core = clean.split(['-', '+']).next()?.trim();
    if core.is_empty() {
        return None;
    }
    let mut parts = core.split('.');
    let maj = parts.next()?.parse::<u32>().ok()?;
    let min = parts.next().unwrap_or("0").parse::<u32>().ok()?;
    let pat = parts.next().unwrap_or("0").parse::<u32>().ok()?;
    Some((maj, min, pat))
}

/// Returns `true` iff `recorded_version` meets or exceeds [`MIN_COMPAT_REPLAY_VERSION`].
pub fn is_replay_engine_compatible(recorded_version: &str) -> bool {
    let Some(rec) = parse_semver_triple(recorded_version) else {
        return false;
    };
    let Some(min_req) = parse_semver_triple(MIN_COMPAT_REPLAY_VERSION) else {
        return true;
    };
    rec >= min_req
}
/// Minimum ticks between automatic idle-overworld segment splits (30 min at 50 Hz).
pub const SEGMENT_MIN_GAP_TICKS: u64 = 90_000;
/// Minimum recorded events before auto-archiving a finished session.
pub const MIN_EVENTS_FOR_ARCHIVE: usize = 20;
/// Safety cap on total in-memory recorded events across all segments (~15 MB worst-case).
pub const MAX_REPLAY_EVENTS: usize = 500_000;
/// Safety cap on replay timeline ticks accepted at parse time (~55 hours at 50 Hz, #265).
pub const MAX_REPLAY_DURATION_TICKS: u64 = 10_000_000;
/// Safety cap on segments accepted in a single replay bundle at parse time (#265).
pub const MAX_REPLAY_SEGMENTS: usize = 4096;
/// Maximum character length of a single AI commentary / thought subtitle (`think <text>`).
/// Full text up to 200 chars is stored in `.repal` (`annotations.json`) and `.srt` export,
/// while on-screen rendering wraps and ellipsizes cleanly to 2 lines (~80 CJK chars).
pub const MAX_AI_THOUGHT_CHARS: usize = 200;
/// Coalescing window in logic ticks (`50` ticks = 1.0 s at 50 Hz): a new thought within this
/// window replaces the active segment's last annotation text in-place while preserving its
/// original start tick.
pub const AI_THOUGHT_COALESCE_TICKS: u64 = 50;
/// Ring cap on AI thought annotations per replay segment (oldest entry dropped when full).
pub const MAX_AI_THOUGHTS_PER_SEGMENT: usize = 200;

/// Sanitize an AI thought string for safe rendering and compact `.repal` / `.srt` storage:
/// - Strips non-BMP emoji/pictographs, dingbats (U+2600..=U+27BF), variation selectors, ZWJ,
///   and Unicode bidi/directional format controls (U+202A..=U+202E, U+2066..=U+2069, U+200E/F, U+061C).
/// - Collapses newlines, tabs, and ASCII control characters into single spaces and trims ends.
/// - Truncates to [`MAX_AI_THOUGHT_CHARS`] characters (appending `…` when truncated).
pub fn sanitize_ai_thought(raw: &str) -> (String, bool) {
        let mut out = String::new();
        out.reserve(raw.len());
        let mut prev_space = true;
        for ch in raw.chars() {
            let cp = ch as u32;
            // Strip emoji/pictograph/symbol/tag blocks (while preserving rare CJK Ext B+ ideographs
            // U+20000..=U+2FA1F), variation selectors, ZWJ, and bidi directional controls (#239, #240).
            let is_emoji_or_control = (0x1F000..=0x1FFFF).contains(&cp)
                || (0xE0000..=0xE01EF).contains(&cp)
                || (0x2600..=0x27BF).contains(&cp)
                || (0xFE00..=0xFE0F).contains(&cp)
                || cp == 0x200D
                || (0x202A..=0x202E).contains(&cp)
                || (0x2066..=0x2069).contains(&cp)
                || cp == 0x200E
                || cp == 0x200F
                || cp == 0x061C;
            if is_emoji_or_control {
                continue;
            }
            if ch.is_control() || ch.is_whitespace() {
                if !prev_space {
                    out.push(' ');
                    prev_space = true;
                }
                continue;
            }
            out.push(ch);
            prev_space = false;
        }
        let cleaned = out;
    let trimmed = cleaned.trim();
    let char_count = trimmed.chars().count();
    if char_count <= MAX_AI_THOUGHT_CHARS {
        (trimmed.to_string(), false)
    } else {
        let mut out: String = trimmed
            .chars()
            .take(MAX_AI_THOUGHT_CHARS.saturating_sub(1))
            .collect();
        out.push('…');
        (out, true)
    }
}

/// Purely cosmetic AI thought / commentary subtitle entry stored in its own per-segment
/// `annotations.json` track (never mixed into deterministic input `events.json`).
/// An empty `text` explicitly clears the subtitle at `tick`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayAnnotation {
    pub tick: u64,
    pub text: String,
}

impl ReplayAnnotation {
    pub fn to_json(&self) -> Value {
        json!({ "t": self.tick, "text": self.text })
    }

    pub fn from_json(v: &Value) -> Option<Self> {
        let tick = v.get("t").or_else(|| v.get("tick"))?.as_u64()?;
        let raw = v.get("text")?.as_str()?;
        let text = if raw.is_empty() {
            String::new()
        } else {
            sanitize_ai_thought(raw).0
        };
        Some(Self { tick, text })
    }
}

/// Normalize a segment's AI thought annotation track on read (#250):
/// - Stable-sorts by `tick` so binary search (`partition_point`) and SRT export never break on unsorted input.
/// - Deduplicates identical-tick entries (keeping the latest entry at that tick).
/// - Enforces [`MAX_AI_THOUGHTS_PER_SEGMENT`] (retaining the latest entries).
pub fn normalize_annotations(anns: &mut Vec<ReplayAnnotation>) {
    anns.sort_by_key(|a| a.tick);
    let mut deduped: Vec<ReplayAnnotation> = Vec::with_capacity(anns.len());
    for ann in anns.drain(..) {
        if let Some(last) = deduped.last_mut() {
            if last.tick == ann.tick {
                last.text = ann.text;
                continue;
            }
        }
        deduped.push(ann);
    }
    if deduped.len() > MAX_AI_THOUGHTS_PER_SEGMENT {
        let drop_n = deduped.len() - MAX_AI_THOUGHTS_PER_SEGMENT;
        deduped.drain(0..drop_n);
    }
    *anns = deduped;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnnotationRecordResult {
    pub recorded: bool,
    pub coalesced: bool,
}

#[derive(Clone, Debug)]
pub enum ReplayAnchor {
    NewGame { seed: u32 },
    SaveJson(Value),
}

impl ReplayAnchor {
    pub fn to_json(&self) -> Value {
        match self {
            ReplayAnchor::NewGame { seed } => json!({ "type": "new_game", "seed": seed }),
            ReplayAnchor::SaveJson(v) => json!({ "type": "save_json", "data": v }),
        }
    }

    pub fn from_json(v: &Value, default_seed: u32) -> Option<Self> {
        match v["type"].as_str()? {
            "new_game" => Some(ReplayAnchor::NewGame {
                seed: v["seed"].as_u64().unwrap_or(default_seed as u64) as u32,
            }),
            "save_json" => {
                let data = v.get("data")?;
                if let Some(s) = data.as_str() {
                    serde_json::from_str::<Value>(s)
                        .ok()
                        .map(ReplayAnchor::SaveJson)
                } else if data.is_object() {
                    Some(ReplayAnchor::SaveJson(data.clone()))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Deterministic debug/cheat mutation recorded when `debug_mode == true` (#235).
/// Rejected / treated as a strict no-op whenever `debug_mode == false`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugAction {
    Tp { scene_id: u16, coords: Option<(i32, i32)> },
    Heal,
    God(bool),
    Cash(i32),
    Item { item_id: u16, qty: i32 },
    EventState { event_id: u16, state: i32 },
    Battle(u16),
    Script(String),
}

impl DebugAction {
    pub fn to_json(&self) -> Value {
        match self {
            DebugAction::Tp { scene_id, coords } => match coords {
                Some((x, y)) => json!({ "op": "tp", "sc": scene_id, "x": x, "y": y }),
                None => json!({ "op": "tp", "sc": scene_id }),
            },
            DebugAction::Heal => json!({ "op": "heal" }),
            DebugAction::God(on) => json!({ "op": "god", "on": on }),
            DebugAction::Cash(n) => json!({ "op": "cash", "n": n }),
            DebugAction::Item { item_id, qty } => json!({ "op": "item", "id": item_id, "n": qty }),
            DebugAction::EventState { event_id, state } => {
                json!({ "op": "ev_state", "id": event_id, "st": state })
            }
            DebugAction::Battle(gid) => json!({ "op": "battle", "gid": gid }),
            DebugAction::Script(lbl) => json!({ "op": "script", "lbl": lbl }),
        }
    }

    pub fn from_json(v: &Value) -> Option<Self> {
        match v.get("op").and_then(Value::as_str)? {
            "tp" => {
                let scene_id = v.get("sc")?.as_u64()? as u16;
                let coords = match (v.get("x").and_then(Value::as_i64), v.get("y").and_then(Value::as_i64)) {
                    (Some(x), Some(y)) => Some((x as i32, y as i32)),
                    _ => None,
                };
                Some(DebugAction::Tp { scene_id, coords })
            }
            "heal" => Some(DebugAction::Heal),
            "god" => Some(DebugAction::God(v.get("on").and_then(Value::as_bool).unwrap_or(true))),
            "cash" => Some(DebugAction::Cash(v.get("n")?.as_i64()? as i32)),
            "item" => Some(DebugAction::Item {
                item_id: v.get("id")?.as_u64()? as u16,
                qty: v.get("n")?.as_i64()? as i32,
            }),
            "ev_state" => Some(DebugAction::EventState {
                event_id: v.get("id")?.as_u64()? as u16,
                state: v.get("st")?.as_i64()? as i32,
            }),
            "battle" => Some(DebugAction::Battle(v.get("gid")?.as_u64()? as u16)),
            "script" => Some(DebugAction::Script(v.get("lbl")?.as_str()?.to_string())),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayEvent {
    HeldDir(i8),
    Interact,
    Click(i32, i32),
    Steer(Option<(i32, i32)>),
    DialogueDone,
    Choice(bool),
    ModalDone,
    BattleAttack(usize),
    BattleMagic { magic: u16, target: usize },
    BattleCoop(usize),
    BattleItem(u16),
    BattleItemTarget { item: u16, target: usize },
    BattleDefend,
    BattleFlee,
    BattleAuto,
    BattleCancelAuto,
    UseItem { item_id: u16, role: Option<usize> },
    EquipItem { role: usize, item_id: u16 },
    CastFieldMagic { caster: usize, magic_id: u16, target: Option<usize> },
    BuyItem { item_id: u16, price: u32 },
    SellItem { item_id: u16, price: u32 },
    /// Deterministic debug mutation gated by `debug_mode` (#235).
    Debug(DebugAction),
}

impl ReplayEvent {
    pub fn to_json(&self, tick: u64) -> Value {
        match self {
            ReplayEvent::HeldDir(d) => json!({ "t": tick, "e": "dir", "d": d }),
            ReplayEvent::Interact => json!({ "t": tick, "e": "interact" }),
            ReplayEvent::Click(x, y) => json!({ "t": tick, "e": "click", "x": x, "y": y }),
            ReplayEvent::Steer(Some((x, y))) => json!({ "t": tick, "e": "steer", "x": x, "y": y }),
            ReplayEvent::Steer(None) => json!({ "t": tick, "e": "steer_end" }),
            ReplayEvent::DialogueDone => json!({ "t": tick, "e": "dlg" }),
            ReplayEvent::Choice(yes) => json!({ "t": tick, "e": "choice", "yes": yes }),
            ReplayEvent::ModalDone => json!({ "t": tick, "e": "modal" }),
            ReplayEvent::BattleAttack(target) => json!({ "t": tick, "e": "b_atk", "target": target }),
            ReplayEvent::BattleMagic { magic, target } => {
                json!({ "t": tick, "e": "b_mag", "id": magic, "target": target })
            }
            ReplayEvent::BattleCoop(target) => json!({ "t": tick, "e": "b_coop", "target": target }),
            ReplayEvent::BattleItem(id) => json!({ "t": tick, "e": "b_item", "id": id }),
            ReplayEvent::BattleItemTarget { item, target } => {
                json!({ "t": tick, "e": "b_item_t", "id": item, "target": target })
            }
            ReplayEvent::BattleDefend => json!({ "t": tick, "e": "b_def" }),
            ReplayEvent::BattleFlee => json!({ "t": tick, "e": "b_flee" }),
            ReplayEvent::BattleAuto => json!({ "t": tick, "e": "b_auto" }),
            ReplayEvent::BattleCancelAuto => json!({ "t": tick, "e": "b_cancel_auto" }),
            ReplayEvent::UseItem { item_id, role } => {
                json!({ "t": tick, "e": "use", "id": item_id, "role": role })
            }
            ReplayEvent::EquipItem { role, item_id } => {
                json!({ "t": tick, "e": "equip", "role": role, "id": item_id })
            }
            ReplayEvent::CastFieldMagic {
                caster,
                magic_id,
                target,
            } => json!({ "t": tick, "e": "cast", "caster": caster, "id": magic_id, "target": target }),
            ReplayEvent::BuyItem { item_id, price } => {
                json!({ "t": tick, "e": "buy", "id": item_id, "price": price })
            }
            ReplayEvent::SellItem { item_id, price } => {
                json!({ "t": tick, "e": "sell", "id": item_id, "price": price })
            }
            ReplayEvent::Debug(act) => json!({ "t": tick, "e": "dbg", "act": act.to_json() }),
        }
    }

    pub fn from_json(v: &Value) -> Option<(u64, Self)> {
        let tick = v.get("t").or_else(|| v.get("tick"))?.as_u64()?;
        let kind = v.get("e").or_else(|| v.get("op")).and_then(Value::as_str)?;
        let ev = match kind {
            "dir" => ReplayEvent::HeldDir(v["d"].as_i64().unwrap_or(-1) as i8),
            "interact" => ReplayEvent::Interact,
            "click" => ReplayEvent::Click(
                v["x"].as_i64().unwrap_or(0) as i32,
                v["y"].as_i64().unwrap_or(0) as i32,
            ),
            "steer" => ReplayEvent::Steer(Some((
                v["x"].as_i64().unwrap_or(0) as i32,
                v["y"].as_i64().unwrap_or(0) as i32,
            ))),
            "steer_end" => ReplayEvent::Steer(None),
            "dlg" | "dialogue_done" => ReplayEvent::DialogueDone,
            "choice" => ReplayEvent::Choice(v["yes"].as_bool().unwrap_or(true)),
            "modal" | "modal_done" => ReplayEvent::ModalDone,
            "b_atk" => ReplayEvent::BattleAttack(v["target"].as_u64().unwrap_or(0) as usize),
            "b_mag" => ReplayEvent::BattleMagic {
                magic: v["id"].as_u64().unwrap_or(0) as u16,
                target: v["target"].as_u64().unwrap_or(0) as usize,
            },
            "b_coop" => ReplayEvent::BattleCoop(v["target"].as_u64().unwrap_or(0) as usize),
            "b_item" => ReplayEvent::BattleItem(v["id"].as_u64().unwrap_or(0) as u16),
            "b_item_t" => ReplayEvent::BattleItemTarget {
                item: v["id"].as_u64().unwrap_or(0) as u16,
                target: v["target"].as_u64().unwrap_or(0) as usize,
            },
            "b_def" => ReplayEvent::BattleDefend,
            "b_flee" => ReplayEvent::BattleFlee,
            "b_auto" => ReplayEvent::BattleAuto,
            "b_cancel_auto" => ReplayEvent::BattleCancelAuto,
            "use" => ReplayEvent::UseItem {
                item_id: v["id"].as_u64().unwrap_or(0) as u16,
                role: v["role"].as_u64().map(|r| r as usize),
            },
            "equip" => ReplayEvent::EquipItem {
                role: v["role"].as_u64().unwrap_or(0) as usize,
                item_id: v["id"].as_u64().unwrap_or(0) as u16,
            },
            "cast" => ReplayEvent::CastFieldMagic {
                caster: v["caster"].as_u64().unwrap_or(0) as usize,
                magic_id: v["id"].as_u64().unwrap_or(0) as u16,
                target: v["target"].as_u64().map(|r| r as usize),
            },
            "buy" => ReplayEvent::BuyItem {
                item_id: v["id"].as_u64().unwrap_or(0) as u16,
                price: v["price"].as_u64().unwrap_or(0) as u32,
            },
            "sell" => ReplayEvent::SellItem {
                item_id: v["id"].as_u64().unwrap_or(0) as u16,
                price: v["price"].as_u64().unwrap_or(0) as u32,
            },
            "dbg" => ReplayEvent::Debug(DebugAction::from_json(v.get("act")?)?),
            _ => return None,
        };
        Some((tick, ev))
    }

    pub fn from_battle_cmd(cmd: BattleCmd) -> Self {
        match cmd {
            BattleCmd::Attack { target } => ReplayEvent::BattleAttack(target),
            BattleCmd::Magic { magic, target } => ReplayEvent::BattleMagic { magic, target },
            BattleCmd::Coop { target } => ReplayEvent::BattleCoop(target),
            BattleCmd::Item { item } => ReplayEvent::BattleItem(item),
            BattleCmd::ItemTarget { item, target } => ReplayEvent::BattleItemTarget { item, target },
            BattleCmd::Defend => ReplayEvent::BattleDefend,
            BattleCmd::Flee => ReplayEvent::BattleFlee,
            BattleCmd::Auto => ReplayEvent::BattleAuto,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReplayEntry {
    pub tick: u64,
    pub ev: ReplayEvent,
}

#[derive(Clone, Debug)]
pub struct ReplaySegment {
    pub index: usize,
    /// Cumulative start tick within the full replay timeline.
    pub start_tick: u64,
    /// Duration of this segment in ticks.
    pub duration_ticks: u64,
    pub scene_id: u16,
    pub label: String,
    pub anchor: ReplayAnchor,
    /// Local events with `tick` starting at 0 for this segment.
    pub events: Vec<ReplayEntry>,
    /// Separate per-segment AI thought / commentary annotation track (`annotations.json`),
    /// kept out of `events` so dialogue reading pause and deterministic input replay stay untouched.
    pub annotations: Vec<ReplayAnnotation>,
    /// Optional PNG thumbnail captured by desktop frontend at segment start.
    pub thumb_png: Option<Vec<u8>>,
}

/// Complete self-contained segmented replay (serializable to `.repal` `.tar.gz` or JSON).
#[derive(Clone, Debug)]
pub struct ReplayBundle {
    pub engine_version: String,
    pub total_ticks: u64,
    pub segments: Vec<ReplaySegment>,
    pub end_screenshot_png: Option<Vec<u8>>,
    pub state_json: Option<Value>,
    pub note: Option<String>,
    /// True when any debug/cheat command mutated world state during this recording (#199).
    pub tainted_by_debug: bool,
    /// Explicit session debug-mode flag (#235): when `true`, `ReplayEvent::Debug` events are
    /// dispatched deterministically and the `DEBUG ENABLED` watermark is rendered during playback.
    pub debug_mode: bool,
}

/// Bug reports keep only the last this-many segments (checkpoints): each segment starts from its
/// own full save anchor, so the tail replays exactly while staying small.
pub const BUG_REPORT_SEGMENTS: usize = 2;

impl ReplayBundle {
    /// Keep only the last `n` segments. Segments are self-contained (save anchor + segment-relative
    /// event ticks), so this renumbers `index`, rebases `start_tick` to 0 and shortens
    /// `total_ticks`. Bundle-level `tainted_by_debug` / `debug_mode` are kept as-is (conservative).
    pub fn keep_last_segments(&mut self, n: usize) {
        if n == 0 || self.segments.len() <= n {
            return;
        }
        let drop = self.segments.len() - n;
        self.segments.drain(..drop);
        let base = self.segments[0].start_tick;
        for (i, seg) in self.segments.iter_mut().enumerate() {
            seg.index = i;
            seg.start_tick -= base;
        }
        self.total_ticks = self.total_ticks.saturating_sub(base);
    }

    /// Validate replay timeline bounds at parse time so crafted archives cannot trigger
    /// unbounded tick loops or out-of-order segment seeks across Desktop/Web/Headless (#265).
    pub fn validate(&self) -> Result<(), String> {
        if self.segments.len() > MAX_REPLAY_SEGMENTS {
            return Err(format!(
                "replay exceeds maximum segment count ({MAX_REPLAY_SEGMENTS})"
            ));
        }
        if self.total_ticks > MAX_REPLAY_DURATION_TICKS {
            return Err(format!(
                "replay total_ticks ({}) exceeds safety limit ({MAX_REPLAY_DURATION_TICKS})",
                self.total_ticks
            ));
        }
        let mut prev_start = 0u64;
        for (i, seg) in self.segments.iter().enumerate() {
            if seg.duration_ticks > MAX_REPLAY_DURATION_TICKS {
                return Err(format!(
                    "segment {i} duration_ticks ({}) exceeds safety limit ({MAX_REPLAY_DURATION_TICKS})",
                    seg.duration_ticks
                ));
            }
            if i > 0 && seg.start_tick < prev_start {
                return Err(format!(
                    "segment {i} start_tick ({}) is not monotonic (prev {prev_start})",
                    seg.start_tick
                ));
            }
            if seg.start_tick.saturating_add(seg.duration_ticks) > MAX_REPLAY_DURATION_TICKS {
                return Err(format!(
                    "segment {i} end tick exceeds safety limit ({MAX_REPLAY_DURATION_TICKS})"
                ));
            }
            prev_start = seg.start_tick;
        }
        Ok(())
    }

    pub fn total_events(&self) -> usize {
        self.segments.iter().map(|s| s.events.len()).sum()
    }

    pub fn last_scene_id(&self) -> u16 {
        self.segments.last().map(|s| s.scene_id).unwrap_or(0)
    }

    /// Export as a single JSON document (for Web WASM / legacy compatibility).
    pub fn to_json(&self) -> Value {
        let segs: Vec<Value> = self
            .segments
            .iter()
            .map(|s| {
                let events: Vec<Value> = s.events.iter().map(|e| e.ev.to_json(e.tick)).collect();
                let mut obj = json!({
                    "index": s.index,
                    "start_tick": s.start_tick,
                    "duration_ticks": s.duration_ticks,
                    "scene_id": s.scene_id,
                    "label": s.label,
                    "anchor": s.anchor.to_json(),
                    "events": events,
                });
                if !s.annotations.is_empty() {
                    let anns: Vec<Value> = s.annotations.iter().map(ReplayAnnotation::to_json).collect();
                    obj["annotations"] = Value::Array(anns);
                }
                obj
            })
            .collect();
        json!({
            "kind": REPLAY_FORMAT_KIND,
            "engine_version": self.engine_version,
            "total_ticks": self.total_ticks,
            "note": self.note,
            "tainted_by_debug": self.tainted_by_debug,
            "debug_mode": self.debug_mode,
            "segments": segs,
        })
    }

    /// Parse `RePal_CoreReplay_v3`, legacy `RePal_CoreReplay_v2`, `PalRemake_CoreReplay_v1`,
    /// or `RePal_CoreDiag_v1` JSON into a unified [`ReplayBundle`].
    pub fn from_json(doc: &Value) -> Result<Self, String> {
        let rep = doc.get("replay").unwrap_or(doc);
        let kind = rep["kind"].as_str().unwrap_or("");
        if !kind.contains("CoreReplay") {
            return Err("not a CoreReplay JSON".into());
        }
        let engine_version = rep["engine_version"]
            .as_str()
            .unwrap_or("legacy-v1")
            .to_string();
        let note = doc
            .get("note")
            .or_else(|| rep.get("note"))
            .and_then(Value::as_str)
            .map(String::from);
        let tainted_by_debug = rep["tainted_by_debug"]
            .as_bool()
            .or_else(|| doc["tainted_by_debug"].as_bool())
            .unwrap_or(false);
        let debug_mode = rep["debug_mode"]
            .as_bool()
            .or_else(|| doc["debug_mode"].as_bool())
            .unwrap_or(false);
        let state_json = doc.get("state").cloned();

        // v3 segmented format
        if let Some(arr) = rep.get("segments").and_then(Value::as_array) {
            let mut segments = Vec::with_capacity(arr.len());
            let mut cum_tick = 0u64;
            for (idx, s) in arr.iter().enumerate() {
                let anchor = ReplayAnchor::from_json(&s["anchor"], 1)
                    .ok_or_else(|| format!("segment {idx} missing valid anchor"))?;
                let mut events = Vec::new();
                if let Some(ev_arr) = s["events"].as_array() {
                    for item in ev_arr {
                        if let Some((tick, ev)) = ReplayEvent::from_json(item) {
                            events.push(ReplayEntry { tick, ev });
                        }
                    }
                }
                let mut annotations = Vec::new();
                if let Some(ann_arr) = s.get("annotations").and_then(Value::as_array) {
                    for item in ann_arr {
                        if let Some(ann) = ReplayAnnotation::from_json(item) {
                            annotations.push(ann);
                        }
                    }
                }
                normalize_annotations(&mut annotations);
                let start_tick = s["start_tick"].as_u64().unwrap_or(cum_tick);
                let duration_ticks = s["duration_ticks"]
                    .as_u64()
                    .unwrap_or_else(|| events.last().map(|e| e.tick + 1).unwrap_or(0));
                cum_tick = start_tick.saturating_add(duration_ticks);
                segments.push(ReplaySegment {
                    index: idx,
                    start_tick,
                    duration_ticks,
                    scene_id: s["scene_id"].as_u64().unwrap_or(0) as u16,
                    label: s["label"].as_str().unwrap_or("Chapter").to_string(),
                    anchor,
                    events,
                    annotations,
                    thumb_png: None,
                });
            }
            let total_ticks = rep["total_ticks"].as_u64().unwrap_or(cum_tick);
            let bundle = Self {
                engine_version,
                total_ticks,
                segments,
                end_screenshot_png: None,
                state_json,
                note,
                tainted_by_debug,
                debug_mode,
            };
            bundle.validate()?;
            return Ok(bundle);
        }

        // Legacy v1 / v2 monolithic format -> convert into segments automatically
        let seed = rep["seed"].as_u64().unwrap_or(1) as u32;
        let root_anchor = ReplayAnchor::from_json(&rep["anchor"], seed)
            .ok_or_else(|| "replay missing anchor".to_string())?;
        let mut all_events = Vec::new();
        if let Some(arr) = rep["log"].as_array() {
            for item in arr {
                if let Some((tick, ev)) = ReplayEvent::from_json(item) {
                    all_events.push(ReplayEntry { tick, ev });
                }
            }
        }
        let total_ticks = rep["total_ticks"]
            .as_u64()
            .unwrap_or_else(|| all_events.last().map(|e| e.tick + 1).unwrap_or(0));

        // Split legacy monolithic log at any recorded checkpoints
        let mut split_points: Vec<(u64, u16, String, ReplayAnchor)> = vec![(
            0,
            1,
            "Start".to_string(),
            root_anchor,
        )];
        if let Some(cps) = rep["checkpoints"].as_array() {
            for c in cps {
                if let (Some(tick), Some(save)) = (c["tick"].as_u64(), c.get("save")) {
                    if tick > 0 && save.is_object() {
                        split_points.push((
                            tick,
                            c["scene_id"].as_u64().unwrap_or(0) as u16,
                            c["label"].as_str().unwrap_or("Checkpoint").to_string(),
                            ReplayAnchor::SaveJson(save.clone()),
                        ));
                    }
                }
            }
        }
        split_points.sort_by_key(|p| p.0);

        let mut segments = Vec::with_capacity(split_points.len());
        for i in 0..split_points.len() {
            let (start_tick, scene_id, label, anchor) = split_points[i].clone();
            let end_tick = split_points
                .get(i + 1)
                .map(|p| p.0)
                .unwrap_or(total_ticks);
            let seg_events: Vec<ReplayEntry> = all_events
                .iter()
                .filter(|e| e.tick >= start_tick && (i + 1 == split_points.len() || e.tick < end_tick))
                .map(|e| ReplayEntry {
                    tick: e.tick - start_tick,
                    ev: e.ev.clone(),
                })
                .collect();
            segments.push(ReplaySegment {
                index: i,
                start_tick,
                duration_ticks: end_tick.saturating_sub(start_tick),
                scene_id,
                label,
                anchor,
                events: seg_events,
                annotations: Vec::new(),
                thumb_png: None,
            });
        }
        let bundle = Self {
            engine_version,
            total_ticks,
            segments,
            end_screenshot_png: None,
            state_json,
            note,
            tainted_by_debug,
            debug_mode,
        };
        bundle.validate()?;
        Ok(bundle)
    }
}

#[derive(Clone, Debug)]
pub struct ReplayRecorder {
    pub enabled: bool,
    pub segments: Vec<ReplaySegment>,
    pub seg_start_abs_tick: u64,
    pub cumulative_ticks: u64,
    pub total_events: usize,
    pub last_held_dir: i8,
    pub last_steer: Option<(i32, i32)>,
    /// True after scene change / battle end so the next eligible idle-overworld tick splits a segment.
    pub pending_split_hint: bool,
    /// True when any debug/cheat command mutated world state during this session (#199).
    pub tainted_by_debug: bool,
    /// True when `debug_mode` was enabled during this session (#235).
    pub debug_mode: bool,
    /// Stashed finished recording waiting for the host to archive to `RePal/replays/`.
    pub finished_archive: Option<ReplayBundle>,
}

impl Default for ReplayRecorder {
    fn default() -> Self {
        Self {
            enabled: true,
            segments: Vec::new(),
            seg_start_abs_tick: 0,
            cumulative_ticks: 0,
            total_events: 0,
            last_held_dir: -1,
            last_steer: None,
            pending_split_hint: false,
            tainted_by_debug: false,
            debug_mode: false,
            finished_archive: None,
        }
    }
}

impl ReplayRecorder {
    pub fn is_active(&self) -> bool {
        self.enabled && !self.segments.is_empty()
    }

    pub fn seg_rel_tick(&self, abs_tick: u64) -> u64 {
        abs_tick.saturating_sub(self.seg_start_abs_tick)
    }

    pub fn push(&mut self, abs_tick: u64, ev: ReplayEvent) {
        if !self.is_active() || self.total_events >= MAX_REPLAY_EVENTS {
            return;
        }
        let tick = self.seg_rel_tick(abs_tick);
        if let Some(seg) = self.segments.last_mut() {
            seg.events.push(ReplayEntry { tick, ev });
            self.total_events += 1;
        }
    }

    /// Record a purely cosmetic AI thought subtitle into the active segment's separate
    /// `annotations` track without counting toward `total_events` or setting `tainted_by_debug`.
    /// - Within [`AI_THOUGHT_COALESCE_TICKS`] (50 ticks / 1 s) of the last non-empty annotation,
    ///   replaces the last annotation's text in-place while preserving its original start `tick`.
    /// - Enforces a per-segment ring cap of [`MAX_AI_THOUGHTS_PER_SEGMENT`] (dropping oldest).
    pub fn push_annotation(&mut self, abs_tick: u64, text: String) -> AnnotationRecordResult {
        if !self.is_active() {
            return AnnotationRecordResult {
                recorded: false,
                coalesced: false,
            };
        }
        let tick = self.seg_rel_tick(abs_tick);
        let Some(seg) = self.segments.last_mut() else {
            return AnnotationRecordResult {
                recorded: false,
                coalesced: false,
            };
        };
        if !text.is_empty() {
            if let Some(last) = seg.annotations.last_mut() {
                if !last.text.is_empty()
                    && tick >= last.tick
                    && tick - last.tick < AI_THOUGHT_COALESCE_TICKS
                {
                    // Coalesce in-place while keeping original start tick for accurate SRT timing.
                    last.text = text;
                    return AnnotationRecordResult {
                        recorded: true,
                        coalesced: true,
                    };
                }
            }
        }
        if seg.annotations.len() >= MAX_AI_THOUGHTS_PER_SEGMENT {
            seg.annotations.remove(0);
        }
        seg.annotations.push(ReplayAnnotation { tick, text });
        AnnotationRecordResult {
            recorded: true,
            coalesced: false,
        }
    }

    pub fn record_tick_input(
        &mut self,
        abs_tick: u64,
        held_dir: i8,
        interact: bool,
        click: Option<(i32, i32)>,
        steer: Option<(i32, i32)>,
    ) {
        if !self.is_active() {
            return;
        }
        if held_dir != self.last_held_dir {
            self.last_held_dir = held_dir;
            self.push(abs_tick, ReplayEvent::HeldDir(held_dir));
        }
        if interact {
            self.push(abs_tick, ReplayEvent::Interact);
        }
        if let Some((x, y)) = click {
            self.push(abs_tick, ReplayEvent::Click(x, y));
        }
        if steer != self.last_steer {
            self.last_steer = steer;
            self.push(abs_tick, ReplayEvent::Steer(steer));
        }
    }

    /// True if the latest segment does not yet have a thumbnail attached.
    pub fn needs_latest_thumb(&self) -> bool {
        self.segments
            .last()
            .map(|s| s.thumb_png.is_none())
            .unwrap_or(false)
    }

    /// Attach a captured PNG thumbnail to the active (latest) segment.
    pub fn set_latest_thumb(&mut self, png: Vec<u8>) {
        if let Some(seg) = self.segments.last_mut() {
            if seg.thumb_png.is_none() {
                seg.thumb_png = Some(png);
            }
        }
    }

    /// Split into a new self-contained segment anchored at `save` (called only in idle overworld).
    pub fn split_segment(&mut self, abs_tick: u64, scene_id: u16, label: String, save: Value) {
        if !self.is_active() {
            return;
        }
        let dur = self.seg_rel_tick(abs_tick);
        if let Some(prev) = self.segments.last_mut() {
            prev.duration_ticks = dur;
        }
        self.cumulative_ticks += dur;
        self.seg_start_abs_tick = abs_tick;
        self.last_held_dir = -1;
        self.last_steer = None;
        self.pending_split_hint = false;
        let idx = self.segments.len();
        self.segments.push(ReplaySegment {
            index: idx,
            start_tick: self.cumulative_ticks,
            duration_ticks: 0,
            scene_id,
            label,
            anchor: ReplayAnchor::SaveJson(save),
            events: Vec::new(),
            annotations: Vec::new(),
            thumb_png: None,
        });
    }

    pub fn export_bundle(&self, abs_tick: u64) -> ReplayBundle {
        let mut segments = self.segments.clone();
        let last_dur = self.seg_rel_tick(abs_tick);
        if let Some(last) = segments.last_mut() {
            last.duration_ticks = last_dur;
        }
        let total_ticks = self.cumulative_ticks + last_dur;
        ReplayBundle {
            engine_version: ENGINE_VERSION.to_string(),
            total_ticks,
            segments,
            end_screenshot_png: None,
            state_json: None,
            note: None,
            tainted_by_debug: self.tainted_by_debug,
            debug_mode: self.debug_mode,
        }
    }

    pub fn export_json(&self, abs_tick: u64) -> Value {
        self.export_bundle(abs_tick).to_json()
    }

    /// Archive the previous session (if it has enough events) and start a fresh recording.
    pub fn reset_with_anchor(
        &mut self,
        abs_tick: u64,
        scene_id: u16,
        label: String,
        anchor: ReplayAnchor,
    ) {
        if self.enabled && !self.segments.is_empty() && self.total_events >= MIN_EVENTS_FOR_ARCHIVE {
            self.finished_archive = Some(self.export_bundle(abs_tick));
        }
        self.segments.clear();
        self.seg_start_abs_tick = abs_tick;
        self.cumulative_ticks = 0;
        self.total_events = 0;
        self.last_held_dir = -1;
        self.last_steer = None;
        self.pending_split_hint = false;
        self.tainted_by_debug = false;
        self.debug_mode = false;
        self.segments.push(ReplaySegment {
            index: 0,
            start_tick: 0,
            duration_ticks: 0,
            scene_id,
            label,
            anchor,
            events: Vec::new(),
            annotations: Vec::new(),
            thumb_png: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_last_segments_rebases_and_keeps_anchors() {
        let seg = |i: u64, start: u64, sc: u16| {
            serde_json::json!({
                "index": i, "start_tick": start, "duration_ticks": 100, "scene_id": sc, "label": format!("s{sc}"),
                "anchor": { "type": "save_json", "data": { "sceneId": sc } },
                "events": [ { "t": 5, "e": "held", "d": 1 } ],
            })
        };
        let doc = serde_json::json!({
            "kind": REPLAY_FORMAT_KIND, "engine_version": "test", "total_ticks": 420,
            "segments": [seg(0, 0, 1), seg(1, 100, 2), seg(2, 200, 3), seg(3, 300, 4)],
        });
        let mut b = ReplayBundle::from_json(&doc).expect("parse");
        b.keep_last_segments(2);
        assert_eq!(b.segments.len(), 2);
        assert_eq!(b.segments[0].scene_id, 3);
        assert_eq!((b.segments[0].index, b.segments[0].start_tick), (0, 0));
        assert_eq!((b.segments[1].index, b.segments[1].start_tick), (1, 100));
        assert_eq!(b.total_ticks, 220);
        assert!(b.validate().is_ok());
        let mut short = ReplayBundle::from_json(&doc).unwrap();
        short.keep_last_segments(10);
        assert_eq!(short.segments.len(), 4, "fewer segments than n: unchanged");
    }
}
