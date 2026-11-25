//! Zero-dependency background self-updater for `pal-desktop`.
//!
//! Uses OS-native `curl` (and `tar` on Unix / `powershell Expand-Archive` on Windows)
//! to check `HclX/RePal` GitHub Releases, download only the compact native engine archive
//! (~3–5 MB, leaving local `assets/` and `saves/` untouched), atomically swap the running
//! executable, and relaunch into the new version.

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

const RELEASE_API_URL: &str = "https://api.github.com/repos/HclX/RePal/releases/latest";
const RELEASE_REPO: &str = "HclX/RePal";

/// Current build version (CI tagged builds inject `REPAL_RELEASE_VERSION`, e.g. `0.3.9`).
pub fn current_version() -> &'static str {
    option_env!("REPAL_RELEASE_VERSION")
        .filter(|s| !s.is_empty())
        .unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// Metadata describing an available GitHub Release update (#166).
#[derive(Debug, Clone)]
pub struct AvailableUpdate {
    pub version: String,
    pub asset_name: String,
    pub download_url: String,
    pub api_asset_url: Option<String>,
    pub sha256_asset_name: Option<String>,
    pub sha256_download_url: Option<String>,
    pub sha256_api_url: Option<String>,
}

#[derive(Debug, Clone)]
pub enum UpdateMsg {
    Available(AvailableUpdate),
    UpToDate {
        version: String,
    },
    Progress(String),
    ReadyToRestart {
        version: String,
        exe_path: PathBuf,
    },
    Error(String),
}

#[cfg(target_os = "windows")]
fn configure_bg_command(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_bg_command(_cmd: &mut Command) {}

/// Parse `v0.3.9` or `0.3.9-rc1` into `(major, minor, patch)`.
#[cfg(test)]
pub fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let (maj, min, pat, _, _) = parse_semver_full(s)?;
    Some((maj, min, pat))
}

/// Parse `v0.3.9` or `0.3.9-rc1` into `(major, minor, patch, is_stable, prerelease)`
/// so that SemVer prerelease precedence (`0.3.9-rc1 < 0.3.9`) holds (#266).
fn parse_semver_full(s: &str) -> Option<(u32, u32, u32, bool, String)> {
    let clean = s.trim().trim_start_matches(['v', 'V']);
    let without_build = clean.split('+').next()?;
    let (core, pre) = match without_build.split_once('-') {
        Some((c, p)) if !p.is_empty() => (c, Some(p)),
        _ => (without_build, None),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch, pre.is_none(), pre.unwrap_or("").to_string()))
}

/// Returns `true` if `remote` is strictly newer than `local`.
pub fn is_newer_version(remote: &str, local: &str) -> bool {
    match (parse_semver_full(remote), parse_semver_full(local)) {
        (Some(r), Some(l)) => r > l,
        _ => false,
    }
}

/// Allowlist GitHub release / asset CDN HTTPS URLs before handing them to `curl` (#266).
fn is_trusted_release_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host_port.split(':').next().unwrap_or("");
    host.eq_ignore_ascii_case("github.com")
        || host.eq_ignore_ascii_case("api.github.com")
        || host.eq_ignore_ascii_case("objects.githubusercontent.com")
        || host.eq_ignore_ascii_case("release-assets.githubusercontent.com")
}

/// RAII guard that removes `.repal_update_tmp` on every early return (#266).
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn platform_asset_name() -> Option<&'static str> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Some("repal-windows-x86_64.zip");
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Some("repal-macos-aarch64.tar.gz");
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Some("repal-linux-x86_64.tar.gz");
    }
    #[allow(unreachable_code)]
    None
}

fn platform_binary_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "repal.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "repal"
    }
}

/// Remove any leftover `repal.exe.old` created by a previous Windows self-update.
pub fn cleanup_old_binary() {
    if let Ok(exe) = env::current_exe() {
        let old = old_backup_path(&exe);
        if old.exists() {
            let _ = fs::remove_file(&old);
        }
    }
}

fn old_backup_path(exe: &Path) -> PathBuf {
    let mut p = exe.as_os_str().to_os_string();
    p.push(".old");
    PathBuf::from(p)
}

/// Split `curl -w "\n%{http_code}"` stdout into `(body, http_status_code)` (#166).
fn split_curl_body_and_status(raw: &[u8]) -> (&[u8], Option<u16>) {
    let trimmed_len = raw
        .iter()
        .rposition(|&b| b != b'\r' && b != b'\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let trimmed = &raw[..trimmed_len];
    if let Some(pos) = trimmed.iter().rposition(|&b| b == b'\n') {
        if let Ok(code) = std::str::from_utf8(&trimmed[pos + 1..])
            .unwrap_or("")
            .trim()
            .parse::<u16>()
        {
            return (&trimmed[..pos], Some(code));
        }
    } else if let Ok(code) = std::str::from_utf8(trimmed).unwrap_or("").trim().parse::<u16>() {
        return (&[], Some(code));
    }
    (raw, None)
}

fn curl_github_auth_cfg() -> Option<String> {
    let tok = env::var("GITHUB_TOKEN").ok()?;
    let clean = tok.trim();
    if clean.is_empty() || clean.contains(['\r', '\n', '"', '\\']) {
        return None;
    }
    Some(format!("header = \"Authorization: Bearer {clean}\"\n"))
}

/// Try fetching the latest release JSON via authenticated `gh api` first so private repos
/// work seamlessly whenever the user has run `gh auth login` (#166).
fn fetch_latest_release_via_gh() -> Option<serde_json::Value> {
    let mut cmd = Command::new("gh");
    configure_bg_command(&mut cmd);
    let out = cmd
        .args(["api", &format!("repos/{RELEASE_REPO}/releases/latest")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Spawn a non-blocking background thread to check GitHub Releases for a newer version.
/// If `silent_on_error` is true (boot check), offline/network failures produce no error toast.
pub fn spawn_update_check(silent_on_error: bool) -> Receiver<UpdateMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let Some(asset_name) = platform_asset_name() else {
            if !silent_on_error {
                let _ = tx.send(UpdateMsg::Error("当前平台暂不支持自动更新".into()));
            }
            return;
        };

        // 1. Prefer `gh api` (works automatically on private repos with `gh auth login`, #166).
        let doc = if let Some(v) = fetch_latest_release_via_gh() {
            v
        } else {
            // 2. Fallback to `curl` (with optional `GITHUB_TOKEN` via `-K -`).
            let auth_cfg = curl_github_auth_cfg();
            let mut cmd = Command::new("curl");
            configure_bg_command(&mut cmd);
            cmd.args([
                "-sSL",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--max-time",
                "8",
                "-w",
                "\n%{http_code}",
                "-H",
                "Accept: application/vnd.github+json",
                "-H",
                "User-Agent: RePal-Desktop",
            ]);
            if auth_cfg.is_some() {
                cmd.args(["-K", "-"]);
            }
            cmd.arg("--")
                .arg(RELEASE_API_URL)
                .stdin(if auth_cfg.is_some() { Stdio::piped() } else { Stdio::null() })
                .stdout(Stdio::piped())
                .stderr(Stdio::null());

            let Ok(mut child) = cmd.spawn() else {
                if !silent_on_error {
                    let _ = tx.send(UpdateMsg::Error("无法调用 curl 检查更新".into()));
                }
                return;
            };
            if let Some(cfg) = auth_cfg {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(cfg.as_bytes());
                }
            }
            let Ok(out) = child.wait_with_output() else {
                if !silent_on_error {
                    let _ = tx.send(UpdateMsg::Error("无法调用 curl 检查更新".into()));
                }
                return;
            };
            if !out.status.success() {
                if !silent_on_error {
                    let _ = tx.send(UpdateMsg::Error("检查更新失败（网络不可达）".into()));
                }
                return;
            }
            let (body, http_code) = split_curl_body_and_status(&out.stdout);
            match http_code {
                Some(200) => {}
                Some(code @ (401 | 404)) => {
                    if !silent_on_error {
                        let _ = tx.send(UpdateMsg::Error(format!(
                            "无法访问发布仓库：请运行 gh auth login 或设置 GITHUB_TOKEN (HTTP {code})"
                        )));
                    }
                    return;
                }
                Some(403 | 429) => {
                    if !silent_on_error {
                        let _ = tx.send(UpdateMsg::Error(format!(
                            "GitHub API 限流，请稍后再试 (HTTP {})",
                            http_code.unwrap()
                        )));
                    }
                    return;
                }
                Some(code) => {
                    if !silent_on_error {
                        let _ = tx.send(UpdateMsg::Error(format!("检查更新失败 (HTTP {code})")));
                    }
                    return;
                }
                None => {
                    if !silent_on_error {
                        let _ = tx.send(UpdateMsg::Error("检查更新失败（网络不可达）".into()));
                    }
                    return;
                }
            }
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
                if !silent_on_error {
                    let _ = tx.send(UpdateMsg::Error("解析版本信息失败".into()));
                }
                return;
            };
            v
        };

        let Some(tag) = doc["tag_name"].as_str() else {
            return;
        };
        let local = current_version();
        if !is_newer_version(tag, local) {
            if !silent_on_error {
                let _ = tx.send(UpdateMsg::UpToDate {
                    version: format!("v{}", local.trim_start_matches('v')),
                });
            }
            return;
        }
        let assets = doc["assets"].as_array();
        let found_asset = assets.and_then(|arr| {
            arr.iter().find(|a| a["name"].as_str() == Some(asset_name))
        });
        let Some(asset_obj) = found_asset else {
            if !silent_on_error {
                let _ = tx.send(UpdateMsg::Error(format!(
                    "发现 {tag}，但未找到 {asset_name}"
                )));
            }
            return;
        };
        let Some(download_url) = asset_obj["browser_download_url"].as_str().map(String::from) else {
            return;
        };
        let api_asset_url = asset_obj["url"].as_str().map(String::from);

        // Look for `<asset_name>.sha256` sidecar or `SHA256SUMS` manifest for integrity check (#166).
        let sidecar_name = format!("{asset_name}.sha256");
        let sha_obj = assets.and_then(|arr| {
            arr.iter()
                .find(|a| a["name"].as_str() == Some(sidecar_name.as_str()))
                .or_else(|| arr.iter().find(|a| a["name"].as_str() == Some("SHA256SUMS")))
        });
        let sha256_asset_name = sha_obj.and_then(|a| a["name"].as_str()).map(String::from);
        let sha256_download_url = sha_obj
            .and_then(|a| a["browser_download_url"].as_str())
            .map(String::from);
        let sha256_api_url = sha_obj.and_then(|a| a["url"].as_str()).map(String::from);

        let _ = tx.send(UpdateMsg::Available(AvailableUpdate {
            version: tag.to_string(),
            asset_name: asset_name.to_string(),
            download_url,
            api_asset_url,
            sha256_asset_name,
            sha256_download_url,
            sha256_api_url,
        }));
    });
    rx
}

/// Download a release asset into `dst_path`, preferring `gh release download` (handles private repos),
/// then authenticated GitHub API asset download (`Accept: application/octet-stream` + `GITHUB_TOKEN`),
/// then anonymous `browser_download_url` (#166).
fn download_release_asset(
    tag: &str,
    asset_name: &str,
    api_url: Option<&str>,
    browser_url: &str,
    dst_path: &Path,
) -> bool {
    // 1. Try `gh release download` into a temporary staging dir.
    if let Some(parent) = dst_path.parent() {
        let gh_dir = parent.join(".gh_dl");
        let _ = fs::remove_dir_all(&gh_dir);
        if fs::create_dir_all(&gh_dir).is_ok() {
            let mut gh = Command::new("gh");
            configure_bg_command(&mut gh);
            let ok = gh
                .args([
                    "release",
                    "download",
                    tag,
                    "--repo",
                    RELEASE_REPO,
                    "--pattern",
                    asset_name,
                    "--dir",
                ])
                .arg(&gh_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            let downloaded = gh_dir.join(asset_name);
            if ok && downloaded.is_file() && fs::rename(&downloaded, dst_path).is_ok() {
                let _ = fs::remove_dir_all(&gh_dir);
                return true;
            }
            let _ = fs::remove_dir_all(&gh_dir);
        }
    }

    // 2. Fallback to `curl`: use GitHub API asset endpoint with token via `-K -` if available.
    let auth_cfg = curl_github_auth_cfg();
    let target_url = if auth_cfg.is_some() {
        api_url.unwrap_or(browser_url)
    } else {
        browser_url
    };
    if !is_trusted_release_url(target_url) {
        return false;
    }
    let mut dl = Command::new("curl");
    configure_bg_command(&mut dl);
    dl.args([
        "-fSL",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--retry",
        "2",
        "--max-time",
        "120",
        "-H",
        "User-Agent: RePal-Desktop",
    ]);
    if auth_cfg.is_some() && api_url.is_some() {
        dl.args(["-H", "Accept: application/octet-stream"]);
    }
    if auth_cfg.is_some() {
        dl.args(["-K", "-"]);
    }
    dl.arg("-o")
        .arg(dst_path)
        .arg("--")
        .arg(target_url)
        .stdin(if auth_cfg.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = dl.spawn() else {
        return false;
    };
    if let Some(cfg) = auth_cfg {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(cfg.as_bytes());
        }
    }
    child.wait().map(|s| s.success()).unwrap_or(false) && dst_path.is_file()
}

/// Download, verify, extract, and atomically swap the current `repal` executable in the background.
pub fn spawn_self_update(info: AvailableUpdate) -> Receiver<UpdateMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let res = perform_self_update(&info, &tx);
        if let Err(err) = res {
            let _ = tx.send(UpdateMsg::Error(err));
        }
    });
    rx
}

/// Require a published checksum sidecar (`*.sha256` or `SHA256SUMS`) before installing updates (#236).
fn require_update_checksum<'a>(info: &'a AvailableUpdate) -> Result<(&'a str, &'a str), String> {
    match (
        info.sha256_asset_name.as_deref(),
        info.sha256_download_url.as_deref(),
    ) {
        (Some(name), Some(url)) if !name.is_empty() && !url.is_empty() => Ok((name, url)),
        _ => Err(format!(
            "发布版本 {} 未提供 SHA-256 校验文件，已拒绝自动更新（防篡改保护）",
            info.version
        )),
    }
}

fn perform_self_update(
    info: &AvailableUpdate,
    tx: &mpsc::Sender<UpdateMsg>,
) -> Result<(), String> {
    // Fail closed if the release lacks a checksum sidecar (#236).
    let (sha_name, sha_url) = require_update_checksum(info)?;

    let version = &info.version;
    let cur_exe = env::current_exe().map_err(|e| format!("无法定位当前程序: {e}"))?;
    let exe_dir = cur_exe
        .parent()
        .ok_or_else(|| "无法获取程序目录".to_string())?;
    let tmp_dir = exe_dir.join(".repal_update_tmp");
    if tmp_dir.exists() {
        let _ = fs::remove_dir_all(&tmp_dir);
    }
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("创建临时目录失败: {e}"))?;
    let _tmp_guard = TempDirGuard(tmp_dir.clone());

    let is_zip = info.asset_name.ends_with(".zip");
    let archive_path = tmp_dir.join(if is_zip { "update.zip" } else { "update.tar.gz" });

    let _ = tx.send(UpdateMsg::Progress(format!("正在下载 {version}...")));
    if !download_release_asset(
        version,
        &info.asset_name,
        info.api_asset_url.as_deref(),
        &info.download_url,
        &archive_path,
    ) {
        return Err("下载更新包失败（如为私有仓库请先运行 gh auth login 或设置 GITHUB_TOKEN）".into());
    }

    // Mandatory SHA-256 integrity verification against the release checksum sidecar (#166, #236).
    let _ = tx.send(UpdateMsg::Progress(format!("正在校验 {version} SHA-256...")));
    let sha_path = tmp_dir.join("checksum.sha256");
    if !download_release_asset(
        version,
        sha_name,
        info.sha256_api_url.as_deref(),
        sha_url,
        &sha_path,
    ) {
        return Err("下载 SHA-256 校验文件失败".into());
    }
    let manifest = fs::read_to_string(&sha_path)
        .map_err(|e| format!("读取校验文件失败: {e}"))?;
    let Some(expected) = parse_sha256_for_asset(&manifest, &info.asset_name) else {
        return Err(format!("校验文件中未找到 {}", info.asset_name));
    };
    let actual = sha256_file_hex(&archive_path).map_err(|e| format!("读取更新包失败: {e}"))?;
    if actual != expected {
        return Err(format!(
            "更新包 SHA-256 校验失败（期望 {expected}，实际 {actual}）"
        ));
    }

    let _ = tx.send(UpdateMsg::Progress(format!("正在解压 {version}...")));
    let extract_dir = tmp_dir.join("extracted");
    fs::create_dir_all(&extract_dir).map_err(|e| format!("创建解压目录失败: {e}"))?;

    let extract_ok = if is_zip {
        #[cfg(target_os = "windows")]
        {
            let src_esc = crate::os_bridge::ps_escape_single_quoted(&archive_path);
            let dst_esc = crate::os_bridge::ps_escape_single_quoted(&extract_dir);
            let mut ps = Command::new("powershell");
            configure_bg_command(&mut ps);
            ps.args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Expand-Archive -LiteralPath '{src_esc}' -DestinationPath '{dst_esc}' -Force"
                ),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    } else {
        let mut tar = Command::new("tar");
        configure_bg_command(&mut tar);
        tar.arg("-xzf")
            .arg(&archive_path)
            .arg("-C")
            .arg(&extract_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };

    if !extract_ok {
        return Err("解压更新包失败".into());
    }

    let bin_name = platform_binary_name();
    let new_exe = find_file_recursive(&extract_dir, bin_name)
        .ok_or_else(|| format!("更新包内未找到 {bin_name}"))?;
    let new_lock = find_file_recursive(&extract_dir, "assets-lock.json");

    // Swap executable FIRST so a failed swap never leaves the old binary paired with a new assets-lock.json (#166).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&new_exe, fs::Permissions::from_mode(0o755));
        fs::rename(&new_exe, &cur_exe).map_err(|e| format!("替换可执行文件失败: {e}"))?;
    }
    #[cfg(windows)]
    {
        let old_exe = old_backup_path(&cur_exe);
        let _ = fs::remove_file(&old_exe);
        fs::rename(&cur_exe, &old_exe).map_err(|e| format!("备份旧程序失败: {e}"))?;
        if let Err(e) = fs::rename(&new_exe, &cur_exe) {
            let _ = fs::rename(&old_exe, &cur_exe);
            return Err(format!("替换新程序失败: {e}"));
        }
    }

    // Copy updated assets-lock.json only AFTER the binary swap has succeeded (#166).
    if let Some(lock_src) = new_lock {
        let _ = fs::copy(&lock_src, exe_dir.join("assets-lock.json"));
    }

    let _ = fs::remove_dir_all(&tmp_dir);
    let _ = tx.send(UpdateMsg::ReadyToRestart {
        version: version.to_string(),
        exe_path: cur_exe,
    });
    Ok(())
}

/// Parse an expected lowercase 64-hex SHA-256 digest for `asset_name` from a `SHA256SUMS` or
/// `<asset>.sha256` manifest (`<hex>  <filename>` or bare `<hex>`).
fn parse_sha256_for_asset(manifest: &str, asset_name: &str) -> Option<String> {
    let mut single_line_hex: Option<String> = None;
    let mut non_empty = 0usize;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        non_empty += 1;
        let mut parts = trimmed.split_whitespace();
        let hex = parts.next()?.to_ascii_lowercase();
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        if let Some(name) = parts.next() {
            let clean = name.trim_start_matches('*');
            let base = clean.rsplit(['/', '\\']).next().unwrap_or(clean);
            if base == asset_name {
                return Some(hex);
            }
        } else {
            single_line_hex = Some(hex);
        }
    }
    if non_empty == 1 {
        single_line_hex
    } else {
        None
    }
}

/// Streaming zero-dependency RFC 6234 SHA-256 hasher (#166, #266).
struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    fn new() -> Self {
        Self {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
            ],
            buf: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    fn compress(&mut self, chunk: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(Self::K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        let add = [a, b, c, d, e, f, g, hh];
        for i in 0..8 {
            self.h[i] = self.h[i].wrapping_add(add[i]);
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            let take = need.min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
        let mut chunks = data.chunks_exact(64);
        for chunk in &mut chunks {
            let mut block = [0u8; 64];
            block.copy_from_slice(chunk);
            self.compress(&block);
        }
        let rem = chunks.remainder();
        if !rem.is_empty() {
            self.buf[..rem.len()].copy_from_slice(rem);
            self.buf_len = rem.len();
        }
    }

    fn finalize_hex(mut self) -> String {
        let bit_len = self.total_len.wrapping_mul(8);
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;
        if self.buf_len > 56 {
            self.buf[self.buf_len..64].fill(0);
            let block = self.buf;
            self.compress(&block);
            self.buf_len = 0;
        }
        self.buf[self.buf_len..56].fill(0);
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        self.compress(&block);

        let mut out = String::with_capacity(64);
        for word in self.h {
            use std::fmt::Write as _;
            let _ = write!(&mut out, "{word:08x}");
        }
        out
    }
}

/// Zero-dependency RFC 6234 SHA-256 returning lowercase 64-character hex (#166).
#[cfg(test)]
fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize_hex()
}

/// Stream-hash a file on disk using pure-Rust SHA-256 (avoids OS shell-outs & big allocations, #266).
pub(crate) fn sha256_file_hex(path: &Path) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| format!("open {} failed: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {} failed: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize_hex())
}

fn find_file_recursive(dir: &Path, target_name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_file() && p.file_name().and_then(|n| n.to_str()) == Some(target_name) {
            return Some(p);
        }
        if p.is_dir() {
            if let Some(found) = find_file_recursive(&p, target_name) {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semver_comparison() {
        assert!(is_newer_version("v0.3.10", "0.3.9"));
        assert!(is_newer_version("v0.4.0", "v0.3.9"));
        assert!(is_newer_version("v1.0.0", "0.9.9"));
        assert!(!is_newer_version("v0.3.9", "0.3.9"));
        assert!(!is_newer_version("v0.3.8", "0.3.9"));
        assert!(is_newer_version("v0.3.9", "0.3.9-rc1"));
        assert!(!is_newer_version("v0.3.9-rc1", "0.3.9"));
        assert!(is_newer_version("v0.3.9-rc2", "0.3.9-rc1"));
        assert_eq!(parse_semver("v0.3.9-beta1"), Some((0, 3, 9)));
        assert!(is_trusted_release_url("https://github.com/HclX/RePal/releases/download/v0.3.11/a.tar.gz"));
        assert!(is_trusted_release_url("https://release-assets.githubusercontent.com/abc"));
        assert!(!is_trusted_release_url("http://github.com/HclX/RePal"));
        assert!(!is_trusted_release_url("https://evil.example/a.tar.gz"));
    }

    #[test]
    fn test_split_curl_body_and_status() {
        let (body, code) = split_curl_body_and_status(b"{\"tag_name\":\"v1.0.0\"}\n200");
        assert_eq!(body, b"{\"tag_name\":\"v1.0.0\"}");
        assert_eq!(code, Some(200));

        let (body404, code404) = split_curl_body_and_status(b"{\"message\":\"Not Found\"}\n404\n");
        assert_eq!(body404, b"{\"message\":\"Not Found\"}");
        assert_eq!(code404, Some(404));

        let (empty, code0) = split_curl_body_and_status(b"000");
        assert!(empty.is_empty());
        assert_eq!(code0, Some(0));
    }

    #[test]
    fn test_sha256_rfc6234_vectors_and_manifest_parser() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let sums = "\
ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  repal-linux-x86_64.tar.gz\n\
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  repal-windows-x86_64.zip\n";
        assert_eq!(
            parse_sha256_for_asset(sums, "repal-linux-x86_64.tar.gz").as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(
            parse_sha256_for_asset(sums, "repal-windows-x86_64.zip").as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(parse_sha256_for_asset(sums, "other.tar.gz"), None);
    }

    #[test]
    fn test_require_update_checksum_fails_closed() {
        let missing = AvailableUpdate {
            version: "v0.3.11".into(),
            asset_name: "repal-linux-x86_64.tar.gz".into(),
            download_url: "https://example.invalid/repal.tar.gz".into(),
            api_asset_url: None,
            sha256_asset_name: None,
            sha256_download_url: None,
            sha256_api_url: None,
        };
        assert!(require_update_checksum(&missing).is_err());

        let present = AvailableUpdate {
            sha256_asset_name: Some("repal-linux-x86_64.tar.gz.sha256".into()),
            sha256_download_url: Some("https://example.invalid/repal.tar.gz.sha256".into()),
            ..missing
        };
        assert_eq!(
            require_update_checksum(&present),
            Ok((
                "repal-linux-x86_64.tar.gz.sha256",
                "https://example.invalid/repal.tar.gz.sha256"
            ))
        );
    }
}
