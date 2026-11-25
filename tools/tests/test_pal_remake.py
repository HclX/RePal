"""End-to-end automated verification suite for PalRemake."""

from __future__ import annotations
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import threading
import tomllib
import urllib.request
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PYLIBS = ROOT / ".pylibs"
if PYLIBS.exists() and str(PYLIBS) not in sys.path:
    sys.path.insert(0, str(PYLIBS))

from PIL import Image  # type: ignore
import pytest  # type: ignore
import yaml  # type: ignore

SITE = ROOT / "site"
DATA = SITE / "assets" / "data"
GFX = SITE / "assets" / "graphics"
AUDIO = SITE / "assets" / "audio"


def test_source_archive_and_wasm_artifact():
    dos_zip = ROOT / "DoSPal.zip"
    if dos_zip.is_file():
        assert dos_zip.read_bytes()[:4] == b"PK\x03\x04", "DoSPal.zip has invalid ZIP header"
    assert (SITE / ".nojekyll").is_file(), "site/.nojekyll required for GitHub Pages"

    wasm_file = SITE / "wasm" / "pal.wasm"
    assert wasm_file.is_file()
    raw = wasm_file.read_bytes()
    assert raw[:4] == b"\x00asm", "Invalid WebAssembly binary header"
    # Full Rust engine (pal-core + pal-render + pal-ui + fontdue + TextEngine): keep compact under 2.0 MB
    assert 100_000 < len(raw) < 2_000_000, f"Unexpected WASM size: {len(raw)}"


def test_yaml_tables_and_terminology():
    roles_doc = yaml.safe_load((DATA / "roles.yaml").read_text(encoding="utf-8"))
    roles = roles_doc["roles"]
    assert len(roles) == 6
    names = [r["name"] for r in roles]
    assert names == ["李逍遥", "赵灵儿", "林月如", "巫后", "阿奴", "盖罗娇"]

    items_doc = yaml.safe_load((DATA / "items.yaml").read_text(encoding="utf-8"))
    assert len(items_doc["items"]) == 234

    magic_doc = yaml.safe_load((DATA / "magic.yaml").read_text(encoding="utf-8"))
    assert len(magic_doc["magics"]) == 103
    magic_names = {m["name"] for m in magic_doc["magics"]}
    assert "乾坤一掷" in magic_names, "乾坤一掷 must not be mistranslated as 干坤一掷"
    assert "御剑术" in magic_names

    enemies_doc = yaml.safe_load((DATA / "enemies.yaml").read_text(encoding="utf-8"))
    assert len(enemies_doc["enemies"]) == 154
    assert len(enemies_doc["teams"]) == 380

    scenes_doc = yaml.safe_load((DATA / "scenes.yaml").read_text(encoding="utf-8"))
    assert len(scenes_doc["scenes"]) == 294

    events_doc = yaml.safe_load((DATA / "events.yaml").read_text(encoding="utf-8"))
    assert len(events_doc["events"]) == 5332


def test_all_script_yamls_and_jump_target_integrity():
    all_blocks: dict[str, list[dict]] = {}
    scripts_dir = DATA / "scripts"
    yaml_files = sorted(scripts_dir.glob("scripts_*.yaml"))
    assert len(yaml_files) == 11

    for yf in yaml_files:
        txt = yf.read_text(encoding="utf-8")
        assert txt.startswith("# ==="), f"{yf.name} should preserve header comments"
        doc = yaml.safe_load(txt)
        blocks = doc["scripts"]
        all_blocks.update(blocks)

    assert len(all_blocks) >= 4200

    # Verify opening bedroom scene script s_1f10 exists and has inline Chinese dialogue
    assert "s_1f10" in all_blocks
    s1f10_ops = [step["op"] for step in all_blocks["s_1f10"]]
    assert "say" in s1f10_ops

    # Every symbolic jump target across ALL 4,262 blocks must exist!
    jump_fields = ("target", "else_jump", "jump_if_less", "on_no", "on_defeat", "on_flee")
    missing_targets = set()
    for lbl, steps in all_blocks.items():
        for step in steps:
            for jf in jump_fields:
                t = step.get(jf)
                if t and t not in all_blocks:
                    missing_targets.add((lbl, jf, t))
    assert not missing_targets, f"Broken symbolic jump targets: {list(missing_targets)[:10]}"

    # Verify sample mod YAML parses cleanly and overrides a valid block
    mod_manifest = yaml.safe_load((SITE / "assets" / "mods" / "manifest.yaml").read_text(encoding="utf-8"))
    assert len(mod_manifest["mods"]) >= 1
    sample_mod = yaml.safe_load((SITE / "assets" / "mods" / "sample_story_mod.yaml").read_text(encoding="utf-8"))
    assert "s_0021" in sample_mod["scripts_override"]


def test_4x_hd_and_1x_retro_webp_atlases():
    pairs = [
        (GFX / "fbp" / "fbp_060_1x.webp", GFX / "fbp" / "fbp_060_4x.webp"),
        (GFX / "ui" / "rgm_1x.webp", GFX / "ui" / "rgm_4x.webp"),
        (GFX / "ui" / "ball_1x.webp", GFX / "ui" / "ball_4x.webp"),
        (GFX / "gop" / "gop_010_1x.webp", GFX / "gop" / "gop_010_4x.webp"),
        (GFX / "mgo" / "mgo_002_1x.webp", GFX / "mgo" / "mgo_002_4x.webp"),
    ]
    for p1, p4 in pairs:
        assert p1.is_file() and p4.is_file(), f"Missing WebP pair: {p1.name}"
        with Image.open(p1) as im1, Image.open(p4) as im4:
            assert im4.width == im1.width * 4
            assert im4.height == im1.height * 4


def test_remastered_audio_assets():
    wavs = sorted((AUDIO / "sfx").glob("sfx_*.wav"))
    assert len(wavs) == 211
    with wave.open(str(wavs[0]), "rb") as wf:
        assert wf.getframerate() == 22050
        assert wf.getsampwidth() == 2
        assert wf.getnframes() > 10

    midis = sorted((AUDIO / "midi").glob("mus_*.mid"))
    assert len(midis) == 86
    assert midis[0].read_bytes()[:4] == b"MThd"


def test_github_pages_static_compliance_and_subpath_serving():
    # 1. Single-file size check (< 50 MB everywhere inside site/)
    max_size = 0
    max_file = None
    total_size = 0
    for f in SITE.rglob("*"):
        if f.is_file():
            sz = f.stat().st_size
            total_size += sz
            if sz > max_size:
                max_size = sz
                max_file = f
    assert max_size < 50 * 1024 * 1024, f"File exceeds 50MB GitHub Pages safety threshold: {max_file} ({max_size})"
    # GitHub Pages hard site artifact cap is 1 GB; guard with 950 MB safety margin
    # (external CDN/R2 asset hosting track is planned before scaling beyond 58 HD maps).
    assert total_size < 950 * 1024 * 1024, f"Total site size approaches GitHub Pages 1 GB hard limit: {total_size}"

    # 2. Verify no root-absolute `/assets/` or `/wasm/` URLs that break GitHub Pages project subpaths
    bad_refs = []
    for code_file in list(SITE.glob("*.html")) + list((SITE / "js").rglob("*.js")) + list((SITE / "css").glob("*.css")):
        txt = code_file.read_text(encoding="utf-8")
        if re.search(r'["\'(]/(assets|wasm|js|css)/', txt):
            bad_refs.append(code_file.name)
    assert not bad_refs, f"Root-absolute paths break GitHub Pages subpaths: {bad_refs}"

    # 3. Serve workspace root over HTTP and fetch via `/site/...` subpath (simulating `https://user.github.io/PalRemake/`)
    handler = partial(SimpleHTTPRequestHandler, directory=str(ROOT))
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    port = server.server_address[1]
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    try:
        for subpath in [
            "/site/index.html",
            "/site/.nojekyll",
            "/site/wasm/pal.wasm",
            "/site/js/main.js",
            "/site/assets/data/scenes.yaml",
            "/site/assets/graphics/fbp/fbp_060_4x.webp",
        ]:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}{subpath}", timeout=5) as resp:
                assert resp.status == 200
    finally:
        server.shutdown()
        server.server_close()


def test_core_diagnostic_report_inspector():
    host_js = SITE / "js" / "host" / "core-host.js"
    txt = host_js.read_text(encoding="utf-8")
    assert "PalRemake_CoreDiag_v1" in txt and "copyTextSafe" in txt

    sys.path.insert(0, str(ROOT / "tools"))
    import inspect_report  # type: ignore

    sample = """```json
{"kind":"PalRemake_CoreDiag_v1","note":"门打不开","replay":{"kind":"PalRemake_CoreReplay_v1","seed":7,"log":[{"t":3,"k":5},{"t":40,"i":[2,0,0,0,0,0]}]},"state":{"sceneId":3,"mapId":10,"cash":0,"party":[{"roleId":0,"x":1264,"y":1352,"dir":1}],"inventory":[],"dirtyEvents":[]}}
```"""
    summary = inspect_report.summarize_report(inspect_report.parse_report(sample))
    assert "#3 (Map 10)" in summary
    assert "seed=7 2 inputs" in summary


def test_headless_node_wasm_e2e():
    import shutil
    import subprocess

    node_candidates = [
        ROOT / ".tools" / "node" / "bin" / "node",
    ]
    which_node = shutil.which("node")
    if which_node:
        node_candidates.append(Path(which_node))
    node_bin = next((p for p in node_candidates if p.is_file()), None)
    if node_bin is None:
        if os.environ.get("CI"):
            pytest.fail("Node.js binary not found in CI (.tools/node/bin/node or PATH)")
        pytest.skip("Node.js binary not found (.tools/node/bin/node or PATH)")
    res = subprocess.run(
        [str(node_bin), str(ROOT / "tools" / "test_rust_core_host.mjs")],
        capture_output=True,
        text=True,
        timeout=300,
        check=False,
    )
    assert res.returncode == 0, f"Headless E2E failed:\nSTDOUT:\n{res.stdout}\nSTDERR:\n{res.stderr}"
    assert "ALL RUST CORE HOST TESTS PASSED" in res.stdout


def test_track_b_hd_maps_spatial_occluders_under_2048():
    import json

    maps_hd_dir = GFX / "maps_hd"
    meta_files = sorted(maps_hd_dir.glob("map_*/meta.json"))
    assert len(meta_files) >= 50, f"Expected Track-B HD maps, found {len(meta_files)}"
    for mf in meta_files:
        meta = json.loads(mf.read_text(encoding="utf-8"))
        occ = meta.get("occluders") or {}
        pages = occ.get("pages")
        assert isinstance(pages, list), f"{mf} must use spatial occluder pages (not monolithic occluders_4x.webp)"
        assert not (mf.parent / "occluders_4x.webp").exists(), f"Stale monolithic occluders_4x.webp in {mf.parent}"
        for pf in pages:
            p = mf.parent / pf
            assert p.is_file(), f"Missing occluder page {p}"
            with Image.open(p) as im:
                assert im.width <= 2048 and im.height <= 2048, (
                    f"{p} exceeds 2048x2048 mobile/WASM texture cap: {im.width}x{im.height}"
                )


def test_orig_dos_rpg_saves_exported():
    manifest_file = SITE / "assets" / "orig_saves" / "manifest.yaml"
    assert manifest_file.is_file()
    doc = yaml.safe_load(manifest_file.read_text(encoding="utf-8"))
    saves = doc.get("saves", [])
    assert len(saves) == 51
    assert (SITE / "assets" / "orig_saves" / "0.rpg").stat().st_size == 183488
    assert (SITE / "assets" / "orig_saves" / "50.rpg").stat().st_size == 183488


def test_canonical_lab_calibration_cross_scene_consistency():
    import numpy as np

    sys.path.insert(0, str(ROOT / "tools"))
    import track_b_pipeline as tbp  # type: ignore

    # Build a 96x96 1x synthetic map with grass, dark water (#1E2428), stone, and pure #000000 doorway
    ref_1x = np.zeros((96, 96, 3), dtype=np.uint8)
    ref_1x[:48, :48] = (72, 98, 52)    # grass
    ref_1x[:48, 48:] = (30, 36, 40)    # dark slate river water (#1E2428)
    ref_1x[48:, :48] = (128, 118, 102) # stone path
    ref_1x[48:, 48:] = (0, 0, 0)       # pure black doorway/portal
    valid_1x = np.ones((96, 96), dtype=bool)
    alpha_4x = np.full((384, 384), 255, dtype=np.uint8)

    # Simulate two connected scenes with extreme opposite AI drift (+40 washed-out vs -25 darkened)
    base_4x = np.repeat(np.repeat(ref_1x, 4, axis=0), 4, axis=1).astype(np.float32)
    bright_drifted = np.clip(base_4x * 1.25 + 32.0, 0, 255).astype(np.uint8)
    dark_drifted = np.clip(base_4x * 0.72 - 8.0, 0, 255).astype(np.uint8)
    bright_drifted[192:, 192:] = 0
    dark_drifted[192:, 192:] = 0

    cal_a, _ = tbp.calibrate_hd_island_to_canonical_1x(bright_drifted, alpha_4x, ref_1x, valid_1x)
    cal_b, _ = tbp.calibrate_hd_island_to_canonical_1x(dark_drifted, alpha_4x, ref_1x, valid_1x)

    # Pure black doorway must stay dark (no lifted grey wash)
    assert float(cal_a[208:368, 208:368].mean()) < 4.0
    assert float(cal_b[208:368, 208:368].mean()) < 4.0

    # Both calibrated scenes must agree tightly (< 2.0 Lab dE across every material region)
    lab_a = tbp.rgb_to_lab_f32(cal_a.reshape(96, 4, 96, 4, 3).mean(axis=(1, 3)))
    lab_b = tbp.rgb_to_lab_f32(cal_b.reshape(96, 4, 96, 4, 3).mean(axis=(1, 3)))
    non_blk = ref_1x.max(axis=2) > 4
    d_ab = np.abs((lab_a[non_blk] - lab_b[non_blk]).mean(axis=0))
    assert float(np.linalg.norm(d_ab)) < 2.0, f"Cross-scene calibration residual too large: {d_ab}"


def _write_test_rgba(path: Path, rgba: tuple[int, int, int, int], w: int = 8, h: int = 8) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = bytes(rgba) * (w * h)
    path.write_bytes(struct.pack("<II", w, h) + payload)


def _write_test_webp(path: Path, rgba: tuple[int, int, int, int], w: int = 8, h: int = 8) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    Image.new("RGBA", (w, h), rgba).save(path, "WEBP", lossless=True)


def test_build_assets_preserves_track_b_metadata_and_4x_assets(tmp_path, monkeypatch):
    if str(ROOT / "tools") not in sys.path:
        sys.path.insert(0, str(ROOT / "tools"))
    import build_assets  # type: ignore

    ws = tmp_path / "ws"
    ws.mkdir(parents=True, exist_ok=True)
    decoded_dir = ws / "raw_data" / "_decoded"
    gfx_dst = ws / "site" / "assets" / "graphics"
    (ws / "DoSPal.zip").write_bytes(b"PK\x05\x06" + b"\x00" * 18)
    (ws / "raw_data" / "SSS.MKF").parent.mkdir(parents=True, exist_ok=True)
    (ws / "raw_data" / "SSS.MKF").write_bytes(b"DUMMY")
    (ws / "site" / "assets" / "maps").mkdir(parents=True, exist_ok=True)
    (ws / "site" / "assets" / "manifest.json").write_text("{}", encoding="utf-8")

    # Executable no-op pal-cli stub so CLI main() path can also be exercised cleanly
    pal_cli = ws / "target" / "release" / "pal-cli"
    pal_cli.parent.mkdir(parents=True, exist_ok=True)
    pal_cli.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    pal_cli.chmod(0o755)

    # Decoded raw inputs (no Track-B keys, procedural red/blue pixels)
    for name, mid in [("mgo_001", 1), ("mgo_002", 2), ("mgo_003", 3)]:
        mgo_dec = decoded_dir / "mgo"
        mgo_dec.mkdir(parents=True, exist_ok=True)
        (mgo_dec / f"{name}.json").write_text(
            json.dumps({"id": mid, "w": 8, "h": 8, "frames": 1}), encoding="utf-8"
        )
        _write_test_rgba(mgo_dec / f"{name}_4x.rgba", (255, 0, 0, 255))
        _write_test_rgba(mgo_dec / f"{name}_1x.rgba", (255, 0, 0, 255), w=2, h=2)

    ui_dec = decoded_dir / "ui"
    ui_dec.mkdir(parents=True, exist_ok=True)
    (ui_dec / "rgm.json").write_text(json.dumps({"w": 8, "h": 8, "frames": 1}), encoding="utf-8")
    _write_test_rgba(ui_dec / "rgm_4x.rgba", (0, 0, 255, 255))
    _write_test_rgba(ui_dec / "rgm_1x.rgba", (0, 0, 255, 255), w=2, h=2)

    # Existing Track-B destination assets (sentinel green for mgo 4x, yellow for rgm 4x)
    mgo_dst = gfx_dst / "mgo"
    mgo_dst.mkdir(parents=True, exist_ok=True)
    (mgo_dst / "mgo_001.json").write_text(
        json.dumps({"id": 1, "w": 8, "h": 8, "frames": 1, "shadowStripped": True, "hdTrackB": True}),
        encoding="utf-8",
    )
    _write_test_webp(mgo_dst / "mgo_001_4x.webp", (0, 255, 0, 255))
    _write_test_webp(mgo_dst / "mgo_001_1x.webp", (10, 10, 10, 255), w=2, h=2)

    # Specifically test shadowStripped: False is preserved via key membership (not truthiness)
    (mgo_dst / "mgo_002.json").write_text(
        json.dumps({"id": 2, "w": 8, "h": 8, "frames": 1, "shadowStripped": False, "hdTrackB": True}),
        encoding="utf-8",
    )
    _write_test_webp(mgo_dst / "mgo_002_4x.webp", (0, 255, 0, 255))
    _write_test_webp(mgo_dst / "mgo_002_1x.webp", (10, 10, 10, 255), w=2, h=2)

    # Non-Track-B mgo_003 should still be refreshable under --refresh-graphics
    (mgo_dst / "mgo_003.json").write_text(
        json.dumps({"id": 3, "w": 8, "h": 8, "frames": 1}), encoding="utf-8"
    )
    _write_test_webp(mgo_dst / "mgo_003_4x.webp", (10, 10, 10, 255))

    ui_dst = gfx_dst / "ui"
    ui_dst.mkdir(parents=True, exist_ok=True)
    (ui_dst / "rgm.json").write_text(json.dumps({"w": 8, "h": 8, "frames": 1}), encoding="utf-8")
    (ui_dst / "rgm_hd_done.json").write_text(json.dumps({"done_portraits": [1, 2]}), encoding="utf-8")
    _write_test_webp(ui_dst / "rgm_4x.webp", (255, 255, 0, 255))
    _write_test_webp(ui_dst / "rgm_1x.webp", (10, 10, 10, 255), w=2, h=2)

    monkeypatch.setattr(build_assets, "ROOT", ws)

    # Case 1: default run (--graphics-only, no --refresh-graphics) must preserve Track-B metadata & 4x assets
    monkeypatch.setattr(sys, "argv", ["build_assets.py", "--graphics-only"])
    build_assets.main()
    m1 = json.loads((mgo_dst / "mgo_001.json").read_text(encoding="utf-8"))
    m2 = json.loads((mgo_dst / "mgo_002.json").read_text(encoding="utf-8"))
    assert m1.get("hdTrackB") is True and m1.get("shadowStripped") is True
    assert m2.get("hdTrackB") is True and ("shadowStripped" in m2 and m2["shadowStripped"] is False)
    with Image.open(mgo_dst / "mgo_001_4x.webp") as im:
        assert im.getpixel((0, 0))[1] > 200 and im.getpixel((0, 0))[0] < 30
    with Image.open(ui_dst / "rgm_4x.webp") as im:
        assert im.getpixel((0, 0))[0] > 200 and im.getpixel((0, 0))[1] > 200 and im.getpixel((0, 0))[2] < 30

    # Case 2: --graphics-only --refresh-graphics must protect Track-B 4x & metadata, while refreshing 1x & non-Track-B
    monkeypatch.setattr(sys, "argv", ["build_assets.py", "--graphics-only", "--refresh-graphics"])
    build_assets.main()
    m1 = json.loads((mgo_dst / "mgo_001.json").read_text(encoding="utf-8"))
    m2 = json.loads((mgo_dst / "mgo_002.json").read_text(encoding="utf-8"))
    assert m1.get("hdTrackB") is True and m1.get("shadowStripped") is True
    assert m2.get("hdTrackB") is True and ("shadowStripped" in m2 and m2["shadowStripped"] is False)
    with Image.open(mgo_dst / "mgo_001_4x.webp") as im:
        assert im.getpixel((0, 0))[1] > 200 and im.getpixel((0, 0))[0] < 30
    with Image.open(ui_dst / "rgm_4x.webp") as im:
        assert im.getpixel((0, 0))[0] > 200 and im.getpixel((0, 0))[1] > 200 and im.getpixel((0, 0))[2] < 30
    with Image.open(mgo_dst / "mgo_001_1x.webp") as im:
        assert im.getpixel((0, 0))[0] > 200 and im.getpixel((0, 0))[1] < 30
    with Image.open(mgo_dst / "mgo_003_4x.webp") as im:
        assert im.getpixel((0, 0))[0] > 200 and im.getpixel((0, 0))[1] < 30
    assert (ui_dst / "rgm_hd_done.json").is_file()

    # Case 3: --graphics-only --refresh-graphics --overwrite-track-b explicitly overwrites Track-B assets & clears rgm_hd_done
    monkeypatch.setattr(
        sys, "argv", ["build_assets.py", "--graphics-only", "--refresh-graphics", "--overwrite-track-b"]
    )
    build_assets.main()
    m1 = json.loads((mgo_dst / "mgo_001.json").read_text(encoding="utf-8"))
    m2 = json.loads((mgo_dst / "mgo_002.json").read_text(encoding="utf-8"))
    assert "hdTrackB" not in m1 and "shadowStripped" not in m1
    assert "hdTrackB" not in m2 and "shadowStripped" not in m2
    with Image.open(mgo_dst / "mgo_001_4x.webp") as im:
        assert im.getpixel((0, 0))[0] > 200 and im.getpixel((0, 0))[1] < 30
    with Image.open(ui_dst / "rgm_4x.webp") as im:
        assert im.getpixel((0, 0))[2] > 200 and im.getpixel((0, 0))[0] < 30
    done_file = ui_dst / "rgm_hd_done.json"
    assert (not done_file.exists()) or (json.loads(done_file.read_text(encoding="utf-8")).get("done_portraits") == [])


def test_dependency_manifests_declare_python_and_node_tooling():
    pyproject_path = ROOT / "pyproject.toml"
    req_path = ROOT / "requirements-dev.txt"
    pkg_path = ROOT / "package.json"
    assert pyproject_path.is_file(), "pyproject.toml must exist at repository root"
    assert req_path.is_file(), "requirements-dev.txt must exist at repository root"
    assert pkg_path.is_file(), "package.json must exist at repository root"

    py_doc = tomllib.loads(pyproject_path.read_text(encoding="utf-8"))
    deps = " ".join(py_doc.get("project", {}).get("dependencies", [])).lower()
    for pkg in ("numpy", "scipy", "pillow", "pyyaml", "opencc-python-reimplemented"):
        assert pkg in deps, f"pyproject.toml missing dependency: {pkg}"
    dev_deps = " ".join(py_doc.get("project", {}).get("optional-dependencies", {}).get("dev", [])).lower()
    assert "pytest" in dev_deps, "pyproject.toml [project.optional-dependencies].dev missing pytest"
    assert py_doc.get("tool", {}).get("pytest", {}).get("ini_options", {}).get("testpaths") == ["tools/tests"]

    req_txt = req_path.read_text(encoding="utf-8").lower()
    for pkg in ("numpy", "scipy", "pillow", "pyyaml", "opencc-python-reimplemented", "pytest"):
        assert pkg in req_txt, f"requirements-dev.txt missing dependency: {pkg}"

    pkg_doc = json.loads(pkg_path.read_text(encoding="utf-8"))
    assert pkg_doc.get("type") == "module"
    assert "node" in pkg_doc.get("engines", {})
    scripts = pkg_doc.get("scripts", {})
    assert "test" in scripts
    all_scripts_blob = json.dumps(scripts)
    for mjs in (
        "test_rust_core_host.mjs",
        "test_host.mjs",
    ):
        assert mjs in all_scripts_blob, f"package.json scripts missing {mjs}"


def test_track_b_pipeline_cli_runs_without_external_pythonpath(tmp_path):
    tbp_src = ROOT / "tools" / "track_b_pipeline.py"
    src_text = tbp_src.read_text(encoding="utf-8")
    pylibs_idx = src_text.find(".pylibs")
    scipy_idx = src_text.find("import numpy")
    assert pylibs_idx != -1 and pylibs_idx < scipy_idx, (
        "track_b_pipeline.py must inject ROOT/.pylibs into sys.path before third-party imports"
    )

    clean_env = {k: v for k, v in os.environ.items() if k != "PYTHONPATH"}

    # Locate a .pylibs directory (either ROOT/.pylibs or from PYTHONPATH) for worktree-safe functional fallback test
    pylibs_dir = ROOT / ".pylibs"
    if not pylibs_dir.is_dir():
        for entry in os.environ.get("PYTHONPATH", "").split(os.pathsep):
            if entry and Path(entry).name == ".pylibs" and Path(entry).is_dir():
                pylibs_dir = Path(entry)
                break

    if pylibs_dir.is_dir():
        fake_root = tmp_path / "repo"
        (fake_root / "tools").mkdir(parents=True)
        shutil.copy2(tbp_src, fake_root / "tools" / "track_b_pipeline.py")
        (fake_root / ".pylibs").symlink_to(pylibs_dir)
        res = subprocess.run(
            [sys.executable, str(fake_root / "tools" / "track_b_pipeline.py"), "--help"],
            env=clean_env,
            cwd=str(fake_root),
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        assert res.returncode == 0, f"track_b_pipeline.py --help failed without PYTHONPATH:\n{res.stderr}"
        assert "status" in res.stdout

    bare_scipy = subprocess.run(
        [sys.executable, "-c", "import scipy"],
        env=clean_env,
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
    )
    if (ROOT / ".pylibs").is_dir() or bare_scipy.returncode == 0:
        res_root = subprocess.run(
            [sys.executable, str(tbp_src), "--help"],
            env=clean_env,
            cwd=str(ROOT),
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        assert res_root.returncode == 0, f"ROOT track_b_pipeline.py --help failed:\n{res_root.stderr}"
        assert "status" in res_root.stdout


def test_headless_node_wasm_e2e_never_silently_passes_when_node_missing(monkeypatch):
    orig_is_file = Path.is_file

    def fake_is_file(self: Path) -> bool:
        if self.name == "node":
            return False
        return orig_is_file(self)

    monkeypatch.setattr(shutil, "which", lambda _name: None)
    monkeypatch.setattr(Path, "is_file", fake_is_file)

    monkeypatch.delenv("CI", raising=False)
    with pytest.raises(pytest.skip.Exception):
        test_headless_node_wasm_e2e()

    monkeypatch.setenv("CI", "true")
    with pytest.raises(pytest.fail.Exception):
        test_headless_node_wasm_e2e()


def test_cargo_lock_tracked_and_not_gitignored():
    gitignore_lines = [
        line.strip()
        for line in (ROOT / ".gitignore").read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.strip().startswith("#")
    ]
    assert "Cargo.lock" not in gitignore_lines and "/Cargo.lock" not in gitignore_lines, (
        ".gitignore must not ignore Cargo.lock"
    )

    lock_path = ROOT / "Cargo.lock"
    assert lock_path.is_file(), "Cargo.lock must exist at repository root"
    lock_text = lock_path.read_text(encoding="utf-8")
    for marker in ("[[package]]", 'name = "pal-core"', 'name = "pal-wasm"', 'name = "pal-cli"'):
        assert marker in lock_text, f"Cargo.lock missing {marker}"

    if (ROOT / ".git").exists() and shutil.which("git"):
        chk = subprocess.run(
            ["git", "-C", str(ROOT), "check-ignore", "Cargo.lock"],
            capture_output=True,
            text=True,
            check=False,
        )
        assert chk.returncode == 1, f"Cargo.lock is still gitignored: {chk.stdout}"
        lsf = subprocess.run(
            ["git", "-C", str(ROOT), "ls-files", "--error-unmatch", "Cargo.lock"],
            capture_output=True,
            text=True,
            check=False,
        )
        assert lsf.returncode == 0, f"Cargo.lock is not tracked by git: {lsf.stderr}"


def test_audit_script_parity_rpg_save_validation(tmp_path):
    if str(ROOT / "tools") not in sys.path:
        sys.path.insert(0, str(ROOT / "tools"))
    import audit_script_parity as asp  # type: ignore

    doc = asp.__doc__ or ""
    assert "site/assets/orig_saves/" in doc
    assert "site/assets/saves/" not in doc

    valid_scenes = set(range(1, 295))
    orig_saves_dir = SITE / "assets" / "orig_saves"
    assert asp.validate_rpg_saves(orig_saves_dir, valid_scenes) == []

    sample_bytes = (orig_saves_dir / "0.rpg").read_bytes()

    # 1. Wrong count (< 51)
    dir_count = tmp_path / "count"
    dir_count.mkdir()
    (dir_count / "0.rpg").write_bytes(sample_bytes)
    assert asp.validate_rpg_saves(dir_count, valid_scenes)

    # 2. Truncated .rpg size
    dir_trunc = tmp_path / "trunc"
    dir_trunc.mkdir()
    (dir_trunc / "0.rpg").write_bytes(b"CORRUPT123")
    errs_trunc = asp.validate_rpg_saves(dir_trunc, valid_scenes, expected_count=1)
    assert any("183488" in e or "bytes" in e for e in errs_trunc)

    # 3. Out-of-bounds scene_id (0 and 999)
    for bad_sid in (0, 999):
        dir_sid = tmp_path / f"sid_{bad_sid}"
        dir_sid.mkdir()
        buf = bytearray(sample_bytes)
        struct.pack_into("<H", buf, 8, bad_sid)
        (dir_sid / "0.rpg").write_bytes(bytes(buf))
        errs_sid = asp.validate_rpg_saves(dir_sid, valid_scenes, expected_count=1)
        assert any("scene_id" in e for e in errs_sid)

    # 4. Out-of-bounds n_party, role_id, party_dir, screen_wave
    dir_party = tmp_path / "party"
    dir_party.mkdir()
    buf = bytearray(sample_bytes)
    struct.pack_into("<H", buf, 6, 9)    # n_party > 4
    struct.pack_into("<H", buf, 12, 4)   # party_dir > 3
    struct.pack_into("<H", buf, 20, 200) # screen_wave >= 200
    struct.pack_into("<H", buf, 44, 99)  # role_id > 5
    (dir_party / "0.rpg").write_bytes(bytes(buf))
    errs_party = asp.validate_rpg_saves(dir_party, valid_scenes, expected_count=1)
    assert any("n_party" in e for e in errs_party)
    assert any("party_dir" in e for e in errs_party)
    assert any("screen_wave" in e for e in errs_party)
    assert any("role_id" in e for e in errs_party)

    # 5. Embedded scene table map_id == 0
    dir_map0 = tmp_path / "map0"
    dir_map0.mkdir()
    buf_map0 = bytearray(sample_bytes)
    sid0 = struct.unpack_from("<H", buf_map0, 8)[0]
    struct.pack_into("<H", buf_map0, 3264 + (sid0 - 1) * 8, 0)
    (dir_map0 / "0.rpg").write_bytes(bytes(buf_map0))
    errs_map0 = asp.validate_rpg_saves(dir_map0, valid_scenes, expected_count=1)
    assert any("map_id=0" in e for e in errs_map0)

    # 6. manifest.yaml consistency mismatch
    dir_mf = tmp_path / "manifest_bad"
    dir_mf.mkdir()
    (dir_mf / "0.rpg").write_bytes(sample_bytes)
    (dir_mf / "manifest.yaml").write_text(
        yaml.safe_dump({"saves": [{"file": "0.rpg", "scene_id": 999, "saved_times": 0, "cash": 0}]}),
        encoding="utf-8",
    )
    errs_mf = asp.validate_rpg_saves(dir_mf, valid_scenes, expected_count=1)
    assert any("manifest.yaml mismatch" in e for e in errs_mf)

