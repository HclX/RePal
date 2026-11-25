"""Complete reverse-engineering & symbolic YAML decompiler for 1995 DOS Legend of Sword and Fairy.

Decompiles:
- `WORD.DAT`, `M.MSG`, `desc.dat`
- `SSS.MKF` (EventObjects, Scenes, Objects, Message Offsets, 42,292 Script Instructions)
- `DATA.MKF` (Shops, Enemies, EnemyTeams, PlayerRoles, Magic, BattleFields, LevelUp tables)

Outputs clean, richly `#`-commented standard `.yaml` files into `site/assets/data/`.
"""

from __future__ import annotations
import re
import struct
import sys
from pathlib import Path
from typing import Any

_PYLIBS = Path(__file__).resolve().parents[2] / ".pylibs"
if _PYLIBS.exists() and str(_PYLIBS) not in sys.path:
    sys.path.insert(0, str(_PYLIBS))

import yaml  # type: ignore
from .text_conv import clean_dialogue_text, decode_big5, to_simplified
from .audio import KNOWN_TRACK_NAMES


# Custom YAML string subclass so multi-line dialogues always serialize with clean `|` block style!
class LiteralStr(str):
    pass


def _literal_representer(dumper: yaml.Dumper, data: LiteralStr):
    return dumper.represent_scalar("tag:yaml.org,2002:str", str(data), style="|")


yaml.add_representer(LiteralStr, _literal_representer)


# Curated scene names synced with `site/assets/data/scenes.yaml` (#278 Proposal E)
SCENE_NAMES_HANS: dict[int, str] = {
    1: "盛渔村·客栈客房与厨房",
    2: "盛渔村·李大娘卧室",
    3: "盛渔村·逍遥客栈大堂",
    4: "盛渔村",
    5: "盛渔村·菜市场与码头",
    6: "十里坡·山道",
    7: "十里坡·山顶",
    8: "盛渔村·王小虎家与丁家",
    9: "盛渔村·民居(皇甫英家)",
    10: "盛渔村·洪大夫药铺",
    11: "十里坡·山神庙",
    12: "盛渔村·铁匠装备铺",
    13: "盛渔村·林木匠铺",
    14: "仙灵岛·灵池",
    15: "仙灵岛·外滩渡口",
    16: "仙灵岛·山道草丛",
    17: "仙灵岛·荷花池迷阵",
    18: "仙灵岛·洞天水路",
    19: "仙灵岛·水月宫外",
    20: "仙灵岛·水月宫内殿",
    21: "苏州城·南门外城",
    22: "苏州城外·码头与官道",
    23: "苏州城",
}


# Opcode definitions: opcode -> (mnemonic, jump_operand_indices)
# jump_operand_indices lists which of (0=p1, 1=p2, 2=p3) hold script addresses `0xXXXX`.
OPCODE_TABLE: dict[int, tuple[str, tuple[int, ...]]] = {
    0x0000: ("end", ()),
    0x0001: ("pause_yield", ()),  # yield execution for 1 tick, resume at next instruction
    0x0002: ("replace_and_yield", (0,)),
    0x0003: ("jump", (0,)),
    0x0004: ("call_script", (0,)),
    0x0005: ("video_redraw", ()),
    0x0006: ("branch_random", (1,)),
    0x0007: ("battle_start", (1, 2)),
    0x0008: ("replace_next_tick", ()),
    0x0009: ("wait_ticks", ()),
    0x000A: ("confirm_choice", (0,)),
    0x000B: ("npc_walk_dir_0", ()),
    0x000C: ("npc_walk_dir_1", ()),
    0x000D: ("npc_walk_dir_2", ()),
    0x000E: ("npc_walk_dir_3", ()),
    0x000F: ("npc_set_dir_frame", ()),
    0x0010: ("npc_walk_to", ()),
    0x0011: ("npc_walk_to_slow", ()),
    0x0012: ("event_set_pos_rel", ()),
    0x0013: ("event_set_pos", ()),
    0x0014: ("npc_set_frame", ()),
    0x0015: ("party_set_dir_frame", ()),
    0x0016: ("event_set_dir_frame", ()),
    0x0017: ("role_set_attr_equip", ()),
    0x0018: ("equip_item", ()),
    0x0019: ("role_add_attr", ()),
    0x001A: ("role_set_attr", ()),
    0x001B: ("party_heal_hp", ()),
    0x001C: ("party_heal_mp", ()),
    0x001D: ("party_heal_hp_mp", ()),
    0x001E: ("cash_add", (1,)),
    0x001F: ("item_add", ()),
    0x0020: ("item_remove", (2,)),
    0x0021: ("enemy_damage", ()),
    0x0022: ("role_revive", ()),
    0x0023: ("role_unequip", ()),
    0x0024: ("event_set_auto_script", (1,)),
    0x0025: ("event_set_trigger_script", (1,)),
    0x0026: ("shop_open", ()),
    0x0027: ("pawnshop_open", ()),
    0x0028: ("enemy_apply_poison", ()),
    0x0029: ("role_apply_poison", ()),
    0x002A: ("enemy_cure_poison", ()),
    0x002B: ("role_cure_poison_id", ()),
    0x002C: ("role_cure_poison_level", ()),
    0x002D: ("role_set_status", ()),
    0x002E: ("enemy_set_status", ()),
    0x002F: ("role_clear_status", ()),
    0x0030: ("role_boost_attr_pct", ()),
    0x0031: ("battle_change_sprite", ()),
    0x0033: ("collect_enemy_gourd", (0,)),
    0x0034: ("gourd_refine", (0,)),
    0x0035: ("camera_shake", ()),
    0x0036: ("rng_set", ()),
    0x0037: ("rng_play", ()),
    0x0038: ("scene_teleport_out", (0,)),
    0x0039: ("drain_hp_from_enemy", ()),
    0x003A: ("battle_flee_nonboss", (0,)),
    0x003B: ("dialogue_pos_center", ()),
    0x003C: ("dialogue_pos_upper", ()),
    0x003D: ("dialogue_pos_lower", ()),
    0x003E: ("dialogue_pos_box", ()),
    0x003F: ("ride_npc_to", ()),
    0x0040: ("event_set_trigger_mode", ()),
    0x0041: ("script_fail", ()),
    0x0042: ("simulate_magic_role", ()),
    0x0043: ("music_play", ()),
    0x0044: ("ride_npc_to_normal", ()),
    0x0045: ("battle_music_set", ()),
    0x0046: ("party_set_pos", ()),
    0x0047: ("sfx_play", ()),
    0x0049: ("event_set_state", ()),
    0x004A: ("battlefield_set", ()),
    0x004B: ("npc_vanish_short", ()),
    0x004C: ("npc_chase_player", ()),
    0x004D: ("wait_any_key", ()),
    0x004E: ("load_last_save", ()),
    0x004F: ("screen_fade_red", ()),
    0x0050: ("screen_fade_out", ()),
    0x0051: ("screen_fade_in", ()),
    0x0052: ("npc_vanish_ticks", ()),
    0x0053: ("palette_time_of_day", ()),
    0x0055: ("role_learn_magic", ()),
    0x0056: ("role_forget_magic", ()),
    0x0057: ("magic_base_dmg_by_mp", ()),
    0x0058: ("branch_if_item_less", (2,)),
    0x0059: ("scene_change", ()),
    0x005A: ("player_halve_hp", ()),
    0x005B: ("enemy_halve_hp", ()),
    0x005C: ("battle_hide_role", ()),
    0x005D: ("branch_if_not_poisoned", (1,)),
    0x005E: ("branch_if_enemy_not_poisoned", (1,)),
    0x005F: ("player_kill_instant", ()),
    0x0060: ("enemy_kill_instant", ()),
    0x0061: ("branch_if_no_poison_any", (0,)),
    0x0062: ("npc_chase_stop_ticks", ()),
    0x0063: ("npc_chase_alert_range", ()),
    0x0064: ("branch_if_enemy_hp_gt", (1,)),
    0x0065: ("role_set_sprite", ()),
    0x0066: ("throw_weapon_magic", ()),
    0x0067: ("enemy_set_magic", ()),
    0x0068: ("branch_if_enemy_turn", (0,)),
    0x0069: ("battle_enemies_escape", ()),
    0x006A: ("battle_steal", ()),
    0x006B: ("battle_blow_enemies", ()),
    0x006C: ("npc_walk_step", ()),
    0x006D: ("scene_set_scripts", (1, 2)),
    0x006E: ("party_walk_step", ()),
    0x006F: ("event_sync_state", ()),
    0x0070: ("party_walk_to", ()),
    0x0071: ("screen_wave", ()),
    0x0073: ("screen_fade_to_scene", ()),
    0x0074: ("branch_if_not_full_hp", (0,)),
    0x0075: ("party_set_roles", ()),
    0x0076: ("fbp_fade_show", ()),
    0x0077: ("music_stop", ()),
    0x0078: ("battle_end_return", ()),
    0x0079: ("branch_if_role_in_party", (1,)),
    0x007A: ("party_walk_to_fast", ()),
    0x007B: ("party_walk_to_fastest", ()),
    0x007C: ("npc_walk_to_2", ()),
    0x007D: ("event_move_pos", ()),
    0x007E: ("event_set_layer", ()),
    0x007F: ("camera_pan", ()),
    0x0080: ("palette_toggle_day_night", ()),
    0x0081: ("branch_if_not_facing_event", (2,)),
    0x0082: ("npc_walk_to_fast_2", ()),
    0x0083: ("branch_if_event_not_in_zone", (2,)),
    0x0084: ("place_item_at_player", (2,)),
    0x0085: ("delay", ()),
    0x0086: ("branch_if_not_equipped", (2,)),
    0x0087: ("npc_animate_in_place", ()),
    0x0088: ("magic_dmg_by_cash", ()),
    0x0089: ("battle_set_result", ()),
    0x008A: ("battle_set_auto_next", ()),
    0x008B: ("palette_set", ()),
    0x008C: ("color_fade", ()),
    0x008D: ("role_level_up", ()),
    0x008E: ("screen_restore", ()),
    0x008F: ("cash_halve", ()),
    0x0090: ("object_set_script", (2,)),
    0x0091: ("branch_if_enemy_not_alone", (0,)),
    0x0092: ("magic_cast_anim", ()),
    0x0093: ("screen_fade_step", ()),
    0x0094: ("branch_if_event_state_eq", (2,)),
    0x0095: ("branch_if_scene_eq", (1,)),
    0x0096: ("ending_animation", ()),
    0x0097: ("ride_npc_to_fast", ()),
    0x0098: ("party_set_follower", ()),
    0x0099: ("scene_set_map", ()),
    0x009A: ("event_set_state_range", ()),
    0x009B: ("fbp_fade_current", ()),
    0x009C: ("enemy_clone", (1,)),
    0x009E: ("enemy_summon", (2,)),
    0x009F: ("enemy_morph", ()),
    0x00A0: ("quit_game", ()),
    0x00A1: ("party_gather_at_leader", ()),
    0x00A2: ("jump_random_next", ()),
    0x00A3: ("cd_or_midi_play", ()),
    0x00A4: ("fbp_scroll", ()),
    0x00A5: ("fbp_show_with_npc", ()),
    0x00A6: ("screen_backup", ()),
    0x00A7: ("dialogue_item_desc", ()),
    0xFFFF: ("say", ()),
}


def s_label(addr: int) -> str:
    return f"s_{addr:04x}" if addr > 0 else ""


def parse_desc_dat(raw_dir: Path) -> dict[int, tuple[str, str]]:
    """Return `obj_id -> (desc_hans, desc_hant)` parsed from `desc.dat`."""
    p = raw_dir / "desc.dat"
    if not p.exists():
        return {}
    txt = decode_big5(p.read_bytes())
    out: dict[int, tuple[str, str]] = {}
    for line in txt.splitlines():
        line = line.strip()
        m = re.match(r"^([0-9a-fA-F]+)\([^)]*\)\s*=\s*(.+)$", line)
        if m:
            oid = int(m.group(1), 16)
            hant = m.group(2).replace("*", "\n").strip()
            hans = to_simplified(hant)
            out[oid] = (hans, hant)
    return out


def dump_yaml_with_header(path: Path, header_comments: list[str], data: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    header = "\n".join(f"# {line}" if line else "#" for line in header_comments) + "\n\n"
    body = yaml.dump(
        data,
        allow_unicode=True,
        sort_keys=False,
        width=10000,
        default_flow_style=False,
    )
    path.write_text(header + body, encoding="utf-8")


def decompile_all_to_yaml(raw_dir: Path, data_out_dir: Path, mods_out_dir: Path) -> dict[str, Any]:
    data_out_dir.mkdir(parents=True, exist_ok=True)
    mods_out_dir.mkdir(parents=True, exist_ok=True)

    # 1. Decode WORD.DAT (565 object/UI names)
    word_bytes = (raw_dir / "WORD.DAT").read_bytes()
    num_words = len(word_bytes) // 10
    words_hant: list[str] = []
    words_hans: list[str] = []
    for i in range(num_words):
        t = decode_big5(word_bytes[i * 10 : (i + 1) * 10]).strip()
        words_hant.append(t)
        words_hans.append(to_simplified(t))

    # 2. Decode desc.dat
    descs = parse_desc_dat(raw_dir)

    # 3. Read SSS.MKF (5 chunks)
    sss = (raw_dir / "SSS.MKF").read_bytes()
    s_offs = struct.unpack("<6I", sss[:24])
    ev_raw = sss[s_offs[0] : s_offs[1]]
    sc_raw = sss[s_offs[1] : s_offs[2]]
    obj_raw = sss[s_offs[2] : s_offs[3]]
    msg_idx_raw = sss[s_offs[3] : s_offs[4]]
    scr_raw = sss[s_offs[4] : s_offs[5]]

    # 4. Decode M.MSG (12,880 messages)
    m_msg = (raw_dir / "M.MSG").read_bytes()
    num_msgs = len(msg_idx_raw) // 4 - 1
    msgs_hant: list[str] = []
    msgs_hans: list[str] = []
    msgs_meta: list[dict] = []
    for i in range(num_msgs):
        o1, o2 = struct.unpack("<II", msg_idx_raw[i * 4 : (i + 2) * 4])
        raw_str = decode_big5(m_msg[o1:o2]) if 0 <= o1 <= o2 <= len(m_msg) else ""
        clean_t, meta = clean_dialogue_text(raw_str)
        msgs_hant.append(clean_t)
        msgs_hans.append(to_simplified(clean_t))
        msgs_meta.append(meta)

    # 5. Read DATA.MKF (15 chunks)
    dmkf = (raw_dir / "DATA.MKF").read_bytes()
    d_offs = struct.unpack("<16I", dmkf[:64])
    d_chunks = [dmkf[d_offs[i] : d_offs[i + 1]] for i in range(15)]

    # --- Export Palettes YAML (PAT.MKF) ---
    pat = (raw_dir / "PAT.MKF").read_bytes()
    pat_n = struct.unpack("<I", pat[:4])[0] // 4 - 1
    palettes_list = []
    for pi in range(pat_n):
        po1, po2 = struct.unpack("<II", pat[pi * 4 : (pi + 2) * 4])
        pb = pat[po1:po2]
        day = [
            [((pb[c * 3 + ch] & 63) << 2) | ((pb[c * 3 + ch] & 63) >> 4) for ch in range(3)]
            for c in range(256)
        ]
        has_night = len(pb) >= 1536
        palettes_list.append({"id": pi, "has_night": has_night, "day_water_cycle": day[0xF9:0xFF]})
    dump_yaml_with_header(
        data_out_dir / "palettes.yaml",
        ["PalRemake 调色盘元数据 (源自 PAT.MKF)", "包含日夜循环标识与原版水波动态色表"],
        {"palettes": palettes_list},
    )

    # --- Parse Objects (565 entries, 6 u16 = 12 bytes each in DOS PAL) ---
    num_objs = len(obj_raw) // 12
    items_yaml = []
    magic_obj_map: dict[int, dict] = {}
    enemy_obj_map: dict[int, dict] = {}

    for oid in range(num_objs):
        w0, w1, w2, w3, w4, w5 = struct.unpack("<6H", obj_raw[oid * 12 : (oid + 1) * 12])
        nhans = words_hans[oid] if oid < len(words_hans) else ""
        nhant = words_hant[oid] if oid < len(words_hant) else ""
        dhans, dhant = descs.get(oid, ("", ""))

        if 61 <= oid <= 294:
            # Item object!
            # DOS Item flags bitmask: 1=Usable, 2=Equipable, 4=Throwable, 8=Consumed, 16=ApplyToAll, 32=Sellable
            # Bits 6..11 indicate who can equip (Li Xiaoyao, Zhao Ling'er, Lin Yueru, Wu Hou, A Nu, Gai Luojiao)
            items_yaml.append(
                {
                    "id": oid,
                    "name": nhans,
                    "name_hant": nhant,
                    "icon_id": w0,  # index in BALL.MKF
                    "price": w1,
                    "script_use": s_label(w2),
                    "script_equip": s_label(w3),
                    "script_throw": s_label(w4),
                    "flags": w5,
                    "usable": bool(w5 & 1),
                    "equipable": bool(w5 & 2),
                    "throwable": bool(w5 & 4),
                    "consumable": bool(w5 & 8),
                    "target_all": bool(w5 & 16),
                    "sellable": bool(w5 & 32),
                    "desc": LiteralStr(dhans) if "\n" in dhans else dhans,
                    "desc_hant": LiteralStr(dhant) if "\n" in dhant else dhant,
                }
            )
        elif 295 <= oid <= 397:
            magic_obj_map[oid] = {
                "obj_id": oid,
                "name": nhans,
                "name_hant": nhant,
                "magic_data_idx": w0,
                "script_success": s_label(w2),
                "script_use": s_label(w3),
                "flags": w5,
                "usable_outside_battle": bool(w5 & 1),
                "usable_in_battle": bool(w5 & 2),
                "target_enemy": bool(w5 & 8),
                "target_all": bool(w5 & 16),
                "desc": LiteralStr(dhans) if "\n" in dhans else dhans,
                "desc_hant": LiteralStr(dhant) if "\n" in dhant else dhant,
            }
        elif 398 <= oid <= 551:
            enemy_obj_map[w0] = {
                "obj_id": oid,
                "enemy_data_idx": w0,
                "name": nhans,
                "name_hant": nhant,
                "resist_poison": w1,
                "script_pre_turn": s_label(w2),
                "script_post_battle": s_label(w3),
                "script_in_turn": s_label(w4),
            }

    dump_yaml_with_header(
        data_out_dir / "items.yaml",
        [
            "PalRemake 物品图鉴与属性表 (逆向自 SSS.MKF + WORD.DAT + desc.dat)",
            "可直接修改 price / desc / script_use 等字段以定制道具效果",
        ],
        {"ui_words": {"hans": words_hans[:61], "hant": words_hant[:61]}, "items": items_yaml},
    )

    # --- Parse Magic (DATA.MKF chunk 4: 104 entries * 32 bytes = 16 u16) ---
    magic_raw = d_chunks[4]
    num_magic_rows = len(magic_raw) // 32
    magic_rows: list[tuple[int, ...]] = [
        struct.unpack("<HHhhhhhHHHHHHhHH", magic_raw[i * 32 : (i + 1) * 32])
        for i in range(num_magic_rows)
    ]
    elem_names = {0: "普通", 1: "风", 2: "雷", 3: "水", 4: "火", 5: "土", 6: "毒", 7: "疗愈/辅助"}
    magics_yaml = []
    for oid, mmeta in sorted(magic_obj_map.items()):
        midx = mmeta["magic_data_idx"]
        if 0 <= midx < len(magic_rows):
            r = magic_rows[midx]
            magics_yaml.append(
                {
                    **mmeta,
                    "fire_effect_id": r[0],  # index in FIRE.MKF
                    "type": r[1],
                    "type_name": elem_names.get(r[1], "特殊"),
                    "x_offset": r[2],
                    "y_offset": r[3],
                    "summon_effect": r[4],
                    "speed": r[5],
                    "keep_effect": r[6],
                    "sfx_delay": r[7],
                    "total_frames": r[8],
                    "shake": r[9],
                    "wave": r[10],
                    "cost_mp": r[12],
                    "base_damage": r[13],
                    "elemental": r[14],
                    "sfx_id": r[15],
                }
            )
    dump_yaml_with_header(
        data_out_dir / "magic.yaml",
        [
            "PalRemake 仙术秘籍表 (逆向自 DATA.MKF#4 + SSS.MKF + desc.dat)",
            "支持直接调整 cost_mp (消耗真气) 与 base_damage (基础威力)",
        ],
        {"magics": magics_yaml},
    )

    # --- Parse PlayerRoles (DATA.MKF chunk 3: 75 rows * 6 u16 column-major) ---
    roles_raw = d_chunks[3]
    role_matrix = [struct.unpack("<6H", roles_raw[r * 12 : (r + 1) * 12]) for r in range(75)]
    roles_yaml = []
    for col in range(6):
        name_wid = role_matrix[3][col]
        initial_spells = [
            role_matrix[r][col] for r in range(32, 64) if role_matrix[r][col] > 0
        ]
        roles_yaml.append(
            {
                "role_id": col,
                "name": words_hans[name_wid] if name_wid < len(words_hans) else f"角色{col}",
                "name_hant": words_hant[name_wid] if name_wid < len(words_hant) else f"角色{col}",
                "avatar_rgm": role_matrix[0][col],
                "battle_sprite_f": role_matrix[1][col],
                "walk_sprite_mgo": role_matrix[2][col],
                "level": role_matrix[6][col],
                "max_hp": role_matrix[7][col],
                "max_mp": role_matrix[8][col],
                "hp": role_matrix[9][col],
                "mp": role_matrix[10][col],
                "equipment": {
                    "hat": role_matrix[11][col],
                    "cloak": role_matrix[12][col],
                    "armor": role_matrix[13][col],
                    "weapon": role_matrix[14][col],
                    "boots": role_matrix[15][col],
                    "ornament": role_matrix[16][col],
                },
                "stats": {
                    "attack": role_matrix[17][col],
                    "magic_power": role_matrix[18][col],
                    "defense": role_matrix[19][col],
                    "dexterity": role_matrix[20][col],
                    "flee": role_matrix[21][col],
                },
                "initial_magics": initial_spells,
                "coop_magic": role_matrix[65][col],
                "sfx": {
                    "death": role_matrix[70][col],
                    "attack": role_matrix[71][col],
                    "weapon": role_matrix[72][col],
                    "critical": role_matrix[73][col],
                    "magic": role_matrix[74][col],
                },
            }
        )

    # Level-up magic table (DATA.MKF chunk 6: 40 entries * 5 roles * 2 u16? 400 bytes = 20 entries * 5 roles * 4 bytes)
    lum_raw = d_chunks[6]
    levelup_magic = []
    for i in range(len(lum_raw) // 20):
        vals = struct.unpack("<10H", lum_raw[i * 20 : (i + 1) * 20])
        entry = {}
        role_keys = ["li_xiaoyao", "zhao_linger", "lin_yueru", "wu_hou", "a_nu"]
        for rk_i, rk in enumerate(role_keys):
            lv, mg = vals[rk_i * 2], vals[rk_i * 2 + 1]
            if lv > 0 and mg > 0:
                entry[rk] = {
                    "level": lv,
                    "magic_id": mg,
                    "magic_name": words_hans[mg] if mg < len(words_hans) else "",
                }
        if entry:
            levelup_magic.append(entry)

    exp_raw = d_chunks[14]
    levelup_exp = list(struct.unpack(f"<{len(exp_raw)//2}H", exp_raw))

    dump_yaml_with_header(
        data_out_dir / "roles.yaml",
        [
            "PalRemake 六大主角初始属性、装备与升级习得仙术表 (逆向自 DATA.MKF#3/#6/#14)",
            "角色顺序: 0=李逍遥, 1=赵灵儿, 2=林月如, 3=巫后, 4=阿奴, 5=盖罗娇",
        ],
        {"roles": roles_yaml, "levelup_magic": levelup_magic, "levelup_exp": levelup_exp},
    )

    # --- Parse Shops (DATA.MKF chunk 0: 21 shops * 9 u16 = 378 bytes) ---
    shop_raw = d_chunks[0]
    shops_yaml = []
    for sid in range(len(shop_raw) // 18):
        s_items = [x for x in struct.unpack("<9H", shop_raw[sid * 18 : (sid + 1) * 18]) if x > 0]
        shops_yaml.append(
            {
                "shop_id": sid,
                "items": [
                    {"item_id": iid, "name": words_hans[iid] if iid < len(words_hans) else ""}
                    for iid in s_items
                ],
            }
        )
    dump_yaml_with_header(
        data_out_dir / "shops.yaml",
        ["PalRemake 店铺商品清单 (逆向自 DATA.MKF#0)"],
        {"shops": shops_yaml},
    )

    # --- Parse Enemies & EnemyTeams (DATA.MKF chunk 1 & 2) ---
    en_raw = d_chunks[1]
    enemies_yaml = []
    for eid in range(len(en_raw) // 70):
        u = struct.unpack("<35H", en_raw[eid * 70 : (eid + 1) * 70])
        emeta = enemy_obj_map.get(eid, {"obj_id": 0, "name": f"妖怪#{eid}", "name_hant": f"妖怪#{eid}"})
        enemies_yaml.append(
            {
                "enemy_id": eid,
                "abc_sprite_id": eid,  # matches ABC.MKF chunk index!
                "name": emeta.get("name", ""),
                "name_hant": emeta.get("name_hant", ""),
                "idle_frames": u[0],
                "magic_frames": u[1],
                "attack_frames": u[2],
                "idle_anim_speed": u[3],
                "act_wait_frames": u[4],
                "y_offset": struct.unpack("<h", struct.pack("<H", u[5]))[0],
                "sfx_attack": struct.unpack("<h", struct.pack("<H", u[6]))[0],
                "sfx_action": struct.unpack("<h", struct.pack("<H", u[7]))[0],
                "sfx_magic": struct.unpack("<h", struct.pack("<H", u[8]))[0],
                "sfx_death": struct.unpack("<h", struct.pack("<H", u[9]))[0],
                "sfx_call": struct.unpack("<h", struct.pack("<H", u[10]))[0],
                "hp": u[11],
                "exp": u[12],
                "cash": u[13],
                "level": u[14],
                "magic_id": u[15],
                "magic_rate": u[16],
                "attack_equiv_item": u[17],
                "attack_equiv_rate": u[18],
                "steal_item": u[19],
                "steal_count": u[20],
                "attack": u[21],
                "magic_power": u[22],
                "defense": u[23],
                "dexterity": u[24],
                "flee": u[25],
                "resist_poison": u[26],
                "resist_elem": list(u[27:32]),  # wind, thunder, water, fire, earth
                "resist_phys": u[32],
                "dual_move": u[33],
                "collect_value": u[34],
                "script_pre_turn": emeta.get("script_pre_turn", ""),
                "script_post_battle": emeta.get("script_post_battle", ""),
                "script_in_turn": emeta.get("script_in_turn", ""),
            }
        )

    team_raw = d_chunks[2]
    teams_yaml = []
    for tid in range(len(team_raw) // 10):
        slots = struct.unpack("<5h", team_raw[tid * 10 : (tid + 1) * 10])
        # Map object IDs (`398..551`) or enemy indices cleanly:
        # Note: in DOS PAL EnemyTeam, each slot >= 0 (except 0xFFFF = -1 empty) is an Object ID (`398..551`)!
        members = []
        for s in slots:
            if s > 0:
                # Look up object table w0 -> enemy_data_idx
                if s < num_objs:
                    ew0 = struct.unpack("<H", obj_raw[s * 12 : s * 12 + 2])[0]
                    ename = words_hans[s] if s < len(words_hans) else ""
                    members.append({"obj_id": s, "enemy_id": ew0, "name": ename})
        teams_yaml.append({"team_id": tid, "slots_raw": list(slots), "members": members})

    bf_raw = d_chunks[5]
    battlefields = []
    for bid in range(len(bf_raw) // 12):
        vals = struct.unpack("<H5h", bf_raw[bid * 12 : (bid + 1) * 12])
        battlefields.append(
            {"id": bid, "fbp_id": bid, "screen_wave": vals[0], "elem_effect": list(vals[1:])}
        )

    dump_yaml_with_header(
        data_out_dir / "enemies.yaml",
        ["PalRemake 怪物图鉴、敌方编队与战场环境表 (逆向自 DATA.MKF#1/#2/#5)"],
        {"enemies": enemies_yaml, "teams": teams_yaml, "battlefields": battlefields},
    )

    # --- Parse Scenes (294 scenes, 1-based in PAL!) & EventObjects (5,332 events, 1-based!) ---
    num_scenes = len(sc_raw) // 8
    num_events = len(ev_raw) // 32

    events_list = []
    for idx0 in range(num_events):
        f = struct.unpack("<hHHh12H", ev_raw[idx0 * 32 : (idx0 + 1) * 32])
        events_list.append(
            {
                "id": idx0 + 1,  # 1-based PAL EventObject ID!
                "vanish_time": f[0],
                "x": f[1],
                "y": f[2],
                "layer": f[3],
                "script_trigger": s_label(f[4]),
                "script_auto": s_label(f[5]),
                "state": f[6],
                "trigger_mode": f[7],
                "sprite_id": f[8],  # index in MGO.MKF
                "sprite_frames": f[9],
                "direction": f[10],
                "current_frame": f[11],
                "script_idle_frame": f[12],
                "sprite_frames_auto": f[14],
            }
        )

    scenes_yaml = []
    entry_annotations: dict[int, list[str]] = {}

    def add_note(addr: int, note: str) -> None:
        if addr > 0:
            entry_annotations.setdefault(addr, [])
            if note not in entry_annotations[addr]:
                entry_annotations[addr].append(note)

    for s0 in range(num_scenes):
        sid = s0 + 1  # 1-based PAL Scene ID!
        map_id, scr_enter, scr_tele, ev_start = struct.unpack("<4H", sc_raw[s0 * 8 : (s0 + 1) * 8])
        ev_end = (
            struct.unpack("<H", sc_raw[(s0 + 1) * 8 + 6 : (s0 + 1) * 8 + 8])[0]
            if s0 + 1 < num_scenes
            else num_events
        )
        sname = SCENE_NAMES_HANS.get(sid, f"场景 #{sid:03d} (Map {map_id})")
        scenes_yaml.append(
            {
                "id": sid,
                "name": sname,
                "map_id": map_id,
                "script_enter": s_label(scr_enter),
                "script_teleport": s_label(scr_tele),
                "event_start": ev_start + 1,  # 1-based inclusive
                "event_end": ev_end,  # 1-based inclusive
            }
        )
        add_note(scr_enter, f"[场景 {sid:03d} {sname}] 进入脚本")
        add_note(scr_tele, f"[场景 {sid:03d} {sname}] 土灵珠/传送脚本")
        for ev_idx0 in range(ev_start, min(ev_end, num_events)):
            ev = events_list[ev_idx0]
            if ev["script_trigger"]:
                t_addr = int(ev["script_trigger"][2:], 16)
                add_note(
                    t_addr,
                    f"[场景 {sid:03d} {sname}] 事件#{ev['id']} (MGO:{ev['sprite_id']}) 互动触发",
                )
            if ev["script_auto"]:
                a_addr = int(ev["script_auto"][2:], 16)
                add_note(
                    a_addr,
                    f"[场景 {sid:03d} {sname}] 事件#{ev['id']} (MGO:{ev['sprite_id']}) 自动脚本",
                )

    dump_yaml_with_header(
        data_out_dir / "scenes.yaml",
        [
            "PalRemake 全 294 个场景索引表 (逆向自 SSS.MKF#1)",
            "1-based 场景编号，关联地图编号、进入触发脚本与场景事件区间",
        ],
        {"scenes": scenes_yaml},
    )

    dump_yaml_with_header(
        data_out_dir / "events.yaml",
        [
            "PalRemake 全 5,332 个场景事件对象表 (逆向自 SSS.MKF#0)",
            "每个事件对象代表地图上的 NPC、宝箱、机关、传送门或隐藏触发器",
        ],
        {"events": events_list},
    )

    # --- Symbolic Basic-Block Decompilation of all 42,292 Script Instructions ---
    num_ins = len(scr_raw) // 8
    raw_ins: list[tuple[int, int, int, int]] = [
        struct.unpack("<4H", scr_raw[i * 8 : (i + 1) * 8]) for i in range(num_ins)
    ]

    leaders: set[int] = {0, 1}
    leaders.update(entry_annotations.keys())
    # Item/Magic/Enemy scripts
    for oid in range(num_objs):
        _, _, w2, w3, w4, _ = struct.unpack("<6H", obj_raw[oid * 12 : (oid + 1) * 12])
        for addr in (w2, w3, w4):
            if 0 < addr < num_ins:
                leaders.add(addr)
                add_note(addr, f"[物件 #{oid} {words_hans[oid]}] 脚本")

    # Scan all instructions for jump/script operands & post-terminator boundaries
    for addr, (op, p1, p2, p3) in enumerate(raw_ins):
        _, jmp_ops = OPCODE_TABLE.get(op, (f"op_{op:04x}", ()))
        operands = (p1, p2, p3)
        for j_idx in jmp_ops:
            target = operands[j_idx]
            if 0 < target < num_ins:
                leaders.add(target)
        if op in (0x0000, 0x0003, 0x0041, 0x00A0) and addr + 1 < num_ins:
            leaders.add(addr + 1)
        if op == 0x00A2 and p1 > 0:
            # jump_random_next jumps to addr + 1 .. addr + p1
            for k in range(1, p1 + 1):
                if addr + k < num_ins:
                    leaders.add(addr + k)

    sorted_leaders = sorted(x for x in leaders if 0 <= x < num_ins)

    # Partition into 11 chapter script YAML files (~0x1000 instructions per file)
    chunk_size = 0x1000
    num_files = (num_ins + chunk_size - 1) // chunk_size
    script_files_index: list[dict] = []
    scripts_dir = data_out_dir / "scripts"
    scripts_dir.mkdir(parents=True, exist_ok=True)

    chapter_titles = [
        "公共事件与道具底层脚本 (0x0000-0x0FFF)",
        "第一章：盛渔村客栈·市集·十里坡 (0x1000-0x1FFF)",
        "第二章：罗刹洞梦境·仙灵岛迷宫·水月宫 (0x2000-0x2FFF)",
        "第三章：苏州城·比武招亲·林家堡 (0x3000-0x3FFF)",
        "第四章：隐龙窟·白河村·玉佛寺·黑水镇 (0x4000-0x4FFF)",
        "第五章：将军冢·鬼阴山·扬州城 (0x5000-0x5FFF)",
        "第六章：蛤蟆山·京城尚书府·彩依蝶恋 (0x6000-0x6FFF)",
        "第七章：蜀山仙剑派·锁妖塔上层 (0x7000-0x7FFF)",
        "第八章：锁妖塔底层·神木林·圣姑居 (0x8000-0x8FFF)",
        "第九章：大理白苗族·试炼窟·女娲神殿 (0x9000-0x9FFF)",
        "终章：南绍王宫·拜月决战·仙术物件脚本 (0xA000-0xA533)",
    ]

    def format_step(addr: int, op: int, p1: int, p2: int, p3: int) -> dict[str, Any]:
        mnem, jmp_ops = OPCODE_TABLE.get(op, (f"op_{op:04x}", ()))
        step: dict[str, Any] = {"op": mnem}

        # Friendly structured fields for the top high-frequency narrative opcodes!
        if op == 0x0000:
            return step
        elif op == 0x0003:
            step["target"] = s_label(p1)
            if p2:
                step["count"] = p2
                if addr + 1 < num_ins:
                    step["else_jump"] = s_label(addr + 1)
            return step
        elif op == 0x0002:
            step["target"] = s_label(p1)
            if p2:
                step["count"] = p2
            return step
        elif op == 0x0004:
            step["target"] = s_label(p1)
            if p2:
                step["event_id"] = p2
            return step
        elif op == 0x0043:
            step["track"] = p1
            step["loop"] = p2 == 0
            if p1 in KNOWN_TRACK_NAMES:
                step["note"] = KNOWN_TRACK_NAMES[p1]
            return step
        elif op == 0x0047:
            step["sfx"] = p1
            return step
        elif op == 0x003B:
            step["side"] = "center"
            if p1:
                step["font_color"] = p1
            return step
        elif op == 0x003C:
            step["side"] = "left"
            step["portrait_id"] = p1
            if p2:
                step["font_color"] = p2
            return step
        elif op == 0x003D:
            step["side"] = "right"
            step["portrait_id"] = p1
            if p2:
                step["font_color"] = p2
            return step
        elif op == 0x001F:
            step["item_id"] = p1
            step["count"] = p2 if p2 > 0 else 1
            if p1 < len(words_hans) and words_hans[p1]:
                step["item_name"] = words_hans[p1]
            return step
        elif op == 0x0020:
            step["item_id"] = p1
            step["count"] = p2 if p2 > 0 else 1
            step["else_jump"] = s_label(p3)
            if p1 < len(words_hans) and words_hans[p1]:
                step["item_name"] = words_hans[p1]
            return step
        elif op == 0x0058:
            step["item_id"] = p1
            step["count"] = p2
            step["jump_if_less"] = s_label(p3)
            if p1 < len(words_hans) and words_hans[p1]:
                step["item_name"] = words_hans[p1]
            return step
        elif op == 0x001E:
            # p1 is signed int16 delta cash!
            signed_cash = struct.unpack("<h", struct.pack("<H", p1))[0]
            step["amount"] = signed_cash
            if p2:
                step["else_jump"] = s_label(p2)
            return step
        elif op == 0x0007:
            step["team_id"] = p1
            if p2:
                step["on_defeat"] = s_label(p2)
            if p3:
                step["on_flee"] = s_label(p3)
            return step
        elif op == 0x0059:
            step["scene_id"] = p1
            if p1 in SCENE_NAMES_HANS:
                step["scene_name"] = SCENE_NAMES_HANS[p1]
            return step
        elif op == 0x0046:
            step["x"] = p1
            step["y"] = p2
            return step
        elif op == 0x0049:
            step["event_id"] = -1 if p1 == 0xFFFF else p1
            step["state"] = p2
            return step
        elif op in (0x0024, 0x0025):
            step["event_id"] = -1 if p1 == 0xFFFF else p1
            step["target"] = s_label(p2)
            return step
        elif op == 0x000A:
            step["on_no"] = s_label(p1)
            return step

        # Generic compact representation with symbolic jump labels where applicable
        args_list: list[Any] = []
        raw_ops = [p1, p2, p3]
        for idx_o, val in enumerate(raw_ops):
            if idx_o in jmp_ops:
                args_list.append(s_label(val))
            else:
                args_list.append(val)
        while args_list and (args_list[-1] == 0 or args_list[-1] == ""):
            args_list.pop()
        if args_list:
            step["args"] = args_list
        return step

    for fi in range(num_files):
        lo_addr = fi * chunk_size
        hi_addr = min(num_ins, (fi + 1) * chunk_size)
        fname = f"scripts_{fi:02d}.yaml"
        blocks_dict: dict[str, Any] = {}
        block_comments: dict[str, list[str]] = {}

        for li_pos, b_start in enumerate(sorted_leaders):
            if b_start < lo_addr or b_start >= hi_addr:
                continue
            b_end = sorted_leaders[li_pos + 1] if li_pos + 1 < len(sorted_leaders) else num_ins
            lbl = s_label(b_start)
            if b_start in entry_annotations:
                block_comments[lbl] = entry_annotations[b_start]

            steps: list[dict[str, Any]] = []
            pc = b_start
            while pc < b_end:
                op, p1, p2, p3 = raw_ins[pc]
                if op == 0xFFFF:
                    # Coalesce consecutive 0xFFFF dialogue lines within the same basic block!
                    hans_lines: list[str] = []
                    hant_lines: list[str] = []
                    msg_ids: list[int] = []
                    speaker_hans = ""
                    speaker_hant = ""
                    while pc < b_end and raw_ins[pc][0] == 0xFFFF:
                        mid = raw_ins[pc][1]
                        msg_ids.append(mid)
                        lhans = msgs_hans[mid] if mid < len(msgs_hans) else ""
                        lhant = msgs_hant[mid] if mid < len(msgs_hant) else ""
                        if not hans_lines and not speaker_hans and lhans.endswith("：") and len(lhans) <= 10:
                            speaker_hans = lhans[:-1]
                            speaker_hant = lhant[:-1] if lhant.endswith("：") else lhant
                        else:
                            hans_lines.append(lhans)
                            hant_lines.append(lhant)
                        pc += 1
                    joined_hans = "\n".join(hans_lines).strip()
                    joined_hant = "\n".join(hant_lines).strip()
                    say_step: dict[str, Any] = {"op": "say"}
                    if speaker_hans:
                        say_step["speaker"] = speaker_hans
                        say_step["speaker_hant"] = speaker_hant
                    say_step["text"] = (
                        LiteralStr(joined_hans) if "\n" in joined_hans else joined_hans
                    )
                    say_step["text_hant"] = (
                        LiteralStr(joined_hant) if "\n" in joined_hant else joined_hant
                    )
                    say_step["msg_ids"] = msg_ids
                    steps.append(say_step)
                else:
                    steps.append(format_step(pc, op, p1, p2, p3))
                    pc += 1

            # Explicit fallthrough if block didn't end with a terminal op (`end`, `jump`, `script_fail`, `quit_game`)
            if steps and steps[-1]["op"] not in ("end", "jump", "script_fail", "quit_game"):
                if b_end < num_ins:
                    steps.append({"op": "jump", "target": s_label(b_end)})
                else:
                    steps.append({"op": "end"})

            blocks_dict[lbl] = steps

        # Write YAML file with inline `# === ... ===` section comments injected cleanly before annotated blocks!
        raw_yaml = yaml.dump(
            {"chapter": fi, "title": chapter_titles[fi], "scripts": blocks_dict},
            allow_unicode=True,
            sort_keys=False,
            width=10000,
            default_flow_style=False,
        )
        # Inject `#` comments right above annotated `  s_XXXX:` keys for pleasant human reading!
        annotated_lines = [
            f"# ============================================================================",
            f"# PalRemake 剧情脚本分卷 {fi:02d}: {chapter_titles[fi]}",
            f"# 地址范围: 0x{lo_addr:04x} .. 0x{hi_addr - 1:04x} | 可直接添加 # 注释或编辑对白与指令",
            f"# ============================================================================",
            "",
        ]
        for line in raw_yaml.splitlines():
            m_lbl = re.match(r"^  (s_[0-9a-f]{4}):$", line)
            if m_lbl and m_lbl.group(1) in block_comments:
                annotated_lines.append("")
                for note in block_comments[m_lbl.group(1)]:
                    annotated_lines.append(f"  # --- {note} ---")
            annotated_lines.append(line)

        (scripts_dir / fname).write_text("\n".join(annotated_lines) + "\n", encoding="utf-8")
        script_files_index.append(
            {
                "chapter": fi,
                "title": chapter_titles[fi],
                "file": f"data/scripts/{fname}",
                "addr_min": f"0x{lo_addr:04x}",
                "addr_max": f"0x{hi_addr - 1:04x}",
                "block_count": len(blocks_dict),
            }
        )

    dump_yaml_with_header(
        data_out_dir / "scripts_index.yaml",
        [
            "PalRemake 剧本分卷索引表",
            "引擎按需异步加载对应分卷 YAML，同时支持 mods/*.yaml 覆盖任意 s_XXXX 段落",
        ],
        {"total_instructions": num_ins, "total_blocks": len(sorted_leaders), "chapters": script_files_index},
    )

    # --- Create Sample Story Mod (`site/assets/mods/`) ---
    dump_yaml_with_header(
        mods_out_dir / "manifest.yaml",
        [
            "PalRemake 剧情扩展补丁清单 (Mod Loader Manifest)",
            "将 enabled 设为 true 或在网页顶部【剧情Mod】开关中启用，刷新即可体验自定义剧情！",
        ],
        {
            "mods": [
                {
                    "id": "remake_easter_egg",
                    "name": "逍遥客栈·现代高清重制彩蛋补丁",
                    "author": "PalRemake",
                    "enabled": False,
                    "description": "演示如何只用几行带 # 注释的 YAML 修改台词或赠送额外盘缠/道具",
                    "file": "mods/sample_story_mod.yaml",
                }
            ]
        },
    )

    sample_mod_text = """# ============================================================================
# PalRemake 自定义剧情补丁示例 (sample_story_mod.yaml)
# 说明：
#   1. 这里可以随意书写 # 注释，使用 | 编写多行中文台词；
#   2. scripts_override 下填写的 s_XXXX 标签会直接无损替换原版对应段落；
#   3. 修改保存后无需任何编译步骤，刷新网页并开启【示例Mod】即可生效！
# ============================================================================

id: remake_easter_egg
name: 逍遥客栈·现代高清重制彩蛋补丁

# 示例：替换盛渔村客栈中查看木箱/桌案时的提示，或覆写任意剧情块
scripts_override:
  s_0021:
    # 原本只是普通翻箱倒柜，现在改为重制版寄语并赠送 100 文铜钱！
    - op: dialogue_pos_center
    - op: say
      speaker: 画外仙音
      text: |
        【PalRemake 彩蛋】
        恭喜你发现了藏在客栈角落的竹简！
        这段剧情直接读取自 assets/mods/sample_story_mod.yaml，
        你可以随时用编辑器修改这段文字并添加自己的支线故事。
      text_hant: |
        【PalRemake 彩蛋】
        恭喜你發現了藏在客棧角落的竹簡！
        這段劇情直接讀取自 assets/mods/sample_story_mod.yaml，
        你可以隨時用編輯器修改這段文字並添加自己的支線故事。
    - op: sfx_play
      sfx: 19
    - op: cash_add
      amount: 100
    - op: say
      text: 意外获得了「100 文钱」私房盘缠！
      text_hant: 意外獲得了「100 文錢」私房盤纏！
    - op: end
"""
    (mods_out_dir / "sample_story_mod.yaml").write_text(sample_mod_text, encoding="utf-8")

    return {
        "words": num_words,
        "messages": num_msgs,
        "scenes": num_scenes,
        "events": num_events,
        "instructions": num_ins,
        "blocks": len(sorted_leaders),
        "items": len(items_yaml),
        "magics": len(magics_yaml),
        "enemies": len(enemies_yaml),
    }
