#!/usr/bin/env python3
"""Generate tools/mgo_review/mgo_data.json from tools/mgo_catalog.yaml and site/assets/graphics/mgo/*.json."""
import hashlib
import json
from pathlib import Path
import yaml

ROOT = Path(__file__).resolve().parent.parent.parent
CATALOG_PATH = ROOT / "tools" / "mgo_catalog.yaml"
MGO_DIR = ROOT / "site" / "assets" / "graphics" / "mgo"
P1_BAK_DIR = ROOT / ".tmp" / "hd_mgo" / "p1_backup"
QC_REPORT_PATH = Path(__file__).resolve().parent / "qc_report.json"
OUT_PATH = Path(__file__).resolve().parent / "mgo_data.json"

GROUP_LABELS = {
    "zhao_linger": "赵灵儿",
    "li_xiaoyao": "李逍遥",
    "lin_yueru": "林月如",
    "a_nu": "阿奴",
    "wu_hou": "巫后",
    "wu_wang": "巫王",
    "gai_luojiao": "盖罗娇",
    "jiu_jianxian": "酒剑仙",
    "wine_sword_immortal": "酒剑仙(旧)",
    "auntie_li": "李大娘",
    "moon_leader": "拜月教主",
    "black_miao_guard": "黑苗护卫",
    "miao_elder_man": "黑苗头领",
    "miao_woman_green": "绿披肩苗女",
    "sheng_gu_green": "圣姑/绿裙女祭司",
    "miao_girl_white": "苗族小女孩",
    "woodcutter": "樵夫/工匠",
    "elder_shi_and_miao": "石长老/苗族",
    "grandma_and_saintess": "姥姥/圣姑",
}

def main() -> None:
    cat = yaml.safe_load(CATALOG_PATH.read_text())["sprites"]
    qc_map = json.loads(QC_REPORT_PATH.read_text()) if QC_REPORT_PATH.exists() else {}
    items = []
    for sp in cat:
        mid = int(sp["id"])
        meta_p = MGO_DIR / f"mgo_{mid:03d}.json"
        if not meta_p.exists():
            continue
        meta = json.loads(meta_p.read_text())
        grp = sp.get("character_group") or ""
        p4x = MGO_DIR / f"mgo_{mid:03d}_4x.webp"
        hash_4x = hashlib.sha1(p4x.read_bytes()).hexdigest()[:10] if p4x.exists() else ""
        items.append({
            "id": mid,
            "id_str": f"{mid:03d}",
            "name": sp.get("name") or f"MGO #{mid:03d}",
            "name_needs_review": bool(sp.get("name_needs_review", False)),
            "category": sp.get("category") or "",
            "group": grp,
            "group_label": GROUP_LABELS.get(grp, "其他"),
            "anchor": sp.get("anchor_mgo"),
            "status": sp.get("status") or "todo",
            "scenes": sp.get("scenes") or [],
            "w": meta["w"],
            "h": meta["h"],
            "frames": meta.get("frames") or [],
            "hdTrackB": bool(meta.get("hdTrackB", False)),
            "shadowStripped": bool(meta.get("shadowStripped", False)),
            "has_p1_backup": (P1_BAK_DIR / f"mgo_{mid:03d}_4x_p1.webp").exists(),
            "hash_4x": hash_4x,
            "qc": qc_map.get(str(mid)),
        })
    OUT_PATH.write_text(json.dumps({"items": items, "groups": GROUP_LABELS}, ensure_ascii=False))
    print(f"Wrote {OUT_PATH} ({len(items)} MGOs)")

if __name__ == "__main__":
    main()
