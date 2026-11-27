# RePal User Manual

**RePal** is a pure-Rust, deterministic modern HD remake of the 1995 DOS classic *Chinese Paladin (仙剑奇侠传)* delivered as a **single unified binary (`repal` / `repal.exe`)** supporting GUI, Headless Text Mode, and GUI + Text Mode over stdio or TCP socket.

---

## 1. Included Files in This Release
- **`repal` / `repal.exe`** — Unified native runtime:
  - `repal` — Native desktop GUI (4× HD graphics, authentic 1995 DOS UI, acoustic/OPL2 audio).
  - `repal --text` — Headless Text Mode over stdio or TCP socket (`--listen`), with plain text or `--json` output and full-UI `shot` / `golden` visual regression.
  - `repal --gui --text` / `repal --listen 127.0.0.1:9999` — GUI + Text Mode (watch an AI Agent play live).
- **`assets-lock.json`** — Pins the encrypted asset-pack release (`packs-vN` manifest URL + SHA-256) in `HclX/RePal_Assets`.
- **`agent-skill/SKILL.md`** — Complete instruction sheet for an LLM / AI Agent to play RePal via `repal --text`.
- **`USER_MANUAL.md`** — This guide.

---

## 2. First Launch & Automatic Asset Download
RePal ships lightweight executables decoupled from copyrighted game assets. Assets come as encrypted `.rpk` packs: a small **base** pack (UI, data, fonts, translations, title screen) plus one pack per **chapter**.

- **First launch**: the splash screen downloads and verifies the base pack (`DOWNLOADING` / `VERIFYING`), then the title screen appears.
- **During play**: entering a new chapter downloads its pack in the background behind a short loading overlay. Verified packs are cached and never re-downloaded; packs are content-addressed, so updates only fetch packs that actually changed.
- **Download everything up front** (for offline play): Title → Options → **Download All**, or in-game Menu → Remake → Download All.
- **Unlocking**: on the first launch you are asked for the **pack passphrase** (or the 64-hex pack key). It is saved to `pack.key` in your profile directory, so you only enter it once. `REPAL_PACK_KEY=<passphrase or hex>` also works.

### Asset Sources (in priority order)
1. **Local folder**: `--assets <DIR>` (or an `assets/` folder placed next to the executable) — loose, unencrypted assets for development.
2. **Local packs**: `--packs-dir <DIR>` (or `REPAL_PACKS_DIR`) — a folder containing `pack-manifest.json` and `.rpk` files.
3. **Default**: the pinned pack manifest from `assets-lock.json` (raw.githubusercontent.com first, jsDelivr as fallback). Override with `--assets-url <URL>` / `REPAL_ASSETS_URL`.

### User Profile Directories (Saves, Config, Bug Reports, Asset Cache)
- **Windows**: `%APPDATA%\RePal\`
- **Linux**: `~/.local/share/RePal/` (or `$XDG_DATA_HOME/RePal/`)
- **macOS**: `~/Library/Application Support/RePal/`

Inside that directory:
- `saves/slot_0.json` .. `slot_99.json` — 100 save slots (20 pages × 5 slots/page; press `←`/`→` in Save/Load menu to flip pages).
- `config.json` — Persisted HD/Retro mode, language (繁/简/EN), music/SFX toggles, and fullscreen preference.
- `pack.key` — Saved pack key (written after you enter the passphrase once).
- `assets_cache/packs/` — Downloaded, verified `.rpk` packs (customizable via `--assets-cache <DIR>` or `REPAL_ASSETS_CACHE_DIR`). Legacy loose-asset caches from older versions are cleaned up automatically.
- `replays/<version>/` — Auto-archived `.repal.gz` deterministic gameplay replays (bucketed by engine version, e.g. `v0.3.11/`).
- `agent_out/` — Default `--out-dir` for `shot` / `save` / `replay-export` when running GUI+Text or `--listen` socket modes.
- `bug_reports/` & `screenshots/` — Saved `F9` bug report bundles (`.repal.gz` + `.png`) and `F10` screenshots.

---

## 3. Controls & Hotkeys (1995 DOS / SDLPAL Aligned)
| Key / Input | Action |
| :--- | :--- |
| **Arrows / Numpad 8·2·4·6** | Move (classic 1995 diagonal isometric mapping) / Navigate menus |
| **Mouse Left-Click / Drag** | Click-to-walk / Click-to-talk / Click UI buttons |
| **Space / Enter / Z / Left Ctrl** | Interact / Search / Confirm / Advance dialogue |
| **Esc / Alt / Insert / Numpad 0 / X / Right-Click** | Open/Close System Menu / Cancel (Esc also cancels an in-flight AI task in GUI+Text mode) |
| **PgUp / PgDn / Home / End** (Numpad `9/3/7/1`) | Page up/down (7 rows) or jump to first/last item in Item & Magic lists |
| **Battle: `R` `A` `D` `E` `W` `Q` `F` `S`** | Repeat (`R`), Auto (`A`), Defend (`D`), Use Item (`E`), Throw (`W`), Flee (`Q`), Force Attack (`F`), Status (`S`) |
| **F2** | Instant live switch between **4× HD** and **1995 1× Retro** (visuals + audio!) |
| **F7** | Toggle the **4× HD AI thought/commentary subtitle bar** (`think`, disabled in 1× Retro) |
| **F8** | Open a local `.repal.gz` / `.json` replay in the built-in Replay Theater (`Tab` opens Chapters inside Replay Theater) |
| **F9** | One-click Bug Report (saves `.repal.gz` replay bundle + `.png` to `RePal/bug_reports/` AND auto-submits a GitHub Issue via `gh` if logged in) |
| **F10** | Save 4× HD PNG screenshot (to `RePal/screenshots/`) |
| **F11** or **Alt+Enter** | Toggle Fullscreen (also available in Menu → Remake → Fullscreen) |

> **Linux icon & launcher**: Linux binaries can't carry their own icon, so the `repal` file itself always shows a generic icon in file managers. On the first GUI launch RePal installs a launcher (`~/.local/share/applications/repal.desktop`) and icon automatically, so **RePal** appears in the applications menu and the taskbar shows its icon; right-click the menu entry to add it to the desktop or panel. The launcher's path is refreshed if you move the folder, and a launcher you edited yourself (without the `X-RePal-Managed=true` line) is never touched. Opt out with `REPAL_NO_DESKTOP_INTEGRATION=1`. `install-desktop-icon.sh` in the release folder does the same by hand.

---

## 4. Text Mode & Watching an AI Agent Play Live (GUI + Text Mode)
You can watch an AI Agent explore, read dialogues, solve puzzles, and fight battles in real time inside `repal` (from the same machine OR over your LAN):

1. **Enable GUI + Text Mode in `repal`**:
   - Toggle **Menu → 重製(Remake) → AI 旁觀(Agent)** right inside the game UI at runtime (`Off` → `Local (127.0.0.1:9999)` → `LAN (0.0.0.0:9999)`), OR
   - Launch with `repal --listen 127.0.0.1:9999` (local socket), `repal --listen 0.0.0.0:9999` (LAN socket), or `repal --gui --text` (stdio).
   - **LAN Security**: Non-loopback binds (`0.0.0.0` / LAN) require authenticating each TCP session with `auth <token>` first (the per-session token is printed on startup and displayed in **Menu → Remake / Title Options**).
2. **Connect using `telnet` / `nc` / any TCP script**:
   ```bash
   printf "new\ncontinue\nlook\nquit\n" | nc 127.0.0.1 9999
   ```
3. Point your AI Agent at `agent-skill/SKILL.md` or `docs/TEXT_MODE_SPEC.md`.

### Playing Directly in Headless Text Mode (`repal --text`)
You can also run RePal directly in any terminal (no GUI window / no `$DISPLAY` required):
- Interactive: `./repal --text --new` (shows `[<mode>]> ` prompt)
- Fair blind playtesting (player-equivalent viewport & anti-cheat): `./repal --text --obs fair --json --new`
- Batch UI automation / visual regression: `./repal --text --json -c "load 10.rpg; view status; shot status.png"`
- Headless TCP server: `./repal --text --listen 127.0.0.1:9999 [--json]`
- Export AI thought subtitles from a `.repal.gz` replay: `./repal --replay-export-srt in.repal.gz out.srt`

---

## 5. CLI Options (`repal --help`)
- `--text` / `--gui` / `--listen <IP:PORT>` / `--json` — Select execution mode, channel, and output format
- `--obs <fair|debug>` (`--fog`) / `--minimap <auto|always|off>` — Observation fairness profile and viewport ASCII minimap cadence
- `-c <CMDS>` / `--script <FILE>` / `--record <FILE>` / `--out-dir <DIR>` / `--keep-going` — Batch commands, recording, and output directory
- `--replay <FILE>` / `--replay-export-srt <IN.repal.gz> <OUT.srt>` — Auto-play a `.repal.gz`/`.cmd` replay or export AI thought subtitles to `.srt`
- `--assets <DIR>` / `--packs-dir <DIR>` / `--assets-cache <DIR>` / `--assets-url <URL>` — Loose assets, local packs, pack cache, and manifest source overrides
- `--saves <DIR>` / `--config <FILE>` — Override default user-profile paths
- `--new` / `--save <FILE>` / `--scene <ID>` / `--debug` / `--hd` / `--retro` / `--lang <hans|hant|en>` / `--fullscreen` / `--pace-ms <MS>` — Boot, debug watermark, display, and spectator pacing overrides
