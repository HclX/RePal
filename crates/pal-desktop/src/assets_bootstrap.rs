//! Hash-pinned automatic asset bootstrap for RePal (`pal-desktop`).
//!
//! Resolves game assets in order:
//! 1. Explicit `--assets <DIR>` CLI flag
//! 2. Local development / adjacent directories (`./site/assets`, `./assets`, `<exe_dir>/assets`)
//! 3. Cached user-profile directory (`<user_profile>/RePal/assets_cache/<short_sha256>/assets`)
//! 4. Background download from `--assets-url` / `REPAL_ASSETS_URL` / pinned GitHub Release
//!    (`HclX/RePal_Assets`), reporting live progress to the splash screen, verifying SHA-256,
//!    and extracting into `<cache_root>/<short_sha256>/assets/`.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

const EMBEDDED_LOCK: &str = include_str!("../../../assets-lock.json");

#[derive(Debug, Clone)]
pub struct AssetsLock {
    pub version: String,
    pub repo: String,
    pub tag: String,
    pub filename: String,
    pub sha256: String,
    pub size_bytes: u64,
}

impl AssetsLock {
    pub fn load() -> Option<Self> {
        let mut raw = EMBEDDED_LOCK.to_string();
        if let Ok(exe) = env::current_exe() {
            if let Some(dir) = exe.parent() {
                if let Ok(txt) = fs::read_to_string(dir.join("assets-lock.json")) {
                    raw = txt;
                }
            }
        }
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        Some(Self {
            version: v["version"].as_str()?.to_string(),
            repo: v["repo"].as_str()?.to_string(),
            tag: v["tag"].as_str()?.to_string(),
            filename: v["filename"].as_str()?.to_string(),
            sha256: v["sha256"].as_str()?.to_lowercase(),
            size_bytes: v["size_bytes"].as_u64().unwrap_or(211_677_822),
        })
    }
}

pub enum BootstrapMsg {
    Progress {
        pct: f32,
        stage: String,
        detail: String,
    },
    Ready(PathBuf),
    ReadyPacks {
        packs_dir: PathBuf,
        fetcher: crate::pack_fetcher::PackFetcher,
    },
    Error(String),
}

fn has_valid_assets(dir: &Path) -> bool {
    dir.join("data/scenes.yaml").is_file()
}

#[cfg(target_os = "windows")]
fn configure_bg_command(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_bg_command(_cmd: &mut Command) {}

/// Fast synchronous check for already-available local or hash-verified cached assets.
/// Returns `None` if assets need to be downloaded in the background with the splash screen.
/// Remove legacy extracted asset-archive caches (`<cache_root>/<12-hex sha>/` holding `assets/`
/// and/or `.verified_sha256`) left behind by pre-pack releases. Only called in packs mode, where
/// assets are read from `<cache_root>/packs/`; never touches anything else.
pub fn gc_legacy_loose_asset_caches(cache_root: &Path) -> usize {
    let Ok(rd) = fs::read_dir(cache_root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let is_hash_dir = name.len() == 12 && name.bytes().all(|b| b.is_ascii_hexdigit());
        let looks_legacy = path.join(".verified_sha256").is_file() || path.join("assets").is_dir();
        if path.is_dir() && is_hash_dir && looks_legacy && fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

pub fn try_resolve_local_assets(explicit: Option<&str>, cache_root: &Path) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(PathBuf::from(p));
    }

    for candidate in ["site/assets", "assets"] {
        let p = PathBuf::from(candidate);
        if has_valid_assets(&p) {
            return Some(p);
        }
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            for rel in ["assets", "site/assets", "../assets", "../site/assets"] {
                let p = dir.join(rel);
                if has_valid_assets(&p) {
                    return Some(p);
                }
            }
        }
    }

    let lock = AssetsLock::load()?;
    let short_hash = &lock.sha256[..lock.sha256.len().min(12)];
    let hash_dir = cache_root.join(short_hash);
    let cached_assets = hash_dir.join("assets");
    let stamp_path = hash_dir.join(".verified_sha256");

    if has_valid_assets(&cached_assets) {
        if let Ok(stamp) = fs::read_to_string(&stamp_path) {
            if stamp.trim().eq_ignore_ascii_case(&lock.sha256) {
                return Some(cached_assets);
            }
        }
    }
    None
}

fn compute_sha256(path: &Path) -> Result<String, String> {
    crate::updater::sha256_file_hex(path)
}

/// Only send `GITHUB_TOKEN` to trusted HTTPS GitHub hosts (#195).
fn is_github_token_host(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host_port.split(':').next().unwrap_or("");
    host.eq_ignore_ascii_case("github.com")
        || host.eq_ignore_ascii_case("api.github.com")
        || host.eq_ignore_ascii_case("objects.githubusercontent.com")
}

/// Format a `curl -K -` stdin config carrying a bearer token so credentials never appear in argv (#195).
fn format_curl_auth_config(token: &str) -> Option<String> {
    let clean = token.trim();
    if clean.is_empty() || clean.contains(['\r', '\n', '"', '\\']) {
        return None;
    }
    Some(format!("header = \"Authorization: Bearer {clean}\"\n"))
}

fn run_download_cmd_with_progress(
    mut cmd: Command,
    stdin_payload: Option<&str>,
    dst: &Path,
    expected_bytes: u64,
    label: &str,
    tx: &Sender<BootstrapMsg>,
) -> Result<(), String> {
    configure_bg_command(&mut cmd);
    if stdin_payload.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    if let Some(payload) = stdin_payload {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
    }
    let total_mb = (expected_bytes as f64) / (1024.0 * 1024.0);
    loop {
        if let Some(status) = child.try_wait().map_err(|e| format!("wait failed: {e}"))? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("command exited with {status}"))
            };
        }
        let cur_bytes = fs::metadata(dst).map(|m| m.len()).unwrap_or(0);
        let cur_mb = (cur_bytes as f64) / (1024.0 * 1024.0);
        let frac = if expected_bytes > 0 {
            ((cur_bytes as f64) / (expected_bytes as f64)).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let _ = tx.send(BootstrapMsg::Progress {
            pct: frac * 0.85,
            stage: "DOWNLOADING ASSETS...".to_string(),
            detail: format!("{cur_mb:.1} / {total_mb:.1} MB  ({label})"),
        });
        thread::sleep(Duration::from_millis(120));
    }
}

fn download_archive_with_progress(
    lock: &AssetsLock,
    url_override: Option<&str>,
    dst: &Path,
    tx: &Sender<BootstrapMsg>,
) -> Result<(), String> {
    let custom_url = url_override
        .map(|s| s.to_string())
        .or_else(|| env::var("REPAL_ASSETS_URL").ok().filter(|s| !s.is_empty()))
        .or_else(|| env::var("PAL_ASSETS_URL").ok().filter(|s| !s.is_empty()));

    if let Some(url) = custom_url {
        eprintln!("[repal] Downloading assets from {url} ...");
        let mut cmd = Command::new("curl");
        cmd.args([
            "-fSL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "-o",
        ])
        .arg(dst);
        let auth_cfg = if is_github_token_host(&url) {
            env::var("GITHUB_TOKEN")
                .ok()
                .and_then(|tok| format_curl_auth_config(&tok))
        } else {
            None
        };
        if auth_cfg.is_some() {
            cmd.args(["-K", "-"]);
        }
        cmd.arg("--").arg(&url);
        return run_download_cmd_with_progress(
            cmd,
            auth_cfg.as_deref(),
            dst,
            lock.size_bytes,
            &lock.filename,
            tx,
        );
    }

    // 1. Try `gh release download` (works seamlessly for private GitHub repos like HclX/RePal_Assets)
    eprintln!(
        "[repal] Fetching pinned asset pack {} ({}) from {} ...",
        lock.filename, lock.version, lock.repo
    );
    let mut gh_cmd = Command::new("gh");
    gh_cmd
        .args([
            "release",
            "download",
            &lock.tag,
            "--repo",
            &lock.repo,
            "--pattern",
            &lock.filename,
            "--clobber",
            "--output",
        ])
        .arg(dst);
    if run_download_cmd_with_progress(gh_cmd, None, dst, lock.size_bytes, &lock.filename, tx).is_ok() {
        return Ok(());
    }

    // 2. Fallback: direct curl to GitHub Releases URL (works for public repos or with GITHUB_TOKEN)
    let _ = fs::remove_file(dst);
    let gh_url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        lock.repo, lock.tag, lock.filename
    );
    eprintln!("[repal] Falling back to curl {gh_url} ...");
    let mut cmd = Command::new("curl");
    cmd.args([
        "-fSL",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "-o",
    ])
    .arg(dst);
    let auth_cfg = env::var("GITHUB_TOKEN")
        .ok()
        .and_then(|tok| format_curl_auth_config(&tok));
    if auth_cfg.is_some() {
        cmd.args(["-K", "-"]);
    }
    cmd.arg("--").arg(&gh_url);
    if run_download_cmd_with_progress(
        cmd,
        auth_cfg.as_deref(),
        dst,
        lock.size_bytes,
        &lock.filename,
        tx,
    )
    .is_ok()
    {
        return Ok(());
    }

    Err(format!(
        "Could not download {} from {}.\n\
         For private repo access:\n\
         1) Run `gh auth login`, OR\n\
         2) Set GITHUB_TOKEN / REPAL_ASSETS_URL, OR\n\
         3) Pass --assets <LOCAL_ASSETS_DIR>",
        lock.filename, lock.repo
    ))
}

/// Spawn a background worker thread that downloads, verifies SHA-256, and prepares either:
/// - v2 level-based `.rpk` packs (`pack-manifest.json` + `boot` packs only for fast cold start, #237), or
/// - legacy v1 tarball archive (`<cache_root>/<short_sha256>/assets/`).
pub fn spawn_asset_download(
    cache_root: PathBuf,
    assets_url_override: Option<String>,
) -> Receiver<BootstrapMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // Check if `--assets-url` / `REPAL_ASSETS_URL` points to a v2 `pack-manifest.json`,
        // or if `assets-lock.json` is itself a v2 `repal-packs/1` manifest lock (#237).
        let custom_url = assets_url_override
            .clone()
            .or_else(|| env::var("REPAL_ASSETS_URL").ok().filter(|s| !s.is_empty()))
            .or_else(|| env::var("PAL_ASSETS_URL").ok().filter(|s| !s.is_empty()));

        let pack_manifest_target: Option<(String, Option<String>)> = if let Some(u) =
            custom_url.as_deref().filter(|u| u.ends_with(".json"))
        {
            Some((u.to_string(), None))
        } else {
            let mut raw = EMBEDDED_LOCK.to_string();
            if let Ok(exe) = env::current_exe() {
                if let Some(dir) = exe.parent() {
                    if let Ok(txt) = fs::read_to_string(dir.join("assets-lock.json")) {
                        raw = txt;
                    }
                }
            }
            match pal_runtime::rpk_manifest::AssetsLockKind::parse_json_str(&raw) {
                Ok(pal_runtime::rpk_manifest::AssetsLockKind::PackManifest {
                    manifest_urls,
                    manifest_sha256,
                    ..
                }) => manifest_urls
                    .first()
                    .cloned()
                    .map(|u| (u, (!manifest_sha256.is_empty()).then_some(manifest_sha256))),
                _ => None,
            }
        };

        if let Some((manifest_url, expected_sha)) = pack_manifest_target {
            let packs_dir = cache_root.join("packs");
            let _ = tx.send(BootstrapMsg::Progress {
                pct: 0.05,
                stage: "FETCHING PACK MANIFEST...".to_string(),
                detail: manifest_url.clone(),
            });
            let fetcher = match crate::pack_fetcher::PackFetcher::fetch_manifest(
                packs_dir.clone(),
                manifest_url,
                expected_sha.as_deref(),
            ) {
                Ok(f) => f,
                Err(e) => {
                    let _ = tx.send(BootstrapMsg::Error(e));
                    return;
                }
            };
            // Packs mode no longer needs the pre-pack extracted asset archive caches.
            gc_legacy_loose_asset_caches(&cache_root);
            let boot_ids: Vec<String> = fetcher.manifest.boot.clone();
            let total = boot_ids.len().max(1);
            for (idx, pid) in boot_ids.iter().enumerate() {
                let stage = if fetcher.is_pack_cached(pid) {
                    "VERIFYING BOOT PACK..."
                } else {
                    "DOWNLOADING BOOT PACK..."
                };
                let _ = tx.send(BootstrapMsg::Progress {
                    pct: 0.10 + 0.80 * (idx as f32 / total as f32),
                    stage: stage.to_string(),
                    detail: format!("{pid}.rpk ({}/{})", idx + 1, total),
                });
                if let Err(e) = fetcher.ensure_pack(pid) {
                    let _ = tx.send(BootstrapMsg::Error(e));
                    return;
                }
            }
            let _ = tx.send(BootstrapMsg::Progress {
                pct: 1.0,
                stage: "STARTING REPAL...".to_string(),
                detail: "MOUNTING ASSET PACKS".to_string(),
            });
            let _ = tx.send(BootstrapMsg::ReadyPacks { packs_dir, fetcher });
            return;
        }

        let Some(lock) = AssetsLock::load() else {
            let _ = tx.send(BootstrapMsg::Error(
                "Missing or invalid assets-lock.json".to_string(),
            ));
            return;
        };
        let short_hash = &lock.sha256[..lock.sha256.len().min(12)];
        let hash_dir = cache_root.join(short_hash);
        let cached_assets = hash_dir.join("assets");
        let stamp_path = hash_dir.join(".verified_sha256");

        let _ = tx.send(BootstrapMsg::Progress {
            pct: 0.02,
            stage: "DOWNLOADING ASSETS...".to_string(),
            detail: format!("CONNECTING TO {} ({})", lock.repo, lock.version),
        });

        if let Err(e) = fs::create_dir_all(&hash_dir) {
            let _ = tx.send(BootstrapMsg::Error(format!(
                "Failed to create cache dir {}: {e}",
                hash_dir.display()
            )));
            return;
        }

        let tmp_tar = hash_dir.join(format!("{}.downloading", lock.filename));
        if let Err(e) =
            download_archive_with_progress(&lock, assets_url_override.as_deref(), &tmp_tar, &tx)
        {
            let _ = fs::remove_file(&tmp_tar);
            eprintln!("[repal] Asset bootstrap error:\n{e}");
            let _ = tx.send(BootstrapMsg::Error(e));
            return;
        }

        let _ = tx.send(BootstrapMsg::Progress {
            pct: 0.88,
            stage: "VERIFYING SHA-256...".to_string(),
            detail: format!("CHECKSUM {short_hash}"),
        });

        match compute_sha256(&tmp_tar) {
            Ok(actual) if actual == lock.sha256 => {
                eprintln!("[repal] SHA-256 verified ({short_hash})! Extracting...");
            }
            Ok(actual) => {
                let _ = fs::remove_file(&tmp_tar);
                let _ = tx.send(BootstrapMsg::Error(format!(
                    "SHA-256 mismatch: expected {short_hash}, got {}",
                    &actual[..actual.len().min(12)]
                )));
                return;
            }
            Err(e) => {
                // Fail-closed: never extract an unverified archive (#194).
                let _ = fs::remove_file(&tmp_tar);
                let _ = tx.send(BootstrapMsg::Error(format!(
                    "SHA-256 verification failed: {e}"
                )));
                return;
            }
        }

        let _ = tx.send(BootstrapMsg::Progress {
            pct: 0.94,
            stage: "EXTRACTING ASSETS...".to_string(),
            detail: format!("UNPACKING {}", lock.filename),
        });

        let mut tar_cmd = Command::new("tar");
        configure_bg_command(&mut tar_cmd);
        let extract = tar_cmd
            .arg("-xzf")
            .arg(&tmp_tar)
            .arg("-C")
            .arg(&hash_dir)
            .status();
        let _ = fs::remove_file(&tmp_tar);

        match extract {
            Ok(st) if st.success() => {
                let _ = fs::write(&stamp_path, format!("{}\n", lock.sha256));
                eprintln!(
                    "[repal] Assets cached & verified at {}",
                    cached_assets.display()
                );
                let _ = tx.send(BootstrapMsg::Progress {
                    pct: 1.0,
                    stage: "STARTING REPAL...".to_string(),
                    detail: "LOADING GAME ENGINE".to_string(),
                });
                let _ = tx.send(BootstrapMsg::Ready(cached_assets));
            }
            Ok(st) => {
                let _ = tx.send(BootstrapMsg::Error(format!(
                    "Failed to extract archive (tar exited {st})"
                )));
            }
            Err(e) => {
                let _ = tx.send(BootstrapMsg::Error(format!("Failed to run tar: {e}")));
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_token_restricted_to_https_github_hosts() {
        assert!(is_github_token_host("https://github.com/HclX/RePal_Assets/releases/download/v1/a.tar.gz"));
        assert!(is_github_token_host("https://api.github.com/repos/HclX/RePal/releases/latest"));
        assert!(is_github_token_host("https://objects.githubusercontent.com/foo"));
        assert!(!is_github_token_host("http://github.com/HclX/RePal"));
        assert!(!is_github_token_host("https://github.com.evil.com/x"));
        assert!(!is_github_token_host("https://evil.example/github.com/x"));
        assert!(!is_github_token_host("file:///etc/passwd"));
    }

    #[test]
    fn curl_auth_config_rejects_injection_chars() {
        assert_eq!(
            format_curl_auth_config("ghp_abc123").as_deref(),
            Some("header = \"Authorization: Bearer ghp_abc123\"\n")
        );
        assert!(format_curl_auth_config("").is_none());
        assert!(format_curl_auth_config("bad\ntoken").is_none());
        assert!(format_curl_auth_config("bad\"token").is_none());
    }
}

#[cfg(test)]
mod legacy_cache_gc_tests {
    use super::*;

    #[test]
    fn removes_only_legacy_hash_asset_dirs() {
        let root = std::env::temp_dir().join(format!("repal_legacy_gc_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("dabe2b493d0c/assets/data")).unwrap();
        fs::write(root.join("dabe2b493d0c/.verified_sha256"), "x").unwrap();
        fs::create_dir_all(root.join("packs")).unwrap();
        fs::write(root.join("packs/pack-manifest.json"), "{}").unwrap();
        fs::create_dir_all(root.join("0123456789ab")).unwrap(); // hash-named but not legacy
        fs::create_dir_all(root.join("notes")).unwrap();
        assert_eq!(gc_legacy_loose_asset_caches(&root), 1);
        assert!(!root.join("dabe2b493d0c").exists());
        assert!(root.join("packs/pack-manifest.json").is_file());
        assert!(root.join("0123456789ab").is_dir());
        assert!(root.join("notes").is_dir());
        let _ = fs::remove_dir_all(&root);
    }
}
