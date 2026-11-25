# RePal User Manual

**RePal** is a pure-Rust, deterministic modern HD remake of the 1995 DOS classic *Chinese Paladin (仙剑奇侠传)* delivered as a **single unified binary (`repal` / `repal.exe`)** supporting GUI, Headless Text Mode, and GUI + Text Mode over stdio or TCP socket.

---

## 1. Included Files in This Release
- **`repal` / `repal.exe`** — Unified native runtime:
  - `repal` — Native desktop GUI (4× HD graphics, authentic 1995 DOS UI, acoustic/OPL2 audio).
  - `repal --text` — Headless Text Mode over stdio or TCP socket (`--listen`), with plain text or `--json` output and full-UI `shot` / `golden` visual regression.
  - `repal --gui --text` / `repal --listen 127.0.0.1:9999` — GUI + Text Mode (watch an AI Agent play live).
- **`assets-lock.json`** — SHA-256 hash pin for the matching static asset pack in `HclX/RePal_Assets`.
- **`agent-skill/SKILL.md`** — Complete instruction sheet for an LLM / AI Agent to play RePal via `repal --text`.
- **`USER_MANUAL.md`** — This guide.

---

## 2. First Launch & Automatic Asset Download
RePal ships lightweight (~1–2 MB) executables decoupled from copyrighted game assets. On first launch, `repal` opens a splash screen with a live progress bar, automatically downloads the SHA-256-pinned asset pack from `HclX/RePal_Assets`, verifies its checksum, and caches it by hash so future upgrades never re-download unchanged assets.

### Authentication / Asset Sources (in priority order)
1. **Local folder**: `--assets <DIR>` (or an `assets/` folder placed next to the executable).
2. **GitHub CLI (`gh`)** *(easiest for private `HclX/RePal_Assets`)*: Run `gh auth login` once, then double-click `repal`.
3. **Custom URL / Token**: Set `REPAL_ASSETS_URL=<URL>` (or `--assets-url <URL>`) and/or `GITHUB_TOKEN=<PAT>`.

### User Profile Directories (Saves, Config, Bug Reports, Asset Cache)
- **Windows**: `%APPDATA%\RePal\`
- **Linux**: `~/.local/share/RePal/` (or `$XDG_DATA_HOME/RePal/`)
- **macOS**: `~/Library/Application Support/RePal/`

Inside that directory:
- `saves/slot_0.json` .. `slot_99.json` — 100 save slots (20 pages × 5 slots/page; press `←`/`→` in Save/Load menu to flip pages).
- `config.json` — Persisted HD/Retro mode, language (繁/简/EN), music/SFX toggles, and fullscreen preference.
- `assets_cache/<hash>/assets/` — Hash-keyed verified asset cache (customizable via `--assets-cache <DIR>` or `REPAL_ASSETS_CACHE_DIR`).
- `replays/<version>/` — Auto-archived `.repal` deterministic gameplay replays (bucketed by engine version, e.g. `v0.3.11/`).
- `agent_out/` — Default `--out-dir` for `shot` / `save` / `replay-export` when running GUI+Text or `--listen` socket modes.
- `bug_reports/` & `screenshots/` — Saved `F9` bug report bundles (`.repal` + `.png`) and `F10` screenshots.

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
| **F8** | Open a local `.repal` / `.json` replay in the built-in Replay Theater (`Tab` opens Chapters inside Replay Theater) |
| **F9** | One-click Bug Report (saves `.repal` replay bundle + `.png` to `RePal/bug_reports/` AND auto-submits a GitHub Issue via `gh` if logged in) |
| **F10** | Save 4× HD PNG screenshot (to `RePal/screenshots/`) |
| **F11** or **Alt+Enter** | Toggle Fullscreen (also available in Menu → Remake → Fullscreen) |

> **Linux Icon Tip**: Linux ELF binaries don't embed file-manager icons directly. Run `./install-desktop-icon.sh` once inside the extracted folder to register the RePal Flying Sword icon & desktop launcher (`~/.local/share/applications/repal.desktop`).

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
- Export AI thought subtitles from a `.repal` replay: `./repal --replay-export-srt in.repal out.srt`

---

## 5. CLI Options (`repal --help`)
- `--text` / `--gui` / `--listen <IP:PORT>` / `--json` — Select execution mode, channel, and output format
- `--obs <fair|debug>` (`--fog`) / `--minimap <auto|always|off>` — Observation fairness profile and viewport ASCII minimap cadence
- `-c <CMDS>` / `--script <FILE>` / `--record <FILE>` / `--out-dir <DIR>` / `--keep-going` — Batch commands, recording, and output directory
- `--replay <FILE>` / `--replay-export-srt <IN.repal> <OUT.srt>` — Auto-play a `.repal`/`.cmd` replay or export AI thought subtitles to `.srt`
- `--assets <DIR>` / `--assets-cache <DIR>` / `--assets-url <URL>` — Asset directory & cache overrides
- `--saves <DIR>` / `--config <FILE>` — Override default user-profile paths
- `--new` / `--save <FILE>` / `--scene <ID>` / `--debug` / `--hd` / `--retro` / `--lang <hans|hant|en>` / `--fullscreen` / `--pace-ms <MS>` — Boot, debug watermark, display, and spectator pacing overrides
