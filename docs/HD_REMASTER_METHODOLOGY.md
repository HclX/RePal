# PalRemake Offline AI HD Remastering Methodology (Track B Reproduction Spec)

> **Purpose**: This document is a complete, self-contained technical specification and step-by-step playbook written so **any AI coding agent or human developer** can deterministically reproduce, debug, and extend PalRemake's **Track-B Faithful Offline AI 4× HD Remaster** across all 294 scenes, 221 maps, 567 `MGO` sprite sheets, and `RGM` dialogue portraits — while keeping the runtime game **100% static HTML + ES Modules + Rust/WASM + WebP/JSON with zero runtime AI dependency**.

---

## 1. Problem Statement & Root-Cause Diagnosis

### Why Traditional Per-Tile / Per-Atlas Procedural 4× Upscalers Fail on 1995 DOS Pixel Art
In classic 1995 DOS *Chinese Paladin* (`仙剑奇侠传`), exploration scenes run at `320×200` logical resolution using a staggered isometric half-tile grid (`128 × 64 × 2` diamonds of `32×15` pixels stored in `MAP.MKF` + `GOP.MKF`), `MGO.MKF` RLE character/prop sprites, and `RGM.MKF` portraits, all quantized to a single 256-color (`6-bit VGA`) indexed palette.

Attempting to upscale individual `32×15` tiles (`gop_XXX_1x.webp -> gop_XXX_4x.webp`) or raw sprite atlases with procedural filters (`xBR`, `HQ4x`, `waifu2x`, bilateral + unsharp) fails visually for four structural reasons:

1. **Diamond Tile Seam Grid Artifacts**: A `32×15` isometric tile has diagonal 1-bit staircase alpha borders. Upscaling each `32×15` tile in isolation loses cross-tile context (wood floor planks, wall wainscots, roof shingles, water ripples) and creates visible `128×60` diamond grid seams at runtime.
2. **Baked 8-Bit Dithering Amplified as Plastic Blotches**: Original artists simulated gradients on wood, stone, skin, and fabric via 2-color checkerboard dithering. Pixel-art upscalers mistake dither dots for intentional geometric features, turning wood grain and faces into melted, posterized blotches.
3. **Baked 1-Bit Checkerboard Foot Shadows on Sprites**: Character sprites in `MGO.MKF` bake dark-green/brown `(0, 36, 0)` / `(48, 28, 16)` checkerboard pixels directly underneath feet. Upscaling these turns shadows into jagged dark blobs hovering on top of HD floors.
4. **Zero High-Frequency Material Semantics**: A `22×45` character sprite or `48×60` portrait lacks true hair strands, fabric folds, wood joinery, or iris detail at `1×`; only generative image-to-image remastering conditioned on the exact `1×` composition can synthesize faithful Chinese brushwork (`工笔重彩 / 国风手绘`) material detail while preserving the 1995 silhouette and color palette.

---

## 2. End-to-End Architecture (`Track B: Faithful Offline AI HD Remaster`)

### Core Design Constraint
* **Zero Runtime AI Dependency**: All generative AI happens **strictly offline** at asset-build time.
* **Pure Static Runtime**: The browser engine (`site/`) loads only static `.webp` images and `.json` metadata (`site/assets/graphics/maps_hd/map_XXX/`, `mgo/mgo_XXX_4x.webp`, `ui/rgm_4x.webp`) and supports instant live `[Tab]` toggling between `4× HD (1280×800)` and `1× 1995 Retro (320×200)`.

```
Offline Asset Build (tools/track_b_pipeline.py)
┌──────────────────────────────────────────────────────────────────────────────┐
│ 1. PREPARE (100% deterministic Python)                                       │
│    • Composite 1× MAP.MKF + GOP.MKF into full 2048×2048 map + sortY buffer   │
│    • Compute camera-reachable pixel envelope via Doorway-Bounded Walkable BFS│
│    • Frame indoor islands or overlapping 688×384 patches -> 1376×768 (43:24) │
│    • Strip DOS foot shadows & pack unbaked MGO sprites (3-per-row aligned)   │
│    • Pack unbaked RGM portraits into 5×2 identity-locked 1376×768 grids      │
├──────────────────────────────────────────────────────────────────────────────┤
│ 2. GENERATE (Offline Multimodal Image-to-Image AI: Gemini / Nano Banana / etc)│
│    • 1 image-to-image call per 1376×768 input sheet (sequential, rate-safe)  │
│    • Preserves exact geometry, perspective, palette & pose; upgrades texture │
├──────────────────────────────────────────────────────────────────────────────┤
│ 3. BAKE (100% deterministic Python)                                          │
│    • 2D Sobel-edge cross-correlation registration (sub-pixel lock to 1×)     │
│    • Overlap affine RGB gain/bias color matching + seam-cut stitching        │
│    • Canonical 1×-anchored joint-guided Lab calibration + texture norm       │
│    • Story-variant HD tile inheritance (exact donor pixels on shared 1×)     │
│    • Painter's-exact cumulative-depth foreground occluder strip extraction   │
│    • Silhouette-gated chroma de-matting, decontamination & foot stabilization│
│    • Voronoi RGB alpha-border extrusion -> static WebP + meta.json           │
└──────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
Runtime Static Engine (crates/pal-render + site/js/host/ — ZERO AI Dependency)
  Pass 1: Stream & blit visible HD room/chunk WebPs (`maps_hd/map_XXX/isl*_r*_c*_4x.webp`)
  Pass 2: Collect visible Y-sorted `occ_r*_c*_p*_4x.webp` (<=2048x2048) strips + `EventObject` & Party MGOs
  Pass 3: Sort strictly by `sortY`; draw soft elliptical ground shadows + 4× sprites + occluders in Rust/WASM FB
```

---

## 3. Resolution & Aspect-Ratio Invariant (`688×384` -> `1376×768` -> `2752×1536`)

### The Aspect-Ratio Trap & Exact Solution
Modern image-to-image generators (including `gemini-3.1-flash-image` / Nano Banana) snap output dimensions to fixed native aspect buckets — specifically **`1376 × 768` (`43:24` = `1.791667`)** for landscape inputs.
* **Trap**: If you feed a `1280×800` (`16:10`) or arbitrary-sized room crop, the model either crops top/bottom margins or non-uniformly stretches X vs Y, destroying tile collision alignment!
* **Exact Invariant**:
  1. Every `1×` map patch is framed in an **exact `688 × 384` logical pixel box** (`688 / 384 = 43 / 24`).
  2. Nearest-neighbor upscale `2×` to **`1376 × 768`** as the reference input image fed to the AI model.
  3. The AI model returns an exact **`1376 × 768`** (`2×`) remastered image with **zero aspect-ratio distortion**.
  4. Register integer `(dx2, dy2)` translation at `2×` via Sobel-edge correlation (`register_translation_2d`, typically `(0, 0)` or `±1 px`), then upscale `2×` (`Lanczos` + mild `UnsharpMask(radius=1.1, percent=65, threshold=2)`) to **`2752 × 1536`** (`4×` logical world resolution, matching `1280×800 = 320×200 × 4`).

### Sub-Pixel Edge Registration (`register_translation_2d`)
```python
# Compute Gaussian-smoothed luminance Sobel edge magnitude on both 2x reference and 2x AI output
# Zero outer 4px border, then maximize dot-product sum(E_ref * shift(E_ai, dx, dy)) for dx, dy in [-10..+10].
```
Because structural edges (walls, doorways, furniture, shorelines, paths) stay aligned even when dithered textures become smooth painted wood/stone, Sobel edge correlation locks registration to `0` pixel drift.

---

## 4. Map Decomposition: Indoor Islands vs. Large Seamless Maps

### Step 4.1: Exact Full-Map 1× Compositing & `sortY` Buffer
From `map_XXX.bin` (`128 × 64 × 2 × 4` bytes) and `gop_XXX_1x.webp`:
* Each half-tile `(ty, tx, h)` draws at `dx = tx * 32 + h * 16 - 16`, `dy = ty * 16 + h * 8 - 8`.
* Bottom layer (`bl == 0`) and ground decals (`raw_top > 0` with `tl == 0`) composite onto ground (`sortY = -1`).
* Elevated tiles (`bl > 0` or `tl > 0`) sort by DOS PAL's exact formula:
  $$\text{sortY} = \text{ty} \times 16 + h \times 8 + 7 + \text{layer} \times 8$$
* Composite elevated tiles in ascending `sortY` order into `full_1x` (`2048 × 2048` RGBA), while recording at each pixel `(y, x)` the maximum elevated `sortY` that wrote an opaque pixel (`sort_y_1x[y, x]`, or `-1` if pure ground).

### Step 4.2: Camera-Reachable Envelope via Doorway-Bounded Walkable BFS
Many DOS PAL maps fill unused exterior regions of the `2048×2048` canvas with a repeated filler tile (`tid = 19` dirt or `tid = 115` cavern rock) without marking those exterior tiles `blocked`, relying on room walls + doorway `scene_change` triggers to keep the player inside.
To avoid wasting AI generations on unreachable exterior filler:
1. Gather all scenes sharing `map_id`.
2. Collect seed positions from:
   * Every `party_set_pos` adjacent to `scene_change -> scene_id` across all `scripts/*.yaml` (including 1-hop `call_script` / `jump` helpers).
   * Every non-zero `EventObject` position `(ev.x, ev.y)` in those scenes, plus any `camera_pan` targets.
3. Identify **doorway/exit proximity triggers**: events with `trigger_mode >= 4` whose trigger script (or 1-hop `call_script`) executes `scene_change`.
4. Run BFS on `blocked == False` half-tiles from seeds, **recording doorway tiles as visited but NOT expanding past them** (`abs(de.x - px) + abs(de.y - py) * 2 < (de.trigger_mode - 4) * 32 + 16`).
5. Every visited half-tile `(px, py)` (and event/pan seed) marks its camera viewport `[px - 184 .. px + 184, py - 136 .. py + 112]` as `camera_visible_1x`.
6. **Zero-Hole Masking (`binary_fill_holes`)**:
   * Keep `bs >= 0` in `render_map_1x_and_sort_y()` (so valid `gop` tile `#0` grass/floor sprites are never skipped).
   * Apply `scipy.ndimage.binary_fill_holes()` both to `camera_visible_1x` (filling enclosed interior camera/forest cavities intersected with `tile_occ_1x`, such as dense unwalkable willow groves) and to `isl_valid_1x` / chunk alpha during `bake_map_hd()` so **zero** interior `alpha <= 200` pinholes or voids can ever appear inside any HD map island.

### Step 4.3: Island Clustering & Reference-Guided Seam-Cut Stitching
Label connected components (dilated by `8px`) of `visible_1x`:
* **Case A — Compact Indoor Room (`w <= 672` and `h <= 368`)**:
  * Frame the entire island inside **1 single centered `688 × 384` box** (`bx0, by0` multiples of 4), masking pixels outside that island to black `(0, 0, 0)` so neighboring rooms never cross-contaminate.
* **Case B — Large Room or Continuous Outdoor/Dungeon Map (`w > 672` or `h > 368`)**:
  * Cover the island's bounding box `[ix0..ix1, iy0..iy1]` with a sliding grid of `688 × 384` boxes using **`overlap_x >= 96` (`384` HD px) and `overlap_y >= 80` (`320` HD px)** (`stride_x <= 592`, `stride_y <= 304`).
  * During **Bake** (`bake_map_hd()`):
    1. Register each patch `(dx2, dy2)` against its true `2×` reference crop.
    2. **Drift-Free Overlap Color Harmonization**: Fit a shared luminance gain (zero multiplicative per-channel hue twist) + tightly clamped chroma offset (`±7.5`), followed by a seam-localized (`w_lf / (w_lf + 0.18)`) `sigma = 18` (`1×` px) illumination delta on agreement pixels and a domain-isolated flat-water/surface residual bridge (`sigma = 12`, gated by low high-pass structural energy on both patches and `1×` reference) so multi-row 28-patch outdoor maps never drift chromatically while river/pond tone shifts bridge smoothly without bleeding onto shorelines.
    3. **Reference-Guided Minimum-Error Seam-Cut Stitching + Structure-Gated Feathering (Zero Ghost Props & Zero Water Sawtooths)**: Wide linear/cosine feather blending across `288 px` turns single-patch AI hallucinations or slightly shifted props into 50%-opacity "ghost" items. Instead, `compute_seam_cut_blend_weight_4x()` solves a dynamic-programming minimum-error boundary cut (`seam_cost = |prev - cur| + 0.8 * min(|prev - ref|, |cur - ref|) + edge_penalty`, lightly smoothed on flat terrain) with exponential border barriers so seams route around tables and furniture through flat floor/grass/water tiles. Feather width adapts by **both color discrepancy and local high-pass structural energy** (`struct_gate`): razor-sharp `~3.5 HD px` (`sigma = 1.2`) right around structured props/tables/walls, `~44 HD px` (`sigma = 11.0`) across textured grass/stone, and `~96 HD px` (`sigma = 24.0`) across textureless open water (eliminating DP staircase teeth). Bidirectional high-pass reference-fidelity override swaps out localized hallucinated props, and a pre-harmonization **Black-Void Guard** (`raw < 16` where `ref_max >= 12`) + redundant `uniq_vis == 0` void-patch pruning prevent `#000000` border cutouts from overwriting valid imagery.
    4. **Cross-Scene Style, Color, Brightness & Texture Consistency (Issue #8 Guarantee)**:
       * **Canonical 1×-Anchored Joint-Guided CIE Lab Calibration (`calibrate_hd_island_to_canonical_1x`)**: Every stitched HD island is calibrated BEFORE slicing chunks or extracting occluders to one universal `1×`-anchored canonical curve `T(ref_1x)` (`L_t = clip(1.04 * L_ref + 2.2 * shadow_ramp, 0, 96)`, `a_t = 0.98 * a_ref`, `b_t = 0.98 * b_ref`, with smooth shadow roll-off below `L* = 10` so pure `#000000` doorways/cave portals stay pitch black). It applies global luminance contrast standardization (`target_std = 1.08 * ref_std`), exact 100% global mean Lab alignment, and an edge-preserving masked guided filter (`radius = 14`, `eps = 18/24`, blend `0.85`) guided by `1×` reference channels so grass, dark slate river water (`#1E2428`), dirt, stone, and wood never bleed across shorelines or wall edges yet land identically across connected maps.
       * **Adaptive Multi-Scale Texture/Brushstroke Normalization**: Replaces blind per-patch `UnsharpMask` with adaptive `L_4x` detail scaling (`sigma = 1.6`, target Lab gradient `3.2` measured strictly on textured `|∇_1x L_ref| > 3.5` regions, `k_detail ∈ [0.72, 1.55]`, gated down to `0.2` over flat calm water) so soft painterly maps and hyper-crisp maps share one unified brushwork crispness.
       * **Story-Variant HD Tile Inheritance (`find_story_variant_donor` / `apply_story_variant_inheritance_4x`)**: Automatically detects already-baked HD donor maps sharing `>= 85%` identical `1×` pixels (`>= 50%` coverage — e.g., `Map 025 ← Map 006`, `Map 050 ← Map 031`, `Map 051 ← Map 033`, `Map 131 ← Map 042`). During `prepare`, unchanged patch boxes are skipped automatically; during `bake` / `calibrate-all-maps`, unchanged `1×` pixels inherit the donor's exact calibrated `4×` RGB with a smooth `20 HD px` feathered blend into story-changed tiles.
       * **Exposure-Locked, Even-Texture Prompts**: `build_map_patch_prompt()` enforces strict exposure, dark-water (`#1E2428`), anti-hallucination, tile-artifact (natural edges instead of diamond staircases between ground materials) and even-texture (no per-tile vignettes or detail clusters) rules. The model only ever receives the single patch image, so the prompt never references other images.
       * **Chained Neighbour-Aware Generation** (`prepare --chain` + `chain-next`): patches of an island are generated one round at a time; each new patch input has every pixel already covered by earlier stitched patches replaced with those final HD pixels, plus a `CHAINED TILE` prompt clause naming the painted edges, so the model continues the existing texture instead of inventing a new one. `chain-next` stitches with the same `_stitch_island_patches_4x()` as `bake`.
       * **Two-Band Seam Blend** (`_two_band_seam_blend_4x()`): lighting/colour (low band) is blended wide across the seam-cut while brushwork (high band) switches narrowly at the cut line with texture-energy matching, so independently painted textures never cross-fade into a blurry band. Patch grids keep at least `128×112` (`1×`) overlap.
    5. Slice the calibrated `4×` canvas into non-overlapping `<= 512 × 512` logical (`2048 × 2048` HD) WebP chunks (`quality=90, alpha_quality=100, exact=True, method=4`) named deterministically by `(row, col)` grid coordinate (`isl{id}_r{row}_c{col}_4x.webp`).

---

## 5. Painter's-Exact Foreground Occluder Extraction & Differential Verification (`occ_r*_c*_p*_4x.webp`)

### Why Foreground Occluders Are Needed
When tiles are baked into seamless room/chunk base images, elevated foreground objects (front walls, pillars, doorways, tree canopies, table fronts, railings) are part of the base painting. Without foreground occluder overlays, characters walking *behind* a front wall or pillar would be drawn *on top* of that wall!

### Step 5.1: Exact Walkable Foot Grid (`build_char_sy_grid_1x`) & Y-Window Filtering
An elevated pixel `(x, y)` with `sub_sy >= 0` can occlude a character or active `EventObject` standing behind it when that entity's feet lie **below** `y` (`py in [y - 8 .. y + 64]`, `px in [x - 16 .. x + 16]`) with `charSortY = py + 8 < sub_sy[y, x]`:
* Build `char_sy_grid` (`2048 × 2048`) directly from `map_XXX.bin`'s walkable half-tiles (`~blk[ty, tx, h]` -> `py + 8`, plus `8px` step midpoints between adjacent walkable half-tiles, plus all scene `EventObject` coordinates) so **elevated walkways** (bridges, 2nd-floor inn balconies, staircases, and stages where `sub_sy >= 0`) are preserved 100%.
* Filter `occluder_1x = (sub_sy >= 0) & (sub_sy > fy_min_char)` via a separable `(73, 33)` `minimum_filter`.

### Step 5.2: CRITICAL PITFALL — Never Merge Occluders Vertically Across Different `sortY`!
* **Postmortem (Bug 1 — Bookshelf Swallowing Player at `(400, 344)` in Map 12)**:
  In `room1_auntie`, a back-wall bookshelf (`y = 271..335`, true `sortY <= 335`) touches a front red-lattice wall (`y = 336..372`, `sortY = 375`) in 2D screen space at `x = 400`. Grouping vertically connected elevated pixels into one strip with `max(sortY) = 375` draws the back bookshelf *over* a player standing in front of it!
* **Postmortem (The Complementary Seam Trap)**:
  Conversely, slicing adjacent `sortY` bands independently with `50%` antialiased alpha at shared boundaries composites as `1 - (1 - 0.5)(1 - 0.5) = 0.75` alpha, letting `25%` of dark hair bleed through wall seams.

### Step 5.3: Exact PAL `sortY` Cumulative-Depth Strips + Color-Guided Edge Snapping
DOS PAL elevated tile depths `tileSortY = 8 * (2 * ty + h + layer) + 7` already lie on a sparse `8px` lattice — **zero quantization** is needed (`bval = exact sub_sy`):
1. For each exact `bval` and each 8-connected component `cc` of `(exact_sy == bval)`:
   * Compute exact cumulative depth `cum_crop = (sub_sy >= bval)` and pad `cc` by `1` logical pixel strictly into higher-depth neighbors (`sub_sy > bval`) so adjacent front bands overlap seamlessly at `alpha = 255`.
   * **Color-Guided Edge Snapping (`_snap_occluder_alpha_edges_4x`)**: Restricted strictly to each strip's exact `exact_sy == bval` rim (`exact_bmask_1x`) so overlapping cumulative depth bands never compound alpha, snap `a4` outward by up to `2 HD px` (`alpha <= 190`) over adjacent ground floor (`sub_sy < 0`) only where `isl_rgb_4x` is much closer to the local inner occluder RGB than to the outer floor RGB.
2. **Spatially Paginated Packing (`<= 2048 × 2048` per page)**: Partition strips by `512 × 512` logical (`2048 × 2048` HD) map quadrants `(row, col)` (splitting any single strip wider or taller than `2048` HD px into sub-strips), then shelf-pack each quadrant's strips into one or more `<= 2048 × 2048` WebP pages (`occ_r{row}_c{col}_p{idx}_4x.webp`, `quality=92, alpha_quality=100, exact=True, method=4`). Write `occluders.pages` (`[{file, w, h, wx, wy, ww, wh}]`) and compact `strips` (`{page, sx, sy, sw, sh, dx, dy, sortY}`) to `meta.json`. This bounds every decoded texture to `<= 16 MB` RGBA and allows `pal-render` to stream only viewport-intersecting quadrant pages instead of decoding multi-hundred-MB monolithic occluder atlases.

### Step 5.4: Hard-Gated Automated Verification (`verify-map`)
Every `bake_map_hd()` / `rebake-map` / `calibrate-map` / `repack-occluders` run automatically invokes `verify_map(map_id)` (also available standalone as `python3 tools/track_b_pipeline.py verify-map --map <ID>`):
1. **Zero-Hole Check**: Scans every chunk WebP in `maps_hd/map_<ID>/` and asserts `binary_fill_holes(alpha > 200) & (alpha <= 200)` count == `0`.
2. **Differential Walkable Silhouette Sweep**: Across every reachable walkable half-tile `(tx, ty, h)` and active `EventObject`, compares `1×` ground-truth `sort_y_1x > py + 8` against reconstructed HD occluder pages (`strip.sortY > py + 8` Porter-Duff alpha composited and downsampled `4×4 -> 1×`) inside `[px - 10 .. px + 10, py - 42 .. py + 4]`, failing with exact `(px, py)` coordinates if any spot has `FN > 16` (character embedded in wall) or `FP > 16` (phantom occlusion).
3. **Canonical Photometric & Contrast Consistency Gate**: Asserts the stitched `1×`-downsampled HD map stays locked to canonical `T(ref_1x)` (`|dL_canon| <= 3.0`, `|da_canon| <= 3.0`, `|db_canon| <= 3.0`, `dE_canon <= 4.0`, and `Lstd_ratio ∈ [0.72, 1.45]` when `ref_std >= 4.0`).
4. **Anti-Hallucination Local Structural Block Gate**: Scans all valid `128×128` logical blocks (`>= 2,500` opaque non-black pixels) and fails if any block exceeds `MAE > 42.0` vs `T_rgb(ref_1x)`, catching gross structural hallucinations (such as invented staircases/cliffs over flat river water).
5. **Spatial Occluder Page Size Gate**: Asserts every occluder WebP page listed in `meta.json` (`occluders.pages`) exists on disk and satisfies `width <= 2048` and `height <= 2048`.

---

## 6. MGO Character & Prop Sprite Remastering

### Step 6.1: Stripping DOS 1-Bit Checkerboard Foot Shadows (`strip_dos_shadow_1x`)
For character sprites (`is_prop == False`), scan the bottom `70%` (`y >= 0.30 * h`) for DOS shadow seed greens `{(0, 36, 0), (0, 32, 20), (0, 28, 0)}` and flood-fill 8-connected pixels belonging to the DOS shadow palette `{(48, 28, 16), (28, 20, 12), (48, 20, 8), (52, 0, 0), (36, 20, 12), (20, 16, 8)}`, setting their RGBA to `(0, 0, 0, 0)`.
* Mark `"shadowStripped": true` in `mgo_XXX.json` so `pal-render` (`crates/pal-render/src/world.rs`) draws a smooth, anti-aliased multi-ring elliptical ground shadow (`draw_soft_ellipse_shadow`) underneath the character's feet in HD mode!

### Step 6.2: Grid Packing & The 3-Frame Directional Row Rule
* **Background Matte**: Medium-dark cool slate-teal `rgb(46, 62, 72)` with `2px` `rgb(34, 46, 54)` cell divider lines on a `1376 × 768` sheet.
* **CRITICAL RULE (Preventing Front/Back Feature Bleed — Bug 2 Postmortem)**:
  Standard DOS PAL walking `MGO` sheets have `12` frames = `4 directions × 3 frames` (`d0=SW front-left [0,1,2]`, `d1=NW back-left [3,4,5]`, `d2=NE back-right [6,7,8]`, `d3=SE front-right [9,10,11]`).
  * **Never** place frame `#8` (back-facing) on the same row immediately adjacent to `#9` (front-facing) via an `8×3` grid — AI models occasionally bleed front-facing headbands/eyes onto `#8`, causing a 1-in-3 step flicker!
  * **Always use a column count divisible by 3 (`cols = 6` or `cols = 9`) for 12-frame directional characters** so every directional triplet `[3d, 3d+1, 3d+2]` sits contiguously on the same row!
* **Automatic Triplet Head Consistency Guard**:
  During `bake`, for any 12-frame character sheet, check the back-facing triplets `[3, 4, 5]` and `[6, 7, 8]`. If 2 of the 3 frames agree on upper-head bright-pixel ratio (`R>170 & G>170 & B>170` in top `32%` of height) while the 3rd frame spikes by `> 2.2×` (e.g., a stray white headband on back of head), automatically harmonize the outlier's upper head from its nearest silhouette-matched sibling (`IoU > 0.90`).

### Step 6.3: Silhouette-Guided Chroma De-Matting & Anti-Wobble Alignment
For each cell in the AI output sheet:
1. Estimate actual cell background color `bg_col` from the robust median of the `4px` cell perimeter (within Euclidean distance `35` of `(46, 62, 72)`).
2. Upscale the clean `1×` reference alpha mask to `4×` (`sw*4 × sh*4`):
   * `outer_gate = binary_dilation(sil_4x, iterations=6)` (`4` for props, `8` for portraits) — **eliminates stray background noise while allowing natural 4× curves**.
   * `inner_core = binary_erosion(sil_4x, iterations=2)` (`4` for portraits) — **protects interior highlights/eyes/metal from ever turning semi-transparent even if their RGB resembles slate-teal**.
3. Compute smooth chroma ramp `raw_a = clip((||rgb - bg_col|| - 13) / 20, 0, 1)`, zero outside `outer_gate`, set `1.0` inside `inner_core`, keep only 8-connected components overlapping `sil_4x`.
4. **Color Decontamination**: On semi-transparent edge pixels (`0.08 < raw_a < 0.95`), un-mix `bg_col`:
   `rgb_clean = clip((rgb - (1 - max(0.25, raw_a)) * bg_col) / max(0.25, raw_a), 0, 255)`.
5. **Foot Baseline & Horizontal Centroid Lock**: Shift character frames by `(dx, dy)` (clamped to `±4` HD px) so `max(y)` (foot baseline) and `mean(x)` match `sil_4x` — preventing 1–2px jitter during walk cycles!
6. Run `extrude_rgb_voronoi(out, iters=5)` and save lossless `mgo_XXX_4x.webp` + `"hdTrackB": true` in `mgo_XXX.json`.

---

## 7. RGM Portrait Remastering (`ui/rgm_4x.webp`)

* Pack up to `10` portraits (`5 × 2` grid, `scale = 3×` nearest-neighbor centered in `275 × 384` cells) on dark slate `rgb(22, 30, 36)` inside `1376 × 768`.
* **Identity Lock Rule**: Always group multiple expressions of the same character onto the **same** `1376 × 768` sheet whenever possible so facial bone structure, hair ornaments, and clothing stay 100% consistent across moods.
* Splice extracted `4×` RGBA portraits into `site/assets/graphics/ui/rgm_4x.webp` (`quality=94, alpha_quality=100, exact=True`) and append completed IDs to `site/assets/graphics/ui/rgm_hd_done.json`.

---

## 8. Proven Image-to-Image Prompt Templates

Use these exact prompt structures when invoking an offline image-to-image model (`1376×768` input -> `1376×768` output):

### 8.1 Indoor Room / Outdoor Map Patch Prompt
> `Faithful 2D HD hand-painted Chinese classic RPG background remaster of this exact isometric scene (<SCENE_NAME> from 1995 Chinese Paladin / Xianjian Qixiazhuan). CRITICAL GEOMETRY & PROP LOCK: Keep every single wall, doorway, floor boundary, furniture piece, staircase, cliff, path, tree trunk, water shoreline, and black exterior background in the EXACT same pixel position, scale, and isometric angle — zero layout changes, zero added characters, zero camera shift. STRICT ANTI-HALLUCINATION RULE: NEVER invent or add extra props (no added steamers, food baskets, buns, jars, bowls, teapots, vases, lanterns, candles, signs, or plants) onto bare counters, bare tables, bare floors, grass, or dark doorways! Bare counters/floors must remain bare and dark doorways must remain dark, remastering ONLY the exact objects visible in the input reference. Replace blocky pixelated dithering with crisp, ultra-clean high-definition hand-painted traditional Chinese game art: fine natural wood grain, crisp carved lattice woodwork, clean stone tiles, lush foliage, realistic woven textiles, warm atmospheric lighting, sharp clean brushwork. Pure black (#000000) background outside the map geometry must stay pure solid black (#000000). Output 1376x768.`

### 8.2 Character / Prop Sprite Grid Prompt
> `Faithful 2D HD hand-painted Chinese classic RPG sprite sheet remaster of this exact <COLS>x<ROWS> grid. CRITICAL CONSTRAINTS: Keep the exact <COLS>x<ROWS> grid layout, keep every sprite inside its exact cell at the exact same pixel position, pose, facing direction, silhouette size, and foot baseline! Notice each row groups directional animation frames — back-facing frames (showing only black hair / back of head) MUST remain back-facing with NO face or front headband drawn on the back of the head! Keep the flat solid dark slate-teal background (#2E3E48) completely uniform and clean behind every sprite (no drop shadows, no floor, no glow). Upgrade pixelated 1995 sprites into crisp, clean high-resolution hand-painted Chinese RPG character/prop art with clean ink linework, smooth fabric shading, consistent faces/outfits across frames. Output 1376x768.`

### 8.3 Dialogue Portrait Grid Prompt
> `Faithful 2D HD hand-painted Chinese classic RPG character portrait sheet remaster of this exact <COLS>x<ROWS> grid from 1995 Chinese Paladin (Xianjian Qixiazhuan). Keep the exact <COLS>x<ROWS> grid layout, exact head/shoulder position, silhouette, hairstyle, clothing colors, and exact facial expression of each cell! Upgrade low-res dithered pixel art into crisp, expressive high-definition classical Chinese RPG painted bust portraits (clean fine brushwork, natural skin shading, detailed eyes, silk & hair strands) while staying 100% faithful to each original character design. Keep the uniform dark slate background (#161E24) solid and clean behind every portrait. Output 1376x768.`

### 8.4 Rate-Limit & Quality Discipline
* Invoke image generation **sequentially (1 image per request/turn)** when using burst-limited endpoints such as `gemini-3.1-flash-image` to avoid `429 RESOURCE_EXHAUSTED`.
* Verify every generated image is non-blank and `register_translation_2d` reports `|dx2| <= 6, |dy2| <= 6`.

---

## 9. Critical Engine & Gameplay Invariants Verified During Playtesting

Any AI modifying or extending `site/js/` must preserve these four invariants:
1. **DOS PAL Isometric Diamond Trigger Metric (`site/js/main.js`)**:
   * Touch/proximity (`trigger_mode >= 4`): `abs(ev.x - lead.x) + abs(ev.y - lead.y) * 2 < (ev.trigger_mode - 4) * 32 + 16`.
   * Search/Spacebar (`trigger_mode` `1..3` around `probe = lead + step(dir)`): `abs(ev.x - probeX) + abs(ev.y - probeY) * 2 < (ev.trigger_mode - 1) * 32 + 16`.
   * *Never* replace this with an axis-aligned rectangle, or stepped bridges/firewood piles/stairs (like `s_002a`) will trigger from adjacent floor tiles and offset scripted climbs by `32px`!
2. **Same-Scene `screen_fade_out` Auto-Fade-In (`site/js/vm/script-vm.js`)**:
   * At least 34 vanilla DOS PAL scripts (e.g., `s_119e`, `s_11c5`, `s_1918`, `s_1afa`) execute `screen_fade_out`, reposition actors in the *same* scene without `scene_change`, and call `video_redraw` -> `end` without an explicit `screen_fade_in`. `ScriptVM.needFadeIn` (matching `SDLPAL`'s `gpGlobals->fNeedToFadeIn`) fades back in on the next `video_redraw` / `wait_ticks` (when `!pendingSceneEnter`) and guarantees `fadeAlpha = 0` on script completion.
3. **Live `[Tab]` Dual-Mode Toggle (`crates/pal-render/src/world.rs` + `site/js/host/wasm-runtime.js`)**:
   * When `hdMode == true` and `maps_hd/map_XXX/meta.json` exists, `pal-render` streams viewport-visible HD chunks + spatial `<= 2048×2048` `occ_r*_c*_p*_4x.webp` occluders; pressing `[Tab]` switches immediately to `1×` `320×200` `gop_XXX_1x.webp` + `mgo_XXX_1x.webp` pixel-exact mode (and switches audio synth between acoustic reverb and OPL2 FM). Maps not yet in `maps_hd/` gracefully fall back to `gop_XXX_4x.webp`.
4. **Browser Cache Busting (`ASSET_VER`)**:
   * Whenever existing `.webp` or `.json` files are updated on disk, bump `ASSET_VER` in `site/js/host/asset-provider.js` and `site/index.html` (`track_b_pipeline.py bake` does this automatically) so live browser sessions fetch updated assets immediately on reload.

---

## 10. Reproducible Per-Scene Execution Workflow (`tools/track_b_pipeline.py`)

To remaster any scene `N` (`1..294`) end-to-end:

```bash
# 1. Inspect overall progress across all 294 scenes & 221 maps
python3 tools/track_b_pipeline.py status

# 2. Prepare 1376x768 reference sheets & manifest_todo.json for Scene N
#    (`--chain` = neighbour-aware generation, recommended for multi-patch maps; `--redo-map` = regenerate a baked map)
python3 tools/track_b_pipeline.py prepare --scene <N> --work-dir .tmp/pal_track_b/scene_<N> --chain

# 3. Generate 1376x768 remastered images for each entry in manifest_todo.json (using each item's own `prompt`),
#    recording {"key": "/abs/path/to/generated.jpg"} into .tmp/pal_track_b/scene_<N>/ai_outputs.json.
#    With --chain, repeat until it reports all map patches done:
python3 tools/track_b_pipeline.py chain-next --scene <N> --work-dir .tmp/pal_track_b/scene_<N>
#    ...then generate the newly listed items, record them, and run chain-next again.

# 4. Bake HD map pack + occluders + MGO sprites + RGM portraits into site/assets/graphics/ (auto-runs verify-map!)
python3 tools/track_b_pipeline.py bake --scene <N> --work-dir /tmp/pal_track_b/scene_<N>

# Optional standalone map helpers (verification, occluder repacking, in-place calibration, mask/seam rebake, or cutout regen):
python3 tools/track_b_pipeline.py verify-map --map <M>
python3 tools/track_b_pipeline.py repack-occluders --map <M>
python3 tools/track_b_pipeline.py repack-all-occluders
python3 tools/track_b_pipeline.py calibrate-map --map <M>
python3 tools/track_b_pipeline.py calibrate-all-maps
python3 tools/track_b_pipeline.py rebake-map --map <M> --work-dir /tmp/pal_track_b/scene_<N>
python3 tools/track_b_pipeline.py prepare-cutout-patches --map <M> --work-dir /tmp/pal_track_b/scene_<N>

# 5. Run Rust, Node & Python verification suites, then commit and push!
cargo test --workspace
node tools/headless_e2e.mjs
node tools/verify_npc_motion.mjs
node tools/verify_story_parity.mjs
python3 -m pytest tools/tests/ -v
git add site tools docs
git commit -m "feat(hd): Track-B HD remaster for Scene <N> (<Scene Name>)"
git push origin main
```

---

## 11. Multi-Frame Sprite (`MGO`) & Cutscene (`FBP` / `RNG`) Quality Standards & Lessons Learned

Playtesting and quantitative auditing (`python3 tools/mgo_consistency.py audit`) uncovered several systematic pitfalls in multi-frame sprite and cutscene remastering. Follow these standards for all future `MGO` / `FBP` / `RNG` passes (including any full chroma-key `MGO` re-bake):

### 11.1 Never Split One `MGO` Across Multiple Sheets (Cross-Frame Consistency)
* **Root Cause**: Splitting a single `mgo_id` across two AI generation sheets caused abrupt mid-animation outfit/material/style changes — e.g., `mgo_339` (Zhao Ling'er bathing: bare-skinned in frames `0..5`, suddenly wearing a blue floral dress in frames `6..16`), `mgo_260` (stalactite turning from dark stone to tan wood at frame 18), `mgo_261` (cave puddle turning from wooden raft to grey rock to blue bathtub), and `mgo_003` (Ling'er wearing a purple headband on frame 0 only).
* **Mandatory Rule**: Always pack **all frames of a single `mgo_id` onto one unified sheet** (or keep all frames of the same character together on one sheet), include an explicit cross-frame identity/material lock in the prompt, and verify with `mgo_consistency audit` after baking.
* **Paired Environmental Animations**: Sprites that play together in-game (e.g., `mgo_260` ceiling stalactite dripping + `mgo_261` floor cave puddle rippling) must share consistent rock/water material palettes and synchronized frame timing.

### 11.2 Background Color, Outline Halo & Chroma-Key Extraction (Magenta / Green)
* **Original DOS Format**: All 1995 DOS `MGO.MKF` sprites are RLE-encoded **pure transparent RGBA (`alpha = 0`)** with no background color.
* **Pitfall 1 — Light-Gray / White Canvas (`#E6E8EC` / `#FFFFFF`)**: Because image generators output opaque RGB, anti-aliased edge pixels blend with bright background colors, producing visible **white halos** around dark hair strands and outlines when rendered over dark maps.
* **Pitfall 2 — Dark Slate / Charcoal Canvas (`#2E3E48` / `#242830`) + Heavy Ink Outlines**:
  1. Blending anti-aliased edges against a dark canvas plus prompting `"clean dark ink linework"` bakes **heavy 3–4px black borders** onto characters (`mgo_002`, `mgo_003`, etc.) that stand out harshly against painted 4× HD maps.
  2. Dark backgrounds collide with dark subjects (black hair, dark iron weapons, dark brown cave rock) during color-distance flood-fill keying, risking erosion of dark pixels.
* **Recommended Standard — Adaptive Chroma-Key Canvas + Full-Cell Despill + Foot-Baseline Scale Lock**:
  1. **Pick a background color absent from the 1× sprite palette**: Default to pure **chroma magenta (`#FF00FF`)**; if the 1× sprite contains magenta/purple hues, automatically fall back to **chroma green (`#00E050`)**.
  2. **Strip DOS foot shadows BEFORE AI generation**: Run `strip_dos_shadow_1x` on the 1× frames before laying out the sheet and record `"shadowStripped": true`.
  3. **Prompt for natural painterly edges (NO thick black outlines, NO drop shadows)**:
     > `Remaster this single <COLS>x<ROWS> sprite sheet into crisp 4x HD hand-painted 1995 Chinese fantasy RPG character/prop art (refined classical painterly shading that blends seamlessly onto hand-painted 4x HD isometric maps). Keep all frames in the exact same pixel position, scale, silhouette, and pose on a COMPLETELY FLAT UNIFORM SOLID MAGENTA (#FF00FF) [or GREEN (#00E050)] chroma-key background. Do NOT draw grid lines, borders, floor, cast shadows, or colored glow. NATURAL PAINTERLY EDGES: Do NOT draw heavy black comic/cartoon outlines — use soft, natural painterly shading and delicate subtle edges. STRICT CROSS-FRAME CONSISTENCY: All frames depict the EXACT same character/object with identical face, hair, outfit, and colors; back-facing views must have NO face on the back of the head.`
  4. **Full-Cell Chroma-Key + Un-Premultiply Despill (Never Exterior-Only Flood-Fill)**: Because `#FF00FF` / `#00E050` does not exist in the sprite palette, mark **all** pixels in the cell matching the chroma-key color as background — including enclosed interior gaps (e.g., triangles between ponytail & neck, or gaps between arms & torso) that exterior-only flood-fill misses! Then apply mathematical un-premultiply + chroma despill on the 2px transition ring adjacent to background pixels so **zero pink/green fringe, zero white halo, and zero black border** survive.
  5. **Tight Foreground BBox Scale-to-Fit + Exact Foot Baseline Lock (Prevents Boot/Head Clipping)**: Image generators adapting non-standard grid aspect ratios to 16:9 / 4:3 routinely render sprites 3–10% taller or shorter than the exact `fw*4 × fh*4` atlas frame rect — splicing by centroid IoU alone slices off boot soles or hair tips. Always extract each frame's tight chroma-keyed foreground bounding box `(ai_w, ai_h)` from the full padded cell, uniformly scale it to match the exact shadow-stripped 1× reference height (clamped so `dst_w <= fw*4` and `dst_h <= fh*4`), and lock the **bottom (`max_y`) to the exact 1× foot baseline** and horizontal center to the 1× centroid.
  6. **Turnkey Tooling (`tools/mgo_consistency.py`)**:
     ```bash
     python3 tools/mgo_consistency.py audit
     python3 tools/mgo_consistency.py prepare <MGO_ID>
     # Generate remastered sheet using the prompt in Step 3 above (1 call at a time)
     python3 tools/mgo_consistency.py bake <MGO_ID> <AI_OUTPUT_PNG>
     ```

### 11.3 Ground Shadow Removal (`shadowStripped: true`)
* **Current State**: Out of 637 total `MGO` sprites, 250 are Track-B HD (`129` with `"shadowStripped": true`, `121` still with `"shadowStripped": false` containing baked chunky DOS ground shadows) and `387` are procedural 4× fallback.
* **Mandatory Rule**: Before AI generation, strip the baked 1× DOS foot shadow (`strip_dos_shadow_1x`), explicitly forbid `"drop shadows / ground shadows"` in the prompt, and set `"shadowStripped": true` in `mgo_XXX.json`. `pal-render` then draws a clean, uniform soft elliptical ground shadow at runtime (`crates/pal-render/src/world.rs`) — or shadows can be tuned/disabled globally without touching sprite assets.

### 11.4 Cutscene Illustrations (`FBP`) & Frame-by-Frame Animations (`RNG`)
* **`FBP` Story Sequences & Panoramas (72 images)**: Continuous story panels (e.g., intro `fbp_71 → 70 → 67 → 66 → 65 → 49`) and `fbp_scroll` stitched panoramas must be remastered together as a multi-panel storyboard or stitched panorama strip rather than independent single frames, to prevent character face/palette/seam drift.
* **`RNG` Cutscenes (12 sequences, 832 frames — currently 1× only)**: Never feed individual `RNG` frames independently into an image-to-image model (causes severe temporal flickering and face/outfit morphing). Instead, categorize each sequence and use:
  1. **Static background + animated foreground split** (for scenes where only water/clouds/sword-glow move over a static backdrop);
  2. **Unified keyframe storyboard grid** (for short sequences); or
  3. **AI-remastered keyframe + video/optical-flow interpolation or temporally stable upscaling** (for long 100–206 frame sequences like `rng_01` and `rng_09`).

