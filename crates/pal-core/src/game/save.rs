//! Save games and the render projection.
//!
//! Saves use the same JSON layout as the legacy `GameState.serializeSave()` (version 1) so
//! existing browser `localStorage` slots keep loading, plus core-only fields (`rng`, chase state)
//! that make a save a complete deterministic snapshot.

use super::world::{CounterKey, Event, PartyMember, Role, EQUIP_SLOTS, STAT_KEYS};
use super::Game;
use crate::script::{ScriptPos, NO_LABEL};
use crate::state::{EventState, GameState, PartyMember as RenderMember, RoleState, SceneState};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

fn i(v: &Value) -> i32 {
    v.as_i64().map(|x| x as i32).or_else(|| v.as_f64().map(|x| x as i32)).unwrap_or(0)
}

impl Game {
    fn label_str(&self, pos: ScriptPos) -> String {
        self.prog.label_name(pos.label).to_string()
    }

    /// Serialize the full world state (legacy save format v1 + core extensions).
    pub fn save_json(&self, note: &str, saved_at: &str) -> Value {
        let w = &self.world;
        let mut scene_diffs = Vec::new();
        for sc in w.scenes.values() {
            let Some(base) = self.data.scenes.get(&sc.id) else { continue };
            let enter = self.label_str(sc.enter);
            let tp = self.prog.label_name(sc.teleport).to_string();
            if sc.map_id != base.map_id || enter != base.script_enter || tp != base.script_teleport || sc.enter.idx != 0 {
                scene_diffs.push(json!([sc.id, sc.map_id, enter, tp, sc.enter.idx]));
            }
        }
        let mut event_diffs = Vec::new();
        for ev in &w.events {
            let Some(b) = self.data.events.get(&ev.id) else { continue };
            let trig = self.label_str(ev.trigger);
            let auto = self.label_str(ev.auto);
            if ev.x != b.x
                || ev.y != b.y
                || ev.layer != b.layer
                || ev.state != b.state
                || ev.direction != b.direction
                || ev.current_frame != b.current_frame
                || ev.sprite_id != b.sprite_id
                || ev.sprite_frames != b.sprite_frames
                || trig != b.script_trigger
                || auto != b.script_auto
                || ev.vanish_time != b.vanish_time
                || ev.trigger_mode != b.trigger_mode
                || ev.trigger.idx != 0
                || ev.auto.idx != 0
                || ev.spawn.is_some()
            {
                let mut obj = json!({
                    "id": ev.id, "x": ev.x, "y": ev.y, "ly": ev.layer, "st": ev.state, "d": ev.direction,
                    "cf": ev.current_frame, "sp": ev.sprite_id, "st_scr": trig, "sa_scr": auto,
                    "vt": ev.vanish_time, "tm": ev.trigger_mode, "to": ev.trigger.idx, "ao": ev.auto.idx,
                });
                if ev.sprite_frames != b.sprite_frames {
                    obj["sf"] = json!(ev.sprite_frames);
                }
                if let Some((sx, sy)) = ev.spawn {
                    obj["sx"] = json!(sx);
                    obj["sy"] = json!(sy);
                }
                event_diffs.push(obj);
            }
        }
        let roles: Vec<Value> = w.roles.iter().map(role_json).collect();
        let mut counters: Vec<(String, i32)> = w
            .script_counters
            .iter()
            .map(|(k, &v)| {
                let base = format!("{}:{}:{}", k.event_id, self.prog.label_name(k.pos.label), k.pos.idx);
                (if k.wait { format!("wait:{base}") } else { base }, v)
            })
            .collect();
        counters.sort();
        let mut magic_pos: Vec<(u16, String, u32)> = w
            .magic_script_pos
            .iter()
            .filter(|(_, pos)| !pos.is_none())
            .map(|(&mid, &pos)| (mid, self.prog.label_name(pos.label).to_string(), pos.idx))
            .collect();
        magic_pos.sort_by_key(|(mid, _, _)| *mid);
        let scene_name = w
            .current_scene()
            .and_then(|s| self.data.scenes.get(&s.id))
            .map(|s| s.name.clone())
            .unwrap_or_else(|| format!("场景 #{}", w.scene_id));
        json!({
            "version": 1,
            "core": 1,
            "savedAt": saved_at,
            "sceneName": scene_name,
            "note": note,
            "sceneId": w.scene_id,
            "musicTrack": w.music_track,
            "battleMusicTrack": w.battle_music_track,
            "battlefieldId": w.battlefield_id,
            "cash": w.cash,
            "partyLayer": w.party_layer,
            "party": w.party.iter().map(|m| json!({"roleId": m.role_id, "x": m.x, "y": m.y, "dir": m.dir, "frame": m.frame})).collect::<Vec<_>>(),
            "trail": w.trail.iter().map(|&(x, y, d)| json!([x, y, d])).collect::<Vec<_>>(),
            "roles": roles,
            "inventory": w.inventory.iter().map(|(&k, &v)| json!([k, v])).collect::<Vec<_>>(),
            "scriptCounters": counters.into_iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
            "objectScripts": w.object_scripts.iter().map(|(&k, &v)| json!([k, self.prog.label_name(v)])).collect::<Vec<_>>(),
            "magicScriptPos": magic_pos.into_iter().map(|(id, lbl, idx)| json!([id, lbl, idx])).collect::<Vec<_>>(),
            "gourdEssences": w.gourd_essences,
            "followerSprites": w.follower_sprites,
            "nightPalette": w.night_palette,
            "paletteId": w.palette_id,
            "screenWave": w.screen_wave,
            "chaseAlertRange": w.chase_alert_range,
            "chaseAlertCycles": w.chase_alert_cycles,
            "chaseDisabledTicks": w.chase_disabled_ticks,
            "rng": w.rng.state,
            "sceneDiffs": scene_diffs,
            "eventDiffs": event_diffs,
        })
    }

    /// Restore a save produced by [`Game::save_json`] or the legacy JS runtime.
    /// The host must then call [`Game::enter_scene`]`(scene, false)` (done here) and service
    /// the resulting `LoadScene` command.
    pub fn load_save_json(&mut self, data: &Value) -> Result<(), String> {
        self.load_save_json_inner(data, true)
    }

    /// Load a save JSON without resetting `self.recorder` or `self.tick_count` (used for
    /// pre-battle defeat retry and checkpoint jumps during replay playback).
    pub fn load_save_json_keeping_recorder(&mut self, data: &Value) -> Result<(), String> {
        self.load_save_json_inner(data, false)
    }

    fn load_save_json_inner(&mut self, data: &Value, reset_recorder: bool) -> Result<(), String> {
        let ver = data.get("version").and_then(Value::as_i64).unwrap_or(0);
        if ver != 1 && ver != 2 {
            return Err(format!("unsupported save version {ver}"));
        }
        let scene_id = data.get("sceneId").and_then(Value::as_u64).ok_or("missing sceneId")? as u16;
        if !self.data.scenes.is_empty() && !self.data.scenes.contains_key(&scene_id) {
            return Err(format!("unknown sceneId {scene_id}"));
        }
        let party = data.get("party").and_then(Value::as_array).ok_or("missing party")?;
        let roles = data.get("roles").and_then(Value::as_array).ok_or("missing roles")?;
        if party.is_empty() || party.len() > 6 {
            return Err("invalid party length".into());
        }
        // Build and validate into a temporary World before aborting/mutating self.world.
        let seed = data.get("rng").and_then(Value::as_u64).unwrap_or(self.world.rng.state as u64) as u32;
        let next_world = super::world::World::new_game(&self.data, &mut self.prog, seed);
        let role_count = next_world.roles.len();
        let parsed_party: Vec<PartyMember> = party
            .iter()
            .take(6)
            .filter_map(|m| {
                let raw_id = i(&m["roleId"]);
                if raw_id < 0 || (raw_id as usize) >= role_count {
                    return None;
                }
                Some(PartyMember {
                    role_id: raw_id as u16,
                    x: i(&m["x"]).clamp(0, 2048),
                    y: i(&m["y"]).clamp(0, 2048),
                    dir: (i(&m["dir"]) & 3) as u8,
                    frame: (i(&m["frame"]).clamp(0, 255)) as u8,
                })
            })
            .collect();
        if parsed_party.is_empty() {
            return Err("empty or out-of-range party".into());
        }
        let prev_lang = self.world.lang;
        self.abort_foreground();
        self.world = next_world;
        self.world.set_lang(prev_lang);
        let w = &mut self.world;
        w.scene_id = scene_id;
        w.music_track = data.get("musicTrack").map(i).unwrap_or(0);
        w.battle_music_track = data.get("battleMusicTrack").map(i).filter(|&v| v != 0).unwrap_or(4);
        w.battlefield_id = data.get("battlefieldId").map(i).unwrap_or(0);
        w.cash = data.get("cash").map(i).unwrap_or(0).max(0) as u32;
        w.party_layer = data.get("partyLayer").map(i).unwrap_or(0);
        w.night_palette = data.get("nightPalette").and_then(Value::as_bool).unwrap_or(false);
        w.palette_id = data.get("paletteId").map(i).unwrap_or(0);
        w.screen_wave = data.get("screenWave").map(i).unwrap_or(0);
        w.chase_alert_range = data.get("chaseAlertRange").map(i).filter(|&v| v != 0).unwrap_or(3);
        w.chase_alert_cycles = data.get("chaseAlertCycles").map(i).unwrap_or(0).max(0);
        w.chase_disabled_ticks = data.get("chaseDisabledTicks").map(i).unwrap_or(0).max(0);
        w.party = parsed_party;
        w.trail.clear();
        if let Some(arr) = data.get("trail").and_then(Value::as_array) {
            for item in arr.iter().take(24) {
                if let Some(t) = item.as_array() {
                    if t.len() >= 2 {
                        let tx = i(&t[0]);
                        let ty = i(&t[1]);
                        let td = (t.get(2).map(i).unwrap_or(0) & 3) as u8;
                        w.trail.push_back((tx, ty, td));
                    }
                }
            }
        }
        // Legacy JSON saves without `trail` (#249): seed trail from party positions in party order
        // so step 1 never teleports followers onto the leader.
        if w.trail.is_empty() && w.party.len() > 1 {
            for m in &w.party {
                w.trail.push_back((m.x, m.y, m.dir));
            }
        }
        let mut roles_need_base_sync = Vec::new();
        for (idx, r) in roles.iter().take(16).enumerate() {
            if let Some(dst) = w.roles.get_mut(idx) {
                let had_base_stats = apply_role_json(dst, r);
                dst.level = dst.level.clamp(0, 99);
                dst.max_hp = dst.max_hp.clamp(0, u16::MAX as i32);
                dst.max_mp = dst.max_mp.clamp(0, u16::MAX as i32);
                dst.hp = dst.hp.clamp(0, u16::MAX as i32);
                dst.mp = dst.mp.clamp(0, u16::MAX as i32);
                if !had_base_stats {
                    roles_need_base_sync.push(idx);
                }
            }
        }
        w.inventory = data
            .get("inventory")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .take(512)
                    .filter_map(|p| Some((i(p.get(0)?) as u16, i(p.get(1)?).clamp(0, u16::MAX as i32))))
                    .filter(|&(_, n)| n > 0)
                    .collect()
            })
            .unwrap_or_default();
        w.gourd_essences = data
            .get("gourdEssences")
            .and_then(Value::as_array)
            .map(|a| a.iter().take(256).map(|v| i(v) as u16).collect())
            .unwrap_or_default();
        w.follower_sprites = data
            .get("followerSprites")
            .and_then(Value::as_array)
            .map(|a| a.iter().take(16).map(|v| i(v) as u16).collect())
            .unwrap_or_default();

        let counters: Vec<(String, i32)> = data
            .get("scriptCounters")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .take(4096)
                    .filter_map(|p| Some((p.get(0)?.as_str()?.to_string(), i(p.get(1)?))))
                    .collect()
            })
            .unwrap_or_default();
        for (k, v) in counters {
            let (wait, rest) = match k.strip_prefix("wait:") {
                Some(r) => (true, r),
                None => (false, k.as_str()),
            };
            let mut parts = rest.splitn(3, ':');
            let (Some(ev), Some(lbl), Some(idx)) = (parts.next(), parts.next(), parts.next()) else { continue };
            let pos = ScriptPos::new(self.prog.intern(lbl), idx.parse().unwrap_or(0));
            self.world.script_counters.insert(CounterKey { wait, event_id: ev.parse().unwrap_or(0), pos }, v);
        }
        let objs: Vec<(i32, String)> = data
            .get("objectScripts")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .take(1024)
                    .filter_map(|p| Some((i(p.get(0)?), p.get(1)?.as_str().unwrap_or("").to_string())))
                    .collect()
            })
            .unwrap_or_default();
        for (k, l) in objs {
            let id = self.prog.intern(&l);
            self.world.object_scripts.insert(k, id);
        }
        let mpos: Vec<(u16, String, u32)> = data
            .get("magicScriptPos")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .take(512)
                    .filter_map(|p| {
                        let mid = i(p.get(0)?).max(0) as u16;
                        let lbl = p.get(1)?.as_str()?.to_string();
                        let idx = i(p.get(2)?).max(0) as u32;
                        Some((mid, lbl, idx))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (mid, lbl, idx) in mpos {
            let lid = self.prog.intern(&lbl);
            if lid != NO_LABEL {
                self.world.magic_script_pos.insert(mid, ScriptPos::new(lid, idx));
            }
        }
        for d in data
            .get("sceneDiffs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(512)
        {
            let Some(a) = d.as_array() else { continue };
            let sid = a.first().map(i).unwrap_or(0) as u16;
            let enter = a.get(2).and_then(Value::as_str).unwrap_or("").to_string();
            let tp = a.get(3).and_then(Value::as_str).unwrap_or("").to_string();
            let (el, tl) = (self.prog.intern(&enter), self.prog.intern(&tp));
            if let Some(sc) = self.world.scenes.get_mut(&sid) {
                sc.map_id = a.get(1).map(i).unwrap_or(0) as u16;
                sc.enter = ScriptPos::new(el, a.get(4).map(i).unwrap_or(0).max(0) as u32);
                sc.teleport = tl;
            }
        }
        for d in data
            .get("eventDiffs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(8192)
        {
            let id = d.get("id").map(i).unwrap_or(0) as u16;
            let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let (tl, al) = (self.prog.intern(&s("st_scr")), self.prog.intern(&s("sa_scr")));
            let Some(ev) = self.world.event_mut(id) else { continue };
            let n = |k: &str, dflt: i32| d.get(k).map(i).unwrap_or(dflt);
            ev.x = n("x", ev.x);
            ev.y = n("y", ev.y);
            ev.layer = n("ly", ev.layer);
            ev.state = n("st", ev.state);
            ev.direction = n("d", ev.direction);
            ev.current_frame = n("cf", ev.current_frame);
            ev.sprite_id = n("sp", ev.sprite_id as i32) as u16;
            if let Some(sf) = d.get("sf") {
                ev.sprite_frames = i(sf);
            }
            if let (Some(sx), Some(sy)) = (d.get("sx"), d.get("sy")) {
                ev.spawn = Some((i(sx), i(sy)));
            }
            ev.vanish_time = n("vt", ev.vanish_time);
            ev.trigger_mode = n("tm", ev.trigger_mode);
            ev.trigger = ScriptPos::new(tl, n("to", 0).max(0) as u32);
            ev.auto = ScriptPos::new(al, n("ao", 0).max(0) as u32);
        }
        for rid in roles_need_base_sync {
            self.sync_base_stats_from_effective(rid);
        }
        let prev_tick = self.tick_count;
        self.reset_runtime();
        if reset_recorder {
            self.debug_mode = false;
            self.god_mode = false;
            let label = self.scene_label(scene_id);
            self.recorder.reset_with_anchor(
                prev_tick,
                scene_id,
                label,
                super::replay_rec::ReplayAnchor::SaveJson(data.clone()),
            );
            self.recorder.seg_start_abs_tick = 0;
        } else {
            self.tick_count = prev_tick;
        }
        if self.world.music_track > 0 {
            let track = self.world.music_track;
            self.emit(super::HostCmd::PlayMusic { track, looped: true });
        }
        self.recompute_all_equip_effects();
        self.enter_scene(scene_id, false);
        // Loading a save is not a scene transition; clear `just_changed_scene` so the first
        // post-load foreground script finishes identically to the live session (#272).
        self.just_changed_scene = false;
        Ok(())
    }

    /// Load an original 1995 DOS `.rpg` save (`183,488` bytes).
    pub fn load_dos_rpg(&mut self, buf: &[u8]) -> Result<(), String> {
        let mut gs = GameState::new();
        gs.load_dos_rpg(buf).map_err(str::to_string)?;
        let prev_lang = self.world.lang;
        self.abort_foreground();
        self.debug_mode = false;
        self.god_mode = false;
        // Deterministic RNG seed derived from the .rpg bytes (FNV-1a 32-bit, non-zero) so loading
        // the same DOS .rpg twice behaves identically regardless of prior session state (#264).
        let mut h: u32 = 0x811c_9dc5;
        for &b in buf {
            h ^= b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
        let seed = if h == 0 { 1 } else { h };
        self.world = super::world::World::new_game(&self.data, &mut self.prog, seed);
        self.world.set_lang(prev_lang);
        let lbl = |prog: &mut crate::script::Program, addr: u16| -> ScriptPos {
            if addr == 0 {
                return ScriptPos::NONE;
            }
            let name = format!("s_{addr:04x}");
            prog.resolve_entry(&name).unwrap_or_else(|| ScriptPos::new(prog.intern(&name), 0))
        };
        let w = &mut self.world;
        w.scene_id = gs.scene_id;
        w.night_palette = gs.night_palette;
        w.music_track = gs.music_track as i32;
        w.battle_music_track = gs.battle_music_track as i32;
        w.battlefield_id = gs.battlefield_id as i32;
        w.screen_wave = gs.screen_wave as i32;
        w.cash = gs.cash;
        w.chase_alert_range = if gs.chase_range > 0 { gs.chase_range as i32 } else { 3 };
        if !gs.party.is_empty() {
            w.party = gs.party.iter().map(|p| PartyMember { role_id: p.role_id, x: p.x, y: p.y, dir: p.dir, frame: p.frame }).collect();
        }
        w.trail.clear();
        for &(tx, ty, td) in &gs.trail {
            w.trail.push_back((tx, ty, td));
        }
        for (dst, src) in w.roles.iter_mut().zip(gs.roles.iter()) {
            dst.avatar_rgm = src.avatar_rgm;
            dst.battle_sprite_f = src.battle_sprite_f;
            dst.walk_sprite_mgo = src.walk_sprite_mgo;
            dst.level = src.level as i32;
            dst.exp = src.exp as i32;
            dst.max_hp = src.max_hp as i32;
            dst.max_mp = src.max_mp as i32;
            dst.hp = src.hp as i32;
            dst.mp = src.mp as i32;
            dst.equipment = src.equipment;
            dst.stats = src.stats.map(|v| v as i32);
            dst.base_stats = dst.stats;
            dst.magics = src.magics.clone();
            dst.coop_magic = src.coop_magic;
            dst.base_coop_magic = src.coop_magic;
            dst.poisons = src.poisons.clone();
        }
        w.inventory = gs.inventory.iter().map(|(&k, &v)| (k, v as i32)).collect();
        // Restore modified enemy object turn scripts from DOS SAVEDGAME.rgObject[600] (#264).
        for (&obj_id, &addr) in &gs.object_scripts {
            if let Some(ed) = self.data.enemies.get(&obj_id) {
                let name = format!("s_{addr:04x}");
                if name != ed.script_pre_turn {
                    let lid = self.prog.intern(&name);
                    self.world.object_scripts.insert(obj_id as i32, lid);
                }
            }
        }
        for s in &gs.scenes {
            let enter = lbl(&mut self.prog, s.script_enter);
            let tp = lbl(&mut self.prog, s.script_teleport).label;
            if let Some(sc) = self.world.scenes.get_mut(&s.id) {
                sc.map_id = s.map_id;
                sc.enter = enter;
                sc.teleport = tp;
            }
        }
        for e in &gs.events {
            let trig = lbl(&mut self.prog, e.script_trigger);
            let auto = lbl(&mut self.prog, e.script_auto);
            if let Some(ev) = self.world.event_mut(e.id) {
                ev.vanish_time = e.vanish_time as i32;
                ev.x = e.x as i32;
                ev.y = e.y as i32;
                ev.layer = e.layer as i32;
                ev.trigger = trig;
                ev.auto = auto;
                ev.state = e.state as i32;
                ev.trigger_mode = e.trigger_mode as i32;
                ev.sprite_id = e.sprite_id;
                ev.sprite_frames = e.sprite_frames as i32;
                ev.direction = e.dir as i32;
                ev.current_frame = e.current_frame as i32;
            }
        }
        let prev_tick = self.tick_count;
        self.reset_runtime();
        // SDLPAL `PAL_InitGameData` + `PAL_UpdateEquipments`: DOS `.rpg` `PlayerRoles` stores base stats;
        // add equipped gear bonuses on top (#271).
        self.recompute_all_equip_effects();
        let sid = self.world.scene_id;
        let label = self.scene_label(sid);
        let snap = self.save_json(&label, "dos_rpg_anchor");
        self.recorder.reset_with_anchor(
            prev_tick,
            sid,
            label,
            super::replay_rec::ReplayAnchor::SaveJson(snap),
        );
        self.recorder.seg_start_abs_tick = 0;
        if self.world.music_track > 0 {
            let track = self.world.music_track;
            self.emit(super::HostCmd::PlayMusic { track, looped: true });
        }
        self.enter_scene(sid, false);
        self.just_changed_scene = false;
        Ok(())
    }

    /// Project the authoritative world into the render/UI-facing [`GameState`].
    pub fn project_render_state(&self, gs: &mut GameState) {
        let w = &self.world;
        let u16c = |v: i32| v.clamp(0, u16::MAX as i32) as u16;
        gs.scene_id = w.scene_id;
        gs.palette_id = u16c(w.palette_id);
        gs.night_palette = w.night_palette;
        gs.music_track = u16c(w.music_track);
        gs.battle_music_track = u16c(w.battle_music_track);
        gs.battlefield_id = u16c(w.battlefield_id);
        gs.screen_wave = u16c(w.screen_wave);
        gs.cash = w.cash;
        gs.party_layer = w.party_layer;
        gs.party = w.party.iter().map(|m| RenderMember { role_id: m.role_id, x: m.x, y: m.y, dir: m.dir, frame: m.frame }).collect();
        for (dst, r) in gs.roles.iter_mut().zip(w.roles.iter()) {
            *dst = RoleState {
                role_id: r.role_id,
                avatar_rgm: r.avatar_rgm,
                battle_sprite_f: r.battle_sprite_f,
                walk_sprite_mgo: r.walk_sprite_mgo,
                level: u16c(r.level),
                exp: r.exp.max(0) as u32,
                max_hp: u16c(r.max_hp),
                max_mp: u16c(r.max_mp),
                hp: u16c(r.hp),
                mp: u16c(r.mp),
                equipment: r.equipment,
                stats: r.stats.map(u16c),
                coop_magic: r.coop_magic,
                magics: r.magics.clone(),
                poisons: r.poisons.clone(),
                statuses: r.statuses.iter().map(|(&k, &v)| (k, u16c(v))).collect(),
            };
        }
        gs.inventory = w.inventory.iter().filter(|(_, &n)| n > 0).map(|(&k, &n)| (k, u16c(n))).collect();
        gs.scenes = w.scenes.values().map(|s| SceneState { id: s.id, map_id: s.map_id, script_enter: u16::from(!s.enter.is_none()), script_teleport: u16::from(s.teleport != NO_LABEL) }).collect();
        gs.events.resize_with(w.events.len(), EventState::default);
        for (dst, e) in gs.events.iter_mut().zip(w.events.iter()) {
            project_event(dst, e);
        }
        gs.gourd_essences = w.gourd_essences.clone();
        gs.follower_sprites = w.follower_sprites.clone();
        gs.follower_positions = w
            .follower_sprites
            .iter()
            .enumerate()
            .map(|(fi, _)| {
                let slot = w.party.len() + fi;
                let k = (slot * 2).saturating_sub(1);
                if let Some(&pos) = w.trail.get(k).or_else(|| w.trail.back()) {
                    pos
                } else {
                    let last = w.party.last().cloned().unwrap_or_default();
                    (last.x, last.y, last.dir)
                }
            })
            .collect();
        gs.ending_triggered = w.ending_triggered;
        gs.game_quit = w.game_quit;
    }
}

fn project_event(dst: &mut EventState, e: &Event) {
    let u16c = |v: i32| v.clamp(0, u16::MAX as i32) as u16;
    *dst = EventState {
        id: e.id,
        vanish_time: e.vanish_time.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        x: u16c(e.x),
        y: u16c(e.y),
        layer: e.layer as i16,
        script_trigger: u16::from(!e.trigger.is_none()),
        script_auto: u16::from(!e.auto.is_none()),
        state: e.state as i16,
        trigger_mode: u16c(e.trigger_mode),
        sprite_id: e.sprite_id,
        sprite_frames: u16c(e.sprite_frames),
        dir: u16c(e.direction),
        current_frame: u16c(e.current_frame),
    };
}

fn role_json(r: &Role) -> Value {
    let mut eq = Map::new();
    for (k, v) in EQUIP_SLOTS.iter().zip(r.equipment) {
        eq.insert((*k).into(), json!(v));
    }
    let mut st = Map::new();
    for (k, v) in STAT_KEYS.iter().zip(r.stats) {
        st.insert((*k).into(), json!(v));
    }
    let mut bst = Map::new();
    for (k, v) in STAT_KEYS.iter().zip(r.base_stats) {
        bst.insert((*k).into(), json!(v));
    }
    let statuses: Map<String, Value> = r.statuses.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    let effects: Vec<Value> = r.equip_effects.iter().map(|(&(s, a), &v)| json!([s, a, v])).collect();
    json!({
        "roleId": r.role_id, "name": r.name, "nameHant": r.name_hant,
        "avatarRgm": r.avatar_rgm, "battleSpriteF": r.battle_sprite_f, "walkSpriteMgo": r.walk_sprite_mgo,
        "level": r.level, "exp": r.exp, "maxHp": r.max_hp, "maxMp": r.max_mp, "hp": r.hp, "mp": r.mp,
        "equipment": eq, "stats": st, "baseStats": bst, "magics": r.magics,
        "coopMagic": r.coop_magic, "baseCoopMagic": r.base_coop_magic,
        "sfx": {"death": r.sfx.death, "attack": r.sfx.attack, "weapon": r.sfx.weapon, "critical": r.sfx.critical, "magic": r.sfx.magic},
        "statuses": statuses, "poisons": r.poisons, "equipEffectsCore": effects,
    })
}

fn apply_role_json(dst: &mut Role, r: &Value) -> bool {
    let n = |k: &str, d: i32| r.get(k).map(i).unwrap_or(d);
    dst.avatar_rgm = n("avatarRgm", dst.avatar_rgm as i32) as u16;
    dst.battle_sprite_f = n("battleSpriteF", dst.battle_sprite_f as i32) as u16;
    dst.walk_sprite_mgo = n("walkSpriteMgo", dst.walk_sprite_mgo as i32) as u16;
    dst.level = n("level", dst.level);
    dst.exp = n("exp", 0);
    dst.max_hp = n("maxHp", dst.max_hp);
    dst.max_mp = n("maxMp", dst.max_mp);
    dst.hp = n("hp", dst.hp);
    dst.mp = n("mp", dst.mp);
    dst.coop_magic = n("coopMagic", dst.coop_magic as i32) as u16;
    dst.base_coop_magic = n("baseCoopMagic", dst.coop_magic as i32) as u16;
    if let Some(eq) = r.get("equipment") {
        for (slot, k) in EQUIP_SLOTS.iter().enumerate() {
            dst.equipment[slot] = eq.get(*k).map(i).unwrap_or(0) as u16;
        }
    }
    if let Some(st) = r.get("stats") {
        for (slot, k) in STAT_KEYS.iter().enumerate() {
            dst.stats[slot] = st.get(*k).map(i).unwrap_or(dst.stats[slot]);
        }
    }
    let has_base_stats = if let Some(bst) = r.get("baseStats") {
        for (slot, k) in STAT_KEYS.iter().enumerate() {
            dst.base_stats[slot] = bst.get(*k).map(i).unwrap_or(dst.stats[slot]);
        }
        true
    } else {
        dst.base_stats = dst.stats;
        false
    };
    if let Some(m) = r.get("magics").and_then(Value::as_array) {
        dst.magics = m.iter().take(64).map(|v| i(v) as u16).collect();
    }
    if let Some(p) = r.get("poisons").and_then(Value::as_array) {
        dst.poisons = p.iter().take(32).map(|v| i(v) as u16).collect();
    }
    if let Some(s) = r.get("statuses").and_then(Value::as_object) {
        dst.statuses = s.iter().take(32).filter_map(|(k, v)| Some((k.parse().ok()?, i(v)))).collect::<BTreeMap<_, _>>();
    }
    if let Some(e) = r.get("equipEffectsCore").and_then(Value::as_array) {
        dst.equip_effects = e.iter().take(64).filter_map(|t| Some(((i(t.get(0)?), i(t.get(1)?)), i(t.get(2)?)))).collect();
    }
    has_base_stats
}
