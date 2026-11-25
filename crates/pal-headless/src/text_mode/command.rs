//! Canonical command AST, alias resolution, and batch tokenizer for RePal Text Mode.
//!
//! Shared identically between headless `--text` and live `--gui --text` / `--listen`.

use pal_core::Language;
use pal_ui::UiKey;

/// Reference to an item or spell by numeric ID (`296`, `#296`) or localized name (`气疗术`, `Cure`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetRef {
    Id(u16),
    Name(String),
}

impl TargetRef {
    pub fn display_token(&self) -> String {
        match self {
            Self::Id(id) => format!("#{id}"),
            Self::Name(s) => format!("'{s}'"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Quit,
    Help(Option<String>),
    Look,
    Map,

    // Exploration & movement
    Walk { dir: u8, steps: usize },
    GotoEvent(u16),
    GotoCoords(i32, i32),
    Talk(Option<u16>),
    Search,
    Wait(usize),

    // Dialogue, choices & modals
    Next,
    Continue,
    Yes,
    No,
    Close,

    // UI menu navigation & automation
    OpenMenu,
    Key(UiKey),
    UiShortcut(UiKey),
    View(String),
    Click { x: i32, y: i32 },

    // Battle (and field magic casting in explore mode, #234)
    BattleAttack { slot: Option<usize> },
    BattleMagic {
        magic: TargetRef,
        slot: Option<usize>,
        caster: Option<usize>,
    },
    BattleItem { item: TargetRef, slot: Option<usize> },
    BattleCoop { slot: Option<usize> },
    BattleDefend,
    BattleFlee,
    BattleAuto,
    BattleCancelAuto,

    // Party, inventory & shops
    Status,
    Bag,
    UseItem { item: TargetRef, party_idx: Option<usize> },
    EquipItem { item: TargetRef, party_idx: usize },
    Buy { shop_idx: usize, qty: usize },
    Sell { item: TargetRef, qty: usize },

    // Display & visual regression
    SetHd(bool),
    SetLang(Language),
    Shot {
        path: Option<String>,
        with_thought: bool,
    },
    CaptureStart {
        path: String,
        stride: u32,
        with_thought: bool,
    },
    CaptureStop,
    CaptureStatus,
    Golden {
        ref_path: String,
        max_diff: f64,
        tolerance: u8,
        diff_out: Option<String>,
    },
    CoreReplay {
        path: String,
        until_tick: Option<u64>,
    },
    DumpState(Option<String>),

    // Session, persistence & diagnostics
    NewGame(Option<u32>),
    Save(String),
    Load(String),
    ExportReplay(String),
    Think(Option<String>),
    BugReport(String),

    // Debug / sandbox (`/` prefix)
    DebugTp {
        scene_id: u16,
        coords: Option<(i32, i32)>,
    },
    DebugHeal,
    DebugGod,
    DebugCash(i32),
    DebugItem { item_id: u16, qty: i32 },
    DebugEventInspect(u16),
    DebugEventSetState(u16, i32),
    DebugBattle(i32),
    DebugScript(String),
    DebugState,
}

/// Split a `-c` string or `--script` file into individual command lines, stripping comments
/// (`#`, `//`) and splitting `;`-chained commands while preserving `;` inside quotes.
pub fn split_batch_commands(text: &str) -> Vec<String> {
    let mut cmds = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("//") {
            continue;
        }
        let mut cur = String::new();
        let mut quote: Option<char> = None;
        let mut prev_ch: Option<char> = None;
        let mut chars = t.chars().peekable();
        while let Some(ch) = chars.next() {
            match quote {
                Some(q) => {
                    if ch == '\\' && q == '"' && chars.peek() == Some(&'"') {
                        cur.push(ch);
                        if let Some(next_ch) = chars.next() {
                            cur.push(next_ch);
                            prev_ch = Some(next_ch);
                            continue;
                        }
                    } else {
                        if ch == q {
                            quote = None;
                        }
                        cur.push(ch);
                    }
                }
                None => {
                    // Only treat `'` as a quote opener at token start (not inside contractions like
                    // `Ling'er` or `don't`), and never treat `'` as a quote on `think`/`thought` (#240).
                    let is_think_cmd = {
                        let head = cur.trim_start().split_whitespace().next().unwrap_or("");
                        head.eq_ignore_ascii_case("think") || head.eq_ignore_ascii_case("thought")
                    };
                    let at_token_start = prev_ch.is_none_or(char::is_whitespace);
                    if ch == '"' || (ch == '\'' && at_token_start && !is_think_cmd) {
                        quote = Some(ch);
                        cur.push(ch);
                    } else if ch == ';' {
                        let p = cur.trim();
                        if !p.is_empty() && !p.starts_with('#') && !p.starts_with("//") {
                            cmds.push(p.to_string());
                        }
                        cur.clear();
                    } else {
                        cur.push(ch);
                    }
                }
            }
            prev_ch = Some(ch);
        }
        let p = cur.trim();
        if !p.is_empty() && !p.starts_with('#') && !p.starts_with("//") {
            cmds.push(p.to_string());
        }
    }
    cmds
}

/// Split a single command line into whitespace-separated tokens, supporting `"..."` and `'...'`
/// quoted arguments (e.g. file paths with spaces on Windows/macOS).
/// Backslashes are preserved literally except `\"` inside double quotes so Windows paths like
/// `"C:\My Saves\1.json"` work without double-escaping.
fn tokenize_command(raw: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut chars = raw.trim().chars().peekable();

    while let Some(ch) = chars.next() {
        match quote {
            Some(q) => {
                if ch == '\\' && q == '"' && chars.peek() == Some(&'"') {
                    let _ = chars.next();
                    cur.push('"');
                } else if ch == q {
                    quote = None;
                } else {
                    cur.push(ch);
                }
            }
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                    in_token = true;
                } else if ch.is_whitespace() {
                    if in_token {
                        tokens.push(std::mem::take(&mut cur));
                        in_token = false;
                    }
                } else {
                    cur.push(ch);
                    in_token = true;
                }
            }
        }
    }
    if let Some(q) = quote {
        return Err(format!("unterminated quote ({q}) in command"));
    }
    if in_token {
        tokens.push(cur);
    }
    Ok(tokens)
}

/// Parse a `UiKey` token for `key <k>` and `ui <k>`.
pub fn parse_ui_key(token: &str) -> Option<UiKey> {
    match token.trim().to_ascii_lowercase().as_str() {
        "up" | "u" => Some(UiKey::Up),
        "down" | "d" => Some(UiKey::Down),
        "left" | "l" => Some(UiKey::Left),
        "right" | "r" => Some(UiKey::Right),
        "confirm" | "ok" | "enter" | "space" | "ctrl" => Some(UiKey::Confirm),
        "cancel" | "back" | "esc" | "escape" | "b" | "insert" | "ins" => Some(UiKey::Cancel),
        "menu" | "alt" | "m" => Some(UiKey::MenuToggle),
        "tab" | "t" => Some(UiKey::Tab),
        "pgup" | "pageup" | "page_up" => Some(UiKey::PageUp),
        "pgdn" | "pagedown" | "page_down" => Some(UiKey::PageDown),
        "home" => Some(UiKey::Home),
        "end" => Some(UiKey::End),
        "repeat" | "battle_repeat" => Some(UiKey::BattleRepeat),
        "auto" | "battle_auto" => Some(UiKey::BattleAuto),
        "defend" | "battle_defend" => Some(UiKey::BattleDefend),
        "useitem" | "battle_use_item" => Some(UiKey::BattleUseItem),
        "throwitem" | "throw" | "battle_throw_item" => Some(UiKey::BattleThrowItem),
        "flee" | "battle_flee" => Some(UiKey::BattleFlee),
        "force" | "battle_force" => Some(UiKey::BattleForce),
        "status" | "battle_status" => Some(UiKey::BattleStatus),
        _ => None,
    }
}

fn parse_event_id(tok: &str) -> Option<u16> {
    tok.trim_start_matches("ev#")
        .trim_start_matches('#')
        .parse::<u16>()
        .ok()
}

fn parse_target_ref(tok: &str) -> Result<TargetRef, String> {
    let trimmed = tok.trim();
    if trimmed.is_empty() {
        return Err("missing item/magic id or name".to_string());
    }
    let stripped = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if let Ok(id) = stripped.parse::<u16>() {
        return Ok(TargetRef::Id(id));
    }
    Ok(TargetRef::Name(trimmed.to_string()))
}

/// Parse a single trimmed command line into a strongly-typed [`Command`].
pub fn parse_command(raw: &str) -> Result<Command, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty command".to_string());
    }
    // Parse `think` / `thought` before shell-quote tokenization so natural prose containing
    // apostrophes (e.g. `think Ling'er is waiting`) never triggers unmatched-quote errors.
    let mut head_split = trimmed.splitn(2, char::is_whitespace);
    let head = head_split.next().unwrap_or("").to_ascii_lowercase();
    if matches!(head.as_str(), "think" | "thought") {
        let rest = head_split.next().unwrap_or("").trim();
        if rest.is_empty() {
            return Err("usage: think <text | clear>".to_string());
        }
        if matches!(rest.to_ascii_lowercase().as_str(), "clear" | "off" | "none") {
            return Ok(Command::Think(None));
        }
        let unwrapped = if (rest.starts_with('"') && rest.ends_with('"') && rest.len() >= 2)
            || (rest.starts_with('\'') && rest.ends_with('\'') && rest.len() >= 2)
        {
            &rest[1..rest.len() - 1]
        } else {
            rest
        };
        return Ok(Command::Think(Some(unwrapped.to_string())));
    }

    let tokens = tokenize_command(raw)?;
    if tokens.is_empty() {
        return Err("empty command".to_string());
    }
    let cmd = tokens[0].to_ascii_lowercase();
    let args: Vec<&str> = tokens[1..].iter().map(|s| s.as_str()).collect();
    let args = args.as_slice();

    match cmd.as_str() {
        "quit" | "exit" | "q" => Ok(Command::Quit),
        "help" | "?" => {
            let topic = if args.is_empty() {
                None
            } else {
                Some(args.join(" "))
            };
            Ok(Command::Help(topic))
        }
        "look" | "l" => Ok(Command::Look),
        "map" | "minimap" | "地图" => Ok(Command::Map),

        // Movement (collision-free canonical isometric directions + Chinese aliases)
        "sw" | "左下" | "nw" | "左上" | "ne" | "右上" | "se" | "右下" => {
            let dir = match cmd.as_str() {
                "sw" | "左下" => 0,
                "nw" | "左上" => 1,
                "ne" | "右上" => 2,
                _ => 3,
            };
            let steps = match args.first() {
                Some(s) => s
                    .parse::<usize>()
                    .map_err(|_| format!("invalid step count '{s}' (usage: {cmd} [steps])"))?
                    .clamp(1, 64),
                None => 1,
            };
            Ok(Command::Walk { dir, steps })
        }

        "goto" | "go" | "walk" => match args.len() {
            1 => parse_event_id(args[0])
                .map(Command::GotoEvent)
                .ok_or_else(|| "usage: goto <ev_id | x y>".to_string()),
            2 => {
                let (Ok(x), Ok(y)) = (args[0].parse::<i32>(), args[1].parse::<i32>()) else {
                    return Err("usage: goto <ev_id | x y>".to_string());
                };
                Ok(Command::GotoCoords(x, y))
            }
            _ => Err("usage: goto <ev_id | x y>".to_string()),
        },

        "talk" => match args.first() {
            None => Ok(Command::Talk(None)),
            Some(tok) => parse_event_id(tok)
                .map(|id| Command::Talk(Some(id)))
                .ok_or_else(|| "usage: talk [ev_id]".to_string()),
        },
        "search" | "space" | "interact" => Ok(Command::Search),

        "wait" | "tick" => {
            let n = match args.first() {
                Some(s) => s
                    .parse::<usize>()
                    .map_err(|_| format!("invalid tick count '{s}' (usage: wait [ticks])"))?
                    .clamp(1, 600),
                None => 1,
            };
            Ok(Command::Wait(n))
        }

        // Dialogue & choices
        "next" | "n" | "ok" => Ok(Command::Next),
        "continue" | "c" | "nextall" => Ok(Command::Continue),
        "yes" | "y" | "1" | "是" => Ok(Command::Yes),
        "no" | "2" | "否" => Ok(Command::No),
        "close" | "leave" => Ok(Command::Close),

        // UI & Menu navigation
        "menu" => Ok(Command::OpenMenu),
        "m" => {
            if args.is_empty() {
                Ok(Command::OpenMenu)
            } else {
                let magic = parse_target_ref(args[0])
                    .map_err(|_| "usage: magic <id|name> [target_slot] [caster_idx]".to_string())?;
                let slot = match args.get(1) {
                    Some(s) => Some(
                        s.parse::<usize>()
                            .map_err(|_| "usage: magic <id|name> [target_slot] [caster_idx]".to_string())?,
                    ),
                    None => None,
                };
                let caster = match args.get(2) {
                    Some(s) => Some(
                        s.parse::<usize>()
                            .map_err(|_| "usage: magic <id|name> [target_slot] [caster_idx]".to_string())?,
                    ),
                    None => None,
                };
                Ok(Command::BattleMagic {
                    magic,
                    slot,
                    caster,
                })
            }
        }
        "esc" => Ok(Command::Key(UiKey::Cancel)),
        "key" | "ui" => {
            let Some(k) = args.first() else {
                return Err(format!(
                    "usage: {cmd} <up|down|left|right|confirm|cancel|menu|tab>"
                ));
            };
            parse_ui_key(k)
                .map(Command::Key)
                .ok_or_else(|| format!("unknown key '{k}'"))
        }
        "up" | "down" | "left" | "right" | "back" | "cancel" | "tab" | "confirm" | "enter" => {
            Ok(Command::UiShortcut(parse_ui_key(&cmd).unwrap()))
        }
        "view" => {
            let Some(v) = args.first() else {
                return Err(
                    "usage: view <title|field|menu|status|equip|magic|items|system|pawnshop|battle|dialog>"
                        .to_string(),
                );
            };
            Ok(Command::View(v.to_ascii_lowercase()))
        }
        "click" => {
            let (Some(Ok(x)), Some(Ok(y))) = (
                args.first().map(|s| s.parse::<i32>()),
                args.get(1).map(|s| s.parse::<i32>()),
            ) else {
                return Err("usage: click <x> <y>".to_string());
            };
            Ok(Command::Click { x, y })
        }

        // Battle commands
        "attack" | "a" | "atk" => {
            let slot = match args.first() {
                Some(s) => Some(
                    s.parse::<usize>()
                        .map_err(|_| "usage: attack [slot]".to_string())?,
                ),
                None => None,
            };
            Ok(Command::BattleAttack { slot })
        }
        "magic" | "cast" => {
            let Some(tok) = args.first() else {
                return Err("usage: magic <id|name> [target_slot] [caster_idx]".to_string());
            };
            let magic = parse_target_ref(tok)
                .map_err(|_| "usage: magic <id|name> [target_slot] [caster_idx]".to_string())?;
            let slot = match args.get(1) {
                Some(s) => Some(
                    s.parse::<usize>()
                        .map_err(|_| "usage: magic <id|name> [target_slot] [caster_idx]".to_string())?,
                ),
                None => None,
            };
            let caster = match args.get(2) {
                Some(s) => Some(
                    s.parse::<usize>()
                        .map_err(|_| "usage: magic <id|name> [target_slot] [caster_idx]".to_string())?,
                ),
                None => None,
            };
            Ok(Command::BattleMagic {
                magic,
                slot,
                caster,
            })
        }
        "item" | "i" => {
            let Some(tok) = args.first() else {
                return Err("usage: item <id|name> [slot]".to_string());
            };
            let item = parse_target_ref(tok)
                .map_err(|_| "usage: item <id|name> [slot]".to_string())?;
            let slot = match args.get(1) {
                Some(s) => Some(
                    s.parse::<usize>()
                        .map_err(|_| "usage: item <id|name> [slot]".to_string())?,
                ),
                None => None,
            };
            Ok(Command::BattleItem { item, slot })
        }
        "coop" => {
            let slot = match args.first() {
                Some(s) => Some(
                    s.parse::<usize>()
                        .map_err(|_| "usage: coop [slot]".to_string())?,
                ),
                None => None,
            };
            Ok(Command::BattleCoop { slot })
        }
        "defend" | "def" => Ok(Command::BattleDefend),
        "flee" | "run" => Ok(Command::BattleFlee),
        "auto" => Ok(Command::BattleAuto),
        "cancel-auto" => Ok(Command::BattleCancelAuto),

        // Party, inventory & shops
        "status" | "st" => Ok(Command::Status),
        "bag" | "items" | "inv" => Ok(Command::Bag),
        "use" => {
            let Some(tok) = args.first() else {
                return Err("usage: use <item_id|name> [slot|party_idx]".to_string());
            };
            let item = parse_target_ref(tok)
                .map_err(|_| "usage: use <item_id|name> [slot|party_idx]".to_string())?;
            let party_idx = match args.get(1) {
                Some(s) => Some(
                    s.parse::<usize>()
                        .map_err(|_| "usage: use <item_id|name> [slot|party_idx]".to_string())?,
                ),
                None => None,
            };
            Ok(Command::UseItem { item, party_idx })
        }
        "equip" => {
            let Some(tok) = args.first() else {
                return Err("usage: equip <item_id|name> [party_idx]".to_string());
            };
            let item = parse_target_ref(tok)
                .map_err(|_| "usage: equip <item_id|name> [party_idx]".to_string())?;
            let party_idx = match args.get(1) {
                Some(s) => s
                    .parse::<usize>()
                    .map_err(|_| "usage: equip <item_id|name> [party_idx]".to_string())?,
                None => 0,
            };
            Ok(Command::EquipItem { item, party_idx })
        }
        "buy" => {
            let Some(Ok(idx)) = args.first().map(|s| s.parse::<usize>()) else {
                return Err("usage: buy <shop_idx> [qty]".to_string());
            };
            let qty = match args.get(1) {
                Some(s) => s
                    .parse::<usize>()
                    .map_err(|_| "usage: buy <shop_idx> [qty]".to_string())?
                    .max(1),
                None => 1,
            };
            Ok(Command::Buy { shop_idx: idx, qty })
        }
        "sell" => {
            let Some(tok) = args.first() else {
                return Err("usage: sell <item_id|name> [qty]".to_string());
            };
            let item = parse_target_ref(tok)
                .map_err(|_| "usage: sell <item_id|name> [qty]".to_string())?;
            let qty = match args.get(1) {
                Some(s) => s
                    .parse::<usize>()
                    .map_err(|_| "usage: sell <item_id|name> [qty]".to_string())?
                    .max(1),
                None => 1,
            };
            Ok(Command::Sell { item, qty })
        }

        // Display & visual regression
        "hd" => Ok(Command::SetHd(true)),
        "retro" => Ok(Command::SetHd(false)),
        "lang" => {
            let Some(l) = args.first() else {
                return Err("usage: lang <hans|hant|en>".to_string());
            };
            let lang = match l.to_ascii_lowercase().as_str() {
                "en" | "english" => Language::En,
                "hans" | "zh-cn" | "cn" => Language::ZhHans,
                "hant" | "zh-tw" | "tw" => Language::ZhHant,
                _ => return Err(format!("unknown lang '{l}' (expected hans|hant|en)")),
            };
            Ok(Command::SetLang(lang))
        }
        "shot" | "screenshot" | "ss" | "take_screenshot" => {
            let mut with_thought = false;
            let mut path: Option<String> = None;
            for &arg in args {
                if arg == "--with-thought" {
                    with_thought = true;
                } else if path.is_none() {
                    path = Some(arg.to_string());
                } else {
                    return Err("usage: shot [--with-thought] [path]".to_string());
                }
            }
            Ok(Command::Shot { path, with_thought })
        }
        "capture" | "rec-video" => {
            let sub = args.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            match sub.as_str() {
                "start" => {
                    let mut path: Option<String> = None;
                    let mut stride: u32 = 1;
                    let mut with_thought = false;
                    let mut i = 1usize;
                    while i < args.len() {
                        match args[i] {
                            "--with-thought" | "--thought" => {
                                with_thought = true;
                                i += 1;
                            }
                            "--stride" => {
                                let Some(v) = args.get(i + 1) else {
                                    return Err("usage: capture start <out.mp4|out.gif|dir> [--stride N] [--with-thought]".to_string());
                                };
                                stride = v
                                    .parse::<u32>()
                                    .ok()
                                    .filter(|&n| n >= 1)
                                    .ok_or_else(|| format!("invalid stride '{v}' (expected >= 1)"))?;
                                i += 2;
                            }
                            other => {
                                if path.is_none() {
                                    path = Some(other.to_string());
                                    i += 1;
                                } else {
                                    return Err(format!("unexpected capture argument '{other}'"));
                                }
                            }
                        }
                    }
                    let Some(path) = path else {
                        return Err("usage: capture start <out.mp4|out.gif|dir> [--stride N] [--with-thought]".to_string());
                    };
                    Ok(Command::CaptureStart {
                        path,
                        stride,
                        with_thought,
                    })
                }
                "stop" => Ok(Command::CaptureStop),
                "status" | "" => Ok(Command::CaptureStatus),
                other => Err(format!(
                    "unknown capture subcommand '{other}' (expected: start, stop, status)"
                )),
            }
        }
        "golden" => {
            let Some(ref_path) = args.first() else {
                return Err(
                    "usage: golden <ref.png> [max_diff_pct] [tolerance] [diff_out.png]".to_string(),
                );
            };
            let max_diff = match args.get(1) {
                Some(s) => s
                    .parse::<f64>()
                    .map_err(|_| format!("invalid max_diff_pct '{s}'"))?,
                None => 0.5,
            };
            let tolerance = match args.get(2) {
                Some(s) => s
                    .parse::<u8>()
                    .map_err(|_| format!("invalid tolerance '{s}'"))?,
                None => 2,
            };
            let diff_out = args.get(3).map(|s| s.to_string());
            Ok(Command::Golden {
                ref_path: (*ref_path).to_string(),
                max_diff,
                tolerance,
                diff_out,
            })
        }
        "core-replay" => {
            let Some(path) = args.first() else {
                return Err("usage: core-replay <file.json> [until_tick]".to_string());
            };
            let until_tick = match args.get(1) {
                Some(s) => Some(
                    s.parse::<u64>()
                        .map_err(|_| format!("invalid until_tick '{s}'"))?,
                ),
                None => None,
            };
            Ok(Command::CoreReplay {
                path: (*path).to_string(),
                until_tick,
            })
        }
        "dump-state" => Ok(Command::DumpState(args.first().map(|s| s.to_string()))),

        // Session & persistence
        "new" | "/new" => {
            let seed = match args.first() {
                Some(s) => Some(
                    s.parse::<u32>()
                        .map_err(|_| format!("invalid seed '{s}'"))?,
                ),
                None => None,
            };
            Ok(Command::NewGame(seed))
        }
        "save" => {
            let path = args.first().copied().unwrap_or("repal_save.json");
            Ok(Command::Save(path.to_string()))
        }
        "load" => {
            let Some(p) = args.first() else {
                return Err("usage: load <path.rpg|path.json>".to_string());
            };
            Ok(Command::Load((*p).to_string()))
        }
        "replay-export" | "export-replay" => {
            let path = args.first().copied().unwrap_or("repal_recording.repal");
            Ok(Command::ExportReplay(path.to_string()))
        }
        "bugreport" | "report" | "bug" => {
            let desc = if args.is_empty() {
                "repal text-mode".to_string()
            } else {
                args.join(" ")
            };
            Ok(Command::BugReport(desc))
        }

        // Debug (`/`) commands
        "/tp" => {
            let Some(Ok(scene_id)) = args.first().map(|s| s.parse::<u16>()) else {
                return Err("usage: /tp <scene_id> [x y]".to_string());
            };
            let coords = if args.len() >= 3 {
                let (Ok(x), Ok(y)) = (args[1].parse::<i32>(), args[2].parse::<i32>()) else {
                    return Err("usage: /tp <scene_id> [x y]".to_string());
                };
                Some((x, y))
            } else if args.len() == 2 {
                return Err("usage: /tp <scene_id> [x y]".to_string());
            } else {
                None
            };
            Ok(Command::DebugTp { scene_id, coords })
        }
        "/heal" => Ok(Command::DebugHeal),
        "/god" => Ok(Command::DebugGod),
        "/cash" => {
            let Some(Ok(n)) = args.first().map(|s| s.parse::<i32>()) else {
                return Err("usage: /cash <amount>".to_string());
            };
            Ok(Command::DebugCash(n))
        }
        "/item" => {
            let Some(Ok(item_id)) = args.first().map(|s| s.parse::<u16>()) else {
                return Err("usage: /item <id> [qty]".to_string());
            };
            let qty = match args.get(1) {
                Some(s) => s
                    .parse::<i32>()
                    .map_err(|_| "usage: /item <id> [qty]".to_string())?,
                None => 1,
            };
            Ok(Command::DebugItem { item_id, qty })
        }
        "/event" => {
            let Some(Ok(eid)) = args.first().map(|s| s.parse::<u16>()) else {
                return Err("usage: /event <id> [state <n>]".to_string());
            };
            if args.get(1).copied() == Some("state") {
                let Some(Ok(st)) = args.get(2).map(|s| s.parse::<i32>()) else {
                    return Err("usage: /event <id> state <n>".to_string());
                };
                return Ok(Command::DebugEventSetState(eid, st));
            }
            Ok(Command::DebugEventInspect(eid))
        }
        "/battle" => {
            let Some(Ok(gid)) = args.first().map(|s| s.parse::<i32>()) else {
                return Err("usage: /battle <group_id>".to_string());
            };
            Ok(Command::DebugBattle(gid))
        }
        "/script" => {
            let Some(lbl) = args.first() else {
                return Err("usage: /script <label>".to_string());
            };
            Ok(Command::DebugScript((*lbl).to_string()))
        }
        "/state" => Ok(Command::DebugState),

        _ => Err(format!("unknown command '{cmd}' (type 'help')")),
    }
}

pub const HELP_TEXT: &str = "\
=== RePal Unified Text Mode ===
探索: look(l) | map | sw/nw/ne/se [步数] | goto <ev_id | x y> | talk [ev_id] | search | wait [ticks]
对话: next(n/ok) | continue(c) | yes(1) | no(2) | close
菜单: menu(m) | ui <up|down|left|right|ok|back|tab|menu> | key <k>
战斗: attack(a) [slot] | magic <id> [slot] | item <id> [slot] | coop [slot] | defend | flee | auto | cancel-auto
队伍: status(st) | bag | use <id> [party_idx] | magic <id> [target_idx] [caster_idx] | equip <id> [party_idx] | buy <idx> [qty] | sell <id> [qty]
UI/视觉: view <name> | click <x> <y> | hd | retro | lang <hans|hant|en> | think <text|clear> | shot [--with-thought] [path] | golden <ref> [max_diff] [tol] [diff_out] | core-replay <json> [tick] | dump-state [path]
存档/调试: new [seed] | save [path] | load <path> | replay-export [path.repal] | bugreport [desc] | /tp <sc> [x y] | /heal | /god | /cash <n> | /item <id> [n] | /event <id> [state <n>] | /battle <g> | /script <lbl> | /state
退出: quit(q)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_disambiguation_n_is_next() {
        assert_eq!(parse_command("n").unwrap(), Command::Next);
        assert_eq!(parse_command("map").unwrap(), Command::Map);
        assert_eq!(parse_command("ne 3").unwrap(), Command::Walk { dir: 2, steps: 3 });
        assert_eq!(parse_command("sw 5").unwrap(), Command::Walk { dir: 0, steps: 5 });
        assert_eq!(parse_command("m").unwrap(), Command::OpenMenu);
        assert_eq!(
            parse_command("m 12 1").unwrap(),
            Command::BattleMagic {
                magic: TargetRef::Id(12),
                slot: Some(1),
                caster: None,
            }
        );
        assert_eq!(
            parse_command("magic 296 0 1").unwrap(),
            Command::BattleMagic {
                magic: TargetRef::Id(296),
                slot: Some(0),
                caster: Some(1),
            }
        );
        assert_eq!(
            parse_command("magic 气疗术 0").unwrap(),
            Command::BattleMagic {
                magic: TargetRef::Name("气疗术".to_string()),
                slot: Some(0),
                caster: None,
            }
        );
        assert_eq!(
            parse_command("use 金创药 0").unwrap(),
            Command::UseItem {
                item: TargetRef::Name("金创药".to_string()),
                party_idx: Some(0),
            }
        );
        assert_eq!(parse_command("help").unwrap(), Command::Help(None));
        assert_eq!(
            parse_command("help battle").unwrap(),
            Command::Help(Some("battle".to_string()))
        );
    }

    #[test]
    fn batch_split_strips_comments_and_semicolons() {
        let cmds = split_batch_commands(
            "# header\nload 10.rpg; view status; shot out.png\n// comment\n  ; look\n",
        );
        assert_eq!(cmds, vec!["load 10.rpg", "view status", "shot out.png", "look"]);
    }

    #[test]
    fn goto_accepts_ev_prefix_and_coords() {
        assert_eq!(parse_command("goto ev#12").unwrap(), Command::GotoEvent(12));
        assert_eq!(parse_command("goto #12").unwrap(), Command::GotoEvent(12));
        assert_eq!(parse_command("goto 12").unwrap(), Command::GotoEvent(12));
        assert_eq!(parse_command("goto 100 200").unwrap(), Command::GotoCoords(100, 200));
        assert!(parse_command("goto abc").is_err());
    }

    #[test]
    fn quoted_paths_and_semicolons_are_preserved() {
        assert_eq!(
            parse_command(r#"load "C:\My Saves\slot 1.json""#).unwrap(),
            Command::Load(r"C:\My Saves\slot 1.json".to_string())
        );
        assert_eq!(
            parse_command("shot 'my screen shot.png'").unwrap(),
            Command::Shot {
                path: Some("my screen shot.png".to_string()),
                with_thought: false,
            }
        );
        assert_eq!(
            parse_command("shot --with-thought 'my shot.png'").unwrap(),
            Command::Shot {
                path: Some("my shot.png".to_string()),
                with_thought: true,
            }
        );
        assert_eq!(
            parse_command("shot --with-thought").unwrap(),
            Command::Shot {
                path: None,
                with_thought: true,
            }
        );
        assert!(parse_command("shot \"unclosed").is_err());

        let cmds = split_batch_commands(
            r#"bugreport "scene 1; dialog test"; shot "out dir/shot 1.png"; look"#,
        );
        assert_eq!(
            cmds,
            vec![
                r#"bugreport "scene 1; dialog test""#,
                r#"shot "out dir/shot 1.png""#,
                "look"
            ]
        );
    }
}
