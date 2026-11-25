//! Canonical state vector for differential testing against the legacy JS runtime and replays.
//!
//! The JSON layout matches `extractCanonicalState()` in `tools/parity/canonical.mjs` (originally
//! `tools/verify_story_parity.mjs`) field for field, so the two runtimes can be diffed directly.

use super::world::EQUIP_SLOTS;
use super::Game;
use serde_json::{json, Map, Value};

impl Game {
    pub fn canonical_state(&self) -> Value {
        let w = &self.world;
        let label = |id| self.prog.label_name(id).to_string();
        let inventory: Vec<Value> = w.inventory.iter().filter(|(_, &c)| c > 0).map(|(&id, &c)| json!([id, c])).collect();

        let mut dirty_events = Vec::new();
        for ev in &w.events {
            let Some(base) = self.data.events.get(&ev.id) else { continue };
            let trig = label(ev.trigger.label);
            let auto = label(ev.auto.label);
            if ev.x != base.x
                || ev.y != base.y
                || ev.state != base.state
                || ev.direction != base.direction
                || trig != base.script_trigger
                || auto != base.script_auto
                || ev.vanish_time != base.vanish_time
            {
                dirty_events.push(json!({
                    "id": ev.id, "x": ev.x, "y": ev.y, "state": ev.state, "dir": ev.direction,
                    "trig": trig, "auto": auto, "vanish": ev.vanish_time,
                }));
            }
        }

        let mut dirty_scenes = Vec::new();
        for sc in w.scenes.values() {
            let Some(base) = self.data.scenes.get(&sc.id) else { continue };
            let enter = label(sc.enter.label);
            let tp = label(sc.teleport);
            if enter != base.script_enter || tp != base.script_teleport {
                dirty_scenes.push(json!({ "id": sc.id, "enter": enter, "tp": tp }));
            }
        }

        let roles: Vec<Value> = w
            .roles
            .iter()
            .map(|r| {
                let mut eq = Map::new();
                for (k, v) in EQUIP_SLOTS.iter().zip(r.equipment) {
                    eq.insert((*k).into(), json!(v));
                }
                let mut magics = r.magics.clone();
                magics.sort_unstable();
                json!({
                    "roleId": r.role_id, "level": r.level, "exp": r.exp, "hp": r.hp, "mp": r.mp,
                    "maxHp": r.max_hp, "maxMp": r.max_mp, "equipment": eq, "magics": magics,
                })
            })
            .collect();

        let mut out = json!({
            "sceneId": w.scene_id,
            "mapId": w.current_scene().map(|s| s.map_id).unwrap_or(0),
            "cash": w.cash,
            "party": w.party.iter().map(|m| json!({"roleId": m.role_id, "x": m.x, "y": m.y, "dir": m.dir})).collect::<Vec<_>>(),
            "inventory": inventory,
            "roles": roles,
            "dirtyEvents": dirty_events,
            "dirtyScenes": dirty_scenes,
        });
        if !w.magic_script_pos.is_empty() {
            let mut mpos: Vec<(u16, String, u32)> = w
                .magic_script_pos
                .iter()
                .filter(|(_, pos)| !pos.is_none())
                .map(|(&mid, &pos)| (mid, label(pos.label), pos.idx))
                .collect();
            mpos.sort_by_key(|(mid, _, _)| *mid);
            if !mpos.is_empty() {
                out["magicScriptPos"] = json!(mpos);
            }
        }
        if w.chase_alert_cycles > 0 {
            out["chaseAlertCycles"] = json!(w.chase_alert_cycles);
        }
        out
    }
}
