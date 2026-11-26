//! Host-agnostic Desktop `.rpk` Pack Fetcher & Windows-safe Cache GC (#237).
//!
//! Fetches `pack-manifest.json` and individual `.rpk` packs from any static host:
//! - `https://`, `http://`, or `file://` URLs
//! - Ordered per-pack `urls[]` mirror fallback (relative URLs resolved against manifest URL)
//! - Mandatory SHA-256 verification before atomic commit (tampered mirror automatically fails over
//!   to the next URL in `urls[]`)
//! - Startup-only, pre-mount garbage collection (`gc_unreferenced_packs`) so Windows never hits
//!   open-handle `ERROR_SHARING_VIOLATION` locks.

use pal_runtime::crypto::Sha256;
use pal_runtime::rpk_manifest::PackManifest;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

#[cfg(target_os = "windows")]
fn configure_bg_command(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_bg_command(_cmd: &mut Command) {}

/// Streaming SHA-256 hex digest of a file on disk (zero full-file RAM buffering).
pub fn sha256_file_streaming(path: &Path) -> Result<String, String> {
    let mut f = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{b:02x}");
    }
    Ok(hex)
}

/// Convert a `file://` URL to a local filesystem path.
fn parse_file_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let path_str = rest.strip_prefix("localhost").unwrap_or(rest);
    #[cfg(target_os = "windows")]
    let path_str = path_str.strip_prefix('/').unwrap_or(path_str);
    Some(PathBuf::from(path_str))
}

/// Download a single URL (`https://`, `http://`, or `file://`) to `dst`.
/// Sends no credentials by default (only attaches `GITHUB_TOKEN` to trusted GitHub HTTPS hosts).
pub fn fetch_url_to_file(url: &str, dst: &Path) -> Result<(), String> {
    fetch_url_to_file_with_progress(url, dst, &mut |_| {})
}

/// Like [`fetch_url_to_file`], reporting the number of bytes written to `dst` so far (polled
/// ~10×/s while `curl` runs) so hosts can render a download progress bar.
pub fn fetch_url_to_file_with_progress(
    url: &str,
    dst: &Path,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<(), String> {
    if let Some(src_path) = parse_file_url(url) {
        if let Some(parent) = dst.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let n = fs::copy(&src_path, dst)
            .map_err(|e| format!("copy {} -> {}: {e}", src_path.display(), dst.display()))?;
        on_bytes(n);
        return Ok(());
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("unsupported URL scheme: {url}"));
    }
    if let Some(parent) = dst.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut cmd = Command::new("curl");
    configure_bg_command(&mut cmd);
    cmd.args([
        "-fsSL",
        "--connect-timeout",
        "15",
        "--proto",
        "=http,https",
        "--proto-redir",
        "=http,https",
        "-o",
    ])
    .arg(dst);

    let auth_cfg = if is_trusted_github_https(url) {
        std::env::var("GITHUB_TOKEN")
            .ok()
            .and_then(|tok| format_curl_auth(&tok))
    } else {
        None
    };
    if auth_cfg.is_some() {
        cmd.args(["-K", "-"]).stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    cmd.arg("--").arg(url);

    let mut child = cmd.spawn().map_err(|e| format!("spawn curl: {e}"))?;
    if let Some(payload) = auth_cfg {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
    }
    // Poll for completion while reporting bytes written so far. `-s` keeps curl's stderr limited
    // to the final error line, so the pipe cannot fill up and stall the child while we poll.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if let Ok(meta) = fs::metadata(dst) {
                    on_bytes(meta.len());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(format!("wait curl: {e}")),
        }
    };
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    if status.success() {
        if let Ok(meta) = fs::metadata(dst) {
            on_bytes(meta.len());
        }
        Ok(())
    } else {
        Err(format!(
            "curl failed ({status}) for {url}: {}",
            stderr.trim()
        ))
    }
}

fn is_trusted_github_https(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host_port.split(':').next().unwrap_or("");
    host.eq_ignore_ascii_case("github.com")
        || host.eq_ignore_ascii_case("api.github.com")
        || host.eq_ignore_ascii_case("objects.githubusercontent.com")
}

fn format_curl_auth(token: &str) -> Option<String> {
    let clean = token.trim();
    if clean.is_empty() || clean.contains(['\r', '\n', '"', '\\']) {
        return None;
    }
    Some(format!("header = \"Authorization: Bearer {clean}\"\n"))
}

/// Windows-safe startup/pre-mount garbage collector (#237).
///
/// MUST be called BEFORE opening any `RpkReader<File>` handles in `cache_dir`.
/// Deletes any `.rpk` file not referenced by `manifest` and any leftover `.downloading*` temp files.
/// Returns the number of deleted files.
pub fn gc_unreferenced_packs(cache_dir: &Path, manifest: &PackManifest) -> usize {
    let referenced: HashSet<&str> = manifest.referenced_files().into_iter().collect();
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let is_temp = name.contains(".downloading");
        let is_stale_rpk = name.ends_with(".rpk") && !referenced.contains(name);
        if is_temp || is_stale_rpk {
            if fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// Host-agnostic pack fetcher bound to a local `cache_dir` and resolved `PackManifest`.
#[derive(Clone, Debug)]
pub struct PackFetcher {
    pub cache_dir: PathBuf,
    pub manifest_url: String,
    pub manifest: PackManifest,
    /// Pack files whose SHA-256 was already verified this session (shared across clones), so
    /// cached packs are not re-hashed on every scene change.
    verified: Arc<Mutex<HashSet<String>>>,
}

impl PackFetcher {
    /// Fetch `pack-manifest.json` from `manifest_url`, optionally verify its SHA-256,
    /// persist it into `cache_dir/pack-manifest.json`, and run pre-mount GC.
    pub fn fetch_manifest(
        cache_dir: PathBuf,
        manifest_url: String,
        expected_manifest_sha256: Option<&str>,
    ) -> Result<Self, String> {
        fs::create_dir_all(&cache_dir)
            .map_err(|e| format!("create cache dir {}: {e}", cache_dir.display()))?;
        let tmp_man = cache_dir.join(format!("pack-manifest.json.downloading.{}", std::process::id()));
        fetch_url_to_file(&manifest_url, &tmp_man)?;
        if let Some(expected) = expected_manifest_sha256 {
            if !expected.is_empty() {
                let actual = sha256_file_streaming(&tmp_man)?;
                if !actual.eq_ignore_ascii_case(expected) {
                    let _ = fs::remove_file(&tmp_man);
                    return Err(format!(
                        "pack-manifest.json SHA-256 mismatch (got {actual}, expected {expected})"
                    ));
                }
            }
        }
        let raw = fs::read_to_string(&tmp_man)
            .map_err(|e| format!("read downloaded manifest: {e}"))?;
        let manifest = PackManifest::from_json_str(&raw)?;
        let final_man = cache_dir.join("pack-manifest.json");
        fs::rename(&tmp_man, &final_man)
            .map_err(|e| format!("commit {}: {e}", final_man.display()))?;
        let _ = gc_unreferenced_packs(&cache_dir, &manifest);
        Ok(Self {
            cache_dir,
            manifest_url,
            manifest,
            verified: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    /// Ensure a single pack is present in `cache_dir` and matches `desc.sha256`.
    /// Tries every URL in `manifest.resolve_pack_urls(pack_id, &self.manifest_url)` in order,
    /// rejecting any corrupted/tampered mirror and failing over to the next URL.
    pub fn ensure_pack(&self, pack_id: &str) -> Result<PathBuf, String> {
        self.ensure_pack_with_progress(pack_id, &mut |_| {})
    }

    fn size_matches(&self, pack_id: &str) -> bool {
        let Some(desc) = self.manifest.packs.get(pack_id) else {
            return false;
        };
        let p = self.cache_dir.join(&desc.file);
        p.is_file()
            && (desc.size == 0
                || fs::metadata(&p)
                    .map(|m| m.len() == desc.size)
                    .unwrap_or(false))
    }

    fn mark_verified(&self, file: &str) {
        if let Ok(mut v) = self.verified.lock() {
            v.insert(file.to_string());
        }
    }

    /// Cheap local check (no hashing): the pack file exists with the manifest size.
    pub fn is_pack_cached(&self, pack_id: &str) -> bool {
        self.size_matches(pack_id)
    }

    /// Manifest size in bytes of `pack_id` (0 if unknown).
    pub fn pack_size(&self, pack_id: &str) -> u64 {
        self.manifest
            .packs
            .get(pack_id)
            .map(|d| d.size)
            .unwrap_or(0)
    }

    /// Packs required for `scene_id` that are not cached locally yet.
    pub fn missing_scene_packs(&self, scene_id: u16, map_id: Option<u16>) -> Vec<String> {
        self.manifest
            .packs_for_scene(scene_id, map_id)
            .into_iter()
            .filter(|pid| !self.is_pack_cached(pid))
            .collect()
    }

    /// Every pack in the manifest that is not cached locally yet (for "download all").
    pub fn missing_packs(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .manifest
            .packs
            .keys()
            .filter(|pid| !pid.starts_with("dev-") && !self.is_pack_cached(pid))
            .cloned()
            .collect();
        ids.sort();
        ids
    }

    /// [`Self::ensure_pack`] reporting bytes downloaded so far for this pack via `on_bytes`.
    pub fn ensure_pack_with_progress(
        &self,
        pack_id: &str,
        on_bytes: &mut dyn FnMut(u64),
    ) -> Result<PathBuf, String> {
        let desc = self
            .manifest
            .packs
            .get(pack_id)
            .ok_or_else(|| format!("pack '{pack_id}' not found in manifest"))?;
        let final_path = self.cache_dir.join(&desc.file);

        // Fast path: already cached on disk & matches size + SHA-256 (hashed once per session).
        if final_path.is_file() {
            if self.size_matches(pack_id) {
                let already = self
                    .verified
                    .lock()
                    .map(|v| v.contains(&desc.file))
                    .unwrap_or(false);
                if already || desc.sha256.is_empty() {
                    return Ok(final_path);
                }
                if let Ok(actual) = sha256_file_streaming(&final_path) {
                    if actual.eq_ignore_ascii_case(&desc.sha256) {
                        self.mark_verified(&desc.file);
                        return Ok(final_path);
                    }
                }
            }
            let _ = fs::remove_file(&final_path);
        }

        let base = pal_runtime::rpk_manifest::manifest_base_url(&self.manifest_url);
        let urls = desc.resolve_urls(&base);
        if urls.is_empty() {
            return Err(format!("no download URLs resolved for pack '{pack_id}'"));
        }

        static NEXT_TMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let mut errors = Vec::new();
        for (idx, url) in urls.iter().enumerate() {
            let seq = NEXT_TMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let tmp_path = self.cache_dir.join(format!(
                "{}.downloading.{}.{}.{}",
                desc.file,
                std::process::id(),
                seq,
                idx
            ));
            let _ = fs::remove_file(&tmp_path);
            if let Err(e) = fetch_url_to_file_with_progress(url, &tmp_path, on_bytes) {
                let _ = fs::remove_file(&tmp_path);
                errors.push(format!("{url}: {e}"));
                continue;
            }
            if !desc.sha256.is_empty() {
                match sha256_file_streaming(&tmp_path) {
                    Ok(actual) if actual.eq_ignore_ascii_case(&desc.sha256) => {}
                    Ok(actual) => {
                        let _ = fs::remove_file(&tmp_path);
                        errors.push(format!(
                            "{url}: SHA-256 mismatch (got {actual}, expected {})",
                            desc.sha256
                        ));
                        continue;
                    }
                    Err(e) => {
                        let _ = fs::remove_file(&tmp_path);
                        errors.push(format!("{url}: hash error: {e}"));
                        continue;
                    }
                }
            }
            if let Err(e) = fs::rename(&tmp_path, &final_path) {
                let _ = fs::remove_file(&tmp_path);
                errors.push(format!("commit {}: {e}", final_path.display()));
                continue;
            }
            self.mark_verified(&desc.file);
            return Ok(final_path);
        }

        Err(format!(
            "failed to download pack '{pack_id}' from all {} URL(s):\n  - {}",
            urls.len(),
            errors.join("\n  - ")
        ))
    }

    /// Ensure all packs required for `scene_id` (and optional `map_id`) are cached locally.
    pub fn ensure_scene_packs(&self, scene_id: u16, map_id: Option<u16>) -> Result<(), String> {
        for pid in self.manifest.packs_for_scene(scene_id, map_id) {
            self.ensure_pack(&pid)?;
        }
        Ok(())
    }

    /// Fire-and-forget background prefetch of adjacent scenes (`manifest.prefetch[scene_id]`)
    /// so walking into the next room/map hits warm local cache with zero network wait.
    pub fn prefetch_adjacent_async(&self, scene_id: u16) {
        let key = scene_id.to_string();
        let Some(adj) = self.manifest.prefetch.get(&key) else {
            return;
        };
        let mut to_fetch = Vec::new();
        for pid in adj {
            if let Some(desc) = self.manifest.packs.get(pid) {
                let p = self.cache_dir.join(&desc.file);
                let cached = p.is_file()
                    && (desc.size == 0
                        || fs::metadata(&p).map(|m| m.len() == desc.size).unwrap_or(false));
                if !cached {
                    to_fetch.push(pid.clone());
                }
            }
        }
        if to_fetch.is_empty() {
            return;
        }
        let fetcher = self.clone();
        let _ = std::thread::Builder::new()
            .name(format!("repal-prefetch-{scene_id}"))
            .spawn(move || {
                for pid in to_fetch {
                    let _ = fetcher.ensure_pack(&pid);
                }
            });
    }

    /// Ensure the pack for `rng_id` is cached locally.
    pub fn ensure_rng_pack(&self, rng_id: u16) -> Result<(), String> {
        if let Some(pid) = self.manifest.pack_for_rng(rng_id) {
            self.ensure_pack(pid)?;
        }
        Ok(())
    }

    /// Optional "Download All for Offline" helper: downloads every pack in the manifest.
    #[allow(dead_code)]
    pub fn download_all_for_offline<F: FnMut(usize, usize, &str)>(
        &self,
        mut on_progress: F,
    ) -> Result<(), String> {
        let mut ids: Vec<&str> = self.manifest.packs.keys().map(String::as_str).collect();
        ids.sort_unstable();
        let total = ids.len();
        for (i, pid) in ids.iter().enumerate() {
            on_progress(i, total, pid);
            self.ensure_pack(pid)?;
        }
        on_progress(total, total, "done");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pal_runtime::crypto::sha256_hex;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;

    fn spawn_http_server(routes: HashMap<String, (u16, Vec<u8>)>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let routes = Arc::new(routes);
        let handle = thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(&stream);
                let mut req_line = String::new();
                if reader.read_line(&mut req_line).is_err() {
                    continue;
                }
                let path = req_line.split_whitespace().nth(1).unwrap_or("/").to_string();
                if path == "/__stop__" {
                    let mut s = stream;
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
                    break;
                }
                let (code, body) = routes
                    .get(&path)
                    .cloned()
                    .unwrap_or_else(|| (404, b"not found".to_vec()));
                let reason = if code == 200 { "OK" } else { "Not Found" };
                let header = format!(
                    "HTTP/1.1 {code} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let mut s = stream;
                let _ = s.write_all(header.as_bytes());
                let _ = s.write_all(&body);
            }
        });
        (format!("http://{addr}"), handle)
    }

    fn unique_tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("repal_pf_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn http_mirror_fallback_and_tamper_rejection() {
        let good_bytes = b"RPK1_VALID_PACK_PAYLOAD_BYTES".to_vec();
        let bad_bytes = b"RPK1_TAMPERED_EVIL_PAYLOAD!!!".to_vec();
        let good_sha = sha256_hex(&good_bytes);

        let manifest_json = serde_json::json!({
            "format": "repal-packs/1",
            "schema_version": 1,
            "pack_format_major": 1,
            "version": "test-v1",
            "boot": ["boot"],
            "packs": {
                "boot": {
                    "file": "boot.rpk",
                    "size": good_bytes.len(),
                    "sha256": good_sha,
                    "urls": ["missing/boot.rpk", "tampered/boot.rpk", "good/boot.rpk"]
                }
            }
        })
        .to_string();

        let mut routes = HashMap::new();
        routes.insert("/packs/pack-manifest.json".into(), (200, manifest_json.into_bytes()));
        routes.insert("/packs/tampered/boot.rpk".into(), (200, bad_bytes));
        routes.insert("/packs/good/boot.rpk".into(), (200, good_bytes.clone()));

        let (base, handle) = spawn_http_server(routes);
        let cache_dir = unique_tmp_dir("fallback");

        // Pre-populate a stale unreferenced .rpk and leftover .downloading file to verify GC!
        fs::write(cache_dir.join("old-stale.rpk"), b"stale").unwrap();
        fs::write(cache_dir.join("boot.rpk.downloading.999"), b"partial").unwrap();

        let fetcher = PackFetcher::fetch_manifest(
            cache_dir.clone(),
            format!("{base}/packs/pack-manifest.json"),
            None,
        )
        .expect("fetch manifest");

        // Pre-mount GC should have cleaned stale files!
        assert!(!cache_dir.join("old-stale.rpk").exists());
        assert!(!cache_dir.join("boot.rpk.downloading.999").exists());

        // ensure_pack should skip 404 mirror #1, reject tampered mirror #2, and commit mirror #3!
        let path = fetcher.ensure_pack("boot").expect("failover to good mirror");
        assert_eq!(fs::read(&path).unwrap(), good_bytes);

        // Shut down local test server cleanly.
        let _ = fetch_url_to_file(&format!("{base}/__stop__"), &cache_dir.join("stop.tmp"));
        let _ = handle.join();
        let _ = fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn file_url_fetch_and_gc_preserves_referenced() {
        let src_dir = unique_tmp_dir("file_src");
        let cache_dir = unique_tmp_dir("file_dst");
        let payload = b"LOCAL_FILE_RPK_PAYLOAD".to_vec();
        let sha = sha256_hex(&payload);
        fs::write(src_dir.join("boot.rpk"), &payload).unwrap();
        let manifest_json = serde_json::json!({
            "format": "repal-packs/1",
            "schema_version": 1,
            "pack_format_major": 1,
            "version": "file-v1",
            "boot": ["boot"],
            "packs": {
                "boot": {
                    "file": "boot.rpk",
                    "size": payload.len(),
                    "sha256": sha,
                    "urls": ["boot.rpk"]
                }
            }
        })
        .to_string();
        let man_path = src_dir.join("pack-manifest.json");
        fs::write(&man_path, manifest_json).unwrap();

        let url = format!("file://{}", man_path.display());
        let fetcher = PackFetcher::fetch_manifest(cache_dir.clone(), url, None).unwrap();
        let committed = fetcher.ensure_pack("boot").unwrap();
        assert_eq!(fs::read(&committed).unwrap(), payload);

        // Running GC again must preserve referenced boot.rpk
        fs::write(cache_dir.join("orphan.rpk"), b"orphan").unwrap();
        let removed = gc_unreferenced_packs(&cache_dir, &fetcher.manifest);
        assert_eq!(removed, 1);
        assert!(cache_dir.join("boot.rpk").is_file());
        assert!(!cache_dir.join("orphan.rpk").exists());

        let _ = fs::remove_dir_all(&src_dir);
        let _ = fs::remove_dir_all(&cache_dir);
    }
}
