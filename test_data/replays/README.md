# RePal Autonomous Playtest Replay Corpus (`test_data/replays/`)

Self-contained segmented `.repal` (`.tar.gz`) replay archives recorded during autonomous AI playthroughs of RePal (Chapters 1–2), organized in versioned subdirectories (`v0.3.11/`, `v0.3.8/`) matching the engine release that produced them.

Each `.repal` contains `manifest.json` (carrying `engine_version`, `tainted_by_debug`, and `debug_mode`) + standalone per-segment `save.json`, `events.json`, and optional `annotations.json` (AI thought commentary subtitles exportable via `repal --replay-export-srt`). Archives are directly playable in `repal` (drag & drop or `F8`), Web Replay Theater, and verifiable headlessly via the unified `pal_runtime::replay_player::ReplayPlayer`:

```bash
# Verify a specific version directory:
REPAL_VERIFY_DIR=test_data/replays/v0.3.11 cargo test --offline -j 1 -p pal-runtime --features disk --test verify_repal -- --nocapture

# Or recursively verify all version directories:
REPAL_VERIFY_DIR=test_data/replays cargo test --offline -j 1 -p pal-runtime --features disk --test verify_repal -- --nocapture
```

## Replay Index

> **Engine version note (`v0.4.0`)**: RePal `v0.4.0` (`#255`) replaced the simplified combat math with verbatim SDLPAL `PAL_CLASSIC` `fight.c` formulas, interleaved dexterity turn order, and `PAL_PlayerLevelUp` stat growth. Legacy `v0.3.11/` and `v0.3.8/` replays remain playable for archival reference, and both Desktop and Web Replay Theater display an engine-version mismatch warning (`[VER]`) when playing older-version archives on `v0.4.0+`.

### `v0.3.11/` (`engine_version: "0.3.11"`, legacy pre-`v0.4.0` battle rules)

| File | Size | Segments | Ticks / Events | Story Coverage & Highlights |
| :--- | :---: | :---: | :---: | :--- |
| `v0.3.11/01_ch1_zero_shot_fair_blind.repal` | 5.1 KiB | 1 | 32,790 / 1,286 | **Continuous Zero-Screenshot `--obs fair` Ch1 Full Clear** (`tainted_by_debug: false`, 0 saves/loads, 0 deaths): New Game → Yuhang Inn → Drunken Daoist → Market & Dock → Aunt Li faints → Miao Leader (`破天锤 #279`) → Fairy Island 4 encounters (`magic 296 0`) → Lotus Pond Maze (6/6 Asura statues + glowing slab `#263`) → Spirit Pool & Water Moon Palace wedding (`紫金丹 #273`) → Return & cure Aunt Li → Nighttime Scene `#1` `(1312, 272)` |
| `v0.3.11/02_ch2_think_fair_blind.repal` | 14 KiB | 1 | 42,351 / 1,265 | **Continuous Zero-Screenshot `--obs fair` + 53 AI Thought Subtitles (`think` / `annotations.json`)** (`debug_mode: false`, `tainted_by_debug: false`, 0 shots/loads): Night escape secret passage → Ten-Mile Slope (`御剑术`) → Rescue Zhao Ling'er → Fairy Island Granny's burial → Black Miao Leader boss (`金刚咒` + `蜂巢` + `御剑术`) → Auntie Li farewell (`飞龙探云手` / `冰心诀`) → Sail to Suzhou → Defeat & rescue Lin Yueru (`还魂咒`) → Suzhou Weapon Shop & Inn (`sc#25 (896, 784)`, exact RNG `565769698`) |

### `v0.3.8/` (`engine_version: "0.1.0"`, legacy pre-v0.3.11 corpus)

| File | Size | Segments | Ticks / Events | Story Coverage & Highlights |
| :--- | :---: | :---: | :---: | :--- |
| `v0.3.8/01_ch1_yuhang_opening.repal` | 3.6 KiB | 4 | 3,890 / 143 | New Game → Yuhang Inn → Miao guests → Drunken Taoist (`桂花酒`) → Yuhang Town & Market (3/3 exact boundary parity) |
| `v0.3.8/02_ch1_fairy_island_to_miao_guards.repal` | 19 KiB | 22 | 27,757 / 585 | Auntie Li collapses → Fairy Island → Asura Statues Maze → Water Moon Palace wedding → Secret Passage → Ten-Mile Slope (`御剑术`) → Miao Guards Boss |
| `v0.3.8/03_ch1_miao_guards_win_to_miao_leader.repal` | 27 KiB | 10 | 11,159 / 230 | Mountain God Temple → Miao Guards Boss WIN → Fairy Island return → Water Moon Palace Massacre → Black Miao Leader Boss |
| `v0.3.8/04_ch1_miao_leader_retry_verify.repal` | 16 KiB | 3 | 4,714 / 106 | Live verification of pre-boss autosave retry prompt (`是否要回到战前重新挑战？`) |
| `v0.3.8/05_ch1_miao_leader_win_to_suzhou_boat.repal` | 31 KiB | 6 | 8,362 / 251 | Gear upgrade → Black Miao Leader Boss WIN (with `观音咒`) → Auntie Li farewell (`手卷` → `飞龙探云手`) → Boss Fang sailboat |
| `v0.3.8/06_ch2_suzhou_full_to_hidden_dragon_cave.repal` | 73 KiB | 12 | 21,917 / 561 | **Full Chapter 2 (Suzhou)**: Suzhou Docks → Lin Yueru Boss #1 → Two Thugs → Gambling Den Boss (7 HP clutch) → Martial Arts Tournament (`比武招亲`) → Lin Family Manor Snake Demon → Lin Yueru joins → Road to Hidden Dragon Cave (`隐龙窟`, Scene #42) |

## Engine Bugs Uncovered & Fixed via This Corpus

1. **Battle ally-targeted healing (`观音咒 #297` healed 0 HP)**: `pal-core/src/game/vm.rs` `item_or_leader_role()` now checks `self.vmc.using_item || self.battle_active()` and emits green floating `+N` heal numbers in battle.
2. **Scripted boss defeat soft-lock & infinite death loop (`s_9c9b` / `load_last_save`)**: Captures clean pre-event snapshot before event scripts start, prompts pre-battle retry on `load_last_save`, and handles `ReturnToTitle` / `DefeatRetryChoice` cleanly in Text Mode.
3. **Chapter 1 → Chapter 2 Suzhou Boat soft-lock (`ev#126` → `ev#127`)**: `explore.rs` now suppresses only `finished_event` on non-scene-change foreground end and immediately runs `check_proximity_triggers()`.
4. **Fallen party members stayed at `0 HP` after battle victory**: `battle.rs` `Phase::Finish` revives `hp <= 0` heroes to `1 HP` upon `BattleOutcome::Victory` (SDLPAL `PAL_BattleWon` parity).
5. **Desktop replay auto-paused on in-game dialogue**: `pal-desktop` now pauses playback only when the exclusive Replay Menu (`replay_menu_open()`) is open, not for in-game `Dialog` / `BattleHud`.
6. **Mid-walk segment splits causing replay drift**: `pal-core` now splits automatic segments only when the overworld is truly at rest (`auto_walk.is_empty() && move_cd == 0 && npc_walks.is_empty() && held_dir < 0`).
7. **Pure-text `--obs fair` zero-screenshot playability (`#234`)**: Observable-effects `use`, field `magic`, real MGO `bbox ∩ viewport`, and GOP-pixel void/occlusion staggered minimap verified end-to-end by `v0.3.11/01_ch1_zero_shot_fair_blind.repal`.
8. **Pure player-input Text Mode recording (`#235` / `4be8c685`)**: `goto` and `talk <ev_id>` now dispatch standard player `TickInput::click` events instead of calling `plan_walk_portal_aware` / mutating `lead.dir` out-of-band, keeping `.repal` recordings 100% pure player inputs and zero-drift across all replay consumers.
