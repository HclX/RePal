//! CLI argument parsing, OS profile directory resolution, and Windows console attachment
//! for the unified `repal` native binary.

use crate::assets_bootstrap::try_resolve_local_assets;
use pal_headless::text_mode::{MinimapMode, ObsMode, TextModeConfig};
use std::env;
use std::path::PathBuf;

pub const DEFAULT_AGENT_PORT: u16 = 9999;
pub const DEFAULT_PACE_MS: u32 = 160;

pub(crate) struct CliConfig {
    pub(crate) user_dir: PathBuf,
    pub(crate) assets_arg: Option<String>,
    pub(crate) packs_dir_arg: Option<PathBuf>,
    pub(crate) assets_url_arg: Option<String>,
    pub(crate) assets_cache_dir: PathBuf,
    pub(crate) assets_root: PathBuf,
    pub(crate) saves_dir: PathBuf,
    pub(crate) config_path: PathBuf,
    pub(crate) hd_mode_override: Option<bool>,
    pub(crate) fullscreen_override: Option<bool>,
    pub(crate) lang_override: Option<pal_core::Language>,
    pub(crate) new_game: bool,
    pub(crate) debug_mode: bool,
    pub(crate) initial_save: Option<PathBuf>,
    pub(crate) initial_replay: Option<PathBuf>,
    pub(crate) initial_scene: Option<u16>,
    pub(crate) listen_addr: Option<String>,
    pub(crate) listen_port: u16,
    pub(crate) pace_ms: u32,
    pub(crate) text_stdio: bool,
    pub(crate) json_mode: bool,
    pub(crate) fog: bool,
    pub(crate) obs_mode: ObsMode,
    pub(crate) minimap_mode: MinimapMode,
    pub(crate) batch_cmds: Option<String>,
    pub(crate) script_file: Option<PathBuf>,
    pub(crate) record_file: Option<PathBuf>,
    pub(crate) keep_going: bool,
    pub(crate) out_dir: PathBuf,
}

pub(crate) enum LaunchMode {
    Gui(CliConfig),
    Text(TextModeConfig),
    ExportSrt { input: PathBuf, output: PathBuf },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CliExit {
    Help(String),
    Error(String),
}

#[cfg(target_os = "windows")]
fn attach_parent_console() {
    extern "system" {
        fn AttachConsole(dw_process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(target_os = "windows"))]
fn attach_parent_console() {}

/// Resolve the OS-standard per-user profile data directory for RePal:
/// - Windows: `%APPDATA%\RePal` (fallback `%USERPROFILE%\AppData\Roaming\RePal`)
/// - macOS:   `$HOME/Library/Application Support/RePal`
/// - Linux:   `$XDG_DATA_HOME/RePal` (fallback `$HOME/.local/share/RePal`)
pub(crate) fn default_user_data_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = env::var("APPDATA") {
            if !appdata.is_empty() {
                return PathBuf::from(appdata).join("RePal");
            }
        }
        if let Ok(profile) = env::var("USERPROFILE") {
            if !profile.is_empty() {
                return PathBuf::from(profile)
                    .join("AppData")
                    .join("Roaming")
                    .join("RePal");
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = env::var("HOME") {
            if !home.is_empty() {
                return PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
                    .join("RePal");
            }
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(xdg) = env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join("RePal");
            }
        }
        if let Ok(home) = env::var("HOME") {
            if !home.is_empty() {
                return PathBuf::from(home)
                    .join(".local")
                    .join("share")
                    .join("RePal");
            }
        }
    }
    PathBuf::from(".")
}

fn next_arg<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Result<&'a str, CliExit> {
    if *i + 1 >= args.len() {
        return Err(CliExit::Error(format!(
            "[repal] missing value for {flag} (run 'repal --help')"
        )));
    }
    *i += 2;
    Ok(&args[*i - 1])
}

fn parse_port_from_addr(addr: &str) -> Option<u16> {
    addr.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok())
}

fn help_text() -> String {
    format!(
        "repal — Unified Native Runtime (GUI | Text Mode | GUI + Text Mode)\n\n\
         MODES:\n\
           repal                           GUI desktop mode (default)\n\
           repal --text                    Headless Text Mode over stdio (no window/DISPLAY)\n\
           repal --text --listen IP:PORT   Headless Text Mode over TCP socket (telnet/nc)\n\
           repal --gui --text              GUI + Text Mode over stdio\n\
           repal --listen IP:PORT          GUI + Text Mode over TCP socket\n\n\
         OPTIONS:\n\
           --json                 Emit strict JSON-Lines responses (default: plain text)\n\
           -c <CMDS>              Run semicolon-separated Text Mode commands and exit\n\
           --script <FILE>        Run commands from a .cmd/.rec file and exit\n\
           --out-dir <DIR>        Output directory for relative `shot` paths\n\
           --keep-going           Continue batch execution after command errors\n\
           --obs <fair|debug>     Observation profile: fair (player-equivalent) or debug (default)\n\
           --fog                  Alias for --obs fair (camera viewport, hidden enemy HP, no /debug)\n\
           --minimap <auto|always|off>  Viewport ASCII minimap cadence (default: auto)\n\
           --record <FILE>        Record executed commands to a .cmd replay file\n\
           --assets <DIR>         Path to local assets directory\n\
           --packs-dir <DIR>      Mount local pack-manifest.json + .rpk asset packs (zero network)\n\
           --assets-cache <DIR>   Hash-keyed asset cache dir\n\
           --assets-url <URL>     Override download URL for the pinned asset archive\n\
           --saves <DIR>          Directory for local JSON saves\n\
           --config <FILE>        Path to user config JSON\n\
           --new                  Start a new game from Scene 1\n\
           --debug                Enable debug_mode (renders DEBUG ENABLED watermark)\n\
           --save <FILE>          Boot directly into a .json or DOS .rpg save file\n\
           --replay <FILE>        Auto-play a recorded .cmd/.repal replay file\n\
           --replay-export-srt <IN.repal> <OUT.srt>  Export AI thought subtitles to SubRip (.srt)\n\
           --scene <ID>           Jump to a scene ID after loading\n\
           --hd / --retro         Start in 4x HD (1280x800) or 1x Retro (320x200)\n\
           --lang <hans|hant|en>  Initial UI & script language\n\
           --fullscreen           Start in borderless fullscreen mode\n\
           --pace-ms <MS>         Spectator pacing delay in GUI+Text mode (default: {DEFAULT_PACE_MS})"
    )
}

/// Pure, unit-testable CLI parser.
pub(crate) fn parse_cli_from(args: &[String]) -> Result<LaunchMode, CliExit> {
    let mut assets_arg: Option<String> = None;
    let mut packs_dir_arg: Option<PathBuf> = env::var("REPAL_PACKS_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let mut assets_url_arg: Option<String> = None;
    let user_dir = default_user_data_dir();
    let mut assets_cache_dir = env::var("REPAL_ASSETS_CACHE_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| env::var("PAL_ASSETS_CACHE_DIR").ok().filter(|s| !s.is_empty()))
        .map(PathBuf::from)
        .unwrap_or_else(|| user_dir.join("assets_cache"));
    let mut saves_dir = user_dir.join("saves");
    let mut config_path = user_dir.join("config.json");
    let mut hd_mode_override: Option<bool> = None;
    let mut fullscreen_override: Option<bool> = None;
    let mut lang_override: Option<pal_core::Language> = None;
    let mut initial_save = None;
    let mut initial_replay = None;
    let mut initial_scene = None;
    let mut agent_port: Option<u16> = None;
    let mut agent_bind = "127.0.0.1".to_string();
    let mut pace_ms: u32 = DEFAULT_PACE_MS;
    let mut want_text = false;
    let mut want_gui = false;
    let mut json_mode = false;
    let mut fog = false;
    let mut obs_mode = ObsMode::Debug;
    let mut minimap_mode = MinimapMode::Auto;
    let mut new_game = false;
    let mut debug_mode = false;
    let mut keep_going = false;
    let mut listen_addr: Option<String> = None;
    let mut batch_cmds: Option<String> = None;
    let mut script_file: Option<PathBuf> = None;
    let mut record_file: Option<PathBuf> = None;
    let mut out_dir_opt: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        let flag = args[i].clone();
        match flag.as_str() {
            "--text" => {
                want_text = true;
                i += 1;
            }
            "--gui" => {
                want_gui = true;
                i += 1;
            }
            "--json" => {
                json_mode = true;
                i += 1;
            }
            "--fog" => {
                fog = true;
                obs_mode = ObsMode::Fair;
                i += 1;
            }
            "--obs" => {
                let raw = next_arg(args, &mut i, "--obs")?;
                match raw.to_ascii_lowercase().as_str() {
                    "fair" => {
                        obs_mode = ObsMode::Fair;
                        fog = true;
                    }
                    "debug" => {
                        obs_mode = ObsMode::Debug;
                        fog = false;
                    }
                    _ => {
                        return Err(CliExit::Error(format!(
                            "[repal] invalid --obs '{raw}' (expected fair|debug)"
                        )));
                    }
                }
            }
            "--minimap" => {
                let raw = next_arg(args, &mut i, "--minimap")?;
                minimap_mode = match raw.to_ascii_lowercase().as_str() {
                    "auto" => MinimapMode::Auto,
                    "always" => MinimapMode::Always,
                    "off" | "none" => MinimapMode::Off,
                    _ => {
                        return Err(CliExit::Error(format!(
                            "[repal] invalid --minimap '{raw}' (expected auto|always|off)"
                        )));
                    }
                };
            }
            "--new" => {
                new_game = true;
                i += 1;
            }
            "--debug" => {
                debug_mode = true;
                i += 1;
            }
            "--keep-going" => {
                keep_going = true;
                i += 1;
            }
            "--listen" => {
                listen_addr = Some(next_arg(args, &mut i, "--listen")?.to_string());
            }
            "-c" => {
                want_text = true;
                batch_cmds = Some(next_arg(args, &mut i, "-c")?.to_string());
            }
            "--script" => {
                want_text = true;
                script_file = Some(PathBuf::from(next_arg(args, &mut i, "--script")?));
            }
            "--record" => {
                record_file = Some(PathBuf::from(next_arg(args, &mut i, "--record")?));
            }
            "--out-dir" => {
                out_dir_opt = Some(PathBuf::from(next_arg(args, &mut i, "--out-dir")?));
            }
            "--lang" => {
                let raw = next_arg(args, &mut i, "--lang")?;
                lang_override = Some(match raw.to_ascii_lowercase().as_str() {
                    "en" | "english" => pal_core::Language::En,
                    "hant" | "zh-tw" | "tw" => pal_core::Language::ZhHant,
                    "hans" | "zh-cn" | "cn" => pal_core::Language::ZhHans,
                    _ => {
                        return Err(CliExit::Error(format!(
                            "[repal] invalid --lang '{raw}' (expected hans|hant|en)"
                        )));
                    }
                });
            }
            "--assets" => {
                assets_arg = Some(next_arg(args, &mut i, "--assets")?.to_string());
            }
            "--packs-dir" => {
                packs_dir_arg = Some(PathBuf::from(next_arg(args, &mut i, "--packs-dir")?));
            }
            "--assets-url" => {
                assets_url_arg = Some(next_arg(args, &mut i, "--assets-url")?.to_string());
            }
            "--assets-cache" => {
                assets_cache_dir = PathBuf::from(next_arg(args, &mut i, "--assets-cache")?);
            }
            "--saves" => {
                saves_dir = PathBuf::from(next_arg(args, &mut i, "--saves")?);
            }
            "--config" => {
                config_path = PathBuf::from(next_arg(args, &mut i, "--config")?);
            }
            "--save" => {
                initial_save = Some(PathBuf::from(next_arg(args, &mut i, "--save")?));
            }
            "--replay" => {
                initial_replay = Some(PathBuf::from(next_arg(args, &mut i, "--replay")?));
            }
            "--replay-export-srt" => {
                if i + 2 >= args.len() {
                    return Err(CliExit::Error(
                        "[repal] usage: --replay-export-srt <in.repal> <out.srt>".to_string(),
                    ));
                }
                let input = PathBuf::from(&args[i + 1]);
                let output = PathBuf::from(&args[i + 2]);
                return Ok(LaunchMode::ExportSrt { input, output });
            }
            "--scene" => {
                let raw = next_arg(args, &mut i, "--scene")?;
                let Ok(sc) = raw.parse::<u16>() else {
                    return Err(CliExit::Error(format!(
                        "[repal] invalid --scene '{raw}' (expected integer)"
                    )));
                };
                initial_scene = Some(sc);
            }
            "--agent-port" => {
                let raw = next_arg(args, &mut i, "--agent-port")?;
                let Ok(port) = raw.parse::<u16>() else {
                    return Err(CliExit::Error(format!(
                        "[repal] invalid --agent-port '{raw}' (expected u16)"
                    )));
                };
                agent_port = Some(port);
            }
            "--agent-lan" => {
                agent_bind = "0.0.0.0".to_string();
                if agent_port.is_none() {
                    agent_port = Some(DEFAULT_AGENT_PORT);
                }
                i += 1;
            }
            "--agent-bind" => {
                agent_bind = next_arg(args, &mut i, "--agent-bind")?.to_string();
                if agent_port.is_none() {
                    agent_port = Some(DEFAULT_AGENT_PORT);
                }
            }
            "--agent-pace-ms" | "--pace-ms" => {
                let raw = next_arg(args, &mut i, &flag)?;
                let Ok(ms) = raw.parse::<u32>() else {
                    return Err(CliExit::Error(format!(
                        "[repal] invalid {flag} '{raw}' (expected integer ms)"
                    )));
                };
                pace_ms = ms;
            }
            "--retro" => {
                hd_mode_override = Some(false);
                i += 1;
            }
            "--hd" => {
                hd_mode_override = Some(true);
                i += 1;
            }
            "--fullscreen" => {
                fullscreen_override = Some(true);
                i += 1;
            }
            "--windowed" => {
                fullscreen_override = Some(false);
                i += 1;
            }
            "-h" | "--help" => {
                return Err(CliExit::Help(help_text()));
            }
            other => {
                return Err(CliExit::Error(format!(
                    "[repal] unknown argument: {other} (run 'repal --help')"
                )));
            }
        }
    }

    // Normalize legacy `--agent-port` / `--agent-bind` / `--agent-lan` into `--listen`.
    if listen_addr.is_none() {
        if let Some(port) = agent_port {
            listen_addr = Some(format!("{agent_bind}:{port}"));
        }
    }
    let listen_port = listen_addr
        .as_deref()
        .and_then(parse_port_from_addr)
        .or(agent_port)
        .unwrap_or(DEFAULT_AGENT_PORT);

    // Reject contradictory `--debug` + `--obs fair` / `--fog` launch combinations (#252).
    if debug_mode && (fog || obs_mode == ObsMode::Fair) {
        return Err(CliExit::Error(
            "[repal] --debug cannot be combined with --obs fair / --fog".to_string(),
        ));
    }

    // Pure headless Text Mode (`--text` without `--gui`)
    if want_text && !want_gui {
        let resolved_assets = try_resolve_local_assets(assets_arg.as_deref(), &assets_cache_dir)
            .unwrap_or_else(|| {
                assets_arg
                    .as_deref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("site/assets"))
            });
        // When serving sockets (`--listen`), default out_dir to a dedicated `<user_dir>/agent_out`
        // directory rather than CWD `.` so desktop/daemon launches never expose `$HOME` (#236).
        let out_dir = out_dir_opt.unwrap_or_else(|| {
            if listen_addr.is_some() {
                user_dir.join("agent_out")
            } else {
                PathBuf::from(".")
            }
        });
        let loose_overlay = assets_arg.as_deref().map(PathBuf::from);
        return Ok(LaunchMode::Text(TextModeConfig {
            assets_root: resolved_assets,
            packs_dir: packs_dir_arg,
            loose_overlay,
            user_dir: Some(user_dir),
            json_mode,
            hd_mode: hd_mode_override.unwrap_or(true),
            lang: lang_override.unwrap_or(pal_core::Language::ZhHans),
            fog,
            obs_mode,
            minimap_mode,
            new_game,
            debug_mode,
            save_path: initial_save,
            scene_id: initial_scene,
            listen_addr,
            batch_cmds,
            script_file,
            record_file,
            keep_going,
            out_dir,
        }));
    }

    let text_stdio = want_gui
        && want_text
        && listen_addr.is_none()
        && batch_cmds.is_none()
        && script_file.is_none();
    // GUI mode can toggle Agent Mode on at runtime, so default out_dir to `<user_dir>/agent_out`
    // rather than CWD `.` unless `--out-dir` was explicitly provided (#236).
    let out_dir = out_dir_opt.unwrap_or_else(|| user_dir.join("agent_out"));
    Ok(LaunchMode::Gui(CliConfig {
        user_dir,
        assets_arg,
        packs_dir_arg,
        assets_url_arg,
        assets_cache_dir,
        assets_root: PathBuf::new(),
        saves_dir,
        config_path,
        hd_mode_override,
        fullscreen_override,
        lang_override,
        new_game,
        debug_mode,
        initial_save,
        initial_replay,
        initial_scene,
        listen_addr,
        listen_port,
        pace_ms,
        text_stdio,
        json_mode,
        fog,
        obs_mode,
        minimap_mode,
        batch_cmds,
        script_file,
        record_file,
        keep_going,
        out_dir,
    }))
}

pub(crate) fn parse_cli() -> LaunchMode {
    let args: Vec<String> = env::args().collect();
    if args.iter().skip(1).any(|a| {
        matches!(
            a.as_str(),
            "--text" | "-c" | "--script" | "--listen" | "--replay-export-srt" | "-h" | "--help"
        )
    }) {
        attach_parent_console();
    }
    match parse_cli_from(&args) {
        Ok(mode) => mode,
        Err(CliExit::Help(msg)) => {
            println!("{msg}");
            std::process::exit(0);
        }
        Err(CliExit::Error(msg)) => {
            eprintln!("{msg}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn cli_rejects_unknown_flags_and_missing_values() {
        assert!(matches!(
            parse_cli_from(&argv(&["repal", "--bogus"])),
            Err(CliExit::Error(_))
        ));
        assert!(matches!(
            parse_cli_from(&argv(&["repal", "--text", "--save"])),
            Err(CliExit::Error(_))
        ));
        assert!(matches!(
            parse_cli_from(&argv(&["repal", "--scene", "not_a_number"])),
            Err(CliExit::Error(_))
        ));
    }

    #[test]
    fn cli_three_orthogonal_modes_and_listen_port_sync() {
        // Pure headless text
        match parse_cli_from(&argv(&["repal", "--text", "--json", "--new"])) {
            Ok(LaunchMode::Text(cfg)) => {
                assert!(cfg.json_mode);
                assert!(cfg.new_game);
            }
            _ => panic!("expected headless Text mode"),
        }

        // GUI + Text socket with custom port & --new preserved
        match parse_cli_from(&argv(&[
            "repal",
            "--listen",
            "127.0.0.1:8888",
            "--new",
            "--pace-ms",
            "250",
        ])) {
            Ok(LaunchMode::Gui(cfg)) => {
                assert_eq!(cfg.listen_addr.as_deref(), Some("127.0.0.1:8888"));
                assert_eq!(cfg.listen_port, 8888);
                assert!(cfg.new_game);
                assert_eq!(cfg.pace_ms, 250);
                assert!(!cfg.text_stdio);
            }
            _ => panic!("expected GUI+Text socket mode"),
        }

        // GUI + Text stdio
        match parse_cli_from(&argv(&["repal", "--gui", "--text"])) {
            Ok(LaunchMode::Gui(cfg)) => {
                assert!(cfg.text_stdio);
                assert_eq!(cfg.pace_ms, DEFAULT_PACE_MS);
            }
            _ => panic!("expected GUI+Text stdio mode"),
        }

        // --obs fair / --fog / --obs debug (#224)
        match parse_cli_from(&argv(&["repal", "--text", "--obs", "fair"])) {
            Ok(LaunchMode::Text(cfg)) => {
                assert_eq!(cfg.obs_mode, ObsMode::Fair);
                assert!(cfg.fog);
            }
            _ => panic!("expected fair text mode"),
        }
        match parse_cli_from(&argv(&["repal", "--text", "--fog"])) {
            Ok(LaunchMode::Text(cfg)) => {
                assert_eq!(cfg.obs_mode, ObsMode::Fair);
            }
            _ => panic!("expected --fog alias for fair mode"),
        }
        assert!(matches!(
            parse_cli_from(&argv(&["repal", "--text", "--obs", "invalid"])),
            Err(CliExit::Error(_))
        ));

        // #236: GUI and headless `--listen` default out_dir to `<user_dir>/agent_out` (not CWD `.`).
        match parse_cli_from(&argv(&["repal", "--listen", "127.0.0.1:9999"])) {
            Ok(LaunchMode::Gui(cfg)) => {
                assert_eq!(cfg.out_dir, cfg.user_dir.join("agent_out"));
            }
            _ => panic!("expected GUI mode"),
        }
        match parse_cli_from(&argv(&["repal", "--text", "--listen", "127.0.0.1:9999"])) {
            Ok(LaunchMode::Text(cfg)) => {
                assert_ne!(cfg.out_dir, PathBuf::from("."));
                assert!(cfg.out_dir.ends_with("agent_out"));
            }
            _ => panic!("expected headless listen mode"),
        }
        match parse_cli_from(&argv(&["repal", "--text", "--listen", "127.0.0.1:9999", "--out-dir", "/tmp/custom_out"])) {
            Ok(LaunchMode::Text(cfg)) => {
                assert_eq!(cfg.out_dir, PathBuf::from("/tmp/custom_out"));
            }
            _ => panic!("expected explicit --out-dir override"),
        }
        match parse_cli_from(&argv(&["repal", "--replay-export-srt", "in.repal", "out.srt"])) {
            Ok(LaunchMode::ExportSrt { input, output }) => {
                assert_eq!(input, PathBuf::from("in.repal"));
                assert_eq!(output, PathBuf::from("out.srt"));
            }
            _ => panic!("expected ExportSrt mode"),
        }
        assert!(matches!(
            parse_cli_from(&argv(&["repal", "--replay-export-srt", "only_one.repal"])),
            Err(CliExit::Error(_))
        ));
    }
}
