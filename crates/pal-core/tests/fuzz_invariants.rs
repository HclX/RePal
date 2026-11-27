//! Layer 2 + Layer 4 (Part 1) Automated Testing (#104, #272, #273):
//! - Randomized + coverage-guided deterministic fuzzing across New Game, all 51 classic
//!   DOS `.rpg` saves, and in-memory frontier snapshots discovering new script blocks (C9).
//! - Richer game-state invariants with explicit, auditable load-time dirty-save tracking (A3).
//! - Wider battle commands (`Attack`, `Magic`, `Item`, `ItemTarget`, `Coop`, `Defend`, `Flee`,
//!   `Auto`, `battle_cancel_auto`, B5) and field/menu actions (`use_item`, `equip_from_menu`,
//!   `cast_field_magic`, `buy_item`, `sell_item`, `split_segment_on_manual_save`, B6).
//! - Save-anchor continuation equivalence oracle across 51 saves × 3 seeds (A1 / #272).
//! - Same-seed determinism twin oracle (A4).
//! - Live failure capture, delta-debugging minimization, and Layer 1 replay export.
//!
//! Environment overrides for extended / nightly runs:
//! - `PAL_FUZZ_TICKS`: ticks per save (default: `180` for fast PR; set `3000+` for nightly)
//! - `PAL_FUZZ_SEEDS`: seeds per save (default: `1` for fast PR; set `4` for nightly)
//! - `PAL_CONT_WARMUP` / `PAL_CONT_FOLLOW`: warmup/continuation ticks for A1 (default `200`/`250`)

mod common;

use common::{assets_dir, NativeHost};
use pal_core::game::replay_rec::ReplayAnchor;
use pal_core::game::{BattleCmd, Game, ScriptCoverage, TickInput, Wait};
use pal_core::isometric::world_to_half_tile;
use pal_core::rng::GameRng;
use pal_core::script::{opcode_name, Program};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::rc::Rc;

/// Independent deterministic PRNG for fuzz inputs (never touches `World::rng`).
#[derive(Clone)]
struct FuzzRng(u64);

impl FuzzRng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }
}

#[derive(Clone)]
enum FuzzAnchor {
    NewGame(u32),
    OrigSave { file: String, bytes: Vec<u8>, rng_override: Option<u32> },
    SaveSnapshot { label: String, data: serde_json::Value },
}

impl FuzzAnchor {
    fn to_json(&self) -> serde_json::Value {
        match self {
            FuzzAnchor::NewGame(seed) => serde_json::json!({ "type": "new_game", "seed": seed }),
            FuzzAnchor::OrigSave { file, .. } => serde_json::json!({ "type": "orig_save", "file": file }),
            FuzzAnchor::SaveSnapshot { data, .. } => serde_json::json!({ "type": "save_json", "data": data }),
        }
    }

    fn apply(&self, host: &mut NativeHost) -> Result<(), String> {
        host.returned_to_title = false;
        host.game_over = false;
        host.battle_chooser = None;
        host.game.coverage = Some(ScriptCoverage::default());
        match self {
            FuzzAnchor::NewGame(seed) => {
                host.game.start_new_game(*seed);
                host.pump();
                Ok(())
            }
            FuzzAnchor::OrigSave { file, bytes, rng_override } => {
                host.game
                    .load_dos_rpg(bytes)
                    .map_err(|e| format!("load {file}: {e}"))?;
                if let Some(seed) = rng_override {
                    host.game.world.rng = GameRng::new((*seed).max(1));
                }
                host.pump();
                Ok(())
            }
            FuzzAnchor::SaveSnapshot { label, data } => {
                host.game
                    .load_save_json(data)
                    .map_err(|e| format!("load snapshot {label}: {e}"))?;
                host.pump();
                Ok(())
            }
        }
    }
}

/// Auditable load-time dirty-save baseline for community DOS `.rpg` files (#273 A3).
/// Records any pre-existing violations present at load time so game logic can never make them
/// worse or introduce new violations, while printing exact exemption counts for auditability.
#[derive(Clone, Debug, Default)]
struct InitialDirtyBaseline {
    /// Per-role initial `hp - max_hp` excess (0 when clean).
    hp_excess: Vec<i32>,
    /// Per-role initial `mp - max_mp` excess (0 when clean).
    mp_excess: Vec<i32>,
    /// Inventory items whose initial qty exceeded 99 (item_id -> initial qty).
    inv_over_99: HashMap<u16, i32>,
    /// Equipment `(role, slot) -> item_id` already invalid/mis-slotted in the raw DOS save.
    bad_equip_slots: HashMap<(usize, usize), u16>,
    /// Count of distinct load-time exemptions observed for reporting.
    exemption_count: usize,
    hp_ex_count: usize,
    mp_ex_count: usize,
    inv_ex_count: usize,
    equip_ex_count: usize,
}

impl InitialDirtyBaseline {
    fn capture(g: &Game) -> Self {
        let w = &g.world;
        let mut b = Self::default();
        for r in &w.roles {
            let hx = (r.hp - r.max_hp).max(0);
            let mx = (r.mp - r.max_mp).max(0);
            if hx > 0 {
                b.exemption_count += 1;
                b.hp_ex_count += 1;
            }
            if mx > 0 {
                b.exemption_count += 1;
                b.mp_ex_count += 1;
            }
            b.hp_excess.push(hx);
            b.mp_excess.push(mx);
        }
        for (&id, &qty) in &w.inventory {
            if qty > 99 {
                b.inv_over_99.insert(id, qty);
                b.exemption_count += 1;
                b.inv_ex_count += 1;
            }
        }
        for (ri, r) in w.roles.iter().enumerate() {
            for (slot, &item_id) in r.equipment.iter().enumerate() {
                if item_id == 0 {
                    continue;
                }
                let valid = g
                    .data
                    .items
                    .get(&item_id)
                    .is_some_and(|it| it.equipable && g.item_slot(item_id) == Some(slot));
                if !valid {
                    b.bad_equip_slots.insert((ri, slot), item_id);
                    b.exemption_count += 1;
                    b.equip_ex_count += 1;
                }
            }
        }
        b
    }
}

fn check_invariants(
    host: &NativeHost,
    baseline: &InitialDirtyBaseline,
    label: &str,
) -> Result<(), String> {
    let g = &host.game;
    let w = &g.world;

    // 1. Valid scene ID & event IDs belong to current scene's valid range
    if !g.data.scenes.contains_key(&w.scene_id) {
        return Err(format!("{label}: invalid scene_id {}", w.scene_id));
    }
    let scene_ev_ids = w.current_event_ids();
    if let Some(sc) = w.scenes.get(&w.scene_id) {
        let lo = sc.event_start;
        let hi = w
            .scenes
            .get(&(w.scene_id + 1))
            .map(|ns| ns.event_start)
            .unwrap_or(w.events.len() as u16 + 1);
        for &eid in &scene_ev_ids {
            if eid == 0 || eid < lo || eid >= hi || w.event(eid).is_none() {
                return Err(format!(
                    "{label}: scene {} returned out-of-range event id {eid} (expected {lo}..{hi})",
                    w.scene_id
                ));
            }
        }
    }

    // 2. Party non-empty, unique valid role IDs, coordinates in valid 2048x2048 world bounds
    if w.party.is_empty() || w.party.len() > 6 {
        return Err(format!("{label}: invalid party len {}", w.party.len()));
    }
    let mut seen_roles = HashSet::new();
    for (i, m) in w.party.iter().enumerate() {
        if (m.role_id as usize) >= w.roles.len() {
            return Err(format!("{label}: party[{i}] invalid role_id {}", m.role_id));
        }
        // Original data stages one scripted battle with a duplicated role: `s_522b` (scene 106,
        // 阿奴 & 盖罗娇 vs 石长老) runs `party_set_roles [5, 6, 5]` and restores `[1]` right after
        // the fight; SDLPAL copies the operands verbatim too (#23). Duplicates are therefore legal
        // only while a battle is running; one that survives into exploration is still a bug.
        if !seen_roles.insert(m.role_id) && g.battle.is_none() {
            return Err(format!("{label}: duplicate role_id {} in party outside battle", m.role_id));
        }
        if !(0..=2048).contains(&m.x) || !(0..=2048).contains(&m.y) {
            return Err(format!(
                "{label}: party[{i}] out of bounds ({}, {}) in scene {}",
                m.x, m.y, w.scene_id
            ));
        }
    }

    // 3. Role stats within legal bounds + post-#271 base/equip consistency + hp<=max_hp / mp<=max_mp
    for (i, r) in w.roles.iter().enumerate() {
        if !(0..=99_999).contains(&r.hp) {
            return Err(format!("{label}: role[{i}] hp={} out of bounds", r.hp));
        }
        if !(0..=99_999).contains(&r.mp) {
            return Err(format!("{label}: role[{i}] mp={} out of bounds", r.mp));
        }
        let allowed_hp_excess = baseline.hp_excess.get(i).copied().unwrap_or(0);
        if r.hp > r.max_hp + allowed_hp_excess {
            return Err(format!(
                "{label}: role[{i}] hp={} exceeds max_hp={} (+{allowed_hp_excess} baseline)",
                r.hp, r.max_hp
            ));
        }
        let allowed_mp_excess = baseline.mp_excess.get(i).copied().unwrap_or(0);
        if r.mp > r.max_mp + allowed_mp_excess {
            return Err(format!(
                "{label}: role[{i}] mp={} exceeds max_mp={} (+{allowed_mp_excess} baseline)",
                r.mp, r.max_mp
            ));
        }
        if !(1..=999).contains(&r.level) {
            return Err(format!("{label}: role[{i}] level={} out of bounds", r.level));
        }
        if r.exp < 0 {
            return Err(format!("{label}: role[{i}] negative exp={}", r.exp));
        }
        // Post-#271 / #253 invariant: effective stats must ALWAYS equal clamp(base + equip_bonus).
        for k in 0..5 {
            let expected = (r.base_stats[k] + r.equip_stat_bonuses[k]).clamp(1, u16::MAX as i32);
            if r.stats[k] != expected {
                return Err(format!(
                    "{label}: role[{i}] stat[{k}]={} != clamp(base {} + equip {}) = {expected}",
                    r.stats[k], r.base_stats[k], r.equip_stat_bonuses[k]
                ));
            }
        }
        // Every non-zero equipped item must exist, be equipable, and sit in its canonical slot.
        for (slot, &item_id) in r.equipment.iter().enumerate() {
            if item_id == 0 {
                continue;
            }
            if baseline.bad_equip_slots.get(&(i, slot)) == Some(&item_id) {
                continue;
            }
            let Some(it) = g.data.items.get(&item_id) else {
                return Err(format!(
                    "{label}: role[{i}] slot {slot} has unknown equipment item #{item_id}"
                ));
            };
            if !it.equipable {
                return Err(format!(
                    "{label}: role[{i}] slot {slot} has non-equipable item #{item_id}"
                ));
            }
            if g.item_slot(item_id) != Some(slot) {
                return Err(format!(
                    "{label}: role[{i}] slot {slot} holds item #{item_id} belonging to slot {:?}",
                    g.item_slot(item_id)
                ));
            }
        }
    }

    // 4. Inventory: known item IDs, 1 <= qty <= 99 (unless pre-existing baseline)
    for (&id, &qty) in &w.inventory {
        if qty <= 0 {
            return Err(format!("{label}: inventory item {id} has non-positive qty={qty}"));
        }
        let cap = baseline.inv_over_99.get(&id).copied().unwrap_or(99).max(99);
        if qty > cap {
            return Err(format!("{label}: inventory item {id} qty={qty} exceeds cap {cap}"));
        }
        if !g.data.items.contains_key(&id) {
            return Err(format!("{label}: inventory contains unknown item id {id}"));
        }
    }

    // 5. Battle invariants
    if let Some(b) = g.battle.as_ref() {
        for (ei, e) in b.enemies.iter().enumerate() {
            if e.hp < 0 {
                return Err(format!("{label}: battle enemy[{ei}] negative hp={}", e.hp));
            }
        }
        if !b.enemies.is_empty() && b.current_target_enemy >= b.enemies.len() {
            return Err(format!(
                "{label}: current_target_enemy={} >= len={}",
                b.current_target_enemy,
                b.enemies.len()
            ));
        }
    }
    Ok(())
}

fn check_save_roundtrip_idempotent(host: &mut NativeHost, label: &str) -> Result<(), String> {
    if host.game.is_busy() || host.game.battle.is_some() || host.game.is_loading_scene() {
        return Ok(());
    }
    let snap1 = host.game.save_json("fuzz", "2026-11-20T00:00:00Z");
    let canon1 = host.game.canonical_state();
    host.game
        .load_save_json(&snap1)
        .map_err(|e| format!("{label}: save round-trip load failed: {e}"))?;
    host.pump();
    let canon2 = host.game.canonical_state();
    if canon1 != canon2 {
        let mut diffs = Vec::new();
        if let (Some(o1), Some(o2)) = (canon1.as_object(), canon2.as_object()) {
            for (k, v1) in o1 {
                if o2.get(k) != Some(v1) {
                    diffs.push(format!("{k}: before={v1} after={:?}", o2.get(k)));
                }
            }
            for k in o2.keys() {
                if !o1.contains_key(k) {
                    diffs.push(format!("{k}: added={}", o2[k]));
                }
            }
        }
        return Err(format!(
            "{label}: save_json round-trip altered canonical_state ({})",
            diffs.join("; ")
        ));
    }
    Ok(())
}

/// B5: Rich deterministic battle command picker covering Attack, Magic (enemy & ally),
/// Item, ItemTarget, Coop, Defend, Flee, and Auto with randomized valid targets.
fn pick_fuzz_battle_cmd(g: &Game, hero_idx: usize, rng: &mut FuzzRng) -> BattleCmd {
    let Some(b) = g.battle.as_ref() else {
        return BattleCmd::Auto;
    };
    let alive_enemies: Vec<usize> = b
        .enemies
        .iter()
        .enumerate()
        .filter(|(_, e)| e.hp > 0)
        .map(|(i, _)| i)
        .collect();
    let target_enemy = if alive_enemies.is_empty() {
        0
    } else {
        alive_enemies[rng.range(alive_enemies.len() as u64) as usize]
    };
    let alive_heroes: Vec<usize> = b
        .heroes
        .iter()
        .enumerate()
        .filter(|(_, h)| g.world.roles.get(h.role).is_some_and(|r| r.hp > 0))
        .map(|(i, _)| i)
        .collect();
    let role_id = b.heroes.get(hero_idx).map(|h| h.role).unwrap_or(0);
    let role = g.world.roles.get(role_id);

    match rng.range(10) {
        0..=2 => BattleCmd::Attack { target: target_enemy },
        3..=4 => {
            // Pick a known, affordable magic spell
            if let Some(r) = role {
                let castable: Vec<u16> = r
                    .magics
                    .iter()
                    .copied()
                    .filter(|mid| {
                        g.data
                            .magics
                            .get(mid)
                            .is_some_and(|m| m.cost_mp <= r.mp)
                    })
                    .collect();
                if !castable.is_empty() {
                    let mid = castable[rng.range(castable.len() as u64) as usize];
                    let is_enemy = g.data.magics.get(&mid).map(|m| m.target_enemy).unwrap_or(true);
                    let target = if is_enemy {
                        target_enemy
                    } else if !alive_heroes.is_empty() {
                        alive_heroes[rng.range(alive_heroes.len() as u64) as usize]
                    } else {
                        0
                    };
                    return BattleCmd::Magic { magic: mid, target };
                }
            }
            BattleCmd::Attack { target: target_enemy }
        }
        5 => {
            // Pick a usable/throwable inventory item
            let mut usable: Vec<u16> = g
                .world
                .inventory
                .iter()
                .filter(|(&id, &qty)| {
                    qty > 0
                        && g.data
                            .items
                            .get(&id)
                            .is_some_and(|it| it.usable || it.throwable)
                })
                .map(|(&id, _)| id)
                .collect();
            usable.sort();
            if !usable.is_empty() {
                let item = usable[rng.range(usable.len() as u64) as usize];
                let throwable = g.data.items.get(&item).is_some_and(|it| it.throwable);
                if rng.range(2) == 0 {
                    let target = if throwable {
                        target_enemy
                    } else if !alive_heroes.is_empty() {
                        alive_heroes[rng.range(alive_heroes.len() as u64) as usize]
                    } else {
                        0
                    };
                    return BattleCmd::ItemTarget { item, target };
                }
                return BattleCmd::Item { item };
            }
            BattleCmd::Attack { target: target_enemy }
        }
        6 if alive_heroes.len() >= 2 => BattleCmd::Coop { target: target_enemy },
        7 => BattleCmd::Defend,
        8 => BattleCmd::Flee,
        _ => BattleCmd::Auto,
    }
}

/// B6: Periodically exercise real field/menu core APIs (`use_item`, `equip_from_menu`,
/// `cast_field_magic`, `buy_item`, `sell_item`, `split_segment_on_manual_save`) while idle.
fn exercise_field_menu_action(host: &mut NativeHost, rng: &mut FuzzRng) {
    if host.game.is_busy() || host.game.battle.is_some() || host.game.is_loading_scene() {
        return;
    }
    match rng.range(6) {
        0 => {
            // equip_from_menu on random equipable inventory item
            let mut eqs: Vec<u16> = host
                .game
                .world
                .inventory
                .iter()
                .filter(|(&id, &q)| {
                    q > 0 && host.game.data.items.get(&id).is_some_and(|it| it.equipable)
                })
                .map(|(&id, _)| id)
                .collect();
            eqs.sort();
            if !eqs.is_empty() {
                let item = eqs[rng.range(eqs.len() as u64) as usize];
                let party = host.game.world.party_role_ids();
                if !party.is_empty() {
                    let role = party[rng.range(party.len() as u64) as usize];
                    let _ = host.game.equip_from_menu(role, item);
                    host.pump();
                }
            }
        }
        1 => {
            // use_item on random usable inventory item
            let mut items: Vec<u16> = host
                .game
                .world
                .inventory
                .iter()
                .filter(|(&id, &q)| {
                    q > 0 && host.game.data.items.get(&id).is_some_and(|it| it.usable)
                })
                .map(|(&id, _)| id)
                .collect();
            items.sort();
            if !items.is_empty() {
                let item = items[rng.range(items.len() as u64) as usize];
                let party = host.game.world.party_role_ids();
                if !party.is_empty() {
                    let role = party[rng.range(party.len() as u64) as usize];
                    let _ = host.game.use_item(item, Some(role));
                    host.pump();
                }
            }
        }
        2 => {
            // cast_field_magic on random usable-outside-battle spell
            let party = host.game.world.party_role_ids();
            if !party.is_empty() {
                let caster = party[rng.range(party.len() as u64) as usize];
                let field_mags: Vec<u16> = host
                    .game
                    .world
                    .roles
                    .get(caster)
                    .map(|r| {
                        r.magics
                            .iter()
                            .copied()
                            .filter(|mid| {
                                host.game
                                    .data
                                    .magics
                                    .get(mid)
                                    .is_some_and(|m| m.usable_outside_battle && m.cost_mp <= r.mp)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if !field_mags.is_empty() {
                    let mid = field_mags[rng.range(field_mags.len() as u64) as usize];
                    let target = party[rng.range(party.len() as u64) as usize];
                    let _ = host.game.cast_field_magic(caster, mid, Some(target));
                    host.pump();
                }
            }
        }
        3 => {
            // buy_item (uses canonical pricing #268)
            if host.game.world.cash >= 100 {
                let mut buyable: Vec<u16> = host
                    .game
                    .data
                    .items
                    .iter()
                    .filter(|(_, it)| it.price > 0 && (it.price as u32) <= host.game.world.cash)
                    .map(|(&id, _)| id)
                    .collect();
                buyable.sort();
                if !buyable.is_empty() {
                    let id = buyable[rng.range(buyable.len() as u64) as usize];
                    let _ = host.game.buy_item(id, 0);
                    host.pump();
                }
            }
        }
        4 => {
            // sell_item (validates sellable flag & canonical half-price #268)
            let mut sellable: Vec<u16> = host
                .game
                .world
                .inventory
                .iter()
                .filter(|(&id, &q)| {
                    q > 0 && host.game.data.items.get(&id).is_some_and(|it| it.sellable && it.price > 0)
                })
                .map(|(&id, _)| id)
                .collect();
            sellable.sort();
            if !sellable.is_empty() {
                let id = sellable[rng.range(sellable.len() as u64) as usize];
                let _ = host.game.sell_item(id, 0);
                host.pump();
            }
        }
        _ => {
            // Trigger deferred manual-save segment split (#272)
            host.game.split_segment_on_manual_save();
        }
    }
}

/// Delta-debug / binary-search a failing input log down to the shortest reproducing subsequence.
fn minimize_failing_log<F>(log: &[(u64, TickInput)], mut reproduces: F) -> Vec<(u64, TickInput)>
where
    F: FnMut(&[(u64, TickInput)]) -> bool,
{
    if log.is_empty() || !reproduces(log) {
        return log.to_vec();
    }
    let mut lo = 1usize;
    let mut hi = log.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if reproduces(&log[..mid]) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    let mut cur = log[..lo].to_vec();
    let mut chunk = (cur.len() / 2).max(1);
    while chunk >= 1 && cur.len() > 1 {
        let mut i = 0usize;
        let mut shrunk = false;
        while i < cur.len() && cur.len() > 1 {
            let end = (i + chunk).min(cur.len());
            let mut cand = Vec::with_capacity(cur.len() - (end - i));
            cand.extend_from_slice(&cur[..i]);
            cand.extend_from_slice(&cur[end..]);
            if !cand.is_empty() && reproduces(&cand) {
                cur = cand;
                shrunk = true;
            } else {
                i += chunk;
            }
        }
        if !shrunk {
            if chunk == 1 {
                break;
            }
            chunk /= 2;
        }
    }
    cur
}

fn encode_input_entry(t: u64, inp: &TickInput) -> serde_json::Value {
    let mut flags = 0u32;
    let (mut cx, mut cy) = (0i32, 0i32);
    let (mut sx, mut sy) = (0i32, 0i32);
    if inp.interact {
        flags |= 1;
    }
    if let Some((x, y)) = inp.click {
        flags |= 2;
        cx = x;
        cy = y;
    }
    if let Some((x, y)) = inp.steer {
        flags |= 4;
        sx = x;
        sy = y;
    }
    serde_json::json!({ "t": t, "i": [inp.held_dir, flags, cx, cy, sx, sy] })
}

fn export_failure_replay(
    name: &str,
    reason: &str,
    anchor: serde_json::Value,
    log: &[(u64, TickInput)],
) -> std::path::PathBuf {
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tmp");
    let _ = std::fs::create_dir_all(&out_dir);
    let safe = name.replace(['/', '\\', '#', ':', ' '], "_");
    let path = out_dir.join(format!("fuzz_failure_{safe}.json"));
    let entries: Vec<serde_json::Value> = log
        .iter()
        .map(|(t, inp)| encode_input_entry(*t, inp))
        .collect();
    let doc = serde_json::json!({
        "issue": "fuzz",
        "title": format!("Auto-minimized fuzz failure ({name}): {reason}"),
        "replay": {
            "kind": "PalRemake_CoreReplay_v1",
            "seed": 1,
            "anchor": anchor,
            "log": entries,
        },
        "settle_after": false,
        "expect": []
    });
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap_or_default());
    eprintln!(
        "[fuzz_invariants] Exported reproducible replay ({} steps) to {}",
        log.len(),
        path.display()
    );
    path
}

/// Replay a candidate `(tick, TickInput)` sequence from `anchor` and return `true` if it fails.
fn replays_with_failure(anchor: &FuzzAnchor, log: &[(u64, TickInput)], label: &str) -> bool {
    let res = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
        let mut h = NativeHost::new();
        anchor.apply(&mut h)?;
        let baseline = InitialDirtyBaseline::capture(&h.game);
        for &(target_t, inp) in log {
            while h.game.tick_count < target_t {
                h.tick(TickInput::IDLE);
            }
            h.tick(inp);
            check_invariants(&h, &baseline, label)?;
        }
        check_save_roundtrip_idempotent(&mut h, label)?;
        Ok(())
    }));
    match res {
        Ok(Ok(())) => false,
        Ok(Err(_)) | Err(_) => true,
    }
}

struct FrontierSnapshot {
    label: String,
    data: serde_json::Value,
    seed: u64,
}

#[derive(Default)]
struct ExemptionStats {
    total: usize,
    hp: usize,
    mp: usize,
    inv: usize,
    equip: usize,
}

impl ExemptionStats {
    fn add(&mut self, b: &InitialDirtyBaseline) {
        self.total += b.exemption_count;
        self.hp += b.hp_ex_count;
        self.mp += b.mp_ex_count;
        self.inv += b.inv_ex_count;
        self.equip += b.equip_ex_count;
    }
}

#[allow(clippy::too_many_arguments)]
fn fuzz_run(
    host: &mut NativeHost,
    anchor: &FuzzAnchor,
    seed: u64,
    ticks: u32,
    label: &str,
    global_ops: &mut HashMap<u16, u64>,
    global_blocks: &mut HashSet<String>,
    frontier: &mut Vec<FrontierSnapshot>,
    frontier_cap: usize,
    exemptions: &mut ExemptionStats,
    enable_field_actions: bool,
) {
    let baseline = InitialDirtyBaseline::capture(&host.game);
    exemptions.add(&baseline);

    let rng = Rc::new(RefCell::new(FuzzRng::new(seed)));
    let battle_rng = rng.clone();
    host.battle_chooser = Some(Box::new(move |g, hero_idx| {
        pick_fuzz_battle_cmd(g, hero_idx, &mut battle_rng.borrow_mut())
    }));

    let mut cur_dir = rng.borrow_mut().range(4) as u8;
    let mut dir_hold = 0u32;
    let mut busy_stall_ticks = 0u32;
    let mut battle_ticks = 0u32;
    let mut last_sig: (u16, i32, i32, u64) = (0, 0, 0, 0);
    let mut prev_cov_ops = 0u64;
    let mut recorded_log: Vec<(u64, TickInput)> = Vec::with_capacity(ticks as usize);
    let mut failure: Option<String> = None;
    let mut captured_frontier = false;
    // Track last walkable explore position so walking directly into a blocked map tile fails (A3).
    let mut prev_walkable: Option<(u16, i32, i32)> = None;

    for t in 0..ticks {
        if host.returned_to_title || host.game_over {
            break;
        }
        host.choice_answer = rng.borrow_mut().range(2) == 0;

        // Occasional cancel-auto during live battle (B5)
        if host.game.battle.is_some() && rng.borrow_mut().range(40) == 0 {
            let _ = host.game.battle_cancel_auto();
        }

        // Occasional real field/menu action (B6)
        if enable_field_actions && t > 0 && t % 45 == 0 {
            exercise_field_menu_action(host, &mut rng.borrow_mut());
        }

        if dir_hold == 0 {
            cur_dir = rng.borrow_mut().range(4) as u8;
            dir_hold = 8 + rng.borrow_mut().range(24) as u32;
        } else {
            dir_hold -= 1;
        }

        // C9 Coverage-guided event clicking: 70% of clicks target active events whose
        // trigger/auto block is not yet in `global_blocks`.
        let click = if !host.game.is_busy() && rng.borrow_mut().range(18) == 0 {
            let ids = host.game.world.current_event_ids();
            if !ids.is_empty() {
                let mut uncovered = Vec::new();
                let mut all_active = Vec::new();
                for &id in &ids {
                    if let Some(e) = host.game.world.event(id) {
                        if e.state > 0 && e.vanish_time <= 0 {
                            all_active.push((e.x, e.y));
                            let trig_new = !e.trigger.is_none()
                                && !global_blocks.contains(host.game.prog.label_name(e.trigger.label));
                            let auto_new = !e.auto.is_none()
                                && !global_blocks.contains(host.game.prog.label_name(e.auto.label));
                            if trig_new || auto_new {
                                uncovered.push((e.x, e.y));
                            }
                        }
                    }
                }
                let use_uncovered = !uncovered.is_empty() && rng.borrow_mut().range(10) < 7;
                let pool = if use_uncovered { &uncovered } else { &all_active };
                if !pool.is_empty() {
                    let pick = rng.borrow_mut().range(pool.len() as u64) as usize;
                    Some(pool[pick])
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let interact = click.is_none() && rng.borrow_mut().range(12) == 0;
        let input = TickInput {
            held_dir: if click.is_some() { -1 } else { cur_dir as i8 },
            interact,
            click,
            steer: None,
        };
        let tick_now = host.game.tick_count;
        recorded_log.push((tick_now, input));
        let was_idle_explore =
            !host.game.is_busy() && host.game.battle.is_none() && !host.game.is_loading_scene();

        let step_res = catch_unwind(AssertUnwindSafe(|| {
            host.tick(input);
        }));
        if let Err(payload) = step_res {
            let msg = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("non-string panic");
            failure = Some(format!("{label}@t={t}: PANIC: {msg}"));
            break;
        }

        // Strict wall-clipping check (A3): during continuous explore movement (no cutscene/warp),
        // leader must never step from a walkable tile onto a blocked `base_grid()` tile.
        if was_idle_explore
            && !host.game.is_busy()
            && host.game.battle.is_none()
            && !host.game.is_loading_scene()
        {
            let sc = host.game.world.scene_id;
            let lead = host.game.world.leader();
            let (hx, hy, hh) = world_to_half_tile(lead.x, lead.y);
            let blocked = host.game.base_grid().is_blocked(hx, hy, hh);
            if !blocked {
                prev_walkable = Some((sc, lead.x, lead.y));
            } else if let Some((psc, px, py)) = prev_walkable {
                if psc == sc && (px != lead.x || py != lead.y) {
                    failure = Some(format!(
                        "{label}@t={t}: leader stepped onto blocked tile in scene {sc}: ({px},{py}) -> ({},{})",
                        lead.x, lead.y
                    ));
                    break;
                }
            }
        } else {
            prev_walkable = None;
        }

        // Bounded VM steps per tick invariant (A3)
        let cov_steps = host
            .game
            .coverage
            .as_ref()
            .map(|c| c.opcodes.values().sum::<u64>())
            .unwrap_or(0);
        let ops_this_tick = cov_steps.saturating_sub(prev_cov_ops);
        prev_cov_ops = cov_steps;
        if ops_this_tick > 20_000 {
            failure = Some(format!(
                "{label}@t={t}: VM executed {ops_this_tick} ops in a single tick (>20000)"
            ));
            break;
        }

        // Bounded battle duration invariant (A3: no infinite battles)
        if host.game.battle.is_some() {
            battle_ticks += 1;
            if battle_ticks > 15_000 {
                failure = Some(format!(
                    "{label}@t={t}: battle exceeded 15000 tick budget"
                ));
                break;
            }
        } else {
            battle_ticks = 0;
        }

        // Anti-stall invariant: foreground script must not freeze without waiting on player input
        let waiting_player = matches!(
            host.game.waiting_on(),
            Some(Wait::Dialogue | Wait::Choice { .. } | Wait::Modal)
        );
        if host.game.is_busy() && !waiting_player && host.game.battle.is_none() {
            let lead = host.game.world.leader();
            let sig = (host.game.world.scene_id, lead.x, lead.y, cov_steps);
            if sig == last_sig {
                busy_stall_ticks += 1;
                if busy_stall_ticks >= 600 {
                    failure = Some(format!(
                        "{label}@t={t}: foreground script stalled for {busy_stall_ticks} ticks (waiting={:?})",
                        host.game.waiting_on()
                    ));
                    break;
                }
            } else {
                busy_stall_ticks = 0;
                last_sig = sig;
            }
        } else {
            busy_stall_ticks = 0;
        }

        if t % 10 == 0 {
            if let Err(e) = check_invariants(host, &baseline, &format!("{label}@t={t}")) {
                failure = Some(e);
                break;
            }
        }
        if t > 0 && t % 100 == 0 {
            if let Err(e) = check_save_roundtrip_idempotent(host, &format!("{label}@t={t}")) {
                failure = Some(e);
                break;
            }
        }

        // C9: Capture a frontier snapshot when idle after discovering new script blocks
        if !captured_frontier
            && frontier.len() < frontier_cap
            && t >= 40
            && !host.game.is_busy()
            && host.game.battle.is_none()
            && !host.game.is_loading_scene()
        {
            let has_new_block = host.game.coverage.as_ref().is_some_and(|cov| {
                cov.steps
                    .keys()
                    .any(|&(lbl, _)| !global_blocks.contains(host.game.prog.label_name(lbl)))
            });
            if has_new_block {
                let snap = host.game.save_json("frontier", "fuzz");
                frontier.push(FrontierSnapshot {
                    label: format!("{label}_sc{}", host.game.world.scene_id),
                    data: snap,
                    seed: seed.wrapping_mul(6364136223846793005).wrapping_add(t as u64 + 1),
                });
                captured_frontier = true;
            }
        }
    }

    host.battle_chooser = None;

    if failure.is_none() {
        if let Err(e) = check_invariants(host, &baseline, &format!("{label}@end")) {
            failure = Some(e);
        }
    }

    if let Some(reason) = failure {
        let minimized = if replays_with_failure(anchor, &recorded_log, label) {
            minimize_failing_log(&recorded_log, |sub| replays_with_failure(anchor, sub, label))
        } else {
            recorded_log
        };
        let path = export_failure_replay(label, &reason, anchor.to_json(), &minimized);
        panic!(
            "{reason}\nReproducible minimized replay ({} steps) saved to: {}",
            minimized.len(),
            path.display()
        );
    }

    if let Some(cov) = host.game.coverage.as_ref() {
        for (&op, &cnt) in &cov.opcodes {
            *global_ops.entry(op).or_insert(0) += cnt;
        }
        for &(lbl, _) in cov.steps.keys() {
            global_blocks.insert(host.game.prog.label_name(lbl).to_string());
        }
    }
}

fn load_all_51_rpgs() -> Vec<(String, Vec<u8>)> {
    let saves_dir = assets_dir().join("orig_saves");
    let mut paths: Vec<_> = std::fs::read_dir(&saves_dir)
        .expect("read orig_saves")
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("rpg"))
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 51, "expected all 51 classic DOS saves");
    paths
        .into_iter()
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            let bytes = std::fs::read(&p).unwrap();
            (name, bytes)
        })
        .collect()
}

#[test]
fn fuzz_all_51_dos_saves_and_new_game_with_coverage() {
    let ticks_per_save: u32 = std::env::var("PAL_FUZZ_TICKS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(180);
    let seeds_per_save: u64 = std::env::var("PAL_FUZZ_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let rpgs = load_all_51_rpgs();
    let mut host = NativeHost::new();
    let mut global_ops: HashMap<u16, u64> = HashMap::new();
    let mut global_blocks: HashSet<String> = HashSet::new();
    let mut frontier: Vec<FrontierSnapshot> = Vec::new();
    let frontier_cap = if ticks_per_save >= 1000 { 48 } else { 12 };
    let mut exemptions = ExemptionStats::default();

    // 1. Fuzz New Game
    for s in 0..seeds_per_save.max(2) {
        let anchor = FuzzAnchor::NewGame(0x95_1995 + s as u32);
        anchor.apply(&mut host).unwrap();
        fuzz_run(
            &mut host,
            &anchor,
            0xCAFE_0001 + s,
            ticks_per_save.max(400),
            &format!("new_game#s{s}"),
            &mut global_ops,
            &mut global_blocks,
            &mut frontier,
            frontier_cap,
            &mut exemptions,
            true,
        );
    }

    // 2. Fuzz every one of the 51 classic DOS saves (with B8 World::rng variation for s > 0)
    for (idx, (name, bytes)) in rpgs.iter().enumerate() {
        for s in 0..seeds_per_save {
            let rng_override = (s > 0).then(|| {
                let mixed = 0x51A7_0001u32
                    .wrapping_add((idx as u32).wrapping_mul(1009))
                    .wrapping_add((s as u32).wrapping_mul(7919));
                mixed.max(1)
            });
            let anchor = FuzzAnchor::OrigSave {
                file: name.clone(),
                bytes: bytes.clone(),
                rng_override,
            };
            anchor.apply(&mut host).unwrap();
            fuzz_run(
                &mut host,
                &anchor,
                0xBEEF_0000 + (idx as u64) * 100 + s,
                ticks_per_save,
                &format!("{name}#s{s}"),
                &mut global_ops,
                &mut global_blocks,
                &mut frontier,
                frontier_cap,
                &mut exemptions,
                true,
            );
        }
    }

    // 3. C9 Coverage-guided frontier follow-up wave from snapshots that reached new blocks
    let wave_snaps = std::mem::take(&mut frontier);
    let wave_count = wave_snaps.len();
    for (wi, snap) in wave_snaps.into_iter().enumerate() {
        let anchor = FuzzAnchor::SaveSnapshot {
            label: snap.label.clone(),
            data: snap.data,
        };
        if anchor.apply(&mut host).is_ok() {
            let mut dummy_frontier = Vec::new();
            fuzz_run(
                &mut host,
                &anchor,
                snap.seed,
                ticks_per_save,
                &format!("frontier#{wi}_{}", snap.label),
                &mut global_ops,
                &mut global_blocks,
                &mut dummy_frontier,
                0,
                &mut exemptions,
                true,
            );
        }
    }

    let total_steps: u64 = global_ops.values().sum();
    let mut by_chapter: BTreeMap<u32, usize> = BTreeMap::new();
    for blk in &global_blocks {
        if let Some(ch) = Program::chapter_for_label(blk) {
            *by_chapter.entry(ch).or_insert(0) += 1;
        }
    }
    let ch_summary: Vec<String> = by_chapter
        .iter()
        .map(|(ch, n)| format!("ch{ch:02}:{n}"))
        .collect();
    let mut top_ops: Vec<(u16, u64)> = global_ops.iter().map(|(&k, &v)| (k, v)).collect();
    top_ops.sort_by(|a, b| b.1.cmp(&a.1));
    let top_summary: Vec<String> = top_ops
        .iter()
        .take(8)
        .map(|(op, cnt)| format!("{}({})", opcode_name(*op), cnt))
        .collect();
    eprintln!(
        "[fuzz_invariants] coverage_trend ticks={ticks_per_save} seeds={seeds_per_save} frontier={wave_count} dirty_exemptions={}(hp={},mp={},inv={},equip={}) distinct_opcodes={} distinct_blocks={} total_vm_steps={} chapters=[{}] top={}",
        exemptions.total,
        exemptions.hp,
        exemptions.mp,
        exemptions.inv,
        exemptions.equip,
        global_ops.len(),
        global_blocks.len(),
        total_steps,
        ch_summary.join(", "),
        top_summary.join(", ")
    );

    // Calibrated coverage floors (measured minus safety margin: PR ~81/175, nightly >=100/500)
    let (min_ops, min_blocks) = if ticks_per_save >= 2000 && seeds_per_save >= 2 {
        (95usize, 450usize)
    } else {
        (70usize, 150usize)
    };
    assert!(
        global_ops.len() >= min_ops,
        "expected fuzz suite to exercise >= {min_ops} distinct VM opcodes, got {}",
        global_ops.len()
    );
    assert!(
        global_blocks.len() >= min_blocks,
        "expected fuzz suite to exercise >= {min_blocks} distinct script blocks, got {}",
        global_blocks.len()
    );
}

/// #272 + #273 A1: Save-anchor continuation equivalence oracle across ALL 51 DOS saves × 3 seeds.
///
/// Mirrors the prototype that exposed 46/141 divergences before #272:
/// 1. Load each DOS save, enable recorder, feed randomized warmup ticks.
/// 2. Call the real `split_segment_on_manual_save()` during exploration and feed idle ticks
///    until the deferred split cleanly anchors a new `SaveJson` segment.
/// 3. Feed the exact same sequence of randomized follow-up ticks to BOTH the live game AND a
///    fresh `NativeHost` loaded from that segment's `SaveJson` anchor, asserting bit-identical
///    final `canonical_state()` and `world.rng.state`.
#[test]
fn fuzz_save_anchor_continuation_equivalence_272() {
    let warmup_ticks: u32 = std::env::var("PAL_CONT_WARMUP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(180);
    let follow_ticks: u32 = std::env::var("PAL_CONT_FOLLOW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(240);

    let rpgs = load_all_51_rpgs();
    let mut live = NativeHost::new();
    let mut cloned = NativeHost::new();

    let mut tested = 0usize;
    let mut deferred_splits = 0usize;
    let mut divergences = Vec::new();

    for (idx, (name, bytes)) in rpgs.iter().enumerate() {
        for s in 0..3u64 {
            let mut rng = FuzzRng::new(0xA1_0000 + (idx as u64) * 31 + s);
            live.returned_to_title = false;
            live.game_over = false;
            live.game.recorder.enabled = true;
            live.game.load_dos_rpg(bytes).expect("load rpg");
            if s > 0 {
                live.game.world.rng = GameRng::new(((idx as u32 + 1) * 97 + s as u32 * 131).max(1));
            }
            live.pump();

            // 1. Warmup ticks with random movement + clicks + interacts
            let mut cur_dir = rng.range(4) as u8;
            let mut hold = 0u32;
            for _ in 0..warmup_ticks {
                if live.returned_to_title || live.game_over {
                    break;
                }
                live.choice_answer = rng.range(2) == 0;
                live.battle_cmd = match rng.range(4) {
                    0 => BattleCmd::Auto,
                    1 => BattleCmd::Defend,
                    2 => BattleCmd::Attack { target: 0 },
                    _ => BattleCmd::Flee,
                };
                if hold == 0 {
                    cur_dir = rng.range(4) as u8;
                    hold = 6 + rng.range(16) as u32;
                } else {
                    hold -= 1;
                }
                let click = if !live.game.is_busy() && rng.range(28) == 0 {
                    let ids = live.game.world.current_event_ids();
                    if !ids.is_empty() {
                        let id = ids[rng.range(ids.len() as u64) as usize];
                        live.game
                            .world
                            .event(id)
                            .filter(|e| e.state > 0 && e.vanish_time <= 0)
                            .map(|e| (e.x, e.y))
                    } else {
                        None
                    }
                } else {
                    None
                };
                let inp = TickInput {
                    held_dir: if click.is_some() { -1 } else { cur_dir as i8 },
                    interact: click.is_none() && rng.range(15) == 0,
                    click,
                    steer: None,
                };
                live.tick(inp);
            }

            if live.returned_to_title || live.game_over {
                continue;
            }
            // Wait briefly for any active dialogue/battle to finish if possible
            for _ in 0..120 {
                if !live.game.is_busy() && live.game.battle.is_none() && !live.game.is_loading_scene() {
                    break;
                }
                live.tick(TickInput::IDLE);
            }
            if live.game.is_busy() || live.game.battle.is_some() || live.game.is_loading_scene() {
                continue;
            }

            // 2. Call the REAL `split_segment_on_manual_save()` (may be mid-move_cd or mid-npc_walk!)
            let segs_before = live.game.recorder.segments.len();
            live.game.split_segment_on_manual_save();
            let mut split_inside_tick = false;
            if live.game.recorder.segments.len() == segs_before {
                deferred_splits += 1;
                // Feed idle ticks until the deferred split cleanly anchors (bounded)
                for _ in 0..80 {
                    live.tick(TickInput::IDLE);
                    if live.game.recorder.segments.len() > segs_before {
                        split_inside_tick = true;
                        break;
                    }
                }
            }
            if live.game.recorder.segments.len() <= segs_before {
                // Still suppressed on a proximity tile or busy; skip this case
                continue;
            }
            let anchor_json = match &live.game.recorder.segments.last().unwrap().anchor {
                ReplayAnchor::SaveJson(v) => v.clone(),
                _ => continue,
            };

            // 3. Load the exact segment anchor into a fresh host and feed identical inputs to BOTH.
            // If the deferred split fired at the top of `live.tick(TickInput::IDLE)`, `live` has
            // already executed that `TickInput::IDLE` tick after saving the anchor, so replay that
            // same tick on `cloned` to align both sessions on the exact same tick.
            cloned.returned_to_title = false;
            cloned.game_over = false;
            cloned.game.recorder.enabled = false;
            cloned
                .game
                .load_save_json_keeping_recorder(&anchor_json)
                .expect("load manual-save anchor");
            cloned.pump();
            if split_inside_tick {
                cloned.tick(TickInput::IDLE);
            }
            if live.game.canonical_state() != cloned.game.canonical_state()
                || live.game.world.rng.state != cloned.game.world.rng.state
            {
                divergences.push(format!(
                    "{name}#s{s} @t=0 (immediately after load/align): live rng={} vs clone rng={}",
                    live.game.world.rng.state, cloned.game.world.rng.state
                ));
                tested += 1;
                continue;
            }

            // Pre-generate follow-up inputs so both live and cloned receive the exact same stream
            let mut follow_hold = 0u32;
            let mut follow_dir = rng.range(4) as u8;
            let mut first_rng_div: Option<u32> = None;
            for ft in 0..follow_ticks {
                if live.returned_to_title || live.game_over || cloned.returned_to_title || cloned.game_over {
                    break;
                }
                let ans = rng.range(2) == 0;
                let bcmd = match rng.range(4) {
                    0 => BattleCmd::Auto,
                    1 => BattleCmd::Defend,
                    2 => BattleCmd::Attack { target: 0 },
                    _ => BattleCmd::Flee,
                };
                live.choice_answer = ans;
                cloned.choice_answer = ans;
                live.battle_cmd = bcmd;
                cloned.battle_cmd = bcmd;

                if follow_hold == 0 {
                    follow_dir = rng.range(4) as u8;
                    follow_hold = 6 + rng.range(16) as u32;
                } else {
                    follow_hold -= 1;
                }
                let inp = TickInput {
                    held_dir: follow_dir as i8,
                    interact: rng.range(14) == 0,
                    click: None,
                    steer: None,
                };
                live.tick(inp);
                cloned.tick(inp);
                if first_rng_div.is_none() && live.game.world.rng.state != cloned.game.world.rng.state {
                    first_rng_div = Some(ft);
                }
            }

            tested += 1;
            let cl = live.game.canonical_state();
            let cc = cloned.game.canonical_state();
            if cl != cc || live.game.world.rng.state != cloned.game.world.rng.state {
                let mut diff_keys = Vec::new();
                if let (Some(o1), Some(o2)) = (cl.as_object(), cc.as_object()) {
                    for (k, v1) in o1 {
                        if o2.get(k) != Some(v1) {
                            diff_keys.push(k.clone());
                        }
                    }
                }
                divergences.push(format!(
                    "{name}#s{s} split_in_tick={split_inside_tick} first_rng_div={first_rng_div:?} live_rng={} clone_rng={} diff_keys={diff_keys:?}",
                    live.game.world.rng.state, cloned.game.world.rng.state
                ));
            }
        }
    }

    eprintln!(
        "[fuzz_invariants] #272 continuation equivalence: tested={tested}/153 deferred={deferred_splits} divergences={}",
        divergences.len()
    );
    assert!(
        tested >= 115,
        "expected >= 115 eligible continuation cases across 51 saves × 3 seeds, got {tested}"
    );
    assert!(
        divergences.is_empty(),
        "#272 manual-save continuation equivalence diverged in {}/{tested} cases:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
}

/// #273 A4: Same-seed determinism twin oracle (runs fast enough for PR CI).
/// Executes New Game + 12 spread DOS saves twice in the same process with identical seeds and
/// mixed battle/field commands, asserting bit-identical `canonical_state()` and `rng.state`.
#[test]
fn fuzz_determinism_twin_same_seed() {
    let rpgs = load_all_51_rpgs();
    let sample_indices = [0usize, 2, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50];

    let run_once = |anchor: &FuzzAnchor, seed: u64, ticks: u32| -> (serde_json::Value, u32) {
        let mut h = NativeHost::new();
        anchor.apply(&mut h).unwrap();
        let mut ops = HashMap::new();
        let mut blks = HashSet::new();
        let mut frontier = Vec::new();
        let mut ex = ExemptionStats::default();
        fuzz_run(
            &mut h,
            anchor,
            seed,
            ticks,
            "twin",
            &mut ops,
            &mut blks,
            &mut frontier,
            0,
            &mut ex,
            true,
        );
        (h.game.canonical_state(), h.game.world.rng.state)
    };

    // New Game twin
    let ng = FuzzAnchor::NewGame(0x95_1995);
    let (s1, r1) = run_once(&ng, 0xD00D_0001, 300);
    let (s2, r2) = run_once(&ng, 0xD00D_0001, 300);
    assert_eq!((&s1, r1), (&s2, r2), "New Game twin diverged (nondeterminism!)");

    // 12 spread DOS saves twin
    for &idx in &sample_indices {
        let (name, bytes) = &rpgs[idx];
        let anchor = FuzzAnchor::OrigSave {
            file: name.clone(),
            bytes: bytes.clone(),
            rng_override: None,
        };
        let seed = 0xD00D_1000 + idx as u64;
        let (a_state, a_rng) = run_once(&anchor, seed, 200);
        let (b_state, b_rng) = run_once(&anchor, seed, 200);
        assert_eq!(
            (a_state, a_rng),
            (b_state, b_rng),
            "Determinism twin diverged on {name} (seed={seed:#x})"
        );
    }
}

#[test]
fn minimizer_shrinks_synthetic_failure_to_minimal_replay() {
    let log: Vec<(u64, TickInput)> = (0..60u64)
        .map(|t| {
            let inp = if t == 17 {
                TickInput { held_dir: -1, interact: true, click: None, steer: None }
            } else if t == 42 {
                TickInput { held_dir: 3, interact: false, click: None, steer: None }
            } else {
                TickInput { held_dir: (t % 3) as i8, interact: false, click: None, steer: None }
            };
            (t, inp)
        })
        .collect();

    let shrunk = minimize_failing_log(&log, |sub| {
        let saw_17 = sub.iter().any(|(_, i)| i.interact);
        let saw_42 = sub.iter().any(|(_, i)| i.held_dir == 3);
        saw_17 && saw_42
    });
    assert_eq!(
        shrunk.len(),
        2,
        "delta-debugger must shrink 60-step log down to the 2 essential inputs, got {}",
        shrunk.len()
    );
    let path = export_failure_replay(
        "synthetic_unit_test",
        "synthetic two-input failure",
        serde_json::json!({"type":"new_game","seed":1}),
        &shrunk,
    );
    assert!(path.exists());
    let _ = std::fs::remove_file(path);
}
