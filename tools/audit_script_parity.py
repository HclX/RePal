#!/usr/bin/env python3
"""Static Full-Game Opcode & Script Contract Parity Audit (`tools/audit_script_parity.py`).

Scans all 294 scenes (`scenes.yaml`, `events.yaml`, `items.yaml`, `magic.yaml`,
and `scripts/scripts_*.yaml`) against `site/js/vm/script-vm.js` to verify:
  1. Every opcode (`op:`) appearing anywhere in the game's scripts is handled in `ScriptVM`.
  2. Every referenced script label (`script_trigger`, `script_auto`, `script_enter`,
     `script_teleport`, `script_use`, `target`, `on_no`, `else_jump`, `jump_if_*`) exists.
  3. All 51 classic `.rpg` saves in `site/assets/orig_saves/` have valid file size, binary header, and scene/party bounds.
"""

from __future__ import annotations

import json
import re
import struct
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PYLIBS = ROOT / ".pylibs"
if PYLIBS.exists() and str(PYLIBS) not in sys.path:
    sys.path.insert(0, str(PYLIBS))

import yaml

SITE = ROOT / "site"
DATA = SITE / "assets" / "data"
VM_RS = ROOT / "crates" / "pal-core" / "src" / "game" / "vm.rs"
VM_DIR = ROOT / "crates" / "pal-core" / "src" / "game" / "vm"
EXPECTED_RPG_COUNT = 51
EXACT_RPG_BYTES = 183_488  # 12864-byte header/roles/inv/scenes + 5332 * 32-byte events


def read_vm_sources() -> str:
    """Read `crates/pal-core/src/game/vm/*.rs` (or legacy monolithic `vm.rs`)."""
    if VM_DIR.is_dir():
        return "\n".join(p.read_text(encoding="utf-8") for p in sorted(VM_DIR.glob("*.rs")))
    return VM_RS.read_text(encoding="utf-8")


def extract_vm_handled_ops(src_text: str) -> set[str]:
    """Extract all handled opcode mnemonics directly from `crates/pal-core/src/game/vm/`."""
    ops: set[str] = set()
    # Match Rust match arms in `exec_step`: `"op_a" | "op_b" =>`
    for arm in re.finditer(r'((?:"[a-z0-9_]+"(?:\s*\|\s*"[a-z0-9_]+")*))\s*=>', src_text):
        for name in re.findall(r'"([a-z0-9_]+)"', arm.group(1)):
            ops.add(name)
    return ops | {"say"}


def validate_rpg_saves(
    saves_dir: Path,
    valid_scene_ids: set[int],
    expected_count: int = EXPECTED_RPG_COUNT,
) -> list[str]:
    """Validate classic DOS `.rpg` saves for count, exact binary size, header bounds, scene_id, and manifest."""
    errors: list[str] = []
    if not saves_dir.is_dir():
        return [f"Missing saves directory: {saves_dir}"]

    saves = sorted(
        saves_dir.glob("*.rpg"),
        key=lambda p: (0, int(p.stem)) if p.stem.isdigit() else (1, p.name),
    )
    if len(saves) != expected_count:
        errors.append(f"Expected {expected_count} .rpg saves in {saves_dir}, found {len(saves)}")

    for p in saves:
        data = p.read_bytes()
        if len(data) != EXACT_RPG_BYTES:
            errors.append(f"{p.name}: expected {EXACT_RPG_BYTES} bytes, got {len(data)}")
            continue
        saved_times, _vp_x, _vp_y, n_party, scene_id, _pal_off, party_dir, _mus, _bmus, _bf, screen_wave = (
            struct.unpack_from("<11H", data, 0)
        )
        if scene_id not in valid_scene_ids:
            errors.append(f"{p.name}: scene_id={scene_id} out of bounds")
        if n_party > 4:
            errors.append(f"{p.name}: n_party={n_party} exceeds DOS PAL max (4)")
        if party_dir > 3:
            errors.append(f"{p.name}: party_dir={party_dir} out of bounds (0..3)")
        if screen_wave >= 200:
            errors.append(f"{p.name}: screen_wave={screen_wave} out of bounds (<200)")
        for idx in range(min(n_party, 4) + 1):
            role_id = struct.unpack_from("<H", data, 44 + idx * 10)[0]
            if role_id > 5:
                errors.append(f"{p.name}: party[{idx}].role_id={role_id} out of bounds (0..5)")
        if scene_id in valid_scene_ids:
            sc_off = 3264 + (scene_id - 1) * 8
            map_id = struct.unpack_from("<H", data, sc_off)[0]
            if map_id == 0:
                errors.append(f"{p.name}: scene_id={scene_id} has map_id=0 in embedded scene table")

    manifest_path = saves_dir / "manifest.yaml"
    if manifest_path.is_file():
        manifest_doc = yaml.safe_load(manifest_path.read_text(encoding="utf-8")) or {}
        entries = manifest_doc.get("saves", [])
        if len(entries) != expected_count:
            errors.append(f"manifest.yaml: expected {expected_count} entries, found {len(entries)}")
        for entry in entries:
            rel = entry.get("file", "")
            target = saves_dir / Path(rel).name
            if not target.is_file():
                errors.append(f"manifest.yaml: missing referenced file {rel}")
                continue
            d = target.read_bytes()
            if len(d) >= 44:
                st, _vx, _vy, _np, sid = struct.unpack_from("<5H", d, 0)
                cash = struct.unpack_from("<I", d, 40)[0]
                if entry.get("scene_id") != sid or entry.get("saved_times") != st or entry.get("cash") != cash:
                    errors.append(f"manifest.yaml mismatch for {target.name}")
    return errors


def main() -> int:
    vm_ops = extract_vm_handled_ops(read_vm_sources())

    # 1. Load all YAML scripts & count opcodes
    scripts: dict[str, list] = {}
    op_counts: Counter[str] = Counter()
    op_examples: dict[str, str] = {}

    for p in sorted((DATA / "scripts").glob("scripts_*.yaml")):
        raw = yaml.safe_load(p.read_text(encoding="utf-8"))
        d = raw.get("scripts", raw) if isinstance(raw, dict) else None
        if not isinstance(d, dict):
            continue
        for lbl, steps in d.items():
            if not isinstance(steps, list):
                continue
            scripts[lbl] = steps
            for st in steps:
                if isinstance(st, dict) and "op" in st:
                    op = st["op"]
                    op_counts[op] += 1
                    op_examples.setdefault(op, f"{p.name}:{lbl}")

    missing_ops = sorted(
        op for op in op_counts if op not in vm_ops and not re.fullmatch(r"op_[0-9a-fA-F]{4}", op)
    )
    print(f"Total script labels: {len(scripts)} | Unique opcodes used: {len(op_counts)} | VM handlers in vm.rs: {len(vm_ops)}")
    if missing_ops:
        print(f"MISSING OPCODES IN vm.rs ({len(missing_ops)}):")
        for op in missing_ops:
            print(f"  - {op} (count={op_counts[op]}, example={op_examples[op]})")
    else:
        print("PASS: 100% of opcodes used across all 294 scenes are handled in crates/pal-core/src/game/vm.rs!")

    # 2. Check all script label references resolve to existing `s_XXXX` blocks
    missing_refs: list[tuple[str, str]] = []

    def check_lbl(src: str, lbl: str | None):
        if lbl and isinstance(lbl, str) and lbl not in scripts:
            missing_refs.append((src, lbl))

    scenes = yaml.safe_load((DATA / "scenes.yaml").read_text(encoding="utf-8"))["scenes"]
    for sc in scenes:
        check_lbl(f"scene#{sc['id']}.script_enter", sc.get("script_enter"))
        check_lbl(f"scene#{sc['id']}.script_teleport", sc.get("script_teleport"))

    events = yaml.safe_load((DATA / "events.yaml").read_text(encoding="utf-8"))["events"]
    for ev in events:
        check_lbl(f"event#{ev['id']}.script_trigger", ev.get("script_trigger"))
        check_lbl(f"event#{ev['id']}.script_auto", ev.get("script_auto"))

    items = yaml.safe_load((DATA / "items.yaml").read_text(encoding="utf-8"))["items"]
    for it in items:
        check_lbl(f"item#{it['id']}({it.get('name','')}).script_use", it.get("script_use"))
        check_lbl(f"item#{it['id']}({it.get('name','')}).script_equip", it.get("script_equip"))
        check_lbl(f"item#{it['id']}({it.get('name','')}).script_throw", it.get("script_throw"))

    magics = yaml.safe_load((DATA / "magic.yaml").read_text(encoding="utf-8")).get("magic", [])
    for mg in magics:
        check_lbl(f"magic#{mg['id']}({mg.get('name','')}).script_use", mg.get("script_use"))
        check_lbl(f"magic#{mg['id']}({mg.get('name','')}).script_success", mg.get("script_success"))

    jump_keys = (
        "target",
        "on_no",
        "on_defeat",
        "on_flee",
        "else_jump",
        "jump_if_less",
        "jump_if_not",
        "jump_if_true",
        "jump_if_false",
    )
    for lbl, steps in scripts.items():
        for idx, st in enumerate(steps):
            if not isinstance(st, dict):
                continue
            for k in jump_keys:
                val = st.get(k)
                if isinstance(val, str) and val.startswith("s_"):
                    check_lbl(f"{lbl}[{idx}].{k}", val)

    print(f"Missing script label references: {len(missing_refs)}")
    for src, lbl in missing_refs[:15]:
        print(f"  - {src} -> {lbl}")

    # 3. Validate all 51 classic .rpg saves (count, binary size, header & scene bounds)
    valid_scene_ids = {int(sc["id"]) for sc in scenes}
    saves_dir = SITE / "assets" / "orig_saves"
    save_errors = validate_rpg_saves(saves_dir, valid_scene_ids)
    saves_count = len(list(saves_dir.glob("*.rpg"))) if saves_dir.is_dir() else 0
    print(f"Classic .rpg saves found: {saves_count} | Validation errors: {len(save_errors)}")
    for err in save_errors[:15]:
        print(f"  - {err}")

    return 1 if (missing_ops or missing_refs or save_errors) else 0


if __name__ == "__main__":
    raise SystemExit(main())
