//! OS integration helpers: folder opener, native file picker, and background `gh issue create`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

/// Escape a filesystem path for inclusion inside a single-quoted PowerShell string literal.
#[cfg(any(target_os = "windows", test))]
pub(crate) fn ps_escape_single_quoted(path: &Path) -> String {
    path.display().to_string().replace('\'', "''")
}

/// Escape an arbitrary (possibly multi-line) string for inclusion inside a single-quoted
/// PowerShell string literal.
#[cfg(any(target_os = "windows", test))]
pub(crate) fn ps_escape_single_quoted_text(text: &str) -> String {
    text.replace('\'', "''")
        .replace('\r', "")
        .replace('\n', "' + [Environment]::NewLine + '")
}

/// Open `dir` in the host OS file manager (creating it first if missing).
pub(crate) fn open_folder_in_os(dir: &Path) {
    let _ = fs::create_dir_all(dir);
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(dir).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
}

/// Show a native OS open-file dialog for `.repal` / `.json` / `.rpg` / `.cmd` files.
pub(crate) fn run_os_file_picker() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("powershell");
        cmd.creation_flags(0x0800_0000);
        let out = cmd
            .args([
                "-NoProfile",
                "-STA",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; \
                 $f = New-Object System.Windows.Forms.OpenFileDialog; \
                 $f.Title = 'Open RePal Replay or Save File'; \
                 $f.Filter = 'RePal Replay / Save (*.repal;*.json;*.rpg;*.cmd;*.txt)|*.repal;*.json;*.rpg;*.cmd;*.txt|All Files (*.*)|*.*'; \
                 if ($f.ShowDialog() -eq 'OK') { $f.FileName }",
            ])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return (!s.is_empty()).then(|| PathBuf::from(s));
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("osascript")
            .args([
                "-e",
                "POSIX path of (choose file with prompt \"Open RePal Replay (.repal/.cmd/.txt) or Save (.json/.rpg)\")",
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return (!s.is_empty()).then(|| PathBuf::from(s));
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(out) = std::process::Command::new("zenity")
            .args([
                "--file-selection",
                "--title=Open RePal Replay (.repal/.json/.cmd) or Save (.json/.rpg)",
                "--file-filter=RePal Files (*.repal *.json *.rpg *.cmd *.txt) | *.repal *.json *.rpg *.cmd *.txt",
                "--file-filter=All Files | *",
            ])
            .output()
        {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return Some(PathBuf::from(s));
                }
            }
        }
        if let Ok(out) = std::process::Command::new("kdialog")
            .args([
                "--getopenfilename",
                ".",
                "*.repal *.json *.rpg *.cmd *.txt | RePal Replay or Save",
            ])
            .output()
        {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return Some(PathBuf::from(s));
                }
            }
        }
        None
    }
}

/// Show a native OS password-entry dialog prompting the user for the 64-hex encrypted asset pack
/// key. Returns `Some(key_hex)` on OK, or `None` on Cancel / unavailable dialog backend.
pub(crate) fn run_os_pack_key_prompt(prompt: &str) -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let escaped = ps_escape_single_quoted_text(prompt);
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; \
             Add-Type -AssemblyName System.Drawing; \
             $f = New-Object System.Windows.Forms.Form; \
             $f.Text = 'RePal — Asset Pack Key'; \
             $f.ClientSize = New-Object System.Drawing.Size(480, 168); \
             $f.StartPosition = 'CenterScreen'; \
             $f.FormBorderStyle = 'FixedDialog'; \
             $f.MaximizeBox = $false; \
             $f.MinimizeBox = $false; \
             $f.TopMost = $true; \
             $lbl = New-Object System.Windows.Forms.Label; \
             $lbl.Text = '{escaped}'; \
             $lbl.Location = New-Object System.Drawing.Point(16, 14); \
             $lbl.Size = New-Object System.Drawing.Size(448, 56); \
             $tb = New-Object System.Windows.Forms.TextBox; \
             $tb.Location = New-Object System.Drawing.Point(16, 78); \
             $tb.Size = New-Object System.Drawing.Size(448, 24); \
             $tb.UseSystemPasswordChar = $true; \
             $ok = New-Object System.Windows.Forms.Button; \
             $ok.Text = 'OK'; \
             $ok.Location = New-Object System.Drawing.Point(296, 120); \
             $ok.Size = New-Object System.Drawing.Size(80, 28); \
             $ok.DialogResult = [System.Windows.Forms.DialogResult]::OK; \
             $cancel = New-Object System.Windows.Forms.Button; \
             $cancel.Text = 'Cancel'; \
             $cancel.Location = New-Object System.Drawing.Point(384, 120); \
             $cancel.Size = New-Object System.Drawing.Size(80, 28); \
             $cancel.DialogResult = [System.Windows.Forms.DialogResult]::Cancel; \
             $f.Controls.AddRange(@($lbl, $tb, $ok, $cancel)); \
             $f.AcceptButton = $ok; \
             $f.CancelButton = $cancel; \
             $f.Add_Shown({{ $f.Activate(); $tb.Focus() }}); \
             if ($f.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ [Console]::Out.Write($tb.Text) }}"
        );
        let mut cmd = std::process::Command::new("powershell");
        cmd.creation_flags(0x0800_0000);
        let out = cmd
            .args(["-NoProfile", "-STA", "-Command", &script])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return (!s.is_empty()).then_some(s);
    }
    #[cfg(target_os = "macos")]
    {
        let script = "on run argv\n\
                      text returned of (display dialog (item 1 of argv) with title \"RePal — Asset Pack Key\" default answer \"\" with hidden answer)\n\
                      end run";
        let out = std::process::Command::new("osascript")
            .args(["-e", script, "--", prompt])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return (!s.is_empty()).then_some(s);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        match std::process::Command::new("zenity")
            .args([
                "--entry",
                "--hide-text",
                "--title=RePal — Asset Pack Key",
                &format!("--text={prompt}"),
                "--width=440",
            ])
            .output()
        {
            Ok(out) => {
                if !out.status.success() {
                    return None;
                }
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                return (!s.is_empty()).then_some(s);
            }
            Err(_) => {}
        }
        if let Ok(out) = std::process::Command::new("kdialog")
            .args(["--password", prompt, "--title", "RePal — Asset Pack Key"])
            .output()
        {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                return (!s.is_empty()).then_some(s);
            }
        }
        None
    }
}

/// Validate and persist the 64-hex master pack key to `<user_dir>/pack.key` (mode `0600` on Unix).
pub(crate) fn save_pack_key_file(user_dir: &Path, key_hex: &str) -> Result<PathBuf, String> {
    let trimmed = key_hex.trim();
    let _ = pal_runtime::crypto::parse_key_hex(trimmed)?;
    fs::create_dir_all(user_dir)
        .map_err(|e| format!("create {}: {e}", user_dir.display()))?;
    let path = user_dir.join("pack.key");
    fs::write(&path, format!("{}\n", trimmed.to_ascii_lowercase()))
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

/// Submit a GitHub Issue on `HclX/RePal` via `gh issue create` in a background thread.
/// When `screenshot_png` is provided, attaches it inline via `gh issue create --attach` (#167),
/// automatically falling back to without `--attach` if an older `gh` CLI does not support it.
pub(crate) fn spawn_gh_issue_create(
    title: String,
    body: String,
    screenshot_png: Option<PathBuf>,
) -> Receiver<Result<String, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let run_gh = |attach: Option<&Path>| {
            let mut cmd = std::process::Command::new("gh");
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000);
            }
            cmd.args([
                "issue",
                "create",
                "--repo",
                "HclX/RePal",
                "--title",
                &title,
                "--body",
                &body,
            ]);
            if let Some(png) = attach {
                if png.is_file() {
                    cmd.arg("--attach")
                        .arg(format!("{}#screenshot", png.display()));
                }
            }
            cmd.output()
        };

        let mut res = run_gh(screenshot_png.as_deref());
        // Graceful fallback if an older `gh` binary doesn't support `--attach`.
        if let Ok(ref out) = res {
            if !out.status.success() && screenshot_png.is_some() {
                let err = String::from_utf8_lossy(&out.stderr);
                if err.contains("unknown flag") && err.contains("attach") {
                    res = run_gh(None);
                }
            }
        }
        let msg = match res {
            Ok(out) if out.status.success() => {
                let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
                eprintln!("[repal] GitHub Issue created: {url}");
                Ok(url)
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                eprintln!("[repal] gh issue create skipped: {err}");
                Err(err)
            }
            Err(e) => {
                eprintln!("[repal] gh not available for auto bug report: {e}");
                Err(e.to_string())
            }
        };
        let _ = tx.send(msg);
    });
    rx
}

/// Sanitize a local filesystem path before including it in a public/remote GitHub issue body
/// so local OS usernames are never leaked (#167).
pub(crate) fn sanitize_local_path_for_issue(path: &Path) -> String {
    let raw = path.display().to_string();
    for var in ["HOME", "USERPROFILE"] {
        if let Ok(home) = std::env::var(var) {
            if !home.is_empty() && raw.starts_with(&home) {
                return format!("~{}", &raw[home.len()..]);
            }
        }
    }
    path.file_name()
        .and_then(|s| s.to_str())
        .map(String::from)
        .unwrap_or(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_single_quote_escaping_doubles_quotes() {
        let p = Path::new("C:\\Users\\O'Brien\\RePal\\update.zip");
        assert_eq!(
            ps_escape_single_quoted(p),
            "C:\\Users\\O''Brien\\RePal\\update.zip"
        );
    }

    #[test]
    fn sanitize_local_path_hides_home_dir() {
        if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(&home).join(".local/share/RePal/bug_reports/test.repal");
            let s = sanitize_local_path_for_issue(&p);
            assert_eq!(s, "~/.local/share/RePal/bug_reports/test.repal");
            assert!(!s.contains(&home));
        }
        let other = Path::new("/var/tmp/repal_bug_sc1.repal");
        assert_eq!(sanitize_local_path_for_issue(other), "repal_bug_sc1.repal");
    }

    #[test]
    fn ps_escape_single_quoted_text_handles_quotes_and_newlines() {
        assert_eq!(
            ps_escape_single_quoted_text("Can't decrypt\nTry again"),
            "Can''t decrypt' + [Environment]::NewLine + 'Try again"
        );
    }

    #[test]
    fn save_pack_key_file_validates_writes_and_loads_back() {
        let dir = std::env::temp_dir().join(format!(
            "repal_pack_key_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Invalid hex must be rejected without creating pack.key.
        assert!(save_pack_key_file(&dir, "not-a-valid-key").is_err());
        assert!(!dir.join("pack.key").exists());

        let valid_hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let saved = save_pack_key_file(&dir, &format!("  {}  ", valid_hex.to_uppercase()))
            .expect("save valid pack key");
        assert_eq!(saved, dir.join("pack.key"));
        let content = fs::read_to_string(&saved).unwrap();
        assert_eq!(content.trim(), valid_hex);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&saved).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
