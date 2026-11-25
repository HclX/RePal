//! Bounded LRU Texture & Asset Cache with Pull-Based Missing Asset Requests.
//!
//! Solves #22 (unbounded 294-scene preload memory leak) and #34 (permanent failed-promise cache poisoning).

use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum AssetKind {
    Texture = 1,
    AtlasMeta = 2,
    MapBinary = 3,
    HdMapMeta = 4,
    FontTtf = 5,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetRequest {
    pub kind: AssetKind,
    pub key: String,
}

#[derive(Clone, Debug)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub last_used_tick: u64,
    pub pinned: bool,
}

#[derive(Clone, Debug, Default)]
pub struct AtlasMeta {
    pub w: u32,
    pub h: u32,
    /// `[x, y, w, h]` in 1x logical atlas coordinates
    pub frames: Vec<[i32; 4]>,
    /// True when baked ground shadows were stripped from HD character frames (renderer draws soft ellipse shadow).
    pub shadow_stripped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HdRoomChunk {
    pub file: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HdOccluderStrip {
    pub page: u16,
    pub sort_y: i32,
    pub sx: i32,
    pub sy: i32,
    pub sw: i32,
    pub sh: i32,
    pub dx: i32,
    pub dy: i32,
    pub dw: i32,
    pub dh: i32,
}

#[derive(Clone, Debug, Default)]
pub struct HdMapMeta {
    pub map_id: u16,
    pub scale: u8,
    pub rooms: Vec<HdRoomChunk>,
    pub occluder_pages: Vec<String>,
    pub strips: Vec<HdOccluderStrip>,
}

impl AtlasMeta {
    /// Zero-dependency parser for PalRemake atlas JSON (`{"w":..,"h":..,"frames":[[x,y,w,h],...]}`).
    pub fn parse_json(text: &str) -> Option<Self> {
        let w = extract_json_u32(text, "\"w\"")?;
        let h = extract_json_u32(text, "\"h\"")?;
        let shadow_stripped = extract_json_bool(text, "\"shadowStripped\"").unwrap_or(false);
        let frames_pos = text.find("\"frames\"")?;
        let after = &text[frames_pos..];
        let arr_start = after.find('[')?;
        let bytes = after[arr_start..].as_bytes();
        let mut frames = Vec::new();
        let mut i = 1usize;
        while i < bytes.len() {
            match bytes[i] {
                b'[' => {
                    i += 1;
                    let mut nums = [0i32; 4];
                    let mut n_idx = 0usize;
                    while i < bytes.len() && bytes[i] != b']' {
                        if bytes[i] == b'-' || bytes[i].is_ascii_digit() {
                            let start = i;
                            if bytes[i] == b'-' {
                                i += 1;
                            }
                            while i < bytes.len() && bytes[i].is_ascii_digit() {
                                i += 1;
                            }
                            if n_idx < 4 {
                                if let Ok(s) = std::str::from_utf8(&bytes[start..i]) {
                                    nums[n_idx] = s.parse().unwrap_or(0);
                                }
                                n_idx += 1;
                            }
                        } else {
                            i += 1;
                        }
                    }
                    if n_idx == 4 {
                        frames.push(nums);
                    }
                    i += 1;
                }
                b']' => break,
                _ => i += 1,
            }
        }
        Some(Self {
            w,
            h,
            frames,
            shadow_stripped,
        })
    }
}

impl HdMapMeta {
    /// Zero-dependency parser for Track-B `maps_hd/map_XXX/meta.json`.
    /// Supports both spatially paginated `"occluders":{"pages":[...],"strips":[{"page":0,...}]}`
    /// and legacy single-atlas `"occluders":{"file":"occluders_4x.webp","strips":[...]}`.
    pub fn parse_json(text: &str) -> Option<Self> {
        let map_id = extract_json_u32(text, "\"mapId\"").unwrap_or(0) as u16;
        let scale = extract_json_u32(text, "\"scale\"").unwrap_or(4) as u8;

        let mut rooms = Vec::new();
        if let Some(rooms_slice) = extract_json_array_slice(text, "\"rooms\"") {
            for obj in iter_json_objects(rooms_slice) {
                let Some(file) = extract_json_str(obj, "\"file\"") else {
                    continue;
                };
                let x = extract_json_i32(obj, "\"x\"").unwrap_or(0);
                let y = extract_json_i32(obj, "\"y\"").unwrap_or(0);
                let w = extract_json_i32(obj, "\"w\"").unwrap_or(0);
                let h = extract_json_i32(obj, "\"h\"").unwrap_or(0);
                if w > 0 && h > 0 {
                    rooms.push(HdRoomChunk { file, x, y, w, h });
                }
            }
        }

        if rooms.is_empty() {
            return None;
        }

        let mut occluder_pages = Vec::new();
        let mut strips = Vec::new();
        if let Some(occ_pos) = text.find("\"occluders\"") {
            let occ_slice = &text[occ_pos..];
            if let Some(pages_slice) = extract_json_array_slice(occ_slice, "\"pages\"") {
                occluder_pages = parse_json_string_array(pages_slice);
            } else if let Some(single_file) = extract_json_str(occ_slice, "\"file\"") {
                occluder_pages.push(single_file);
            }
            if let Some(strips_slice) = extract_json_array_slice(occ_slice, "\"strips\"") {
                for obj in iter_json_objects(strips_slice) {
                    let page = extract_json_u32(obj, "\"page\"").unwrap_or(0) as u16;
                    let sort_y = extract_json_i32(obj, "\"sortY\"").unwrap_or(0);
                    let sx = extract_json_i32(obj, "\"sx\"").unwrap_or(0);
                    let sy = extract_json_i32(obj, "\"sy\"").unwrap_or(0);
                    let sw = extract_json_i32(obj, "\"sw\"").unwrap_or(0);
                    let sh = extract_json_i32(obj, "\"sh\"").unwrap_or(0);
                    let dx = extract_json_i32(obj, "\"dx\"").unwrap_or(0);
                    let dy = extract_json_i32(obj, "\"dy\"").unwrap_or(0);
                    let dw = extract_json_i32(obj, "\"dw\"").unwrap_or(0);
                    let dh = extract_json_i32(obj, "\"dh\"").unwrap_or(0);
                    if sw > 0 && sh > 0 && dw > 0 && dh > 0 {
                        strips.push(HdOccluderStrip {
                            page,
                            sort_y,
                            sx,
                            sy,
                            sw,
                            sh,
                            dx,
                            dy,
                            dw,
                            dh,
                        });
                    }
                }
            }
        }

        Some(Self {
            map_id,
            scale,
            rooms,
            occluder_pages,
            strips,
        })
    }
}

fn extract_json_u32(text: &str, key: &str) -> Option<u32> {
    let pos = text.find(key)?;
    let rest = &text[pos + key.len()..];
    let colon = rest.find(':')?;
    let digits = rest[colon + 1..].trim_start();
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end].parse().ok()
}

fn extract_json_i32(text: &str, key: &str) -> Option<i32> {
    let pos = text.find(key)?;
    let rest = &text[pos + key.len()..];
    let colon = rest.find(':')?;
    let val = rest[colon + 1..].trim_start();
    let bytes = val.as_bytes();
    let mut end = 0usize;
    if end < bytes.len() && bytes[end] == b'-' {
        end += 1;
    }
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == 0 || (end == 1 && bytes[0] == b'-') {
        return None;
    }
    val[..end].parse().ok()
}

fn extract_json_bool(text: &str, key: &str) -> Option<bool> {
    let pos = text.find(key)?;
    let rest = &text[pos + key.len()..];
    let colon = rest.find(':')?;
    let val = rest[colon + 1..].trim_start();
    if val.starts_with("true") {
        Some(true)
    } else if val.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn decode_json_string_at(text: &str) -> Option<(String, usize)> {
    let mut chars = text.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    let mut out = String::new();
    while let Some((idx, ch)) = chars.next() {
        match ch {
            '\\' => {
                let (_, esc) = chars.next()?;
                match esc {
                    '"' | '\\' | '/' => out.push(esc),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000C}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    other => out.push(other),
                }
            }
            '"' => return Some((out, idx + 1)),
            c => out.push(c),
        }
    }
    None
}

fn extract_json_str(text: &str, key: &str) -> Option<String> {
    let pos = text.find(key)?;
    let rest = &text[pos + key.len()..];
    let colon = rest.find(':')?;
    let val = rest[colon + 1..].trim_start();
    decode_json_string_at(val).map(|(s, _)| s)
}

fn extract_json_array_slice<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let pos = text.find(key)?;
    let rest = &text[pos + key.len()..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    if !after.starts_with('[') {
        return None;
    }
    let bytes = after.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_str = false;
            }
        } else {
            match b {
                b'"' => in_str = true,
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&after[1..i]);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn iter_json_objects(array_inner: &str) -> Vec<&str> {
    let bytes = array_inner.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut in_str = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_str = false;
            }
        } else {
            match b {
                b'"' => in_str = true,
                b'{' => {
                    if depth == 0 {
                        start = i;
                    }
                    depth += 1;
                }
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        out.push(&array_inner[start..=i]);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    out
}

fn parse_json_string_array(array_inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = array_inner;
    while let Some(q) = rest.find('"') {
        if let Some((s, consumed)) = decode_json_string_at(&rest[q..]) {
            out.push(s);
            rest = &rest[q + consumed..];
        } else {
            break;
        }
    }
    out
}

pub struct AssetCache {
    textures: HashMap<String, Texture>,
    atlases: HashMap<String, AtlasMeta>,
    maps: HashMap<String, Vec<u8>>,
    hd_maps: HashMap<String, HdMapMeta>,
    total_texture_bytes: usize,
    pub max_texture_bytes: usize,
    pub tick: u64,
    pending_queue: VecDeque<AssetRequest>,
    in_flight: HashSet<(AssetKind, String)>,
    failed_until: HashMap<(AssetKind, String), u64>,
    permanent_missing: HashSet<(AssetKind, String)>,
}

impl Default for AssetCache {
    fn default() -> Self {
        Self::new(256 * 1024 * 1024) // 256 MB default LRU budget (fits 4x UI atlases + active scene)
    }
}

impl AssetCache {
    pub fn new(max_texture_bytes: usize) -> Self {
        Self {
            textures: HashMap::new(),
            atlases: HashMap::new(),
            maps: HashMap::new(),
            hd_maps: HashMap::new(),
            total_texture_bytes: 0,
            max_texture_bytes,
            tick: 0,
            pending_queue: VecDeque::new(),
            in_flight: HashSet::new(),
            failed_until: HashMap::new(),
            permanent_missing: HashSet::new(),
        }
    }

    pub fn advance_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        if self.total_texture_bytes > self.max_texture_bytes {
            self.evict_lru_if_needed();
        }
    }

    pub fn total_texture_bytes(&self) -> usize {
        self.total_texture_bytes
    }

    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }

    fn enqueue_if_needed(&mut self, kind: AssetKind, key: &str) {
        let id = (kind, key.to_string());
        if self.in_flight.contains(&id) || self.permanent_missing.contains(&id) {
            return;
        }
        if let Some(&retry_tick) = self.failed_until.get(&id) {
            if self.tick < retry_tick {
                return;
            }
        }
        self.in_flight.insert(id);
        self.pending_queue.push_back(AssetRequest {
            kind,
            key: key.to_string(),
        });
    }

    pub fn pop_request(&mut self) -> Option<AssetRequest> {
        self.pending_queue.pop_front()
    }

    pub fn drain_requests(&mut self) -> Vec<AssetRequest> {
        self.pending_queue.drain(..).collect()
    }

    pub fn has_pending_requests(&self) -> bool {
        !self.pending_queue.is_empty()
    }

    pub fn get_or_request_texture(&mut self, key: &str) -> Option<&Texture> {
        if let Some(tex) = self.textures.get_mut(key) {
            tex.last_used_tick = self.tick;
            return self.textures.get(key);
        }
        self.enqueue_if_needed(AssetKind::Texture, key);
        None
    }

    pub fn has_texture(&self, key: &str) -> bool {
        self.textures.contains_key(key)
    }

    pub fn get_texture(&self, key: &str) -> Option<&Texture> {
        self.textures.get(key)
    }

    pub fn get_or_request_atlas(&mut self, key: &str) -> Option<&AtlasMeta> {
        if self.atlases.contains_key(key) {
            return self.atlases.get(key);
        }
        self.enqueue_if_needed(AssetKind::AtlasMeta, key);
        None
    }

    pub fn get_atlas(&self, key: &str) -> Option<&AtlasMeta> {
        self.atlases.get(key)
    }

    pub fn get_or_request_map(&mut self, key: &str) -> Option<&[u8]> {
        if self.maps.contains_key(key) {
            return self.maps.get(key).map(|v| v.as_slice());
        }
        self.enqueue_if_needed(AssetKind::MapBinary, key);
        None
    }

    pub fn get_or_request_hd_map(&mut self, key: &str) -> Option<&HdMapMeta> {
        if self.hd_maps.contains_key(key) {
            return self.hd_maps.get(key);
        }
        self.enqueue_if_needed(AssetKind::HdMapMeta, key);
        None
    }

    pub fn get_hd_map(&self, key: &str) -> Option<&HdMapMeta> {
        self.hd_maps.get(key)
    }

    pub fn is_missing_permanent(&self, kind: AssetKind, key: &str) -> bool {
        self.permanent_missing.contains(&(kind, key.to_string()))
    }

    pub fn upload_texture(&mut self, key: String, width: u32, height: u32, pixels: Vec<u8>, pinned: bool) {
        let expected = (width as usize).saturating_mul(height as usize).saturating_mul(4);
        if width == 0 || height == 0 || pixels.len() < expected {
            self.mark_failed(AssetKind::Texture, &key);
            return;
        }
        self.in_flight.remove(&(AssetKind::Texture, key.clone()));
        self.failed_until.remove(&(AssetKind::Texture, key.clone()));
        if let Some(old) = self.textures.remove(&key) {
            self.total_texture_bytes = self.total_texture_bytes.saturating_sub(old.pixels.len());
        }
        let byte_len = pixels.len();
        self.textures.insert(
            key,
            Texture {
                width,
                height,
                pixels,
                last_used_tick: self.tick,
                pinned,
            },
        );
        self.total_texture_bytes += byte_len;
        self.evict_lru_if_needed();
    }

    pub fn upload_atlas_json(&mut self, key: String, json_text: &str) -> bool {
        self.in_flight.remove(&(AssetKind::AtlasMeta, key.clone()));
        if let Some(meta) = AtlasMeta::parse_json(json_text) {
            self.failed_until.remove(&(AssetKind::AtlasMeta, key.clone()));
            self.atlases.insert(key, meta);
            true
        } else {
            self.mark_failed(AssetKind::AtlasMeta, &key);
            false
        }
    }

    pub fn upload_map_binary(&mut self, key: String, bytes: Vec<u8>) {
        self.in_flight.remove(&(AssetKind::MapBinary, key.clone()));
        self.failed_until.remove(&(AssetKind::MapBinary, key.clone()));
        self.maps.insert(key, bytes);
    }

    pub fn upload_hd_map_json(&mut self, key: String, json_text: &str) -> bool {
        self.in_flight.remove(&(AssetKind::HdMapMeta, key.clone()));
        if let Some(meta) = HdMapMeta::parse_json(json_text) {
            self.failed_until.remove(&(AssetKind::HdMapMeta, key.clone()));
            self.permanent_missing.remove(&(AssetKind::HdMapMeta, key.clone()));
            self.hd_maps.insert(key, meta);
            true
        } else {
            self.mark_failed(AssetKind::HdMapMeta, &key);
            false
        }
    }

    /// Record a transient fetch/decode failure with cooldown (180 frames ~ 3s) instead of permanent poisoning.
    pub fn mark_failed(&mut self, kind: AssetKind, key: &str) {
        let id = (kind, key.to_string());
        self.in_flight.remove(&id);
        self.failed_until.insert(id, self.tick.saturating_add(180));
    }

    /// Mark an optional asset (such as an un-remastered map's `maps_hd/map_XXX/meta.json` returning 404)
    /// as permanently absent for this session so we never spam repeated 404 requests.
    pub fn mark_missing_permanent(&mut self, kind: AssetKind, key: &str) {
        let id = (kind, key.to_string());
        self.in_flight.remove(&id);
        self.failed_until.remove(&id);
        self.permanent_missing.insert(id);
    }

    fn evict_lru_if_needed(&mut self) {
        while self.total_texture_bytes > self.max_texture_bytes {
            let victim = self
                .textures
                .iter()
                .filter(|(_, t)| !t.pinned && t.last_used_tick < self.tick)
                .min_by_key(|(_, t)| t.last_used_tick)
                .map(|(k, _)| k.clone());
            let Some(k) = victim else {
                break;
            };
            if let Some(removed) = self.textures.remove(&k) {
                self.total_texture_bytes = self.total_texture_bytes.saturating_sub(removed.pixels.len());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lru_eviction_never_evicts_current_frame_texture() {
        let mut cache = AssetCache::new(64);
        cache.advance_tick();
        cache.upload_texture("cur_a".into(), 4, 4, vec![255; 64], false);
        // Same frame (last_used_tick == self.tick): must not evict cur_a when cur_b arrives
        cache.upload_texture("cur_b".into(), 4, 4, vec![255; 64], false);
        assert!(cache.get_texture("cur_a").is_some());
        assert!(cache.get_texture("cur_b").is_some());
        // Next frame: cur_a/cur_b become eligible for eviction
        cache.advance_tick();
        cache.upload_texture("next_c".into(), 4, 4, vec![255; 64], false);
        assert!(cache.total_texture_bytes() <= 64);
        assert!(cache.get_texture("next_c").is_some());
    }

    #[test]
    fn test_lru_eviction_respects_budget_and_pins() {
        let mut cache = AssetCache::new(100);
        cache.advance_tick();
        cache.upload_texture("pinned".into(), 4, 4, vec![255; 64], true);
        cache.advance_tick();
        cache.upload_texture("old".into(), 3, 3, vec![255; 36], false);
        assert_eq!(cache.total_texture_bytes(), 100);

        cache.advance_tick();
        cache.upload_texture("new".into(), 3, 3, vec![255; 36], false);
        assert!(cache.get_texture("pinned").is_some());
        assert!(cache.get_texture("old").is_none());
        assert!(cache.get_texture("new").is_some());
        assert!(cache.total_texture_bytes() <= 100);
    }

    #[test]
    fn test_atlas_json_parser() {
        let s = r#"{"w":832,"h":765,"shadowStripped":true,"frames":[[2,2,48,47],[54,2,48,47]]}"#;
        let meta = AtlasMeta::parse_json(s).unwrap();
        assert_eq!(meta.w, 832);
        assert_eq!(meta.h, 765);
        assert!(meta.shadow_stripped);
        assert_eq!(meta.frames.len(), 2);
        assert_eq!(meta.frames[1], [54, 2, 48, 47]);
    }

    #[test]
    fn test_hd_map_meta_parser_paginated_and_legacy() {
        let paginated = r#"{"mapId":55,"scale":4,"rooms":[{"name":"isl1_r0_c0","file":"isl1_r0_c0_4x.webp","x":24,"y":48,"w":488,"h":512}],"occluders":{"pages":["occ_r0_c0_p0_4x.webp","occ_r1_c0_p0_4x.webp"],"strips":[{"page":1,"sortY":775,"sx":4,"sy":4,"sw":128,"sh":300,"dx":511,"dy":695,"dw":32,"dh":75}]}}"#;
        let meta = HdMapMeta::parse_json(paginated).unwrap();
        assert_eq!(meta.map_id, 55);
        assert_eq!(meta.rooms.len(), 1);
        assert_eq!(meta.rooms[0].file, "isl1_r0_c0_4x.webp");
        assert_eq!(meta.occluder_pages.len(), 2);
        assert_eq!(meta.occluder_pages[1], "occ_r1_c0_p0_4x.webp");
        assert_eq!(meta.strips.len(), 1);
        assert_eq!(meta.strips[0].page, 1);
        assert_eq!(meta.strips[0].sort_y, 775);

        let legacy = r#"{"mapId":1,"scale":4,"rooms":[{"file":"r0.webp","x":0,"y":0,"w":512,"h":512}],"occluders":{"file":"occluders_4x.webp","strips":[{"sortY":911,"sx":4,"sy":4,"sw":64,"sh":64,"dx":10,"dy":20,"dw":16,"dh":16}]}}"#;
        let lmeta = HdMapMeta::parse_json(legacy).unwrap();
        assert_eq!(lmeta.occluder_pages, vec!["occluders_4x.webp".to_string()]);
        assert_eq!(lmeta.strips[0].page, 0);
        assert_eq!(lmeta.strips[0].dx, 10);
    }

    #[test]
    fn test_permanent_missing_stops_retry_spam() {
        let mut cache = AssetCache::default();
        assert!(cache.get_or_request_hd_map("graphics/maps_hd/map_200/meta.json").is_none());
        assert!(cache.pop_request().is_some());
        cache.mark_missing_permanent(AssetKind::HdMapMeta, "graphics/maps_hd/map_200/meta.json");
        for _ in 0..500 {
            cache.advance_tick();
        }
        assert!(cache.get_or_request_hd_map("graphics/maps_hd/map_200/meta.json").is_none());
        assert!(cache.pop_request().is_none());
    }

    #[test]
    fn test_json_utf8_strings_and_advance_tick_lru_eviction() {
        let meta_json = r#"{"mapId":7,"scale":4,"rooms":[{"file":"客栈_r0.webp","x":0,"y":0,"w":64,"h":64}],"occluders":{"pages":["遮挡_0.webp"],"strips":[]}}"#;
        let meta = HdMapMeta::parse_json(meta_json).expect("utf8 meta");
        assert_eq!(meta.rooms[0].file, "客栈_r0.webp");
        assert_eq!(meta.occluder_pages, vec!["遮挡_0.webp".to_string()]);

        // #269: advance_tick() evicts over-budget textures from prior frames even without a new upload.
        let mut cache = AssetCache::new(64);
        cache.advance_tick();
        cache.upload_texture("a".into(), 4, 4, vec![1; 64], false);
        cache.upload_texture("b".into(), 4, 4, vec![2; 64], false);
        assert_eq!(cache.total_texture_bytes(), 128);
        cache.advance_tick();
        assert!(cache.total_texture_bytes() <= 64, "advance_tick must evict stale over-budget textures");
    }
}
