//! Unified `OverlayVfs` (loose `--assets` overlay > mounted `.rpk` packs) for `pal-runtime` (#237).
//!
//! Supports:
//! - Pure loose directory mode (`--assets <DIR>`): zero pack overhead, 100% backward compatible.
//! - Pure local pack directory mode (`--packs-dir <DIR>`): mounts `pack-manifest.json` + `.rpk`
//!   files straight from disk with zero network access (plain or AES-256-GCM encrypted).
//! - Hybrid overlay mode: loose files (e.g. `mods/` or dev edits) override mounted `.rpk` entries.

use crate::crypto::{parse_key_hex, sha256_hex, unwrap_pack_key};
use crate::rpk::{normalize_rpk_key, RpkReader};
use crate::rpk_manifest::PackManifest;
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

/// Read an optional 32-byte master pack key from `REPAL_PACK_KEY` env var or `<user_dir>/pack.key`.
/// Never accepts keys on CLI argv and never logs key material (#195, #237).
pub fn load_pack_key_from_env_or_dir(user_dir: Option<&Path>) -> Result<Option<[u8; 32]>, String> {
    if let Ok(raw) = std::env::var("REPAL_PACK_KEY") {
        if !raw.trim().is_empty() {
            return parse_key_hex(&raw).map(Some);
        }
    }
    if let Some(dir) = user_dir {
        let key_path = dir.join("pack.key");
        if key_path.is_file() {
            let raw = fs::read_to_string(&key_path)
                .map_err(|e| format!("read {}: {e}", key_path.display()))?;
            if !raw.trim().is_empty() {
                return parse_key_hex(&raw).map(Some);
            }
        }
    }
    Ok(None)
}

/// Resolve user-entered key material: a 64-hex master key, or a passphrase unlocked through the
/// manifest's `master_key_wraps` (#18). Returns the 32-byte master key.
pub fn resolve_pack_key_input(
    raw: &str,
    manifest: Option<&PackManifest>,
) -> Result<[u8; 32], String> {
    let trimmed = raw.trim();
    if trimmed.len() == 64 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        return parse_key_hex(trimmed);
    }
    let wraps = manifest
        .map(|m| m.master_key_wraps.as_slice())
        .unwrap_or(&[]);
    if wraps.is_empty() {
        return Err(
            "expected a 64-hex pack key (this pack release has no passphrase configured)".into(),
        );
    }
    for w in wraps.iter().filter(|w| w.kdf == "pbkdf2-sha256") {
        if let Ok(k) = crate::crypto::unwrap_master_key_with_passphrase(
            trimmed,
            &w.salt,
            w.iterations,
            &w.wrapped,
        ) {
            return Ok(k);
        }
    }
    Err("wrong passphrase".into())
}

/// Like [`load_pack_key_from_env_or_dir`], but `REPAL_PACK_KEY` may also hold a passphrase, which
/// is unlocked via `<packs_dir>/pack-manifest.json`.
pub fn load_pack_key_for_packs(
    user_dir: Option<&Path>,
    packs_dir: &Path,
) -> Result<Option<[u8; 32]>, String> {
    if let Ok(raw) = std::env::var("REPAL_PACK_KEY") {
        if !raw.trim().is_empty() {
            let manifest = fs::read_to_string(packs_dir.join("pack-manifest.json"))
                .ok()
                .and_then(|t| PackManifest::from_json_str(&t).ok());
            return resolve_pack_key_input(&raw, manifest.as_ref()).map(Some);
        }
    }
    load_pack_key_from_env_or_dir(user_dir)
}

/// Resolve a relative key safely under `root`, rejecting `..`, absolute paths, and OS prefixes.
fn resolve_loose_path(root: &Path, key: &str) -> Option<PathBuf> {
    let norm = normalize_rpk_key(key).ok()?;
    let mut out = root.to_path_buf();
    for seg in norm.split('/') {
        out.push(seg);
    }
    Some(out)
}

/// Overlay Virtual File System combining an optional loose directory (highest priority) with
/// zero-extraction random-access `RPK1` packs.
pub struct OverlayVfs {
    loose_root: Option<PathBuf>,
    packs_dir: Option<PathBuf>,
    manifest: Option<PackManifest>,
    master_key: Option<[u8; 32]>,
    mounted: HashMap<String, RpkReader<File>>,
    /// Map of `asset_key -> mounted_pack_id` (populated on mount).
    mounted_keys: HashMap<String, String>,
    /// Optional `asset_key -> owning_pack_id` loaded from `boot`'s `pack_key_index.json` for
    /// lazy on-demand mounting of any dynamic/unplanned asset key.
    key_index: HashMap<String, String>,
    /// When `true`, verify full-file SHA-256 against `manifest` on first mount.
    pub verify_pack_sha256: bool,
}

impl OverlayVfs {
    /// Create a loose-directory-only VFS (`--assets <DIR>`).
    pub fn from_loose_dir(dir: PathBuf) -> Self {
        Self {
            loose_root: Some(dir),
            packs_dir: None,
            manifest: None,
            master_key: None,
            mounted: HashMap::new(),
            mounted_keys: HashMap::new(),
            key_index: HashMap::new(),
            verify_pack_sha256: false,
        }
    }

    /// Mount a local `--packs-dir <DIR>` containing `pack-manifest.json` and `.rpk` files,
    /// with an optional loose overlay directory (`--assets`) taking priority.
    pub fn from_packs_dir(
        packs_dir: PathBuf,
        master_key: Option<[u8; 32]>,
        loose_overlay: Option<PathBuf>,
        verify_sha256: bool,
    ) -> Result<Self, String> {
        let man_path = packs_dir.join("pack-manifest.json");
        let man_raw = fs::read_to_string(&man_path)
            .map_err(|e| format!("read {}: {e}", man_path.display()))?;
        let manifest = PackManifest::from_json_str(&man_raw)?;
        let mut vfs = Self {
            loose_root: loose_overlay,
            packs_dir: Some(packs_dir),
            manifest: Some(manifest.clone()),
            master_key,
            mounted: HashMap::new(),
            mounted_keys: HashMap::new(),
            key_index: HashMap::new(),
            verify_pack_sha256: verify_sha256,
        };
        for pid in &manifest.boot {
            vfs.mount_pack_from_dir(pid)?;
        }
        // Load optional `pack_key_index.json` from `boot` for O(1) dynamic miss resolution.
        if let Some(idx_key) = manifest.key_index_entry.clone() {
            if let Ok(bytes) = vfs.read_bytes(&idx_key) {
                if let Ok(map) = serde_json::from_slice::<HashMap<String, String>>(&bytes) {
                    vfs.key_index = map;
                }
            }
        }
        for pid in &manifest.shared {
            let _ = vfs.mount_pack_from_dir(pid);
        }
        Ok(vfs)
    }

    pub fn loose_root(&self) -> Option<&Path> {
        self.loose_root.as_deref()
    }

    pub fn manifest(&self) -> Option<&PackManifest> {
        self.manifest.as_ref()
    }

    pub fn mounted_pack_count(&self) -> usize {
        self.mounted.len()
    }

    pub fn is_pack_mounted(&self, pack_id: &str) -> bool {
        self.mounted.contains_key(pack_id)
    }

    /// Mount a specific pack ID from `packs_dir` (idempotent; returns `Ok(true)` if newly mounted).
    pub fn mount_pack_from_dir(&mut self, pack_id: &str) -> Result<bool, String> {
        if self.mounted.contains_key(pack_id) {
            return Ok(false);
        }
        let packs_dir = self
            .packs_dir
            .as_ref()
            .ok_or_else(|| "OverlayVfs has no packs_dir configured".to_string())?;
        let desc = self
            .manifest
            .as_ref()
            .and_then(|m| m.packs.get(pack_id))
            .ok_or_else(|| format!("pack '{pack_id}' not found in manifest"))?
            .clone();

        let pack_path = packs_dir.join(&desc.file);
        if self.verify_pack_sha256 && !desc.sha256.is_empty() {
            let bytes = fs::read(&pack_path)
                .map_err(|e| format!("read {} for SHA-256 verify: {e}", pack_path.display()))?;
            let actual = sha256_hex(&bytes);
            if !actual.eq_ignore_ascii_case(&desc.sha256) {
                return Err(format!(
                    "pack '{pack_id}' SHA-256 mismatch (got {actual}, expected {})",
                    desc.sha256
                ));
            }
        }

        let content_key = if desc.encrypted {
            let mk = self.master_key.as_ref().ok_or_else(|| {
                format!("pack '{pack_id}' is encrypted; set REPAL_PACK_KEY or <user_dir>/pack.key")
            })?;
            let wrapped = desc.wrapped_key.as_deref().ok_or_else(|| {
                format!("pack '{pack_id}' is marked encrypted but manifest lacks wrapped_key")
            })?;
            Some(unwrap_pack_key(mk, pack_id, wrapped)?)
        } else {
            None
        };

        let file =
            File::open(&pack_path).map_err(|e| format!("open {}: {e}", pack_path.display()))?;
        let reader = RpkReader::open(file, pack_id, content_key)?;
        for k in reader.keys() {
            self.mounted_keys.insert(k.to_string(), pack_id.to_string());
        }
        self.mounted.insert(pack_id.to_string(), reader);
        Ok(true)
    }

    /// Ensure all packs required for `scene_id` (and optional `map_id`) are mounted from `packs_dir`.
    pub fn ensure_scene_packs(&mut self, scene_id: u16, map_id: Option<u16>) -> Result<usize, String> {
        let Some(man) = self.manifest.clone() else {
            return Ok(0);
        };
        let mut newly_mounted = 0usize;
        for pid in man.packs_for_scene(scene_id, map_id) {
            if self.mount_pack_from_dir(&pid)? {
                newly_mounted += 1;
            }
        }
        Ok(newly_mounted)
    }

    /// Ensure the pack for `rng_id` is mounted from `packs_dir`.
    pub fn ensure_rng_pack(&mut self, rng_id: u16) -> Result<bool, String> {
        let pid = self
            .manifest
            .as_ref()
            .and_then(|m| m.pack_for_rng(rng_id))
            .map(str::to_string);
        match pid {
            Some(p) => self.mount_pack_from_dir(&p),
            None => Ok(false),
        }
    }

    /// Check whether `key` exists in loose overlay, mounted packs, or `key_index`.
    pub fn exists(&self, key: &str) -> bool {
        if let Some(root) = &self.loose_root {
            if let Some(p) = resolve_loose_path(root, key) {
                if p.is_file() {
                    return true;
                }
            }
        }
        self.mounted_keys.contains_key(key) || self.key_index.contains_key(key)
    }

    /// Read raw bytes for `key`: loose overlay > mounted `.rpk` > lazy `key_index` mount.
    pub fn read_bytes(&mut self, key: &str) -> Result<Vec<u8>, String> {
        if let Some(root) = &self.loose_root {
            if let Some(p) = resolve_loose_path(root, key) {
                if p.is_file() {
                    return fs::read(&p).map_err(|e| format!("read {}: {e}", p.display()));
                }
            }
        }
        if let Some(pid) = self.mounted_keys.get(key).cloned() {
            if let Some(reader) = self.mounted.get_mut(&pid) {
                return reader.read_entry(key);
            }
        }
        if let Some(pid) = self.key_index.get(key).cloned() {
            if self.mount_pack_from_dir(&pid).is_ok() {
                if let Some(reader) = self.mounted.get_mut(&pid) {
                    return reader.read_entry(key);
                }
            }
        }
        Err(format!("asset '{key}' not found in OverlayVfs"))
    }

    pub fn read_to_string(&mut self, key: &str) -> Result<String, String> {
        let bytes = self.read_bytes(key)?;
        String::from_utf8(bytes).map_err(|e| format!("UTF-8 decode '{key}': {e}"))
    }

    /// List all known keys under a directory prefix (e.g. `data/i18n/en/`).
    pub fn list_prefix(&self, prefix: &str) -> Vec<String> {
        let mut out = std::collections::BTreeSet::new();
        if let Some(root) = &self.loose_root {
            if let Some(dir) = resolve_loose_path(root, prefix.trim_end_matches('/')) {
                if let Ok(rd) = fs::read_dir(&dir) {
                    for entry in rd.flatten() {
                        if entry.path().is_file() {
                            if let Some(name) = entry.file_name().to_str() {
                                out.insert(format!("{}/{name}", prefix.trim_end_matches('/')));
                            }
                        }
                    }
                }
            }
        }
        for k in self.mounted_keys.keys().chain(self.key_index.keys()) {
            if k.starts_with(prefix) {
                out.insert(k.clone());
            }
        }
        out.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpk::{RpkBuilder, RpkWriteOptions};
    use crate::rpk_manifest::{PackDescriptor, PACK_MANIFEST_FORMAT};
    use std::collections::BTreeMap;

    #[test]
    fn overlay_vfs_mounts_plain_and_encrypted_packs_with_loose_override() {
        let tmp = std::env::temp_dir().join(format!(
            "repal_vfs_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        let packs_dir = tmp.join("packs");
        let loose_dir = tmp.join("loose");
        fs::create_dir_all(&packs_dir).unwrap();
        fs::create_dir_all(loose_dir.join("data")).unwrap();

        let master = [0x5Au8; 32];
        let ckey_boot = [0x11u8; 32];
        let ckey_ch1 = [0x22u8; 32];

        let mut b_boot = RpkBuilder::new(RpkWriteOptions {
            pack_id: "boot".into(),
            compress_toc: true,
            content_key: Some(ckey_boot),
            encrypt_toc: true,
            advisory_flags: 0,
        });
        b_boot.add("data/scenes.yaml", b"scenes: []\n".to_vec()).unwrap();
        b_boot
            .add(
                "pack_key_index.json",
                br#"{"maps/map_012.bin":"ch01a"}"#.to_vec(),
            )
            .unwrap();
        let boot_bytes = b_boot.finish().unwrap();
        let boot_sha = sha256_hex(&boot_bytes);
        fs::write(packs_dir.join("boot.rpk"), &boot_bytes).unwrap();

        let mut b_ch1 = RpkBuilder::new(RpkWriteOptions {
            pack_id: "ch01a".into(),
            compress_toc: true,
            content_key: Some(ckey_ch1),
            encrypt_toc: true,
            advisory_flags: 0,
        });
        b_ch1.add("maps/map_012.bin", vec![0x77; 128]).unwrap();
        let ch1_bytes = b_ch1.finish().unwrap();
        let ch1_sha = sha256_hex(&ch1_bytes);
        fs::write(packs_dir.join("ch01a.rpk"), &ch1_bytes).unwrap();

        let mut packs = BTreeMap::new();
        packs.insert(
            "boot".into(),
            PackDescriptor {
                file: "boot.rpk".into(),
                sha256: boot_sha,
                size: boot_bytes.len() as u64,
                urls: vec![],
                encrypted: true,
                wrapped_key: Some(crate::crypto::wrap_pack_key(
                    &master,
                    "boot",
                    &ckey_boot,
                    &[1u8; 12],
                )),
                entry_count: 2,
            },
        );
        packs.insert(
            "ch01a".into(),
            PackDescriptor {
                file: "ch01a.rpk".into(),
                sha256: ch1_sha,
                size: ch1_bytes.len() as u64,
                urls: vec![],
                encrypted: true,
                wrapped_key: Some(crate::crypto::wrap_pack_key(
                    &master,
                    "ch01a",
                    &ckey_ch1,
                    &[2u8; 12],
                )),
                entry_count: 1,
            },
        );
        let manifest = PackManifest {
            format: PACK_MANIFEST_FORMAT.into(),
            schema_version: 1,
            version: "test".into(),
            boot: vec!["boot".into()],
            shared: vec![],
            packs,
            scenes: BTreeMap::from([("1".into(), vec!["ch01a".into()])]),
            key_index_entry: Some("pack_key_index.json".into()),
            ..Default::default()
        };
        fs::write(
            packs_dir.join("pack-manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        // Loose override file shadows boot pack's `data/scenes.yaml`
        fs::write(loose_dir.join("data/scenes.yaml"), b"scenes: [override]\n").unwrap();

        // Wrong master key fails cleanly
        assert!(OverlayVfs::from_packs_dir(packs_dir.clone(), Some([0xFFu8; 32]), None, true).is_err());

        // Correct master key mounts boot, lazily mounts ch01a on miss, and respects loose override!
        let mut vfs =
            OverlayVfs::from_packs_dir(packs_dir, Some(master), Some(loose_dir), true).unwrap();
        assert_eq!(vfs.mounted_pack_count(), 1);
        assert_eq!(vfs.read_to_string("data/scenes.yaml").unwrap(), "scenes: [override]\n");
        assert_eq!(vfs.read_bytes("maps/map_012.bin").unwrap(), vec![0x77; 128]);
        assert_eq!(vfs.mounted_pack_count(), 2);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_pack_key_input_accepts_hex_or_passphrase() {
        let master = [0x5Au8; 32];
        let salt = [9u8; 16];
        let wrapped = crate::crypto::wrap_master_key_with_passphrase(
            "Ling Er", &salt, 1000, &master, &[3u8; 12],
        );
        let manifest = PackManifest {
            master_key_wraps: vec![crate::rpk_manifest::MasterKeyWrap {
                kdf: "pbkdf2-sha256".into(),
                iterations: 1000,
                salt: crate::crypto::hex_encode(&salt),
                wrapped,
            }],
            ..Default::default()
        };
        let hex = crate::crypto::hex_encode(&master);
        assert_eq!(resolve_pack_key_input(&hex, None).unwrap(), master);
        assert_eq!(
            resolve_pack_key_input(" ling  er ", Some(&manifest)).unwrap(),
            master
        );
        assert!(resolve_pack_key_input("wrong", Some(&manifest)).is_err());
        assert!(resolve_pack_key_input("ling er", None).is_err());
    }
}
