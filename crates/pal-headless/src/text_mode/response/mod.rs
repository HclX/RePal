//! Canonical observation, mode detection, JSON-Lines envelope (`repal-text/1`),
//! and plain-text formatter shared identically between headless `--text` and GUI+Text.


mod envelope;
mod menu;
mod plain;

pub use envelope::{build_response_json, build_response_json_ex, ResponseOpts};
pub use menu::build_ui_menu;
pub use plain::format_plain_response;
use crate::text_mode::{ObsMode, RngStatus};
use pal_core::game::battle::{status_display_name, BattleFeedEntry};
use pal_core::game::data::{ItemDef, MagicDef};
use pal_core::game::world::Event;
use pal_core::isometric::{direction_between, half_tile_to_world, step_half_tile, world_to_half_tile};
use pal_core::render::compute_camera;
use pal_runtime::{PalRuntime, TextNotice};
use pal_ui::{BattleSubPhase, RoleSelectTarget, SaveSlotMode, UiView};
use std::collections::HashSet;

/// Protocol / schema identifier included in every JSON-Lines response (#225).
pub const TEXT_PROTOCOL_VERSION: &str = "repal-text/1";

/// Stable machine-readable snake_case identifier for `UiView` (#225).
pub fn ui_view_id(view: &UiView) -> &'static str {
    match view {
        UiView::None => "none",
        UiView::Title => "title",
        UiView::TitleOptions => "title_options",
        UiView::Dialog => "dialog",
        UiView::RootMenu => "root_menu",
        UiView::RoleSelect(RoleSelectTarget::Status) => "role_select_status",
        UiView::RoleSelect(RoleSelectTarget::Magic) => "role_select_magic",
        UiView::RoleSelect(RoleSelectTarget::Equip) => "role_select_equip",
        UiView::StatusFbp000 => "status",
        UiView::EquipFbp001 => "equip",
        UiView::MagicGrid => "magic_grid",
        UiView::ItemSubmenu => "item_submenu",
        UiView::ItemGrid => "item_grid",
        UiView::SystemSubmenu => "system_submenu",
        UiView::RemakeSubmenu => "remake_submenu",
        UiView::SaveLoadSlots(SaveSlotMode::SaveInGame) => "save_slots",
        UiView::SaveLoadSlots(_) => "load_slots",
        UiView::ReplayChapters => "replay_chapters",
        UiView::ReplayFinished => "replay_finished",
        UiView::Shop => "shop",
        UiView::PawnshopSell => "pawnshop",
        UiView::BattleHud(BattleSubPhase::CommandWheel) => "battle_command_wheel",
        UiView::BattleHud(BattleSubPhase::MiscMenu) => "battle_misc_menu",
        UiView::BattleHud(BattleSubPhase::SelectEnemyForAttack) => "battle_select_enemy_attack",
        UiView::BattleHud(BattleSubPhase::SelectEnemyForCoop) => "battle_select_enemy_coop",
        UiView::BattleHud(BattleSubPhase::SelectMagic) => "battle_select_magic",
        UiView::BattleHud(BattleSubPhase::SelectEnemyForMagic(_)) => "battle_select_enemy_magic",
        UiView::BattleHud(BattleSubPhase::SelectAllyForMagic(_)) => "battle_select_ally_magic",
        UiView::BattleHud(BattleSubPhase::SelectItem) => "battle_select_item",
        UiView::BattleHud(BattleSubPhase::SelectEnemyForItem(_)) => "battle_select_enemy_item",
        UiView::BattleHud(BattleSubPhase::SelectAllyForItem(_)) => "battle_select_ally_item",
        UiView::BattleVictory => "battle_victory",
    }
}

/// Resolve the exact clamped `320x200` logical camera origin `(cam_x, cam_y)` using the
/// same math as `pal_render::world::resolve_world_camera`, reading live core coords even
/// when `rt.render_frame()` has not been invoked yet (#222, #224).
pub fn runtime_camera(rt: &PalRuntime) -> (i32, i32) {
    if let Some(g) = rt.game.as_ref() {
        if let Some((cx, cy)) = g.world.view.cam_override {
            return (cx.clamp(0, 2048 - 320), cy.clamp(0, 2048 - 200));
        }
        let lead = g.world.leader();
        let cam = compute_camera(lead.x, lead.y, 320, 200);
        return (cam.cam_x, cam.cam_y);
    }
    pal_render::world::resolve_world_camera(&rt.state, &rt.world)
}

/// Classify item target scope for `bag` / `BattleHud(SelectItem)` metadata (#220).
pub fn item_target_kind(it: &ItemDef) -> &'static str {
    if it.throwable && !it.usable {
        if it.target_all {
            "all_enemies"
        } else {
            "enemy"
        }
    } else if it.usable {
        if it.target_all {
            "all_allies"
        } else {
            "ally"
        }
    } else {
        "none"
    }
}

/// Classify magic target scope for `status` / `MagicGrid` / `BattleHud(SelectMagic)` metadata (#220).
pub fn magic_target_kind(m: &MagicDef) -> &'static str {
    match (m.target_enemy, m.target_all) {
        (true, true) => "all_enemies",
        (true, false) => "enemy",
        (false, true) => "all_allies",
        (false, false) => "ally",
    }
}

/// Canonical mode enum shared by headless Text Mode and GUI+Text Mode.
pub fn current_mode(rt: &PalRuntime) -> &'static str {
    if rt.is_in_title() {
        return "title";
    }
    if rt.ui.view == UiView::Dialog {
        return if !rt.ui.dialog.choices.is_empty() {
            "choice"
        } else {
            "dialog"
        };
    }
    if rt.victory_open || rt.ui.view == UiView::BattleVictory {
        return "battle_victory";
    }
    if let Some(g) = rt.game.as_ref() {
        if let Some(b) = g.battle.as_ref() {
            if b.active {
                return if b.prompting.is_some() {
                    "battle"
                } else {
                    "battle_anim"
                };
            }
        }
    }
    if matches!(rt.ui.view, UiView::BattleHud(_)) {
        return "battle";
    }
    if matches!(rt.ui.view, UiView::Shop | UiView::PawnshopSell) {
        return "shop";
    }
    if rt.ui.view == UiView::ReplayFinished {
        return "modal";
    }
    if rt.ui.is_modal_open() {
        return "menu";
    }
    if let Some(g) = rt.game.as_ref() {
        if g.is_busy() {
            return "cutscene";
        }
    }
    "explore"
}

/// Interactive plain-text prompt showing the current mode: `[<mode>]> `.
pub fn format_prompt(mode: &str) -> String {
    format!("[{mode}]> ")
}

pub fn dir_code(dir: u8) -> &'static str {
    match dir & 3 {
        0 => "sw",
        1 => "nw",
        2 => "ne",
        _ => "se",
    }
}

pub fn lang_code(lang: pal_core::Language) -> &'static str {
    match lang {
        pal_core::Language::En => "en",
        pal_core::Language::ZhHans => "hans",
        pal_core::Language::ZhHant => "hant",
    }
}

pub fn scene_name(rt: &PalRuntime, sid: u16) -> String {
    let Some(g) = rt.game.as_ref() else {
        return format!("Scene #{sid}");
    };
    g.data
        .scenes
        .get(&sid)
        .map(|s| {
            if g.world.lang.is_en() {
                g.data
                    .i18n_en
                    .scenes
                    .get(&sid)
                    .cloned()
                    .unwrap_or_else(|| s.name.clone())
            } else {
                s.name.clone()
            }
        })
        .unwrap_or_else(|| format!("Scene #{sid}"))
}

fn tr_lang<'a>(lang: pal_core::Language, hans: &'a str, hant: &'a str, en: &'a str) -> &'a str {
    match lang {
        pal_core::Language::ZhHans => hans,
        pal_core::Language::ZhHant => hant,
        pal_core::Language::En => en,
    }
}

/// Compute an event sprite's exact logical screen bounding box `(dx, dy, fw, fh)` inside the
/// `320x200` camera viewport using the exact same anchor math as `pal_render::world` (#234).
pub fn event_screen_bbox(rt: &PalRuntime, ev: &Event, cam_x: i32, cam_y: i32) -> (i32, i32, i32, i32) {
    let sx = ev.x - cam_x;
    let sy = ev.y - cam_y;
    let frame_idx = if ev.sprite_frames > 0 {
        (ev.direction.max(0) as usize) * (ev.sprite_frames as usize)
            + (ev.current_frame.max(0) as usize)
    } else {
        ev.current_frame.max(0) as usize
    };
    let (fw, fh) = rt
        .game
        .as_ref()
        .and_then(|g| g.sprite_frame_dims.get(&ev.sprite_id))
        .filter(|dims| !dims.is_empty())
        .map(|dims| dims[frame_idx.min(dims.len() - 1)])
        .unwrap_or((24, 40));
    let anchor_pad = if ev.layer < 0 { 5 } else { 6 };
    let dx = sx - fw / 2;
    let dy = sy - (fh - anchor_pad);
    (dx, dy, fw, fh)
}

/// Shared visibility predicate (`bbox ∩ viewport`) used consistently by `events[]`, fair-mode
/// `goto`/`talk` gating, and viewport minimap `E` markers (#234).
pub fn event_visible_in_viewport(rt: &PalRuntime, ev: &Event, cam_x: i32, cam_y: i32) -> bool {
    if ev.sprite_id == 0 || ev.state <= 0 || ev.vanish_time > 0 {
        return false;
    }
    let (dx, dy, fw, fh) = event_screen_bbox(rt, ev, cam_x, cam_y);
    dx < 320 && dx + fw > 0 && dy < 200 && dy + fh > 0
}

/// Returns `true` if `ev` is an on-screen unconditional exit (`0x0059` `scene_change`) or
/// initially-active pure stair-walk trigger (`0x006E` `party_walk_step`) safe to surface in
/// `--obs fair` without leaking secret passages, conditional/story-gated portals, traps, or
/// destination scene names (#234, #277).
pub fn is_fair_visible_exit(rt: &PalRuntime, ev: &Event, cam_x: i32, cam_y: i32) -> bool {
    let Some(g) = rt.game.as_ref() else {
        return false;
    };
    if ev.state <= 0 || ev.vanish_time > 0 || ev.trigger_mode < 4 || ev.trigger.is_none() {
        return false;
    }
    let sx = ev.x - cam_x;
    let sy = ev.y - cam_y;
    if !(0..320).contains(&sx) || !(0..200).contains(&sy) {
        return false;
    }
    let (hx, hy, hh) = world_to_half_tile(ev.x, ev.y);
    let tile_occluded = g.base_grid().is_occluded(hx, hy, hh);

    // 1. Unconditional `0x0059` (`scene_change`) doorway/exit: abort if any branch/label-jump,
    // dialogue, battle, or state mutation occurs before `0x0059`. Runtime `ev.state > 0` already
    // keeps locked/unrevealed doors hidden until unlocked (fixing e.g. Yuhang Inn bedroom door
    // ev#4 whose static state is 0 before Aunt Li's intro cutscene unlocks it, #277).
    let Some(block) = g.prog.block(ev.trigger.label) else {
        return false;
    };
    let mut is_uncond_scene_change = false;
    for op in block.iter().take(16) {
        if op.op == 0x0059 {
            is_uncond_scene_change = true;
            break;
        }
        if op.op == 0x0007
            || op.op == 0x0049
            || op.text > 0
            || op.lbl.iter().any(|&l| l != 0)
        {
            break;
        }
    }
    if is_uncond_scene_change {
        return !tile_occluded;
    }

    // 2. Initially-active (`def.state > 0`) pure stair-walk triggers (`0x006E` `party_walk_step`,
    // optionally ending with an unconditional `0x0003` `jump` into a tail walk block, #277).
    // Indoor stairs (e.g. Yuhang Inn 1F <-> 2F ev#47/ev#48) often sit directly under an upper-floor
    // railing/arch (`tile_occluded`), so allow surfacing when either the trigger tile or an
    // immediate half-tile neighbor landing is unoccluded.
    if !g.data.events.get(&ev.id).is_some_and(|def| def.state > 0) {
        return false;
    }
    if tile_occluded {
        let has_visible_landing = (0..4u8).any(|dir| {
            let (nx, ny, nh) = step_half_tile(hx, hy, hh, dir);
            (0..64).contains(&nx)
                && (0..128).contains(&ny)
                && !g.base_grid().is_occluded(nx, ny, nh)
                && !g.base_grid().is_void(nx, ny, nh)
        });
        if !has_visible_landing {
            return false;
        }
    }
    is_pure_stair_walk_script(&g.prog, ev.trigger.label)
}

/// Returns `true` iff `start_label` executes ONLY pure stair-walk opcodes (`party_set_dir_frame`,
/// `party_walk_step`, `wait_ticks`, `video_redraw`, `end`, and unconditional `jump` with counter==0),
/// with at least one `party_walk_step` (`0x006E`) and zero dialogue/battle/branch/state mutations.
fn is_pure_stair_walk_script(prog: &pal_core::script::Program, start_label: u32) -> bool {
    let mut cur_label = start_label;
    let mut seen_walk = false;
    let mut visited = [0u32; 4];
    let mut hops = 0usize;
    loop {
        if cur_label == 0 || hops >= visited.len() || visited[..hops].contains(&cur_label) {
            return false;
        }
        visited[hops] = cur_label;
        hops += 1;
        let Some(block) = prog.block(cur_label) else {
            return false;
        };
        let mut jumped_to = 0u32;
        for op in block.iter().take(32) {
            if op.text > 0 {
                return false;
            }
            match op.op {
                0x0000 => return seen_walk, // end
                0x006E => seen_walk = true, // party_walk_step
                0x0015 | 0x0009 | 0x0005 => {} // party_set_dir_frame, wait_ticks, video_redraw
                0x0003 => {
                    // Unconditional jump only (`a[1] == 0`, non-zero target label)
                    if op.a[1] != 0 || op.lbl[0] == 0 {
                        return false;
                    }
                    jumped_to = op.lbl[0];
                    break;
                }
                _ => return false,
            }
        }
        if jumped_to != 0 {
            cur_label = jumped_to;
        } else {
            return seen_walk;
        }
    }
}

/// Purely visual character-vs-prop classification in `--obs fair` based on directional sprite
/// metadata and frame proportions (zero script inspection, #234).
fn is_visual_character_sprite(rt: &PalRuntime, ev: &Event) -> bool {
    if ev.sprite_frames > 0 {
        return true;
    }
    if let Some(dims) = rt
        .game
        .as_ref()
        .and_then(|g| g.sprite_frame_dims.get(&ev.sprite_id))
    {
        if dims.len() >= 8 {
            if let Some(&(fw, fh)) = dims.first() {
                // Human-proportioned multi-frame sprites (excludes wide ferries, short chests, 2-frame doors).
                return fw <= 44 && fh >= 36;
            }
        }
    }
    false
}

/// Localized display label for a visual-only `visual_kind` tag (#278).
pub fn visual_kind_label(lang: pal_core::Language, vk: &str) -> &'static str {
    match vk {
        "door" => tr_lang(lang, "物件·门", "物件·門", "Object·Door"),
        "chest" => tr_lang(lang, "物件·宝箱", "物件·寶箱", "Object·Chest"),
        "person" => tr_lang(lang, "NPC", "NPC", "NPC"),
        "animal" => tr_lang(lang, "动物", "動物", "Animal"),
        "container" => tr_lang(lang, "物件·容器", "物件·容器", "Object·Container"),
        "decor" => tr_lang(lang, "物件·摆设", "物件·擺設", "Object·Decor"),
        "obstacle" => tr_lang(lang, "物件·障碍", "物件·障礙", "Object·Obstacle"),
        "mechanism" => tr_lang(lang, "物件·机关", "物件·機關", "Object·Mechanism"),
        "sign" => tr_lang(lang, "物件·牌匾", "物件·牌匾", "Object·Sign"),
        "boat" => tr_lang(lang, "物件·船只", "物件·船隻", "Object·Boat"),
        "effect" => tr_lang(lang, "物件·特效", "物件·特效", "Object·Effect"),
        _ => tr_lang(lang, "物件", "物件", "Object"),
    }
}

/// Optional visual-only `(visual_kind, localized_visual_desc)` for an event's sprite (#278).
/// Strictly visual appearance only — zero script/dialogue identity leakage.
pub fn event_visual_info(rt: &PalRuntime, ev: &Event) -> Option<(String, String)> {
    if ev.sprite_id == 0 {
        return None;
    }
    let sv = rt.game.as_ref()?.data.sprite_visuals.get(&ev.sprite_id)?;
    let desc = match rt.ui.lang {
        pal_core::Language::ZhHans => {
            if !sv.visual.is_empty() {
                sv.visual.as_str()
            } else if !sv.visual_hant.is_empty() {
                sv.visual_hant.as_str()
            } else {
                sv.visual_en.as_str()
            }
        }
        pal_core::Language::ZhHant => {
            if !sv.visual_hant.is_empty() {
                sv.visual_hant.as_str()
            } else if !sv.visual.is_empty() {
                sv.visual.as_str()
            } else {
                sv.visual_en.as_str()
            }
        }
        pal_core::Language::En => {
            if !sv.visual_en.is_empty() {
                sv.visual_en.as_str()
            } else if !sv.visual.is_empty() {
                sv.visual.as_str()
            } else {
                sv.visual_hant.as_str()
            }
        }
    };
    Some((sv.visual_kind.clone(), desc.to_string()))
}

/// Describe an event as `(stable_kind_enum, localized_kind_label, hint)`.
/// In `fair` mode, script-inspection spoilers (`→ Scene`, `Battle #N`, `Shop #N`) are never leaked (#224, #225, #234).
pub fn describe_event(rt: &PalRuntime, ev: &Event, fair: bool) -> (&'static str, &'static str, String) {
    let lang = rt.ui.lang;
    if fair {
        let (cam_x, cam_y) = runtime_camera(rt);
        if ev.sprite_id == 0 && is_fair_visible_exit(rt, ev, cam_x, cam_y) {
            return (
                "exit",
                tr_lang(lang, "出口/通道", "出口/通道", "Exit"),
                String::new(),
            );
        }
        if let Some((vkind, vdesc)) = event_visual_info(rt, ev) {
            let kind = if vkind == "person"
                || (vkind.is_empty() && is_visual_character_sprite(rt, ev))
            {
                "npc"
            } else {
                "object"
            };
            let label = if !vkind.is_empty() {
                visual_kind_label(lang, &vkind)
            } else if kind == "npc" {
                tr_lang(lang, "NPC", "NPC", "NPC")
            } else {
                tr_lang(lang, "物件", "物件", "Object")
            };
            let hint = if !vdesc.is_empty() {
                vdesc
            } else {
                format!("sprite={}", ev.sprite_id)
            };
            return (kind, label, hint);
        }
        let kind = if is_visual_character_sprite(rt, ev) {
            "npc"
        } else {
            "object"
        };
        let label = match kind {
            "npc" => tr_lang(lang, "NPC", "NPC", "NPC"),
            _ => tr_lang(lang, "物件", "物件", "Object"),
        };
        return (kind, label, format!("sprite={}", ev.sprite_id));
    }

    let mut is_portal = false;
    let mut hint = String::new();
    let mut fallback_protag: Option<String> = None;
    if let Some(g) = rt.game.as_ref() {
        if let Some(block) = g.prog.block(ev.trigger.label) {
            for op in block.iter().take(24) {
                if op.op == 0x0059 {
                    is_portal = true;
                    let target_sc = op.a[0] as u16;
                    hint = format!("→ {}", scene_name(rt, target_sc));
                    break;
                }
                if op.op == 0x0053 {
                    hint = format!(
                        "{} #{}",
                        tr_lang(lang, "商店", "商店", "Shop"),
                        op.a[0]
                    );
                    break;
                }
                if op.op == 0x0007 {
                    hint = format!(
                        "{} #{}",
                        tr_lang(lang, "战斗", "戰鬥", "Battle"),
                        op.a[0]
                    );
                    break;
                }
                if op.text > 0 {
                    if let Some(say) = g.prog.says.get(op.text as usize) {
                        if !say.speaker.is_empty() {
                            if matches!(say.speaker.as_str(), "李逍遥" | "李逍遙" | "Li Xiaoyao")
                            {
                                if fallback_protag.is_none() {
                                    fallback_protag = Some(say.speaker.clone());
                                }
                            } else {
                                hint = format!("「{}」", say.speaker);
                                break;
                            }
                        }
                    }
                }
            }
        }
        if hint.is_empty() {
            if let Some(sp) = fallback_protag {
                hint = format!("「{sp}」");
            } else {
                let lbl = g.prog.label_name(ev.trigger.label);
                if !lbl.is_empty() {
                    hint = format!("sprite={} ({lbl})", ev.sprite_id);
                }
            }
        }
    }
    let kind = if ev.trigger_mode >= 4 {
        if is_portal {
            "portal"
        } else {
            "trigger"
        }
    } else if ev.trigger_mode >= 1 {
        "npc"
    } else {
        "object"
    };
    let label = match kind {
        "portal" => tr_lang(lang, "出口/传送", "出口/傳送", "Portal/Exit"),
        "trigger" => tr_lang(lang, "踩踏/剧情", "踩踏/劇情", "Trigger"),
        "npc" => tr_lang(lang, "NPC/交互", "NPC/互動", "NPC/Interact"),
        _ => tr_lang(lang, "物件/动态", "物件/動態", "Object"),
    };
    if hint.is_empty() {
        hint = format!("sprite={}", ev.sprite_id);
    }
    (kind, label, hint)
}
