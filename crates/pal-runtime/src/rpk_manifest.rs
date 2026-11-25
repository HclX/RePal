//! Forward- & backward-compatible `pack-manifest.json` (`repal-packs/1`) and `assets-lock.json`
//! schema types shared by Desktop, Headless, and WASM (#237).
//!
//! ## Compatibility Rules
//! - `format` identifies the major protocol (`"repal-packs/1"`).
//! - `schema_version` (default `1`) tracks additive minor extensions; all optional fields use
//!   `#[serde(default)]` and unknown JSON keys are silently ignored by older readers.
//! - `urls[]` supports per-pack fallback mirrors; relative URLs (or omitted `urls`) resolve
//!   against the manifest's own base URL, keeping clients 100% host-agnostic.
//! - `assets-lock.json` parses BOTH legacy v1 monolithic tarballs AND v2 `repal-packs/1` manifests!

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

pub const PACK_MANIFEST_FORMAT: &str = "repal-packs/1";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PackDescriptor {
    /// On-disk / URL filename (e.g. `ch01a-yuhang.99aabbcc.rpk` or opaque `p-99aabbcc.rpk`).
    pub file: String,
    /// Lowercase hex SHA-256 over the exact `.rpk` bytes (ciphertext when encrypted).
    pub sha256: String,
    /// Byte length of the `.rpk` file.
    pub size: u64,
    /// Ordered list of download URLs (absolute `https://`/`http://`/`file://` or relative to manifest).
    /// When empty, clients fall back to resolving `file` relative to the manifest URL.
    pub urls: Vec<String>,
    /// True when entries in this `.rpk` are AES-256-GCM encrypted.
    pub encrypted: bool,
    /// Optional master-key-wrapped content key (`v1:<hex>`), bound to `pack_id` via AAD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrapped_key: Option<String>,
    /// Number of entries inside this pack (advisory metadata).
    pub entry_count: u32,
}

impl PackDescriptor {
    /// Resolve all candidate URLs for this pack against `manifest_base_url` (e.g. `https://host/packs/`).
    pub fn resolve_urls(&self, manifest_base_url: &str) -> Vec<String> {
        let base = manifest_base_url.trim_end_matches('/');
        if self.urls.is_empty() {
            return vec![resolve_relative_url(base, &self.file)];
        }
        self.urls
            .iter()
            .map(|u| resolve_relative_url(base, u))
            .collect()
    }
}

/// Resolve `target` against `base` (preserving `https://`, `http://`, `file://`, or `/`-rooted URLs).
pub fn resolve_relative_url(base: &str, target: &str) -> String {
    let t = target.trim();
    if t.starts_with("https://")
        || t.starts_with("http://")
        || t.starts_with("file://")
        || t.starts_with('/')
    {
        return t.to_string();
    }
    let clean_t = t.strip_prefix("./").unwrap_or(t);
    if base.is_empty() {
        clean_t.to_string()
    } else {
        format!("{}/{clean_t}", base.trim_end_matches('/'))
    }
}

/// Derive the directory base URL from a manifest URL (stripping the trailing filename & query).
pub fn manifest_base_url(manifest_url: &str) -> String {
    let no_q = manifest_url.split(['?', '#']).next().unwrap_or(manifest_url);
    match no_q.rfind('/') {
        Some(idx) => no_q[..idx].to_string(),
        None => String::new(),
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PackManifest {
    pub format: String,
    pub schema_version: u32,
    pub version: String,
    /// Optional minimum client version advisory string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_client_version: Option<String>,
    /// Pack IDs required before showing the Title Screen (typically `["boot"]`).
    pub boot: Vec<String>,
    /// Pack IDs required for any gameplay session (typically `["shared"]`).
    pub shared: Vec<String>,
    /// Map of `pack_id -> PackDescriptor`.
    pub packs: BTreeMap<String, PackDescriptor>,
    /// Map of `scene_id (decimal string) -> list of pack_ids needed for that scene`.
    pub scenes: BTreeMap<String, Vec<String>>,
    /// Map of `map_id (decimal string) -> optional per-map HD pack_id`.
    pub maps: BTreeMap<String, String>,
    /// Map of `rng_id (decimal string) -> pack_id`.
    pub rng: BTreeMap<String, String>,
    /// Optional adjacency map `scene_id -> neighboring pack_ids` for idle background prefetch.
    pub prefetch: BTreeMap<String, Vec<String>>,
    /// Optional key-index entry path inside `boot` pack (e.g. `pack_key_index.json`) mapping
    /// any arbitrary `asset_key -> pack_id` for dynamic edge cases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_index_entry: Option<String>,
}

impl PackManifest {
    pub fn from_json_str(raw: &str) -> Result<Self, String> {
        let m: Self = serde_json::from_str(raw).map_err(|e| format!("parse pack-manifest: {e}"))?;
        if m.format != PACK_MANIFEST_FORMAT {
            return Err(format!(
                "unsupported pack-manifest format '{}' (expected '{PACK_MANIFEST_FORMAT}')",
                m.format
            ));
        }
        if m.packs.is_empty() {
            return Err("pack-manifest contains no packs".to_string());
        }
        Ok(m)
    }

    /// Return deduplicated pack IDs needed before entering `scene_id` (always includes `boot` + `shared`
    /// + scene packs + optional per-map HD pack).
    pub fn packs_for_scene(&self, scene_id: u16, map_id: Option<u16>) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut push = |id: &str| {
            if seen.insert(id.to_string()) && self.packs.contains_key(id) {
                out.push(id.to_string());
            }
        };
        for id in &self.boot {
            push(id);
        }
        for id in &self.shared {
            push(id);
        }
        if let Some(list) = self.scenes.get(&scene_id.to_string()) {
            for id in list {
                push(id);
            }
        }
        if let Some(mid) = map_id {
            if let Some(pid) = self.maps.get(&mid.to_string()) {
                push(pid);
            }
        }
        out
    }

    pub fn pack_for_rng(&self, rng_id: u16) -> Option<&str> {
        self.rng
            .get(&rng_id.to_string())
            .map(String::as_str)
            .filter(|id| self.packs.contains_key(*id))
    }

    /// All filenames currently referenced by this manifest (used for safe cache GC).
    pub fn referenced_files(&self) -> HashSet<&str> {
        self.packs.values().map(|p| p.file.as_str()).collect()
    }
}

/// Backward-compatible parser for `assets-lock.json` supporting BOTH:
/// - Legacy v1 monolithic tarball lock (`filename: "pal-assets-*.tar.gz"`)
/// - New v2 `repal-packs/1` manifest lock (`manifest: { url, sha256, size_bytes }`)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetsLockKind {
    LegacyTarball {
        version: String,
        repo: String,
        tag: String,
        filename: String,
        sha256: String,
        size_bytes: u64,
    },
    PackManifest {
        version: String,
        manifest_urls: Vec<String>,
        manifest_sha256: String,
        manifest_size_bytes: u64,
    },
}

impl AssetsLockKind {
    pub fn parse_json_str(raw: &str) -> Result<Self, String> {
        let v: serde_json::Value =
            serde_json::from_str(raw).map_err(|e| format!("parse assets-lock.json: {e}"))?;
        if v.get("format").and_then(|x| x.as_str()) == Some(PACK_MANIFEST_FORMAT)
            || v.get("manifest").is_some()
        {
            let version = v["version"].as_str().unwrap_or("assets-v2").to_string();
            let man = &v["manifest"];
            let mut urls = Vec::new();
            if let Some(arr) = man.get("urls").and_then(|x| x.as_array()) {
                for u in arr {
                    if let Some(s) = u.as_str() {
                        if !s.is_empty() {
                            urls.push(s.to_string());
                        }
                    }
                }
            }
            if let Some(u) = man.get("url").and_then(|x| x.as_str()) {
                if !u.is_empty() && !urls.contains(&u.to_string()) {
                    urls.push(u.to_string());
                }
            }
            let sha256 = man["sha256"]
                .as_str()
                .ok_or_else(|| "assets-lock manifest missing sha256".to_string())?
                .to_ascii_lowercase();
            let size_bytes = man["size_bytes"].as_u64().unwrap_or(0);
            return Ok(Self::PackManifest {
                version,
                manifest_urls: urls,
                manifest_sha256: sha256,
                manifest_size_bytes: size_bytes,
            });
        }
        Ok(Self::LegacyTarball {
            version: v["version"]
                .as_str()
                .ok_or("legacy assets-lock missing version")?
                .to_string(),
            repo: v["repo"]
                .as_str()
                .ok_or("legacy assets-lock missing repo")?
                .to_string(),
            tag: v["tag"]
                .as_str()
                .ok_or("legacy assets-lock missing tag")?
                .to_string(),
            filename: v["filename"]
                .as_str()
                .ok_or("legacy assets-lock missing filename")?
                .to_string(),
            sha256: v["sha256"]
                .as_str()
                .ok_or("legacy assets-lock missing sha256")?
                .to_ascii_lowercase(),
            size_bytes: v["size_bytes"].as_u64().unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_url_resolution_and_forward_compat_unknown_fields() {
        let raw = r#"{
            "format": "repal-packs/1",
            "schema_version": 2,
            "future_field_ignored": { "x": 123 },
            "version": "assets-v3",
            "boot": ["boot"],
            "shared": ["shared"],
            "packs": {
                "boot": { "file": "boot.11111111.rpk", "sha256": "aaaa", "size": 100 },
                "shared": { "file": "shared.22222222.rpk", "sha256": "bbbb", "size": 200 },
                "ch01a": {
                    "file": "ch01a.33333333.rpk",
                    "sha256": "cccc",
                    "size": 300,
                    "urls": ["https://cdn-a.example/packs/ch01a.33333333.rpk", "./ch01a.33333333.rpk"],
                    "future_pack_tag": true
                }
            },
            "scenes": { "1": ["ch01a"] },
            "maps": { "12": "map-012" },
            "rng": { "0": "rng-00" }
        }"#;
        let m = PackManifest::from_json_str(raw).expect("parse forward-compat manifest");
        assert_eq!(m.schema_version, 2);
        assert_eq!(m.packs_for_scene(1, Some(12)), vec!["boot", "shared", "ch01a"]);
        assert_eq!(
            m.packs["boot"].resolve_urls("https://pages.example/RePal/packs"),
            vec!["https://pages.example/RePal/packs/boot.11111111.rpk"]
        );
        assert_eq!(
            m.packs["ch01a"].resolve_urls("https://pages.example/RePal/packs"),
            vec![
                "https://cdn-a.example/packs/ch01a.33333333.rpk",
                "https://pages.example/RePal/packs/ch01a.33333333.rpk"
            ]
        );
        assert_eq!(
            manifest_base_url("https://pages.example/RePal/packs/pack-manifest.json?v=1"),
            "https://pages.example/RePal/packs"
        );
    }

    #[test]
    fn assets_lock_parses_both_legacy_tarball_and_pack_manifest() {
        let legacy = r#"{
            "version": "assets-v2",
            "repo": "HclX/RePal_Assets",
            "tag": "assets-v2",
            "filename": "pal-assets-b57760af.tar.gz",
            "sha256": "B57760AF",
            "size_bytes": 263964883
        }"#;
        assert!(matches!(
            AssetsLockKind::parse_json_str(legacy).unwrap(),
            AssetsLockKind::LegacyTarball { .. }
        ));

        let v2 = r#"{
            "format": "repal-packs/1",
            "version": "assets-v3",
            "manifest": {
                "urls": ["https://hclx.github.io/RePal/packs/pack-manifest.json"],
                "sha256": "deadbeef",
                "size_bytes": 12345
            }
        }"#;
        assert!(matches!(
            AssetsLockKind::parse_json_str(v2).unwrap(),
            AssetsLockKind::PackManifest { .. }
        ));
    }
}
