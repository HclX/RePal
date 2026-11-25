//! Shared helpers for integration tests that read the real decompiled game data in `site/assets`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub fn assets_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../site/assets")
}

/// Parse a YAML asset (relative to `site/assets/`) into a JSON value.
pub fn load_yaml(rel: &str) -> serde_json::Value {
    let path = assets_dir().join(rel);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_yaml::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

pub fn chapter_file(ch: u32) -> String {
    format!("data/scripts/scripts_{ch:02}.yaml")
}

use pal_core::game::data::GameData;
use pal_core::game::{BattleCmd, Game, HostCmd, HostReply, TickInput};

/// Load every static table the core needs.
pub fn load_game_data() -> GameData {
    let mut d = GameData::default();
    for t in ["scenes", "events", "roles", "items", "magic", "enemies", "shops"] {
        d.load_table(t, &load_yaml(&format!("data/{t}.yaml")));
    }
    d
}

/// A native stand-in for the browser host: answers every blocking command immediately.
pub struct NativeHost {
    pub game: Game,
    pub dialogues: Vec<String>,
    pub battles: Vec<i32>,
    pub toasts: Vec<String>,
    pub choice_answer: bool,
    /// Command given to every hero prompt (`Auto` = attack the first living enemy forever).
    pub battle_cmd: BattleCmd,
    /// Optional dynamic battle command selector invoked on `HostCmd::BattlePrompt { hero }`.
    pub battle_chooser: Option<Box<dyn FnMut(&Game, usize) -> BattleCmd>>,
    pub victories: Vec<(i32, i32, Vec<String>)>,
    pub returned_to_title: bool,
    pub game_over: bool,
}

impl NativeHost {
    pub fn new() -> Self {
        let mut game = Game::new(load_game_data());
        for ch in 0..=10 {
            game.load_chapter(ch, &load_yaml(&chapter_file(ch))).unwrap();
        }
        Self {
            game,
            dialogues: Vec::new(),
            battles: Vec::new(),
            toasts: Vec::new(),
            choice_answer: true,
            battle_cmd: BattleCmd::Auto,
            battle_chooser: None,
            victories: Vec::new(),
            returned_to_title: false,
            game_over: false,
        }
    }

    fn load_scene_assets(&mut self, map_id: u16) {
        let path = assets_dir().join(format!("maps/map_{map_id:03}.bin"));
        if let Ok(bytes) = std::fs::read(&path) {
            assert!(self.game.set_map_collision(&bytes), "bad map {}", path.display());
        }
        let ids: Vec<u16> = self
            .game
            .world
            .current_event_ids()
            .into_iter()
            .filter_map(|id| self.game.world.event(id).map(|e| e.sprite_id))
            .filter(|&s| s > 0)
            .collect();
        for sid in ids {
            if self.game.sprite_frame_counts.contains_key(&sid) {
                continue;
            }
            let p = assets_dir().join(format!("graphics/mgo/mgo_{sid:03}.json"));
            if let Ok(text) = std::fs::read_to_string(p) {
                let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                let n = v["frames"].as_array().map(|a| a.len()).unwrap_or(0) as i32;
                self.game.set_sprite_frame_count(sid, n);
            }
        }
    }

    /// Service all pending host commands. Returns `true` if anything was answered.
    pub fn pump(&mut self) -> bool {
        let mut any = false;
        loop {
            let cmds = self.game.drain_cmds();
            if cmds.is_empty() {
                return any;
            }
            for c in cmds {
                any = true;
                match c {
                    HostCmd::LoadScene { map_id, .. } => {
                        self.load_scene_assets(map_id);
                        self.game.reply(HostReply::SceneLoaded);
                    }
                    HostCmd::ShowDialogue { speaker, text, .. } => {
                        self.dialogues.push(if speaker.is_empty() { text } else { format!("{speaker}：{text}") });
                        self.game.reply(HostReply::DialogueDone);
                    }
                    HostCmd::AskYesNo => self.game.reply(HostReply::Choice(self.choice_answer)),
                    HostCmd::OpenShop(_) | HostCmd::OpenPawnshop => self.game.reply(HostReply::ModalDone),
                    HostCmd::BattleStarted => self.battles.push(self.game.world.battlefield_id),
                    HostCmd::BattlePrompt { hero } => {
                        let cmd = if let Some(mut chooser) = self.battle_chooser.take() {
                            let c = chooser(&self.game, hero);
                            self.battle_chooser = Some(chooser);
                            c
                        } else {
                            self.battle_cmd
                        };
                        assert!(self.game.battle_command(cmd));
                    }
                    HostCmd::BattleVictory { exp, cash, level_ups } => {
                        self.victories.push((exp, cash, level_ups));
                        self.game.reply(HostReply::ModalDone);
                    }
                    HostCmd::PlayRng { .. } | HostCmd::LoadChapter(_) => self.game.reply(HostReply::Done),
                    HostCmd::ReturnToTitle => {
                        self.returned_to_title = true;
                        self.game.abort_foreground();
                    }
                    HostCmd::Toast(t) => self.toasts.push(t),
                    HostCmd::GameOver => self.game_over = true,
                    _ => {}
                }
            }
        }
    }

    /// Switch scene (optionally running its enter script) and finish loading.
    pub fn enter_scene(&mut self, scene: u16, run_enter: bool) {
        self.game.in_title = false;
        self.game.enter_scene(scene, run_enter);
        assert!(self.settle(50_000), "scene {scene} never settled");
    }

    /// One background auto-script pass, finishing any script it triggers.
    pub fn auto_tick(&mut self) {
        self.game.run_auto_tick();
        self.pump();
        if self.game.is_busy() {
            assert!(self.settle(50_000));
        }
    }

    /// Use an item from the menu and return the legacy `useItem` result.
    pub fn use_item(&mut self, item: u16, role: usize) -> bool {
        if !self.game.use_item(item, Some(role)) {
            return false;
        }
        assert!(self.settle(50_000));
        self.game.last_item_result.unwrap_or(false)
    }

    /// Is the event standing on a blocked tile (map + other solid events)?
    pub fn event_on_blocked_tile(&mut self, id: u16) -> bool {
        self.game.sync_scene_obstacles_excluding(id);
        let e = self.game.world.event(id).unwrap().clone();
        pal_core::npc_ai::is_world_blocked(&self.game.grid, e.x, e.y)
    }

    /// Fresh new-game world without running the intro.
    pub fn reset_new_game(&mut self) {
        self.game.start_new_game(0x95_1995);
        self.game.abort_foreground();
        self.game.drain_cmds();
    }

    /// Tick with `input` once and service commands.
    pub fn tick(&mut self, input: TickInput) {
        self.pump();
        self.game.tick(&input);
        self.pump();
    }

    /// Run idle ticks until no foreground script is running (or `max` ticks elapse).
    pub fn settle(&mut self, max: u32) -> bool {
        for _ in 0..max {
            self.tick(TickInput::IDLE);
            if !self.game.is_busy() && !self.game.is_loading_scene() {
                return true;
            }
        }
        false
    }
}
