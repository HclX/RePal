//! Multi-language support (`ZhHans` / `ZhHant` / `En`) and decoupled English translation overlay.

use serde_json::Value;
use std::collections::HashMap;

/// Active display language across `pal-core`, `pal-ui`, `pal-render`, `pal-wasm`, and `pal-desktop`.
/// Wire codes (`0 = ZhHans`, `1 = ZhHant`, `2 = En`) remain backwards-compatible with legacy `hant: 0|1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Language {
    ZhHans = 0,
    #[default]
    ZhHant = 1,
    En = 2,
}

impl Language {
    pub fn from_code(code: u32) -> Self {
        match code {
            0 => Self::ZhHans,
            2 => Self::En,
            _ => Self::ZhHant,
        }
    }

    pub fn as_code(self) -> u32 {
        self as u32
    }

    /// Cycle `ZhHant -> ZhHans -> En -> ZhHant`.
    pub fn next(self) -> Self {
        match self {
            Self::ZhHant => Self::ZhHans,
            Self::ZhHans => Self::En,
            Self::En => Self::ZhHant,
        }
    }

    pub fn is_hant(self) -> bool {
        self == Self::ZhHant
    }

    pub fn is_en(self) -> bool {
        self == Self::En
    }
}

/// Decoupled English translation overlay loaded from `site/assets/data/i18n/en/*.json`.
#[derive(Clone, Debug, Default)]
pub struct I18nOverlay {
    pub roles: HashMap<u16, String>,
    pub items: HashMap<u16, (String, String)>,
    pub magics: HashMap<u16, (String, String)>,
    pub enemies: HashMap<u16, String>,
    pub scenes: HashMap<u16, String>,
    pub speakers: HashMap<String, String>,
    pub dialogues_by_msg_id: HashMap<u32, String>,
    pub dialogues_by_text: HashMap<String, String>,
}

impl I18nOverlay {
    /// Merge one JSON overlay document (`tables.json` or `scripts_*.json`) into this overlay.
    pub fn merge_json(&mut self, doc: &Value) {
        if let Some(obj) = doc.get("roles").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let (Ok(id), Some(s)) = (k.parse::<u16>(), v.as_str()) {
                    self.roles.insert(id, s.to_string());
                }
            }
        }
        if let Some(obj) = doc.get("items").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Ok(id) = k.parse::<u16>() {
                    if let Some(arr) = v.as_array() {
                        let name = arr.first().and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let desc = arr.get(1).and_then(|x| x.as_str()).unwrap_or("").to_string();
                        self.items.insert(id, (name, desc));
                    } else if let Some(s) = v.as_str() {
                        self.items.insert(id, (s.to_string(), String::new()));
                    }
                }
            }
        }
        if let Some(obj) = doc.get("magics").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Ok(id) = k.parse::<u16>() {
                    if let Some(arr) = v.as_array() {
                        let name = arr.first().and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let desc = arr.get(1).and_then(|x| x.as_str()).unwrap_or("").to_string();
                        self.magics.insert(id, (name, desc));
                    } else if let Some(s) = v.as_str() {
                        self.magics.insert(id, (s.to_string(), String::new()));
                    }
                }
            }
        }
        if let Some(obj) = doc.get("enemies").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let (Ok(id), Some(s)) = (k.parse::<u16>(), v.as_str()) {
                    self.enemies.insert(id, s.to_string());
                }
            }
        }
        if let Some(obj) = doc.get("scenes").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let (Ok(id), Some(s)) = (k.parse::<u16>(), v.as_str()) {
                    self.scenes.insert(id, s.to_string());
                }
            }
        }
        if let Some(obj) = doc.get("speakers").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    self.speakers.insert(k.clone(), s.to_string());
                }
            }
        }
        if let Some(obj) = doc.get("dialogues").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let (Ok(id), Some(s)) = (k.parse::<u32>(), v.as_str()) {
                    self.dialogues_by_msg_id.insert(id, s.to_string());
                }
            }
        }
        if let Some(obj) = doc.get("dialogues_by_text").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    self.dialogues_by_text.insert(k.clone(), s.to_string());
                }
            }
        }
    }

    pub fn role_name(&self, role_id: u16) -> Option<&str> {
        self.roles.get(&role_id).map(|s| s.as_str()).filter(|s| !s.is_empty())
    }

    pub fn item_name(&self, item_id: u16) -> Option<&str> {
        self.items.get(&item_id).map(|(n, _)| n.as_str()).filter(|s| !s.is_empty())
    }

    pub fn item_desc(&self, item_id: u16) -> Option<&str> {
        self.items.get(&item_id).map(|(_, d)| d.as_str()).filter(|s| !s.is_empty())
    }

    pub fn magic_name(&self, magic_id: u16) -> Option<&str> {
        self.magics.get(&magic_id).map(|(n, _)| n.as_str()).filter(|s| !s.is_empty())
    }

    pub fn magic_desc(&self, magic_id: u16) -> Option<&str> {
        self.magics.get(&magic_id).map(|(_, d)| d.as_str()).filter(|s| !s.is_empty())
    }

    pub fn enemy_name(&self, enemy_id: u16) -> Option<&str> {
        self.enemies.get(&enemy_id).map(|s| s.as_str()).filter(|s| !s.is_empty())
    }

    pub fn scene_name(&self, scene_id: u16) -> Option<&str> {
        self.scenes.get(&scene_id).map(|s| s.as_str()).filter(|s| !s.is_empty())
    }

    pub fn speaker(&self, hans: &str, hant: &str) -> Option<&str> {
        let trim_hans = hans.trim();
        if !trim_hans.is_empty() {
            if let Some(s) = self.speakers.get(trim_hans) {
                return Some(s.as_str());
            }
            // Also try stripping full-width spaces (e.g. "彩　依" -> "彩依")
            let compact: String = trim_hans.chars().filter(|c| !c.is_whitespace() && *c != '\u{3000}').collect();
            if let Some(s) = self.speakers.get(&compact) {
                return Some(s.as_str());
            }
        }
        let trim_hant = hant.trim();
        if !trim_hant.is_empty() {
            if let Some(s) = self.speakers.get(trim_hant) {
                return Some(s.as_str());
            }
        }
        None
    }

    pub fn dialogue(&self, msg_id: u32, raw_hans: &str) -> Option<&str> {
        if msg_id > 0 {
            if let Some(s) = self.dialogues_by_msg_id.get(&msg_id) {
                return Some(s.as_str());
            }
        }
        self.dialogues_by_text.get(raw_hans).map(|s| s.as_str())
    }
}
