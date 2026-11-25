//! Local filesystem save-slot manager (`slot_0.json`..`slot_99.json`) and user `config.json`.

use pal_runtime::PalRuntime;
use std::fs;
use std::path::{Path, PathBuf};

/// Persistent desktop user preferences stored in `<user_profile>/RePal/config.json`.
#[derive(Debug, Clone)]
pub struct UserConfig {
    pub hd_mode: bool,
    pub lang: pal_core::Language,
    pub music_on: bool,
    pub sfx_on: bool,
    pub fullscreen: bool,
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            hd_mode: true,
            lang: pal_core::Language::ZhHant,
            music_on: true,
            sfx_on: true,
            fullscreen: false,
        }
    }
}

impl UserConfig {
    pub fn load(path: &Path) -> Self {
        let mut cfg = Self::default();
        let Ok(raw) = fs::read_to_string(path) else {
            return cfg;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return cfg;
        };
        if let Some(b) = v["hd_mode"].as_bool() {
            cfg.hd_mode = b;
        }
        if let Some(b) = v["music_on"].as_bool() {
            cfg.music_on = b;
        }
        if let Some(b) = v["sfx_on"].as_bool() {
            cfg.sfx_on = b;
        }
        if let Some(b) = v["fullscreen"].as_bool() {
            cfg.fullscreen = b;
        }
        if let Some(s) = v["lang"].as_str() {
            cfg.lang = match s {
                "en" => pal_core::Language::En,
                "hans" => pal_core::Language::ZhHans,
                _ => pal_core::Language::ZhHant,
            };
        }
        cfg
    }

    pub fn save(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let lang_str = match self.lang {
            pal_core::Language::En => "en",
            pal_core::Language::ZhHans => "hans",
            pal_core::Language::ZhHant => "hant",
        };
        let json = serde_json::json!({
            "hd_mode": self.hd_mode,
            "lang": lang_str,
            "music_on": self.music_on,
            "sfx_on": self.sfx_on,
            "fullscreen": self.fullscreen,
        });
        if let Ok(text) = serde_json::to_string_pretty(&json) {
            let _ = fs::write(path, text);
        }
    }
}

pub struct SaveStore {
    pub dir: PathBuf,
}

impl SaveStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn slot_path(&self, slot: usize) -> PathBuf {
        self.dir.join(format!("slot_{slot}.json"))
    }

    /// Populate all `rt.ui.save_slots` (100 slots) from local JSON files (with slot 0 falling
    /// back to `orig_saves/10.rpg` in `vfs` or `assets_root`, matching the web build).
    #[allow(dead_code)]
    pub fn refresh_slots(&self, assets_root: &Path, rt: &mut PalRuntime) {
        pal_runtime::disk::refresh_save_slots_from_dir(&self.dir, assets_root, rt);
    }

    pub fn refresh_slots_vfs(
        &self,
        vfs: &pal_runtime::pack_vfs::OverlayVfs,
        rt: &mut PalRuntime,
    ) {
        pal_runtime::disk::refresh_save_slots_from_vfs(&self.dir, vfs, rt);
    }

    pub fn save_slot(&self, slot: usize, rt: &PalRuntime) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("create save dir: {e}"))?;
        let note = format!("slot {}", slot + 1);
        let text = rt
            .save_json_str(&note, "local")
            .ok_or_else(|| "game not initialized".to_string())?;
        fs::write(self.slot_path(slot), text).map_err(|e| format!("write save: {e}"))
    }

    #[allow(dead_code)]
    pub fn load_slot(&self, slot: usize, assets_root: &Path, rt: &mut PalRuntime) -> bool {
        if let Ok(raw) = fs::read_to_string(self.slot_path(slot)) {
            if rt.load_save_json_str(&raw) {
                return true;
            }
        }
        if slot == 0 {
            if let Ok(bytes) = fs::read(assets_root.join("orig_saves/10.rpg")) {
                return rt.load_dos_rpg(&bytes);
            }
        }
        false
    }

    pub fn load_slot_vfs(
        &self,
        slot: usize,
        vfs: &mut pal_runtime::pack_vfs::OverlayVfs,
        rt: &mut PalRuntime,
    ) -> bool {
        if let Ok(raw) = fs::read_to_string(self.slot_path(slot)) {
            if rt.load_save_json_str(&raw) {
                return true;
            }
        }
        if slot == 0 {
            if let Ok(bytes) = vfs.read_bytes("orig_saves/10.rpg") {
                return rt.load_dos_rpg(&bytes);
            }
        }
        false
    }
}
