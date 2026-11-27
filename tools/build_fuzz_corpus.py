#!/usr/bin/env python3
"""Build the fuzz starting-point corpus (test_data/fuzz_corpus/checkpoints.jsonl.gz).

Sources (all `save_json` documents, i.e. what `Game::save_json` writes):
  * segment anchors inside `.repal` recordings (`segments/*/save.json`, one per scene change);
  * loose save/checkpoint JSON files, e.g. agent playtest checkpoints under `.tmp/playtest*/`.

Snapshots are de-duplicated by story progress (scene + event/scene diffs + script counters +
inventory). Per scene, the least and the most advanced snapshot are kept so the fuzzer starts
from diverse points across the story without committing hundreds of near-identical saves.

Usage:
  python3 tools/build_fuzz_corpus.py [--repal GLOB ...] [--saves GLOB ...] [--per-scene N] [--out PATH]
Defaults: --repal 'site/replays/*/*.repal' --saves '.tmp/playtest*/**/*.json' --per-scene 2
"""
import argparse
import glob
import gzip
import hashlib
import json
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def is_save(d) -> bool:
    return isinstance(d, dict) and "sceneId" in d and "party" in d and "roles" in d


def progress_key(d) -> str:
    inv = d.get("inventory")
    if isinstance(inv, dict):
        inv = sorted(inv.items())
    sig = [d.get("sceneId"), d.get("eventDiffs"), d.get("sceneDiffs"), d.get("scriptCounters"), inv]
    return hashlib.sha256(json.dumps(sig, sort_keys=True, ensure_ascii=False).encode()).hexdigest()


def progress_size(d) -> int:
    """Rough story-progress measure: how much world state differs from the initial data."""
    return len(d.get("eventDiffs") or []) + len(d.get("sceneDiffs") or []) + len(d.get("scriptCounters") or {})


def from_repal(path: str):
    with tarfile.open(path) as t:
        for m in t.getmembers():
            if m.name.endswith("/save.json"):
                d = json.load(t.extractfile(m))
                if is_save(d):
                    yield f"{Path(path).stem}/{m.name.split('/')[-2]}", d


def from_saves(pattern: str):
    for f in sorted(glob.glob(pattern, recursive=True)):
        try:
            d = json.load(open(f, encoding="utf-8"))
        except (OSError, ValueError):
            continue
        if is_save(d):
            yield str(Path(f).relative_to(ROOT) if Path(f).is_absolute() else f), d


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repal", action="append", help="glob of .repal recordings")
    ap.add_argument("--saves", action="append", help="glob of save/checkpoint JSON files")
    ap.add_argument("--per-scene", type=int, default=2)
    ap.add_argument("--out", default=str(ROOT / "test_data/fuzz_corpus/checkpoints.jsonl.gz"))
    a = ap.parse_args()

    entries = []
    for pat in a.repal or ["site/replays/*/*.repal"]:
        for f in sorted(glob.glob(pat)):
            entries.extend(from_repal(f))
    for pat in a.saves or [".tmp/playtest*/**/*.json"]:
        entries.extend(from_saves(pat))

    unique = {}
    for label, d in entries:
        unique.setdefault(progress_key(d), (label, d))

    by_scene = {}
    for label, d in unique.values():
        by_scene.setdefault(d["sceneId"], []).append((progress_size(d), label, d))
    picked = []
    for sid in sorted(by_scene):
        group = sorted(by_scene[sid], key=lambda e: (e[0], e[1]))
        keep = [group[0]] if a.per_scene == 1 else [group[0], group[-1]] if len(group) > 1 else [group[0]]
        # Fill with evenly spaced extras when more than two per scene are requested.
        if a.per_scene > 2 and len(group) > 2:
            step = (len(group) - 1) / (a.per_scene - 1)
            keep = [group[round(i * step)] for i in range(a.per_scene)]
        seen = set()
        for _, label, d in keep:
            if label not in seen:
                seen.add(label)
                picked.append({"label": label, "save": d})

    out = Path(a.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with gzip.open(out, "wt", encoding="utf-8", compresslevel=9) as fh:
        for e in picked:
            fh.write(json.dumps(e, ensure_ascii=False, separators=(",", ":")) + "\n")
    print(
        f"{len(entries)} snapshots -> {len(unique)} distinct -> {len(picked)} kept "
        f"across {len(by_scene)} scenes; wrote {out} ({out.stat().st_size // 1024} KB)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
