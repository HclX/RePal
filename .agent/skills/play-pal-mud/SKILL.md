---
name: play-pal-mud
description: How an AI agent can play, explore, test, and take 4x HD screenshots of RePal (1995 Chinese Paladin) through the unified `repal` Text Mode / JSON-Lines console (`repal --text` and `repal --listen`).
---

# Playing RePal via `repal` Unified Text Mode

RePal ships as a **single unified native binary (`repal` / `repal.exe`)** supporting 3 orthogonal modes (`GUI`, `Text`, `GUI + Text`), 2 channels (`stdio`, `socket`), and 2 output formats (`plain`, `--json`). Both headless `--text` and live `--gui --text` / `--listen` execute the **exact same command engine and JSON-Lines schema**. See `docs/TEXT_MODE_SPEC.md` for the full specification.

## 1. Launching `repal` in Text Mode

In a release archive, `./repal` (or `repal.exe`) is prebuilt — no build step required. (In a source checkout, build with `cargo build --offline -j 1 -p pal-desktop --bin repal` and use `target/debug/repal`.)

- **GUI + Text Mode (watch the Agent play live inside `repal` with full 4× HD graphics, DOS UI & audio!)**:
  1. Launch `repal --listen 127.0.0.1:9999 [--json]` (or open `repal` GUI and enable **Menu → Remake → Agent: Local / LAN** at runtime), or `repal --gui --text [--json]` for stdio.
  2. Connect directly with standard `nc` / `telnet` / Python socket (line-oriented UTF-8):
     ```bash
     printf "new\ncontinue\nlook\nquit\n" | nc 127.0.0.1 9999
     ```
     - **Important TCP framing note (`resp["cmd"]`)**: Immediately on connect (in both plain and `--json` modes), `repal` sends an **unsolicited initial `look` greeting line** before reading commands. Persistent socket clients MUST drain that initial greeting line on connect AND check **`resp["cmd"]`** (echoed on every JSON reply) to guarantee 1:1 alignment — otherwise every reply (`goto`, `bag`, `sell`, etc.) will appear shifted by 1 command!
       ```python
       # Persistent TCP --json client pattern:
       greeting = json.loads(sock_file.readline())  # drain initial "look" greeting first!
       def send(cmd: str) -> dict:
           sock.sendall((cmd + "\n").encode())
           resp = json.loads(sock_file.readline())
           assert resp["cmd"] == cmd, f"out-of-sync reply: sent {cmd!r}, got {resp['cmd']!r}"
           return resp
       ```
- **Headless Text Mode** (no window / no `$DISPLAY` required; boots to Title by default unless `--new` or `--save` is passed):
  - Interactive or piped: `./repal --text --new [--json]`
  - **Fair (player-equivalent) blind playtesting**: `./repal --text --obs fair --new [--json]` (alias `--fog`; enforces 320×200 viewport visibility, hides enemy HP, blocks `/debug` & `dump-state`, provides visual-only `map` & `exit` hints)
  - Minimap cadence: `--minimap <auto|always|off>` (default `auto` emits the 20×25 isometric ASCII viewport minimap on scene entry and `look`/`map`)
  - One-shot batch / UI visual regression: `./repal --text --json -c "load 10.rpg; view status; shot status.png"`
  - Headless TCP server: `./repal --text --listen 127.0.0.1:9999 [--json]` (non-loopback LAN binds require per-session `auth <token>`, shown on startup and in TitleOptions/Remake menus; default socket `--out-dir` is `<user_dir>/agent_out`)
  - Resume from a saved checkpoint: `./repal --text --save .tmp/agent_play.json`
  - Export AI thought subtitles from a `.repal` replay: `./repal --replay-export-srt in.repal out.srt`
- **Language flags**: `--lang hans` (default), `--lang hant`, `--lang en`.
- **Human Takeover in GUI+Text Mode**: Pressing `Esc` in the desktop window immediately cancels any in-flight AI task (`interrupted_by: "human"`) and returns control to the human player. Pressing `F7` toggles the 4× HD AI thought subtitle bar.

### Stateful Multi-Turn Play Pattern (One-Shot Shell Calls)
In one-shot stdio/batch invocations, each shell call starts a fresh process — end each batch with `save .tmp/agent_play.json` and `quit`, then resume with `--save .tmp/agent_play.json`. (When connected to a running `--listen` TCP server, state stays live in memory across client connections.)

## 2. Complete Command Reference

### Observation & Screenshots
| Command | Aliases | Description |
| :--- | :--- | :--- |
| `look` | `l` | Print current scene name/ID, coordinates, facing, party HP/MP, 4 walkable directions + `walkable_dist` (free steps per direction), viewport minimap, and active nearby NPCs/exits sorted by distance. |
| `map` | `minimap`, `地图` | Print the `20×25` staggered isometric viewport ASCII minimap (`@` player, `E` visible event/NPC, `>` visible exit, `.` walkable, `#` wall/blocked, `?` occluded, ` ` exterior void) using actual visual tile graphics with zero hidden-event spoilers. |
| `screenshot [--with-thought] [path]` | `shot`, `ss`, `take_screenshot` | Render a settled `1280×800` 4× HD PNG (including `pal-ui`) to `[path]` (default `.tmp/repal_shots/shot_NNN_scXXX.png`). Default `shot` (plus `golden`, `bugreport`, thumbnails) is structurally thought-free; pass `--with-thought` to include the HD AI thought subtitle bar. |
| `bugreport [description]` | `report`, `bug` | Export a 4× HD screenshot + state diagnostics (`.repal` + background GitHub Issue via `gh` in GUI mode; `.png` + `.json` in headless mode; disabled over TCP sockets). |
| `help [cmd \| topic]` | `?` | Show localized command overview (debug commands hidden in `--obs fair`) or detailed usage, aliases, examples & tips for a specific command/topic (e.g. `help battle`, `help magic`, `help items`, `help save`; structured JSON in `artifacts.help`). |
| `status` | `st` | Print full party stats, level, EXP, equipment, and known spells with `#id`, MP cost, field/battle tags & effect descriptions in both plain text and JSON (`artifacts.status`). |
| `bag` | `items`, `inv` | List all inventory items with `#id`, counts, usability/throwable/equip tags & effect descriptions in both plain text and JSON (`artifacts.bag`). |
| `think <text \| clear>` | `thought` | Display a short AI commentary/thought subtitle at the top of the 4× HD screen (disabled in 1× Retro, `F7` toggles visibility, auto-dims after 12 s, clears on scene change) AND record it into `.repal` replays' separate `annotations.json` track (untainted, allowed in `--obs fair` & `.cmd` replays, exportable via `repal --replay-export-srt`). **Limits**: max 200 chars stored (wraps to 2 lines ~80 chars on screen), plain CJK/Latin/punctuation (no emoji/bidi), rapid calls within 50 ticks (1 s) coalesce in-place preserving original start tick (always `ok: true`), ring cap 200/segment. Wrap in quotes if your thought contains `;`. Use sparingly at key story/puzzle/battle decisions (15–40 chars)! |

### Movement & Exploration
| Command | Aliases | Description |
| :--- | :--- | :--- |
| `sw [steps]` | `左下` | Walk South-West (`1..64` half-tile steps) on the isometric collision grid. |
| `nw [steps]` | `左上` | Walk North-West (`1..64` steps). |
| `ne [steps]` | `右上` | Walk North-East (`1..64` steps). *(Use `ne`, never `n`, which means `next`.)* |
| `se [steps]` | `右下` | Walk South-East (`1..64` steps). |
| `goto <ev_id>` | `go`, `walk` | Send a standard player `Click` at an active event `ev_id` in the current scene (`click_world` runs portal-aware A\* avoiding non-target doorway/stair triggers). |
| `goto <x> <y>` | `go`, `walk` | Send a standard player `Click` at world coordinates `(x, y)` to walk via portal-aware A\*. |
| `talk [ev_id]` | — | If `ev_id` is given, send a standard player `Click` on that event's sprite (`click_world` walks up and interacts via `talk_if_close`, falling back to `Interact` on arrival); otherwise press `Interact` directly in front of the player. |
| `search` | `space`, `interact` | Trigger overworld confirm/search directly in front of the player. |
| `wait [ticks]` | `tick` | Advance `ticks` (default 1) for background NPC movement or scripted timers. |

### Dialogue, Choices & Shops
| Command | Aliases | Description |
| :--- | :--- | :--- |
| `next` | `n`, `ok` | Advance one dialogue line or dismiss battle victory. |
| `continue` | `c`, `nextall` | Advance **all consecutive dialogue lines** (returning every skipped line in `log`) until a `Yes/No` choice, battle, shop, or free exploration. |
| `yes` / `no` | `y`/`1`/`是`, `2`/`否` | Answer a `Yes/No` prompt. |
| `close` | `leave` | Close an open shop/menu modal (`UiKey::Cancel`). |

### Hierarchical In-Game Menu (Preferred in Live Spectator Mode!)
> **Spectator Mode Rule (IMPORTANT)**: When connected to `repal --listen` / `--gui --text`, **ALWAYS prefer using the real in-game RPG menu** (`menu` + `ui up/down/left/right/ok/back/tab` or `up`/`down`/`back`/`ok`/`tab`) for checking status, using items, equipping gear, casting field magic, shopping, saving/loading, and selecting battle commands in the Bagua wheel so human viewers can watch every cursor movement and decision on screen! Only fall back to direct `use`/`equip`/`attack`/`magic` shortcuts in headless batch testing.

In explore mode, open the full RPG menu with `menu` (or `m`). Both headless and GUI+Text modes set `"mode": "menu"` and populate `"menu"` with the active screen, cursor, and visible items.

| Command (inside menu / battle HUD) | Aliases | Description |
| :--- | :--- | :--- |
| `menu` | `m` | Open the main menu (`Status`, `Magic`, `Items -> Use / Equip`, `System -> Save / Load`, `Remake`). |
| `ui up` / `ui down` | `key up/down`, `up`, `down` | Move menu cursor up/down (or select Bagua Up=Attack / Down=Misc in battle). |
| `ui left` / `ui right` | `key left/right`, `left`, `right` | Move cursor left/right (vertical menus: prev/next; Status: cycle party members; Save/Load: flip pages; Battle: Bagua Left=Magic / Right=Coop). |
| `ui pgup` / `ui pgdn` / `ui home` / `ui end` | `key pgup/pgdn/home/end` | Page up/down (7 rows) or jump to start/end in Item & Magic lists. |
| `ui ok` | `key confirm`, `confirm`, `enter`, `ctrl` | Confirm highlighted menu / shop / battle option. |
| `ui back` | `key cancel`, `back`, `cancel`, `esc`, `ins`, `close` | Go back one menu level (or close menu from Root). |

### Items, Field Magic & Equipment (Headless Batch Shortcuts — avoid in Live Spectator Mode)
| Command | Description |
| :--- | :--- |
| `use <id \| name> [slot \| party_idx]` | Use a usable item (by `#id` or name) from the bag on party slot `party_idx` (default leader `0`), trigger field/quest items (e.g. `破天锤`, `桂花酒`), OR in battle automatically use/throw the item (equivalent to `item`). |
| `magic <id \| name> [target_idx] [caster_idx]` | Cast a field-usable spell (by `#id` or name, e.g. `magic 气疗术 0` or `magic 296 0`) in `explore` mode (aliases: `cast`, `m <id\|name> ...`). |
| `equip <id \| name> [party_idx]` | Equip a weapon/armor/accessory (by `#id` or name) on party slot `party_idx` (default `0`). |
| `buy <shop_idx> [qty]` / `sell <id \| name> [qty]` | Transact inside active Shop / Pawnshop. **Note**: Regular shops (`ui_view: shop`, e.g. blacksmith/medicine shops) are **buy-only** (`buy`); `sell` ONLY works inside a **Pawnshop** (`ui_view: pawnshop` / 当铺). |

### Turn-Based Battles
| Command | Aliases | Description |
| :--- | :--- | :--- |
| `attack [slot]` | `a`, `atk` | Physical attack on enemy slot (default first alive enemy). |
| `magic <id \| name> [slot]` | `cast` | Cast spell by `#id` or name on enemy/ally slot (omitting `slot` picks first alive enemy or lowest-HP ally). |
| `item <id \| name> [slot]` | `i`, `use` | Use or throw item by `#id` or name in battle (`use` auto-routes here during battle). |
| `coop [slot]` | — | Execute party Coop Magic on enemy slot. |
| `defend` | `def` | Defend (halves incoming damage). |
| `flee` | `run` | Attempt to flee from battle. |
| `auto` / `cancel-auto` | — | Toggle auto-battle (`围攻`). |

### Persistence & Replays
| Command | Description |
| :--- | :--- |
| `new [seed]` | Start a new game from Scene 1 (starts a fresh untainted `.repal` recording). |
| `save [path]` | Save full state JSON (default `repal_save.json`). |
| `load <path>` | Load a `.json` save or original DOS `.rpg` save. |
| `replay-export [path.repal]` | Export the current session's deterministic `.repal` replay archive (including `annotations.json` AI thoughts, `tainted_by_debug`, and `debug_mode` status; alias `export-replay`). |
| `quit` | `exit`, `q` — Disconnect client (socket) or exit `repal --text` (stdio). |

### Embedded Debug Commands (`/` prefix — **DO NOT USE** mutating commands during legitimate playtesting!)
- **Read-only diagnosis (safe)**: `/state`, `/event <id>`
- **Mutating debug/cheat (enables `debug_mode: true` with `DEBUG ENABLED` watermark, sets `tainted_by_debug: true`, and records deterministic `ReplayEvent::Debug` events; blocked in `--obs fair` & remote TCP)**: `/tp <scene> [x y]`, `/heal`, `/god`, `/cash <amt>`, `/item <id> [count]`, `/event <id> state <n>`, `/battle <group_id>`, `/script <label>`.

## 3. Agent Exploration Tips
1. **Read the dialogues**: Quest objectives and next NPCs are told directly in the dialogue lines returned by `continue` (`log`).
2. **Giving quest items to NPCs**: In 1995 Chinese Paladin, giving a quest item (e.g., 桂花酒 `#272` to 醉道士) requires standing right next to / facing the NPC (`goto <ev_id>`) and running **`use <item_id>`** — `talk` alone just repeats their hint!
3. **Navigate blind with `map` + `walkable_dist` (`--obs fair`)**: In `--obs fair` mode, `map` shows the exact walkable floor (`.`), walls (`#`), occluded cells (`?`), exterior void (` `), visible events/NPCs (`E`), and visible doorways/stairs (`>`). Use `goto <ev_id>` for visible exits/NPCs, or `sw`/`nw`/`ne`/`se [steps]` guided by `walkable_dist` to explore rooms without taking any screenshots.
4. **Interacting with exits vs NPCs**:
   - For `出口/传送` / `出口` (`portal` / `exit`): `goto <ev_id>` walks onto the trigger zone and transitions scenes automatically.
   - **Blocked exits / closed doors**: If an exit is blocked by adjacent objects (or `goto` returns `unreachable` with `artifacts.goto.blockers`), try `talk <id>` or `search` on the blocking object first — many closed doors, curtains, and fences open when examined, after which `goto <exit_id>` enters cleanly.
   - For `NPC/交互` (people/chests/items): `talk <ev_id>` walks up to them and triggers their interaction script.
   - If `goto <ev_id>` stops 1–2 steps short because the NPC is solid, follow up with `talk` or step 1 tile toward them and `search`.
5. **Narrate key decisions with `think`**: Emit concise `think <text>` commentary (15–40 chars, plain CJK/Latin without emoji; wrap in quotes if containing `;`) when arriving at new objectives, solving puzzles, or making tactical battle choices — it appears live in 4× HD and is recorded cleanly in `.repal` (`annotations.json`) and exportable to `.srt`.
6. **Take screenshots & file bugs**: `shot [path]` saves a settled thought-free 4× HD PNG (`shot --with-thought [path]` includes the HD subtitle bar), and `bugreport <description>` exports full diagnostics!
7. **Spectator Mode pacing**: When connected to `repal --listen` / `--gui --text` with a human watching, do **not** spam skip keys during cutscenes, and let dialogues advance naturally so spectators can read the story.
