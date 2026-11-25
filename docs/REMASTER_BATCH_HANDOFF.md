# RePal 4× HD Batch Remaster Handoff Guide (MGO Sprites + RNG Cutscenes)

This guide is designed for an AI Agent (`agy`) running on a high-RAM primary workstation to batch-remaster all remaining **MGO character/object sprites (634 remaining)** and **RNG full-screen cutscenes (12 cutscenes / 802 frames)** under the new quality standard.

---

## 1. One-Time Workspace Setup on the Primary Machine

Clone both decoupled repositories side-by-side and symlink `site/assets`:
```bash
mkdir -p ~/pal && cd ~/pal
git clone git@github.com:HclX/RePal.git
git clone git@github.com:HclX/RePal_Assets.git
ln -s ../../RePal_Assets/assets RePal/site/assets
```
Ensure Python 3 with Pillow (`pip install pillow`) and Rust (`cargo`) are installed.

**Strict Repo Rule**:
- ALL remastered `.webp` / `.json` / `.yaml` asset changes MUST be committed & pushed ONLY inside **`RePal_Assets/`**.
- Never commit copyrighted assets into `RePal/`.

---

## 2. Part A — 12 RNG Cutscenes (`rng_00` .. `rng_11`, 802 frames total)

The engine (`pal-render`) already loads `graphics/rng/rng_XX/f_YYY_4x.webp` automatically in 4× HD mode!

### Step-by-Step Per Cutscene
1. Check progress:
   ```bash
   cd ~/pal/RePal
   python3 tools/rng_remaster.py status
   ```
2. Pack a cutscene into 3×3 (9-frame, 960×600) contact sheets:
   ```bash
   python3 tools/rng_remaster.py prepare 0
   ```
   Outputs `.tmp/hd_rng/rng_00/sheet_00_in.png`, `sheet_01_in.png`, ...
3. Remaster each `sheet_XX_in.png` with `image-generator` using this prompt template:
   > Remaster this 3×3 grid of 9 consecutive frames from a 1995 DOS Chinese Paladin (仙剑奇侠传) cutscene into crisp 4× HD hand-painted Chinese classical watercolor/gouache 2D animation art. Preserve the EXACT 3×3 grid layout, cell boundaries (zero padding, no borders), camera framing, colors, and smooth motion continuity across all 9 frames. No text, no UI, no thick black cartoon outlines.
4. Slice & bake directly into `RePal_Assets`:
   ```bash
   python3 tools/rng_remaster.py bake 0 0 /path/to/ai_sheet_00.jpg
   ```
5. Visually spot-check 1–2 baked frames (`f_000_4x.webp`, `f_004_4x.webp`) with `read_file` to verify zero seam bleed and faithful framing.

---

## 3. Part B — All MGO Sprites (`mgo_000` .. `mgo_636`, 637 total)

**Already completed under the new rules (SKIP these)**:
- `mgo_002` (Li Xiaoyao)
- `mgo_003` (Zhao Ling'er)
- `mgo_208` (Li Xiaoyao carrying food/wine tray, with white headband)

**Recommended Priority Order**:
1. Core party members & Chapter 1–2 main NPCs (Lin Yueru, Anu, Wu Hou, Gai Luojiao, Auntie Li, Drunken Taoist / Wine Sword Immortal, Miao Chief, Grandma, etc.)
2. Remaining multi-frame characters/NPCs (`480` total)
3. Single-frame props (`157` total)

### Step-by-Step Per MGO ID
See `docs/HD_REMASTER_METHODOLOGY.md` Section 11 for full technical rationale.

1. Prepare the magenta `#FF00FF` chroma-key, shadow-stripped reference sheet:
   ```bash
   cd ~/pal/RePal
   python3 tools/mgo_consistency.py prepare <ID>
   ```
   Outputs `.tmp/hd_mgo/mgo_<ID>_in.png` (pure `#FF00FF` background, original 1× shadows stripped).
2. Remaster `.tmp/hd_mgo/mgo_<ID>_in.png` with `image-generator`:
   - **CRITICAL RULES**:
     1. Preserve the EXACT grid layout, cell alignment, pose, facing direction, and foot baseline of every frame (zero scaling drift).
     2. Keep the ENTIRE background a **flat, solid, pure neon magenta `#FF00FF`** (RGB 255, 0, 255) — NO gradients, NO grid lines, NO borders.
     3. **NO ground shadows** under the character's feet (the engine renders dynamic shadows).
     4. **NO thick black cartoon outlines** — soft, natural painted edges in traditional Chinese classical RPG watercolor/gouache style.
3. Convert `.jpg` → `.png` (if `image-generator` outputs `.jpg`) and bake:
   ```bash
   python3 -c "from PIL import Image; Image.open('<ai_out.jpg>').convert('RGBA').save('.tmp/hd_mgo/mgo_<ID>_hd.png')"
   python3 tools/mgo_consistency.py bake <ID> .tmp/hd_mgo/mgo_<ID>_hd.png
   ```
   This automatically strips `#FF00FF` chroma-key (with purple fringe despill), locks foot baselines to the 1× reference, and writes `site/assets/graphics/mgo/mgo_<ID>_4x.webp`.
4. Visually inspect the baked `mgo_<ID>_4x.webp` with `read_file` to verify clean alpha cutout and zero purple halo / frame drift.

---

## 4. Publishing a New Asset Pack (`assets-v2`, `assets-v3`, ...)

After completing a milestone batch (e.g., all 12 RNGs, or Chapter 1–2 MGOs):
1. In `~/pal/RePal_Assets`:
   ```bash
   git add assets/
   git commit -m "Remaster batch: <summary>"
   git push origin main
   git tag assets-v2
   git push origin assets-v2
   ```
2. Wait for `RePal_Assets` GitHub Actions (`release-assets.yml`) to publish `pal-assets-<hash>.tar.gz` and print its SHA-256 + `size_bytes`.
3. Update `~/pal/RePal/assets-lock.json` with the new `version`, `tag`, `filename`, `sha256`, and `size_bytes`, commit & push to `RePal`.
