"""Export original DOS `.rpg` saves (`0.rpg`..`50.rpg`) into `site/assets/orig_saves/` with rich metadata."""

from __future__ import annotations
import shutil
import struct
from pathlib import Path
from typing import Any
from .decompiler import SCENE_NAMES_HANS, dump_yaml_with_header

ROLE_NAMES = ["李逍遥", "赵灵儿", "林月如", "巫后", "阿奴", "盖罗娇"]


def export_orig_saves(raw_dir: Path, site_assets_dir: Path) -> int:
    saves_src = raw_dir / "SAVES"
    if not saves_src.is_dir():
        return 0
    out_dir = site_assets_dir / "orig_saves"
    out_dir.mkdir(parents=True, exist_ok=True)

    entries: list[dict[str, Any]] = []
    rpg_files = sorted(
        saves_src.glob("*.rpg"),
        key=lambda p: int(p.stem) if p.stem.isdigit() else 999,
    )

    for rf in rpg_files:
        data = rf.read_bytes()
        if len(data) < 180000:
            continue
        shutil.copy2(rf, out_dir / rf.name)

        saved_times, vpx, vpy, n_party, scene_id = struct.unpack("<5H", data[:10])
        cash = struct.unpack("<I", data[40:44])[0]
        party_cnt = min(5, n_party + 1)

        # Read levels for each role from PlayerRoles row 6 (offset 508 + 6*12 = 580)
        levels = struct.unpack("<6H", data[508 + 6 * 12 : 508 + 7 * 12])
        party_members = []
        for i in range(party_cnt):
            role_id = struct.unpack("<H", data[44 + i * 10 : 46 + i * 10])[0]
            if 0 <= role_id < len(ROLE_NAMES):
                party_members.append(f"{ROLE_NAMES[role_id]} Lv.{levels[role_id]}")

        map_id = 0
        if 1 <= scene_id <= 294:
            sc_off = 3264 + (scene_id - 1) * 8
            map_id = struct.unpack("<H", data[sc_off : sc_off + 2])[0]

        sname = SCENE_NAMES_HANS.get(scene_id, f"场景 #{scene_id:03d} (Map {map_id})")
        slot_num = int(rf.stem) if rf.stem.isdigit() else 0
        entries.append(
            {
                "slot": slot_num,
                "file": f"orig_saves/{rf.name}",
                "scene_id": scene_id,
                "map_id": map_id,
                "scene_name": sname,
                "party_summary": " / ".join(party_members) if party_members else "李逍遥",
                "saved_times": saved_times,
                "cash": cash,
            }
        )

    dump_yaml_with_header(
        out_dir / "manifest.yaml",
        [
            "PalRemake 原版 DOS .rpg 经典节点存档库清单 (源自 DoSPal.zip SAVES/*.rpg)",
            "支持一键在线加载进入任意章节场景，同时支持玩家导入本地任意 .rpg 原版存盘文件",
        ],
        {"saves": entries},
    )
    return len(entries)
