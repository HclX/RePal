# RePal Unified Binary (`repal`) — Text Mode Specification (`repal-text/1`)

> **Goal**: Consolidate `pal-desktop`, `pal-headless`, and `pal-mud` into **one native binary (`repal` / `repal.exe`)** built on a single `PalRuntime` (`pal-core` + `pal-render` + `pal-ui`). `pal-cli` remains a separate dev-only DOS `.MKF` asset baker.
>
> **Machine Schema**: See [`docs/text_mode.schema.json`](text_mode.schema.json) for the formal JSON Schema validated by `pal-headless` integration tests.

---

## 1. Orthogonal Axes & Observation Profiles

| Axis | Values | Default | Notes |
| :--- | :--- | :--- | :--- |
| **Mode** | `GUI` / `Text` / `GUI + Text` | `GUI` | `--text` enters headless Text Mode (no window / no `$DISPLAY`). `--gui --text` or `--listen <addr>` runs GUI + Text simultaneously. |
| **Channel** | `stdio` / `socket` | `stdio` (`--text`), `socket` (`--listen`) | `stdio`: interactive TTY, pipe, `-c`, or `--script`. `socket`: line-oriented TCP (`telnet`/`nc`). |
| **Format** | `plain` / `json` | `plain` | `--json` switches all responses to strict **JSON-Lines** (`protocol: "repal-text/1"`). |
| **Observation** | `--obs debug` / `--obs fair` | `debug` | `--obs fair` (alias `--fog`) enforces player-equivalent vision & anti-cheat rules. |

### Observation Profiles (`--obs fair` vs `--obs debug`)

| Capability | `--obs debug` (Default) | `--obs fair` (Alias: `--fog`) |
| :--- | :--- | :--- |
| **Events visible** | All active events in current scene | Only visible sprites (`sprite_id > 0`) and visible floor exits (`kind: "exit"`) inside the real clamped `320×200` camera viewport |
| **Event metadata** | Includes `trigger_mode`, `state`, and script-derived `hint` (`→ Scene`, `Battle #N`, `Shop #N`) | Omits `trigger_mode`/`state`; visual-only `hint` (`sprite=N` / `exit`), no script spoilers |
| **Battle enemy HP** | Exact `hp` / `max_hp` integers | `hp: null`, `max_hp: null` (authentic DOS PAL; observe `alive` + `battle.feed[]` damage numbers) |
| **`goto` / `talk <ev_id>`** | Global `CollisionGrid` A* to any active scene event / coord | Restricted to targets currently visible inside the `320×200` camera viewport (prevents maze-solving A* cheat; off-screen targets fail with `error_code: "no_target"` / `"out_of_view"`) |
| **Debug commands** | `/tp`, `/heal`, `/god`, `/cash`, `/item`, `/event`, `/battle`, `/script`, `/state`, `dump-state`, `view`, `core-replay` enabled (mutating `/debug` ops enable `debug_mode: true`, render `DEBUG ENABLED` watermark, set `tainted_by_debug: true`, and record deterministic `ReplayEvent::Debug` events) | All `/debug` commands, `dump-state`, `view`, and `core-replay` rejected with `error_code: "forbidden_in_fair_mode"` (`debug_mode` stays `false`) |

### Core Startup Flags

| Flag | Applies To | Description |
| :--- | :--- | :--- |
| `--text` | Mode | Enable Text Mode command engine (headless unless `--gui` is also passed). |
| `--gui` | Mode | Explicitly enable GUI window (combine with `--text` for GUI + stdio). |
| `--listen <IP:PORT>` | Channel | Start line-oriented TCP socket server. Alone implies `GUI + Text`; with `--text` runs headless socket server. Non-loopback binds require per-session `auth <token>`. |
| `--json` | Format | Emit single-line JSON-Lines responses (`repal-text/1`). |
| `--obs <fair\|debug>` | Text | Select observation/fairness profile (`--fog` is an alias for `--obs fair`). |
| `--minimap <auto\|always\|off>` | Text | Viewport `20×25` ASCII minimap emission cadence (default: `auto`). |
| `-c <CMDS>` | Batch | Execute semicolon/newline-separated commands in Text Mode and exit (implies `--text` if `--gui` not set). |
| `--script <FILE>` | Batch | Execute commands from a `.cmd` / `.rec` file in Text Mode and exit. |
| `--out-dir <DIR>` | Text | Output directory for `shot`, `save`, `replay-export`, `dump-state`, `bugreport`, and `Slot N` saves (`slot_{N}.json`). Defaults to `<user_dir>/agent_out` in GUI and `--listen` socket modes. `load` checks `--out-dir` first. |
| `--keep-going` | Batch | Continue batch execution after command errors. |
| `--assets <DIR>` | All | Override local assets directory. |
| `--new` / `--save <FILE>` | All | Boot into New Game or load a `.rpg` / `.json` save (default without either: Title screen). |
| `--debug` | All | Opt into explicit `debug_mode: true` on launch (renders `DEBUG ENABLED` watermark and records `debug_mode: true` in `.repal.gz`). Rejected if combined with `--obs fair` / `--fog`. |
| `--replay <FILE>` | GUI | Auto-play a `.repal.gz` or `.cmd` replay file. |
| `--replay-export-srt <IN.repal.gz> <OUT.srt>` | CLI | Export AI thought annotations from a `.repal.gz` archive to standard SubRip (`.srt`) subtitles and exit. |
| `--scene <ID>` | All | Jump to Scene ID after boot. |
| `--hd` / `--retro` | All | Initial resolution (`1280×800` 4× HD default vs `320×200` 1× Retro). |
| `--lang <hans\|hant\|en>` | All | Initial UI & script language. |
| `--pace-ms <MS>` | GUI+Text | Minimum pacing delay between steps for spectators (default `160`). |
| `--record <FILE>` | Text | Append executed commands to a replayable `.cmd` script. |

---

## 2. Wire Protocol & Lifecycle

- **Framing & Initial Greeting**: UTF-8, newline-delimited (`\n`; `\r\n` stripped). TCP lines are capped at 64 KiB; up to 16 concurrent clients (max 8 per IP), with a 10 s auth deadline, 600 s idle timeout, and per-session remote write quota (64 files / 64 MiB). Immediately upon TCP connect (after `auth <token>` if remote), `repal` emits one **initial `look` greeting line** (in both plain and `--json` modes) before reading commands — persistent clients must read/drain that greeting line on connect (or verify `resp["cmd"]`) so replies stay aligned 1:1 with commands.
- **Comments & Blank Lines**: Lines starting with `#` or `//` are ignored. In interactive plain TTY mode, a blank line advances dialogue/victory (`next`) or runs `look`.
- **Disconnecting (`quit` / `exit` / `q`)**:
  - **stdio (`--text`)**: replies `{"ok":true,"bye":true,...}` (or `Bye.`) and exits with code `0` (or `2` if an earlier `golden` check failed). Headless `--text` does not auto-archive `.repal.gz` recordings; GUI sessions auto-archive on exit.
  - **socket (`--listen`)**: replies `bye` and closes only that TCP client connection; the host stays alive.
- **Lazy Rendering & Deterministic Pagination**:
  - Headless `--text` never runs `PalCompositor` during gameplay ticks; only `shot`, `golden`, `click` (in UI), and `bugreport` render a settled frame.
  - Dialog pagination (`prompt.page`, `prompt.total_pages`, `prompt.page_text`) is computed deterministically via the shared `layout_dialog_pages` function on `ShowDialogue`, `set_lang`, and `set_hd_mode` — never deferred to `draw_dialog` — guaranteeing identical pagination in headless (with or without `shot`) and GUI.
- **Instant vs Multi-Tick Commands**:
  - Instant (`ExecOutcome::Done`): `look`, `map`, `status`, `bag`, `menu`, `view`, `hd`/`retro`, `lang`, `think`, `shot`, `golden`, `save`, `replay-export`, `buy`, `sell`, `equip`, `bugreport`, `dump-state`, `core-replay`, `/heal`, `/god`, `/cash`, `/item`, `/event`, `/state`, `help`, `quit`.
  - Multi-tick (`ExecOutcome::Pending`): `sw`/`nw`/`ne`/`se`, `goto`, `talk`, `search`, `wait`, `next`, `continue`, `yes`, `no`, `close`, `key`/`ui`, `click`, `use`, `new`, `load`, `/tp`, `/battle`, `/script`, and battle actions.

---

## 3. Command Reference & Outcome Semantics

### Success, Error Codes & Interruptions
- **`ok: true`** means the command achieved its intent, OR a movement/auto-walk command was interrupted by a legitimate game event (`"interrupted_by": "dialog" | "battle" | "scene_change"`). Pressing `Esc` in GUI+Text mode cancels an in-flight agent task with `"interrupted_by": "human"`.
- **`ok: false`** is returned with `"error"` (human-readable) and `"error_code"` (machine-readable):
  - `parse_error`, `invalid_args`, `wrong_mode`, `busy`, `blocked` (`walk` moved 0 steps), `unreachable` (`goto`/`talk` stopped with diamond `dist > 48`), `no_target`, `out_of_view` (`--obs fair` off-screen target), `still_pending` (`yes`/`no` failed to leave choice), `item_refused` (e.g. full-HP heal rejected), `magic_not_known`, `magic_not_field`, `insufficient_mp`, `magic_refused`, `invalid_target`, `forbidden_in_fair_mode`, `forbidden_over_socket`, `forbidden_remote`, `path_forbidden`, `quota_exceeded`, `golden_mismatch`, `disallowed`, `cancelled`.

### Command Summary
- **Exploration**: `look`, `map`, `sw|nw|ne|se [n]`, `goto <ev_id | x y>`, `talk [ev_id]`, `search`, `wait [ticks]`
  - `goto` and `talk <ev_id>` dispatch standard player `TickInput::click` events (`click_world`), keeping `.repal.gz` recordings 100% pure player inputs. `goto <ev_id>` selects a nearby walkable half-tile outside NPC click circles so walking near an NPC does not accidentally trigger conversation; `talk <ev_id>` clicks the event directly (and turns 1 tick via standard `HeldDir` before interacting when blocked across a counter).
  - `events[].dist` is half-tile isometric Manhattan distance: `(|dx| + 2*|dy|) / 16`.
  - `events[].dir` is the relative isometric direction (`sw`/`nw`/`ne`/`se`) from the party leader to the event.
  - `events[].screen` is `{ "x", "y" }` in logical `320×200` screen coordinates (or `null` if off-screen).
- **Dialogue & Modals**: `next`, `continue` (logs each multi-page dialog once), `yes`/`no` (auto-advances remaining pages of a multi-page choice prompt before submitting), `close`
- **Help**: `help [cmd|topic]` (`? [cmd|topic]`): returns localized command overview (omitting forbidden debug commands in `--obs fair`) or detailed per-command/per-topic syntax, examples, and caveats in both `log` and `artifacts.help`.
- **Battle & Field Magic**: `attack [slot]`, `magic <id|name> [slot] [caster_idx]`, `item|use <id|name> [slot]`, `coop [slot]`, `defend`, `flee`, `auto`, `cancel-auto`
  - `battle.prompting` is a 0-based index into `battle.heroes[]` (not `party[]`).
  - For `magic <id|name> [slot]` in battle, `slot` indexes `battle.enemies[]` when `target` is `"enemy"`/`"all_enemies"`, or `battle.heroes[]` when `target` is `"ally"`/`"all_allies"`. In `explore` mode, `magic <id|name> [target_party_idx] [caster_party_idx]` casts field-usable spells.
  - `use <id|name> [slot]` automatically routes to `BattleItem` when in `battle` mode (and validates inventory/usability before consuming a turn).
- **Party & Shops**: `status` (plain-text & JSON include spell `#id`, MP cost, scope/usability tags & `desc`), `bag` (plain-text & JSON include item `#id`, count, usability/throwable/equip tags & `desc`), `use <id|name> [slot|party_idx]`, `equip <id|name> [party_idx]`, `buy <idx> [qty]` (only in buy-only shops `ui_view: shop`), `sell <id|name> [qty]` (only in pawnshops `ui_view: pawnshop` / 当铺)
- **UI, Subtitles & Visuals**: `menu`, `ui|key <k>`, `view <name>`, `click <x> <y>`, `hd`/`retro`, `lang <hans|hant|en>`, `think <text|clear>` (HD-only top subtitle bar + `.repal.gz` `annotations.json` track; always `ok: true` with 50-tick coalescing; hidden while top-anchored system menus are open and reserves right clearance for the `DEBUG ENABLED` watermark), `shot [--small] [--with-thought] [path]` (default `shot` structurally excludes the thought bar; `--small` writes `640×400` of the canonical `320×200` viewport, centre-cropped to 16:10 when the compositor is wide, allowed in `--obs fair`), `golden <ref.png> [max_diff] [tol] [diff.png]`
- **Persistence & Diagnostics**:
  - `new [seed]`, `save <path>`, `load <path>` (checks `--out-dir` first, then path, then `assets/orig_saves/`), `replay-export [path.repal.gz]` (returns `segments`, `total_ticks`, `engine_version`, `tainted_by_debug`, `debug_mode`), `dump-state [path]`
  - `core-replay <file> [until_tick]`: runs through the unified `pal_runtime::replay_player::ReplayPlayer` and supports `.repal.gz` (`.tar.gz`), `RePal_CoreReplay_v3`/`v2`, `PalRemake_CoreReplay_v1`, and `CoreDiag_v1`. Embedded `ReplayEvent::Debug` events are strict no-ops unless the bundle declares `debug_mode: true`.
  - `bugreport [desc]`: headless writes `<out_dir>/bug_reports/*.png` + `*.json`; GUI writes `<user_dir>/bug_reports/*.repal.gz` (including `end_screenshot.png` + `state.json`) and submits via `gh` (disabled over TCP sockets).
  - Save slots (`UiAction::SaveSlot` / `LoadSlot`): both headless (`<out_dir>/slot_{N}.json`) and desktop (`<saves_dir>/slot_{N}.json`) use 0-based `slot_{N}.json`, with slot `0` falling back to `orig_saves/10.rpg`.

---

## 4. JSON Response Envelope (`repal-text/1`)

Every command emits one single-line JSON object conforming to [`docs/text_mode.schema.json`](text_mode.schema.json):

- `protocol`: `"repal-text/1"`
- `obs_mode`: `"debug"` or `"fair"`
- `ok`, `cmd`, `mode`, `lang` (`"hans"|"hant"|"en"`), `ui_view` (stable snake_case id), `tick`
- `view`: `{ "camera": { "x", "y", "w": 320, "h": 200 }, "hd": bool, "lang", "fb": { "w", "h" } }`
- `scene`, `party` (with `statuses` & `poisons`), `cash`, `walkable`, `walkable_dist`, `minimap` (`20×25` staggered isometric viewport grid or `null`), `events` (with `kind: "portal"|"trigger"|"npc"|"object"|"exit"`, `kind_label`, `screen`)
- `prompt`: for dialogs/choices includes `page`, `total_pages`, `page_text`, `text`, `speaker`, `choices`
- `battle`: `{ active, prompting, can_flee, spell_banner, heroes[], enemies[], feed[] }`
- `menu`: complete per-screen snapshot (items, values, grid `cols`/`rows`/`page`/`total_pages`, equip slots + candidates + stat deltas, save slots, replay chapters, battle pickers)
- `rng`: `{ id, frame, end, skippable }` when an RNG movie is playing in GUI+Text, else `null`
- `notices`: incremental array of `{ tick, kind, text }` emitted since the previous reply (toasts, scene changes, battle start/end/victory, RNG movies, game over)
- `artifacts` (command-specific structured outputs such as `shot`, `golden`, `think`, `replay`, `bag`, `status`, `walk`, `goto`, `field_magic`, `close`, `save`, `bugreport`), `log` (human-readable lines), `interrupted_by`, `error`, `error_code`

---

## 5. Exit Codes (Batch / CI)

Both headless (`-c` / `--script`) and GUI batch modes follow the same precedence:

| Code | Meaning |
| :---: | :--- |
| `0` | All commands succeeded and all `golden` checks passed. |
| `2` | One or more `golden` visual diff checks failed (takes precedence over code `1`). |
| `1` | Invalid CLI arguments, file I/O error, or command execution error (when no `golden` check failed). |
