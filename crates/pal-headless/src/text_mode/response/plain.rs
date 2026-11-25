//! Human-readable plain-text formatter for `repal-text/1` responses.

/// Format a concise human-readable plain-text observation matching `TEXT_MODE_SPEC.md`.
pub fn format_plain_response(resp: &serde_json::Value, show_events: bool) -> String {
    let mut out = String::new();
    let mode = resp["mode"].as_str().unwrap_or("explore");
    let ok = resp["ok"].as_bool().unwrap_or(true);
    let is_en = resp["lang"].as_str() == Some("en");
    if resp["bye"].as_bool() == Some(true) {
        return "Bye.\n".to_string();
    }
    if !ok {
        if let Some(err) = resp["error"].as_str() {
            out.push_str(&format!("[{mode}] ERROR: {err}\n"));
        }
    }
    if let Some(notices) = resp["notices"].as_array() {
        let lbl = if is_en { "Notice" } else { "提示" };
        for n in notices {
            if let Some(tx) = n["text"].as_str() {
                if !tx.is_empty() {
                    out.push_str(&format!("  [{lbl}] {tx}\n"));
                }
            }
        }
    }
    if let Some(logs) = resp["log"].as_array() {
        for l in logs {
            if let Some(s) = l.as_str() {
                out.push_str(s);
                out.push('\n');
            }
        }
    }
    if let Some(sc) = resp["scene"].as_object() {
        let sid = sc.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
        let sname = sc.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let cash = resp["cash"].as_u64().unwrap_or(0);
        let pos = resp["party"]
            .as_array()
            .and_then(|p| p.first())
            .map(|lead| {
                format!(
                    "({},{}) {}",
                    lead["x"].as_i64().unwrap_or(0),
                    lead["y"].as_i64().unwrap_or(0),
                    lead["dir"].as_str().unwrap_or("sw")
                )
            })
            .unwrap_or_default();
        let cash_lbl = if is_en { "Wen" } else { "金钱" };
        out.push_str(&format!(
            "[{mode}] #{sid} {sname} {pos} | {cash_lbl}:{cash} | ui:{}\n",
            resp["ui_view"].as_str().unwrap_or("none")
        ));
        if matches!(mode, "explore" | "battle" | "battle_anim" | "shop") || show_events {
            if let Some(party) = resp["party"].as_array() {
                if !party.is_empty() {
                    let members: Vec<String> = party
                        .iter()
                        .map(|m| {
                            format!(
                                "{} HP:{}/{} MP:{}/{}",
                                m["name"].as_str().unwrap_or(""),
                                m["hp"].as_i64().unwrap_or(0),
                                m["max_hp"].as_i64().unwrap_or(0),
                                m["mp"].as_i64().unwrap_or(0),
                                m["max_mp"].as_i64().unwrap_or(0)
                            )
                        })
                        .collect();
                    let party_lbl = if is_en { "Party" } else { "队伍" };
                    out.push_str(&format!("  [{party_lbl}] {}\n", members.join(" | ")));
                }
            }
        }
    }
    if let Some(prompt) = resp["prompt"].as_object() {
        let kind = prompt.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "dialog" | "choice" => {
                let sp = prompt.get("speaker").and_then(|v| v.as_str()).unwrap_or("");
                let tx = prompt
                    .get("page_text")
                    .and_then(|v| v.as_str())
                    .or_else(|| prompt.get("text").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let page = prompt.get("page").and_then(|v| v.as_u64()).unwrap_or(0);
                let total_pages = prompt.get("total_pages").and_then(|v| v.as_u64()).unwrap_or(1);
                let page_suffix = if total_pages > 1 {
                    format!(" ({}/{})", page + 1, total_pages)
                } else {
                    String::new()
                };
                if sp.is_empty() {
                    out.push_str(&format!("  「{tx}」{page_suffix}\n"));
                } else {
                    out.push_str(&format!("  {sp}: 「{tx}」{page_suffix}\n"));
                }
                if let Some(choices) = prompt.get("choices").and_then(|v| v.as_array()) {
                    if !choices.is_empty() {
                        let list: Vec<String> = choices
                            .iter()
                            .enumerate()
                            .filter_map(|(i, c)| c.as_str().map(|s| format!("{}.{s}", i + 1)))
                            .collect();
                        let ch_lbl = if is_en { "Choices" } else { "选项" };
                        out.push_str(&format!("  {ch_lbl}: {}\n", list.join(" / ")));
                    }
                }
            }
            "battle_victory" => {
                if is_en {
                    out.push_str(&format!(
                        "  [Victory] EXP +{} | Wen +{}\n",
                        prompt.get("exp").and_then(|v| v.as_u64()).unwrap_or(0),
                        prompt.get("cash").and_then(|v| v.as_u64()).unwrap_or(0)
                    ));
                } else {
                    out.push_str(&format!(
                        "  [战斗胜利] 经验 +{} | 金钱 +{}\n",
                        prompt.get("exp").and_then(|v| v.as_u64()).unwrap_or(0),
                        prompt.get("cash").and_then(|v| v.as_u64()).unwrap_or(0)
                    ));
                }
            }
            "shop" | "pawnshop" => {
                let is_pawn = kind == "pawnshop";
                out.push_str(match (is_en, is_pawn) {
                    (true, true) => "  [Pawnshop]\n",
                    (true, false) => "  [Shop]\n",
                    (false, true) => "  [典当列表]\n",
                    (false, false) => "  [商店列表]\n",
                });
                let unit = if is_en { "Wen" } else { "文" };
                if let Some(items) = prompt.get("items").and_then(|v| v.as_array()) {
                    for it in items.iter().take(16) {
                        if is_pawn {
                            out.push_str(&format!(
                                "    [{}] {} ({} {unit}, x{})\n",
                                it["idx"].as_u64().unwrap_or(0),
                                it["name"].as_str().unwrap_or(""),
                                it["price"].as_u64().unwrap_or(0),
                                it["count"].as_u64().unwrap_or(1)
                            ));
                        } else {
                            out.push_str(&format!(
                                "    [{}] {} ({} {unit})\n",
                                it["idx"].as_u64().unwrap_or(0),
                                it["name"].as_str().unwrap_or(""),
                                it["price"].as_u64().unwrap_or(0)
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(battle) = resp["battle"].as_object() {
        if let Some(enemies) = battle.get("enemies").and_then(|v| v.as_array()) {
            let summary: Vec<String> = enemies
                .iter()
                .map(|e| {
                    let slot = e["slot"].as_u64().unwrap_or(0);
                    let name = e["name"].as_str().unwrap_or("");
                    if let (Some(hp), Some(mhp)) = (e["hp"].as_i64(), e["max_hp"].as_i64()) {
                        format!("#{slot} {name}({hp}/{mhp})")
                    } else if e["alive"].as_bool() == Some(true) {
                        format!("#{slot} {name}")
                    } else {
                        format!("#{slot} {name}(KO)")
                    }
                })
                .collect();
            let en_lbl = if is_en { "Enemies" } else { "敌方" };
            out.push_str(&format!("  [{en_lbl}] {}\n", summary.join(" | ")));
        }
        if let Some(hero_idx) = battle.get("prompting").and_then(|v| v.as_u64()) {
            let hero_name = battle
                .get("heroes")
                .and_then(|v| v.as_array())
                .and_then(|h| h.get(hero_idx as usize))
                .and_then(|m| m["name"].as_str())
                .unwrap_or(if is_en { "Hero" } else { "角色" });
            if is_en {
                out.push_str(&format!("  [Turn] Awaiting command for #{hero_idx} {hero_name}\n"));
            } else {
                out.push_str(&format!("  [行动] 等待 #{hero_idx} {hero_name} 下达指令\n"));
            }
        } else if is_en {
            out.push_str("  [Turn] Resolving battle animation...\n");
        } else {
            out.push_str("  [行动] 等待行动条/动画结算...\n");
        }
    }
    if matches!(mode, "menu" | "title" | "modal") {
        if let Some(menu) = resp["menu"].as_object() {
            let screen = menu.get("screen").and_then(|v| v.as_str()).unwrap_or("");
            let cursor = menu.get("cursor").and_then(|v| v.as_u64());
            if let Some(items) = menu.get("items").and_then(|v| v.as_array()) {
                let labels: Vec<String> = items
                    .iter()
                    .enumerate()
                    .take(12)
                    .map(|(i, v)| {
                        let name = v
                            .as_str()
                            .or_else(|| v.get("label").and_then(|n| n.as_str()))
                            .or_else(|| v.get("name").and_then(|n| n.as_str()))
                            .unwrap_or("?");
                        let val_s = v
                            .get("value")
                            .and_then(|x| x.as_str())
                            .map(|s| format!(":{s}"))
                            .unwrap_or_default();
                        if cursor == Some(i as u64) {
                            format!(">[{name}{val_s}]")
                        } else {
                            format!("{name}{val_s}")
                        }
                    })
                    .collect();
                if !labels.is_empty() {
                    out.push_str(&format!("  [{screen}] {}\n", labels.join(" | ")));
                }
            }
        }
    }
    if show_events && mode == "explore" {
        if let Some(w) = resp["walkable"].as_object() {
            let wd = resp["walkable_dist"].as_object();
            let dirs: Vec<String> = ["sw", "nw", "ne", "se"]
                .into_iter()
                .filter(|d| w.get(*d).and_then(|v| v.as_bool()) == Some(true))
                .map(|d| {
                    if let Some(n) = wd.and_then(|m| m.get(d)).and_then(|v| v.as_u64()) {
                        format!("{d}({n})")
                    } else {
                        d.to_string()
                    }
                })
                .collect();
            let walk_lbl = if is_en { "Walkable" } else { "可走" };
            let blocked_lbl = if is_en { "(blocked)" } else { "(受阻)" };
            out.push_str(&format!(
                "  [{walk_lbl}] {}\n",
                if dirs.is_empty() {
                    blocked_lbl.to_string()
                } else {
                    dirs.join(" ")
                }
            ));
        }
        if let Some(mm) = resp["minimap"].as_object() {
            if let Some(rows) = mm.get("grid").and_then(|v| v.as_array()) {
                let hdr = if is_en {
                    "  [Map 320x200] @=party .=walk #=block ?=occl E=ev >=exit (nw↖ ne↗ sw↙ se↘)\n"
                } else {
                    "  [地图 320x200] @=主角 .=可走 #=障碍 ?=遮挡 E=事件 >=出口 (nw↖ ne↗ sw↙ se↘)\n"
                };
                out.push_str(hdr);
                for r in rows {
                    if let Some(line) = r.as_str() {
                        out.push_str("  ");
                        out.push_str(line);
                        out.push('\n');
                    }
                }
            }
        }
        if let Some(evs) = resp["events"].as_array() {
            for ev in evs.iter().take(24) {
                let kind_lbl = ev["kind_label"]
                    .as_str()
                    .or_else(|| ev["kind"].as_str())
                    .unwrap_or("");
                if is_en {
                    out.push_str(&format!(
                        "  ev#{} [{}] {} ({},{}) d={} {}\n",
                        ev["id"].as_u64().unwrap_or(0),
                        kind_lbl,
                        ev["hint"].as_str().unwrap_or(""),
                        ev["x"].as_i64().unwrap_or(0),
                        ev["y"].as_i64().unwrap_or(0),
                        ev["dist"].as_i64().unwrap_or(0),
                        ev["dir"].as_str().unwrap_or("")
                    ));
                } else {
                    out.push_str(&format!(
                        "  ev#{} [{}] {} ({},{}) 距{}步 {}\n",
                        ev["id"].as_u64().unwrap_or(0),
                        kind_lbl,
                        ev["hint"].as_str().unwrap_or(""),
                        ev["x"].as_i64().unwrap_or(0),
                        ev["y"].as_i64().unwrap_or(0),
                        ev["dist"].as_i64().unwrap_or(0),
                        ev["dir"].as_str().unwrap_or("")
                    ));
                }
            }
            if evs.len() > 24 {
                if is_en {
                    out.push_str(&format!("  ... {} visible events total\n", evs.len()));
                } else {
                    out.push_str(&format!("  ... 共 {} 个可见事件\n", evs.len()));
                }
            }
        }
    }
    out
}
