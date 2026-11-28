# AGENTS.md — PalRemake Project Instructions

**PalRemake** is a pure-static, zero-backend 4× HD web remake of the 1995 DOS RPG *Chinese Paladin* (`仙剑奇侠传`), built with **Rust + WebAssembly** (full-WASM software renderer & DOS UI), **native ES Modules**, **readable YAML scripts**, and an **offline Track-B AI HD asset pipeline**.

---

## 1. Non-Negotiable Architecture Constraints

1. **100% Pure Static Runtime (`site/`)**:
   - The `site/` directory deploys directly to GitHub Pages (`.github/workflows/deploy-pages.yml`).
   - **Zero runtime backend and zero runtime AI dependencies** — all unpacking, upscaling, and AI HD remastering happen strictly offline into static `.webp`, `.wav`, `.mid`, `.yaml`, `.json`, `.bin`, `.ttf`, and `.wasm` assets.
   - Keep `site/.nojekyll` intact and keep every file under `site/` strictly **`< 50 MB`**.
2. **Relative URLs Only (GitHub Pages Subpath Safety)**:
   - Never use root-absolute paths (`/assets/...`, `/wasm/...`, `/js/...`, `/css/...`) in `site/*.html`, `site/js/**`, or `site/css/**`; always use relative paths (`./assets/...`, `./wasm/...`) so subpath hosting (`/PalRemake/`) works.
   - Preserve `resolveAssetBaseUrl()` (`window.PAL_ASSET_BASE_URL` / `?assets=` override defaulting to `./assets`) in `site/js/host/asset-provider.js` for external CDN/R2 asset hosting (Section 5 of `docs/ARCHITECTURE.md`).
3. **Live `[F2]` Dual-Mode Parity**:
   - Pressing `[F2]` must always switch instantaneously between **`4× HD (1280×800)` + acoustic reverb audio** and **`1× 1995 Retro (320×200)` + OPL2 FM audio** across world maps, sprites, RNG cutscenes, battles, and audio.
   - Never delete or break `*_1x.webp` / `gop_*_1x.webp` / `mgo_*_1x.webp` retro assets.

---

## 2. Repository Layout

- **`crates/`** (Rust workspace, `Cargo.toml`):
  - `pal-core`: Shared 1995 DOS PAL engine logic — staggered-diamond isometric math & A* collision grid, MKF/YJ1/RLE decoders, battle math, NPC AI, dual-mode synth, and **`game`: the deterministic game core** (the single authoritative `World` state, the resumable script VM in `game/vm.rs`, battles, exploration, menus, saves, and segmented `RePal_CoreReplay_v3` recorder in `game/replay_rec.rs`). `script.rs` re-assembles the decompiled YAML steps into original `SCRIPTENTRY` operands (opcode table generated from `tools/pal_pipeline/decompiler.py` into `script_ops.rs`); `rng.rs` is the only randomness source. `state::GameState` is just the render/UI projection of `World`.
  - `pal-render`: Pure-Rust software framebuffer (`1280×800` HD / `320×200` retro), pull-based bounded 256 MB LRU `AssetCache`, Track-B HD chunk + spatial `<=2048×2048` occluder streaming, `shadowStripped` ground shadows, RNG cutscenes, battle spell VFX & floating damage numbers, and TTF/ASC font engine — shared across browser WASM and native `repal`.
  - `pal-ui`: Pure-Rust 1995 DOS UI state machine, input router, and framebuffer UI renderer (including trilingual `ZhHans`/`ZhHant`/`En`, Replay Chapters, and Title Options).
  - `pal-runtime`: Platform-neutral orchestrator (`PalRuntime`) wiring `pal-core` + `pal-render` + `pal-ui` + `DualSynth`, unified `replay_player::ReplayPlayer` (shared across Desktop, Web WASM, Headless `core-replay`, and `verify_repal`, with `debug_mode` gate & `annotations.json` AI subtitle playback), plus optional `disk` feature (local YAML/asset loader, `.repal.gz` `.tar.gz` reader/writer, and `.srt` subtitle exporter).
  - `pal-wasm`: `wasm32-unknown-unknown` target exporting `PalRuntime` (`core_*`, `wasm_*`, synth) into `site/wasm/pal.wasm` (~1.2 MB).
  - `pal-headless`: Native browserless library (`HeadlessApp`), deterministic PNG/golden replay helpers (`replay.rs`), and the **unified Text Mode engine** (`text_mode/{command,engine,response,transport,mod}.rs`: shared `TextEngine` + `TextHost` + `ChannelServer`) used identically by headless `repal --text` and live `repal --gui --text` / `--listen`, plus visual E2E & Text Mode integration tests.
  - `pal-desktop`: Produces the **unified native binary `repal` / `repal.exe`** (`winit` + `softbuffer` GUI, `cpal` audio, SHA-256 `assets-lock.json` bootstrapper, zero-dep self-updater, `.repal.gz` replay theater, and thin `TextHost` bridge in `text_host.rs`).
  - `pal-cli`: Multi-threaded offline `DoSPal.zip` unpacker & procedural 4× fallback baker.
- **`site/`** (deployable static site — **no legacy JS Canvas2D `renderer.js` or DOM `hud.js`**):
  - `index.html`, `wasm/pal.wasm`, `css/pal-ui.css`, `sw.js`, `manifest.json`
  - `js/main.js` (boot + input), `js/host/core-host.js` (ticks the core, services its commands, saves, frame-exact replay), `js/host/` (`wasm-runtime.js`, `asset-provider.js`, `wasm-presenter.js`, `gamepad.js`, `repal-loader.js`, `replay-theater.js`), `js/wasm/wasm-bridge.js`, `js/audio/`, `js/mods/`. **There is no game logic in JavaScript.**
  - `assets/` (gitignored; hosted in `HclX/RePal_Assets` and SHA-256 pinned in `assets-lock.json`; symlinked locally as `site/assets -> ../../RePal_Assets/assets`): `data/*.yaml`, `data/scripts/scripts_*.yaml`, `data/i18n/en/*.json`, `graphics/` (`maps_hd/`, `gop/`, `mgo/`, `fbp/`, `rng/`, `ui/`), `maps/map_XXX.bin`, `audio/`, `fonts/`, `mods/`, `orig_saves/` (51 `.rpg` saves).
- **`tools/`**:
  - `track_b_pipeline.py`: Generalized offline Track-B HD remaster pipeline (`status`, `prepare`, `bake`, `verify-map`, `repack-occluders`, `repack-all-occluders`, `rebake-map`, `prepare-cutout-patches`, `calibrate-map`, `calibrate-all-maps`).
  - `build_assets.py`: Unpacks `DoSPal.zip` via `pal-cli` into `site/assets/`.
  - Automated verification suites (wired into `.github/workflows/ci.yml`): `cargo test --workspace` (incl. `crates/pal-core/tests/`: script assembly, native story runs, parity scenarios against `tools/parity/golden/`, NPC/battle/item/story regressions, 51 DOS saves), `test_rust_core_host.mjs`, `tests/test_host.mjs`, `tests/test_headless_harness.mjs`, `tests/test_pal_remake.py`.
  - `inspect_report.py`, `audit_script_parity.py`: Diagnostic and script-parity utilities.
- **`docs/`**:
  - [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): Unified `pal-runtime` + WASM + `repal` native architecture, Track-B tiled streaming, CI matrix, and `HclX/RePal_Assets` pinning.
  - [`docs/TEXT_MODE_SPEC.md`](docs/TEXT_MODE_SPEC.md): Specification for `repal`'s unified Text Mode (`GUI` / `Text` / `GUI+Text`, `stdio` / `socket`, `plain` / `--json`).
  - [`docs/USER_MANUAL.md`](docs/USER_MANUAL.md): End-user guide for the `repal` native release binary.
  - [`docs/HD_REMASTER_METHODOLOGY.md`](docs/HD_REMASTER_METHODOLOGY.md): Full technical spec & runbook for Track-B HD remastering. Read this before modifying `track_b_pipeline.py` or remastering scenes.

---

## 3. Core Engine & Gameplay Invariants

- **WASM Renderer Rebuild**: Rendering & UI run exclusively in `pal-render` + `pal-ui` compiled into `site/wasm/pal.wasm` (gitignored build artifact — never commit compiled `.wasm` binaries). After modifying any Rust crate, rebuild locally via `make build-wasm` (CI and GitHub Pages workflows build `site/wasm/pal.wasm` automatically) and run `cargo test --workspace` + all Node test suites.
- **DOS PAL Isometric Diamond Trigger Metric**:
  - Proximity (`trigger_mode >= 4`): `|ev.x - lead.x| + |ev.y - lead.y| * 2 < (trigger_mode - 4) * 32 + 16`.
  - Search (`trigger_mode` `1..3` around `probe = lead + step(dir)`): `|ev.x - probeX| + |ev.y - probeY| * 2 < (trigger_mode - 1) * 32 + 16`.
  - Never replace this with an axis-aligned bounding box.
- **Determinism (non-negotiable)**: All game logic lives in `pal_core::game` and advances only through `Game::tick` (fixed 20 ms) and `Game::reply`. Never read clocks, `Math.random`, or platform RNGs in the core; draw every random number from `World::rng` (it is saved). Host load waits (`is_blocked_on_host`) must not advance `tick_count`, so replays stay frame-exact. Never move game rules back into JavaScript.
- **Host contract**: On `HostCmd::LoadScene` the host uploads the map tiles (`core_set_map`) and the `mgo_XXX.json` frame counts of `core_scene_sprite_ids()` (`core_set_sprite_frames`) before replying `SceneLoaded`, so idle NPCs get `sprite_frames_auto`.
- **Same-Scene `screen_fade_out` Auto-Fade-In**: Preserve `VmContext::need_fade_in` (mirroring `SDLPAL`'s `fNeedToFadeIn`) so scripts that fade out and reposition actors without `scene_change` fade back in cleanly.
- **Parity baselines**: `tools/parity/golden/*.json` were recorded from the retired JS runtime; any behaviour change that alters them must be intentional and documented in the commit.
- **Script & Terminology Integrity**: Preserve authentic 1995 Chinese Paladin terms (e.g., `乾坤一掷`, never `干坤一掷`) and keep all symbolic jump targets valid across `site/assets/data/scripts/scripts_*.yaml`.
- **Browser Cache Busting**: Bump `ASSET_VER` in `site/js/host/asset-provider.js` and `?v=...` in `site/index.html` when updating existing static assets (`track_b_pipeline.py bake` does this automatically).

---

## 4. Track-B Offline HD Remastering Guidelines (`tools/track_b_pipeline.py`)

Read [docs/HD_REMASTER_METHODOLOGY.md](docs/HD_REMASTER_METHODOLOGY.md) for full details. Note: Scene 61+ generation is currently **paused** pending the external asset hosting strategy (Section 5 of [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)) so `site/` stays safely under GitHub Pages' ~1 GB limit.

1. **Per-Scene Workflow**:
   ```bash
   python3 tools/track_b_pipeline.py status
   python3 tools/track_b_pipeline.py prepare --scene <N> --work-dir <SCRATCH_DIR>/sc<NNN> --chain
   # Generate 1376x768 remasters sequentially (each item's own `prompt`) -> write <SCRATCH_DIR>/sc<NNN>/ai_outputs.json
   # Chained maps: run `chain-next --scene <N> --work-dir ...` after each round until it reports all patches done
   python3 tools/track_b_pipeline.py bake --scene <N> --work-dir <SCRATCH_DIR>/sc<NNN>
   ```
   - **Prepare lazily** (right after prior scenes bake) so `find_story_variant_donor` (`≥68%` identical `1×` pixels) skips redundant patches.
   - **Chained generation** (`--chain`): each later patch's input carries its already-painted neighbour pixels and a `CHAINED TILE` prompt clause; the model must keep those pixels and continue their texture. Use `--redo-map` to regenerate an already-baked map.
   - **Never commit `--work-dir` or scratch files**; put temporary workdirs, git worktrees, `TMPDIR`, and headless PNG exports under the gitignored workspace `.tmp/` directory (avoid `/tmp` which may be a small RAM-backed `tmpfs`).
2. **Image Generation Discipline**:
   - Run **at most 1 image-to-image call at a time (sequentially)** to avoid `429` burst rate limits.
   - Pass **only the single patch/sheet input image** per generation call so `1:1` isometric geometry stays pixel-locked (`shift_2x ≈ (0,0)`).
   - Retry transient `503`/RPC failures once; on `429 QUOTA_EXHAUSTED`, stop immediately (do not retry in a loop) and record partial progress in `ai_outputs.json`.
   - Always inspect/check outputs for hallucinated props (no invented items on bare counters/floors, no invented cliffs/stairs over flat dark slate-grey `#1E2428` water, no lit flames on unlit lanterns, no faces on back-facing character sprite frames).
3. **Coordinate-Stable Chunk & Spatial Occluder Naming**:
   - Map chunks use grid-coordinate names (`isl{id}_r{row}_c{col}_4x.webp`; `map_012` retains `room1_auntie`..`room6_miao`). Never renumber chunk filenames during rebakes or calibrations.
   - Foreground occluders are partitioned by `512×512` logical quadrants into `<= 2048×2048` HD WebP pages (`occ_r{row}_c{col}_p{idx}_4x.webp`) referenced by `meta.json` (`occluders.pages` + compact `strips[].page`).
4. **Hard Verification Gates (`verify_map`) — Never Bypass**:
   - Every `bake`, `rebake-map`, `calibrate-map`, and `repack-occluders` enforces **5 hard gates**:
     1. **Zero interior holes**: `0` pixels matching `binary_fill_holes(alpha > 200) & (alpha <= 200)`.
     2. **Walkable occlusion parity**: Differential `1×` vs `4×` sweep across all reachable half-tiles & active events (`max_FN ≤ 16`, `max_FP ≤ 16`).
     3. **Cross-scene canonical Lab consistency (Issue #8)**: Anchored via `calibrate_hd_island_to_canonical_1x` + adaptive `4×` texture normalization (`|dL|, |da|, |db| ≤ 3.0`, `dE ≤ 4.0`, `LstdR ∈ [0.72, 1.45]`), with pure `#000000` doorways/cave portals preserved.
     4. **Anti-hallucination block check**: Every valid `128×128` block has `MAE ≤ 42.0` vs canonical target.
     5. **Spatial occluder texture cap**: Every occluder page in `occluders.pages` exists and satisfies `width ≤ 2048` and `height ≤ 2048`.
   - When modifying map pixels in-place, always keep spatial occluder page RGB synchronized with the calibrated chunks (`calibrate-map` / `calibrate-all-maps` handles this while preserving verified alpha and pure-black cave tunnel portals).

---

## 5. Build, Preview & Test Commands

```bash
# Local static preview (open http://localhost:8080/site/)
python3 -m http.server 8080

# Rust tests (game core, render, UI, runtime, pal-headless visual E2E + Text Mode, repal) & builds
cargo test --workspace
make build-wasm
make build-repal

# Unified native binary smoke check (headless Text Mode)
target/release/repal --text --json --save 0.rpg -c "look; sw 3"

# Headless browser-host suites (boot the real site on pal.wasm)
node tools/test_rust_core_host.mjs
node tools/tests/test_host.mjs
node tools/tests/test_headless_harness.mjs

# Python data/static/pipeline tests & HD map verification
python3 -m pytest tools/tests/ -v
python3 tools/track_b_pipeline.py verify-map --map <ID>
```

---

## 6. Git & Hygiene Conventions

- Use clear conventional commit messages (`feat(hd): Track-B HD Scene <N> (...)`, `fix(hd): ...`, `feat(headless): ...`, `fix: ...`).
- For continuous HD remastering, commit and push **one verified scene per commit** (clean, easy to review and bisect).
- Fetch and rebase onto `origin/main` before pushing if upstream commits landed while working locally.
- Never commit scratch directories, `.tmp/`, temporary logs, `target/`, or untracked binary dumps.
- **Temporary Files & Worktrees**: Always place temporary files, `TMPDIR`, `pal-headless` screenshot dumps, and `git worktree` checkouts inside the gitignored workspace `.tmp/` directory (`<repo-root>/.tmp/`) rather than `/tmp/` so small `tmpfs` RAM disks are never exhausted.
