import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BINARY_EXTS = {
    ".webp",
    ".png",
    ".jpg",
    ".jpeg",
    ".wav",
    ".mid",
    ".ogg",
    ".ttf",
    ".wasm",
    ".bin",
    ".rpg",
    ".zip",
    ".rix",
    ".ico",
}
SKIP_DIRS = {".git", ".pylibs", ".tools", "target", "node_modules", "__pycache__"}

# Build patterns via concatenation so this file never self-matches.
PERSONAL_PATH_PATTERNS = [
    ("unix_home", re.compile(r"/" + r"home/[A-Za-z0-9_.-]+")),
    ("macos_users", re.compile(r"/" + r"Users/[A-Za-z0-9_.-]+")),
    ("google_corp", re.compile(r"/" + r"usr/local/google/")),
    ("unix_root", re.compile(r"/" + r"root/")),
    ("author_handle", re.compile(r"xuan" + r"xing", re.IGNORECASE)),
    ("dot_gem_dir", re.compile(r"\.gem" + r"ini/", re.IGNORECASE)),
    ("jet_dir", re.compile(r"jet" + r"ski/", re.IGNORECASE)),
    ("win_users", re.compile(r"[A-Za-z]:[/\\]" + r"Users[/\\]")),
]


def iter_tracked_text_files(root: Path):
    rel_paths = []
    if (root / ".git").exists():
        res = subprocess.run(
            ["git", "-C", str(root), "ls-files", "-z"],
            capture_output=True,
            check=False,
        )
        if res.returncode == 0:
            rel_paths = [
                raw.decode("utf-8", errors="replace")
                for raw in res.stdout.split(b"\x00")
                if raw
            ]

    if not rel_paths:
        for p in root.rglob("*"):
            if p.is_file() and not any(part in SKIP_DIRS for part in p.parts):
                rel_paths.append(str(p.relative_to(root)))

    for rel in rel_paths:
        p = root / rel
        if not p.is_file():
            continue
        if p.suffix.lower() in BINARY_EXTS:
            continue
        raw = p.read_bytes()
        if b"\x00" in raw:
            continue
        yield rel, raw.decode("utf-8", errors="replace")


def test_no_hardcoded_personal_paths_in_tracked_files():
    violations = []
    for rel, text in iter_tracked_text_files(ROOT):
        for lineno, line in enumerate(text.splitlines(), 1):
            for label, pat in PERSONAL_PATH_PATTERNS:
                if pat.search(line):
                    violations.append(f"{rel}:{lineno} [{label}]: {line.strip()[:140]}")
                    break

    assert not violations, (
        "Hardcoded personal workstation paths found in tracked files:\n"
        + "\n".join(violations)
    )


def test_no_obsolete_single_scene_bake_script():
    obsolete = ROOT / "tools" / "bake_scene1_track_b.py"
    replacement = ROOT / "tools" / "track_b_pipeline.py"
    assert not obsolete.exists(), (
        f"Obsolete single-scene script still exists: {obsolete}"
    )
    assert replacement.is_file(), (
        f"Canonical Track-B pipeline script missing: {replacement}"
    )


def test_shared_headless_harness_suite():
    bundled_node = ROOT / ".tools" / "node" / "bin" / "node"
    node_bin = (
        str(bundled_node)
        if bundled_node.is_file()
        else shutil.which("node")
    )
    assert node_bin is not None, (
        "Node.js binary not found (checked .tools/node/bin/node and PATH)"
    )

    test_script = ROOT / "tools" / "tests" / "test_headless_harness.mjs"
    assert test_script.is_file(), f"Missing Node harness test suite: {test_script}"

    res = subprocess.run(
        [node_bin, str(test_script)],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
        timeout=90,
        check=False,
    )
    assert res.returncode == 0, (
        f"test_headless_harness.mjs failed (rc={res.returncode}):\n"
        f"STDOUT:\n{res.stdout}\nSTDERR:\n{res.stderr}"
    )
