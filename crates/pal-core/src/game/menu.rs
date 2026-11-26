//! Field-menu actions: equip, field magic, shops. Ports of the legacy `GameState` helpers
//! (`equipItemOnRole`, `castFieldMagic`) and the HUD's buy/sell handlers.

use super::{Game, HostCmd};

/// Equip row (`11..=16`, as used by `equip_item` / `role_set_attr_equip`) → slot index.
fn row_to_slot(row: i32) -> Option<usize> {
    (11..=16).contains(&row).then(|| (row - 11) as usize)
}

impl Game {
    /// Equipment slot for an item (legacy `getItemSlot`).
    pub fn item_slot(&self, item_id: u16) -> Option<usize> {
        let it = self.data.items.get(&item_id).filter(|i| i.equipable)?;
        let lbl = self.prog.label_id(&it.script_equip).unwrap_or(0);
        if let Some(block) = self.prog.block(lbl) {
            if let Some(s) = block.iter().find(|s| s.op == 0x0018).and_then(|s| row_to_slot(s.a[0])) {
                return Some(s);
            }
        }
        Some(match item_id {
            163..=195 => 3,
            196..=207 => 0,
            208..=224 => 2,
            225..=234 => 1,
            235..=247 => 4,
            _ => 5,
        })
    }

    /// Role equip permission from item flag bits 6..11 (legacy `canRoleEquipItem`).
    pub fn can_role_equip(&self, role: usize, item_id: u16) -> bool {
        if role >= 6 {
            return false;
        }
        let Some(it) = self.data.items.get(&item_id).filter(|i| i.equipable) else { return false };
        let mask = (it.flags >> 6) & 0x3f;
        mask == 0 || mask & (1 << role) != 0
    }

    /// Stat bonuses `[attack, magic_power, defense, dexterity, flee]` granted by an item.
    pub fn item_stat_bonuses(&self, item_id: u16) -> [i32; 5] {
        let mut b = [0; 5];
        let Some(it) = self.data.items.get(&item_id).filter(|i| i.equipable) else { return b };
        let lbl = self.prog.label_id(&it.script_equip).unwrap_or(0);
        if let Some(block) = self.prog.block(lbl).filter(|bl| !bl.is_empty()) {
            for s in block.iter().filter(|s| s.op == 0x0017) {
                if (17..=21).contains(&s.a[1]) {
                    b[(s.a[1] - 17) as usize] += s.s16(2);
                }
            }
            return b;
        }
        // Legacy fallback: parse "武术+N" style numbers out of the description (review B10).
        for (i, key) in ["武术", "灵力", "防御", "身法", "吉运"].iter().enumerate() {
            if let Some(pos) = it.desc.find(key) {
                let rest = &it.desc[pos + key.len()..];
                let num: String = rest.chars().take_while(|c| *c == '+' || *c == '-' || c.is_ascii_digit()).collect();
                b[i] = num.parse().unwrap_or(0);
            }
        }
        b
    }

    /// Merge an English i18n JSON overlay into `self.data.i18n_en`.
    pub fn load_i18n_en(&mut self, doc: &serde_json::Value) {
        self.data.load_i18n_en(doc);
    }

    pub fn role_display_name(&self, role_id: usize) -> String {
        if self.world.lang.is_en() {
            if let Some(en) = self.data.i18n_en.role_name(role_id as u16) {
                return en.to_string();
            }
        }
        self.world
            .roles
            .get(role_id)
            .map(|r| self.world.t(&r.name, &r.name_hant).to_string())
            .unwrap_or_default()
    }

    pub fn item_display_name(&self, item_id: u16) -> String {
        if self.world.lang.is_en() {
            if let Some(en) = self.data.i18n_en.item_name(item_id) {
                return en.to_string();
            }
        }
        self.data
            .items
            .get(&item_id)
            .map(|it| self.world.t(&it.name, &it.name_hant).to_string())
            .unwrap_or_default()
    }

    pub fn item_display_desc(&self, item_id: u16) -> String {
        if self.world.lang.is_en() {
            if let Some(en) = self.data.i18n_en.item_desc(item_id) {
                return en.to_string();
            }
        }
        self.data
            .items
            .get(&item_id)
            .map(|it| self.world.t(&it.desc, &it.desc_hant).to_string())
            .unwrap_or_default()
    }

    pub fn magic_display_name(&self, magic_id: u16) -> String {
        if self.world.lang.is_en() {
            if let Some(en) = self.data.i18n_en.magic_name(magic_id) {
                return en.to_string();
            }
        }
        self.data
            .magics
            .get(&magic_id)
            .map(|m| self.world.t(&m.name, &m.name_hant).to_string())
            .unwrap_or_default()
    }

    pub fn magic_display_desc(&self, magic_id: u16) -> String {
        if self.world.lang.is_en() {
            if let Some(en) = self.data.i18n_en.magic_desc(magic_id) {
                return en.replace('\r', "").replace('\n', " ");
            }
        }
        self.data
            .magics
            .get(&magic_id)
            .map(|m| {
                self.world
                    .t(&m.desc, &m.desc_hant)
                    .replace('\r', "")
                    .replace('\n', "")
            })
            .unwrap_or_default()
    }

    pub fn enemy_display_name(&self, enemy_id: u16, hans: &str, hant: &str) -> String {
        if self.world.lang.is_en() {
            if let Some(en) = self.data.i18n_en.enemy_name(enemy_id) {
                return en.to_string();
            }
        }
        self.world.t(hans, hant).to_string()
    }

    /// Equip `item_id` from the inventory onto `role` (legacy `equipItemOnRole`).
    pub fn equip_from_menu(&mut self, role: usize, item_id: u16) -> bool {
        self.recorder
            .push(self.tick_count, super::ReplayEvent::EquipItem { role, item_id });
        let msg = |g: &Game, hans: &str, hant: &str, en: &str| g.world.t3(hans, hant, en).to_string();
        let ok = matches!((self.world.roles.get(role), self.data.items.get(&item_id)), (Some(_), Some(it)) if it.equipable);
        let result: Result<String, String> = if !ok {
            Err(msg(self, "无法装备该物品", "無法裝備該物品", "Cannot equip this item"))
        } else if !self.can_role_equip(role, item_id) {
            Err(msg(self, "这项物品此人无法装备", "這項物品此人無法裝備", "This character cannot equip it"))
        } else if self.world.inventory.get(&item_id).copied().unwrap_or(0) <= 0 {
            Err(msg(self, "行囊中已无此装备", "行囊中已無此裝備", "Item not in inventory"))
        } else {
            let slot = self.item_slot(item_id).unwrap_or(5);
            let old = self.world.roles[role].equipment[slot];
            self.world.remove_item(item_id, 1);
            if old > 0 {
                self.world.add_item(old, 1);
            }
            self.world.roles[role].equipment[slot] = item_id;
            self.recompute_equip_effects(role);
            let rname = self.role_display_name(role);
            let iname = self.item_display_name(item_id);
            if self.world.lang.is_en() {
                Ok(format!("{rname} equipped {iname}"))
            } else {
                Ok(if self.world.lang.is_en() {
                    format!("{rname} equipped [{iname}]")
                } else {
                    format!("{} {}「{}」", rname, self.world.t("已装备", "已裝備"), iname)
                })
            }
        };
        let (ok, text) = match result {
            Ok(t) => (true, t),
            Err(t) => (false, t),
        };
        self.emit(HostCmd::Toast(text));
        ok
    }

    /// Recompute equipment-granted stat bonuses, passive effects (`role_set_attr_equip` resistances),
    /// and `role_set_attr(65, ...)` coop magic overrides from the currently equipped items' `script_equip`
    /// blocks without clobbering `base_stats` or script-assigned `base_coop_magic` (#214, #253, #264).
    pub(crate) fn recompute_equip_effects(&mut self, role: usize) {
        let Some(equip_ids) = self.world.roles.get(role).map(|r| r.equipment) else {
            return;
        };
        let mut effects = std::collections::BTreeMap::new();
        let mut stat_bonuses = [0i32; 5];
        let mut coop_override = None;

        for &item_id in &equip_ids {
            if item_id == 0 {
                continue;
            }
            let b = self.item_stat_bonuses(item_id);
            for k in 0..5 {
                stat_bonuses[k] += b[k];
            }
            let Some(script_name) = self
                .data
                .items
                .get(&item_id)
                .map(|it| it.script_equip.clone())
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let lbl = self.prog.intern(&script_name);
            let Some(blk) = self.prog.block(lbl) else {
                continue;
            };
            for ins in blk {
                match crate::script::opcode_name(ins.op) {
                    "role_set_attr_equip" => {
                        effects.insert((ins.a[0], ins.a[1]), ins.s16(2));
                    }
                    "role_set_attr" if ins.a[0] == 65 && ins.a[1] > 0 => {
                        coop_override = Some(ins.a[1] as u16);
                    }
                    "end" => break,
                    _ => {}
                }
            }
        }

        if let Some(r) = self.world.roles.get_mut(role) {
            r.equip_effects = effects;
            r.equip_stat_bonuses = stat_bonuses;
            r.equip_coop_override = coop_override;
            r.recompute_effective_stats();
        }
    }

    /// Derive `base_stats` from current effective `r.stats` minus equipped item bonuses
    /// (used at new game start, legacy JSON saves without `baseStats`, and DOS `.rpg` loads) (#253).
    pub(crate) fn sync_base_stats_from_effective(&mut self, role: usize) {
        let Some(equip_ids) = self.world.roles.get(role).map(|r| r.equipment) else {
            return;
        };
        let mut stat_bonuses = [0i32; 5];
        for &item_id in &equip_ids {
            if item_id > 0 {
                let b = self.item_stat_bonuses(item_id);
                for k in 0..5 {
                    stat_bonuses[k] += b[k];
                }
            }
        }
        if let Some(r) = self.world.roles.get_mut(role) {
            for k in 0..5 {
                r.base_stats[k] = r.stats[k] - stat_bonuses[k];
            }
            r.equip_stat_bonuses = stat_bonuses;
        }
    }

    pub(crate) fn recompute_all_equip_effects(&mut self) {
        let count = self.world.roles.len();
        for rid in 0..count {
            self.recompute_equip_effects(rid);
        }
    }

    /// Cast a field magic from the menu. When `target` is `None` the most injured living party
    /// member is chosen (legacy HUD `CastMagic` handler, #95).
    /// Records `ReplayEvent::CastFieldMagic` only after validation succeeds (#234).
    pub fn cast_field_magic(&mut self, caster: usize, magic_id: u16, target: Option<usize>) -> bool {
        let resolved_target = target.unwrap_or_else(|| {
            let mut injured: Vec<usize> = self
                .world
                .party_role_ids()
                .into_iter()
                .filter(|&r| self.world.roles.get(r).map(|x| x.hp > 0 && x.hp < x.max_hp).unwrap_or(false))
                .collect();
            injured.sort_by(|&a, &b| {
                let fa = self.world.roles[a].hp as f64 / self.world.roles[a].max_hp.max(1) as f64;
                let fb = self.world.roles[b].hp as f64 / self.world.roles[b].max_hp.max(1) as f64;
                fa.total_cmp(&fb)
            });
            injured.first().copied().unwrap_or(caster)
        });
        let res = self.cast_field_magic_inner(caster, magic_id, resolved_target);
        match res {
            Ok((sfx, text)) => {
                self.recorder.push(
                    self.tick_count,
                    super::ReplayEvent::CastFieldMagic {
                        caster,
                        magic_id,
                        target,
                    },
                );
                self.emit(HostCmd::PlaySfx(sfx));
                self.emit(HostCmd::Toast(text));
                true
            }
            Err(text) => {
                self.emit(HostCmd::Toast(text));
                false
            }
        }
    }

    fn cast_field_magic_inner(&mut self, caster: usize, magic_id: u16, target: usize) -> Result<(i32, String), String> {
        let t = |g: &Game, a: &str, b: &str, c: &str| g.world.t3(a, b, c).to_string();
        let (Some(c), Some(m)) = (self.world.roles.get(caster), self.data.magics.get(&magic_id).cloned()) else {
            return Err(t(self, "无法施展该仙术", "無法施展該仙術", "Cannot cast this spell"));
        };
        if c.hp <= 0 {
            return Err(t(self, "已昏厥，无法施展仙术", "已昏厥，無法施展仙術", "Fallen character cannot cast"));
        }
        if !c.magics.contains(&magic_id) {
            return Err(t(self, "尚未习得此仙术", "尚未習得此仙術", "Spell not learned"));
        }
        if !m.usable_outside_battle {
            return Err(t(self, "此仙术仅可在战斗中施展", "此仙術僅可在戰鬥中施展", "Battle-only spell"));
        }
        if c.mp < m.cost_mp {
            return Err(t(self, "真气不足", "真氣不足", "Not enough MP"));
        }
        let target = if self.world.roles.get(target).is_some() { target } else { caster };
        let targets: Vec<usize> = if m.target_all { self.world.party_role_ids() } else { vec![target] };
        let lbl = self.prog.label_id(&m.script_success).unwrap_or(0);
        let steps = self.prog.block(lbl).map(|b| b.to_vec()).unwrap_or_default();
        let is_revive = steps.iter().any(|s| s.op == 0x0022) || magic_id == 301 || magic_id == 302;
        if is_revive && !m.target_all && self.world.roles[target].hp > 0 {
            return Err(t(self, "目标并未昏厥，无需施展", "目標並未昏厥，無需施展", "Target is not fallen"));
        }
        let r = &mut self.world.roles[caster];
        r.mp = (r.mp - m.cost_mp).max(0);
        if !steps.is_empty() {
            for s in &steps {
                if s.op == 0x001B {
                    let all = s.a[0] != 0 || m.target_all;
                    let delta = if s.has & 2 != 0 { s.s16(1) } else if s.has & 1 != 0 { s.s16(0) } else { 75 };
                    let ids = if all { self.world.party_role_ids() } else { targets.clone() };
                    for id in ids {
                        if let Some(r) = self.world.roles.get_mut(id) {
                            if delta > 0 && r.hp <= 0 {
                                continue;
                            }
                            r.hp = (r.hp + delta).max(0).min(r.max_hp);
                        }
                    }
                } else if s.op == 0x0022 {
                    let tenths = s.arg_nz(1, 1).max(1);
                    for &id in &targets {
                        let r = &mut self.world.roles[id];
                        if r.hp <= 0 {
                            r.hp = (((r.max_hp * tenths) as f64 / 10.0).round().max(1.0) as i32).min(r.max_hp.max(1));
                        }
                    }
                }
            }
        } else {
            // Legacy fallback: heal amount from "HP+N" in the description, else 75.
            let upper = m.desc.to_ascii_uppercase();
            let heal = upper
                .find("HP+")
                .map(|p| upper[p + 3..].chars().take_while(char::is_ascii_digit).collect::<String>())
                .and_then(|n| n.parse().ok())
                .unwrap_or(75);
            let is_revive = magic_id == 301 || magic_id == 302;
            for &id in &targets {
                let r = &mut self.world.roles[id];
                if heal > 0 && r.hp <= 0 && !is_revive {
                    continue;
                }
                r.hp = (r.hp + heal).min(r.max_hp);
            }
        }
        let cn = self.role_display_name(caster);
        let tn = if m.target_all {
            t(self, "全队", "全隊", "Party")
        } else {
            self.role_display_name(target)
        };
        let mn = self.magic_display_name(magic_id);
        let text = if self.world.lang.is_en() {
            format!("{cn} cast {mn} -> {tn}")
        } else {
            if self.world.lang.is_en() {
                format!("{cn} casts [{mn}] → {tn}")
            } else {
                format!("{} {}「{}」→ {}", cn, self.world.t("施展", "施展"), mn, tn)
            }
        };
        Ok((if m.sfx_id != 0 { m.sfx_id as i32 } else { 138 }, text))
    }

    /// Shop purchase (legacy HUD `BuyItem`), always charging canonical item price (#268).
    pub fn buy_item(&mut self, item_id: u16, _price: u32) -> bool {
        let Some(it) = self.data.items.get(&item_id) else {
            return false;
        };
        let canonical = if it.price != 0 { it.price } else { 100 }.max(10) as u32;
        if self.world.cash >= canonical {
            self.recorder
                .push(self.tick_count, super::ReplayEvent::BuyItem { item_id, price: canonical });
            self.world.cash = self.world.cash.saturating_sub(canonical);
            self.world.add_item(item_id, 1);
            self.emit(HostCmd::Toast(self.world.t3("购买成功！", "購買成功！", "Purchased!").into()));
            true
        } else {
            self.emit(HostCmd::Toast(self.world.t3("金钱不足！", "金錢不足！", "Not enough money!").into()));
            false
        }
    }

    /// Pawnshop sale (legacy HUD `SellItem`), validating sellable flag and always paying canonical price (#268).
    pub fn sell_item(&mut self, item_id: u16, _price: u32) -> bool {
        let Some(it) = self.data.items.get(&item_id) else {
            return false;
        };
        if !it.sellable || it.price <= 0 {
            return false;
        }
        let canonical = (it.price / 2).max(1) as u32;
        if self.world.remove_item(item_id, 1) {
            self.recorder
                .push(self.tick_count, super::ReplayEvent::SellItem { item_id, price: canonical });
            self.world.cash = self.world.cash.saturating_add(canonical);
            let msg = if self.world.lang.is_en() {
                format!("Sold for {canonical} Wen!")
            } else {
                if self.world.lang.is_en() {
                    format!("Sold for {canonical} Wen!")
                } else {
                    format!("{}{canonical}{}", self.world.t("售出获得 ", "售出獲得 "), self.world.t(" 文！", " 文！"))
                }
            };
            self.emit(HostCmd::Toast(msg));
            true
        } else {
            false
        }
    }
}
