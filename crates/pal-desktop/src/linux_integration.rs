//! Linux desktop integration: install/refresh a `repal.desktop` launcher and the app icon in the
//! user's XDG data dir on GUI start, so file managers, menus, docks and the taskbar show the RePal
//! icon (ELF binaries cannot embed icons). Replaces running `install-desktop-icon.sh` by hand.
//!
//! Only files RePal manages (marked `X-RePal-Managed=true`) are ever rewritten; a user-edited
//! launcher without the marker is left alone. Set `REPAL_NO_DESKTOP_INTEGRATION=1` to opt out.

use std::fs;
use std::path::{Path, PathBuf};

const MARKER: &str = "X-RePal-Managed=true";

fn data_home() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(x));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|h| PathBuf::from(h).join(".local").join("share"))
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    if fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp-repal");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(true)
}

/// Desktop entry text for the given executable and icon paths.
pub(crate) fn desktop_entry(exe: &Path, icon: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=RePal — 仙剑奇侠传 HD\n\
         Comment=HD remake of the 1995 Chinese Paladin\n\
         Exec=\"{exe}\"\n\
         Path={dir}\n\
         Icon={icon}\n\
         Terminal=false\n\
         Categories=Game;RolePlaying;\n\
         StartupWMClass=repal\n\
         {MARKER}\n",
        exe = exe.display(),
        dir = exe
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        icon = icon.display(),
    )
}

/// Install or refresh the launcher + icon under `data_home` (no-op when already up to date).
pub(crate) fn install_into(data_home: &Path, exe: &Path, icon_png: &[u8]) -> std::io::Result<bool> {
    let icon_path = data_home.join("icons/hicolor/256x256/apps/repal.png");
    let desktop_path = data_home.join("applications/repal.desktop");
    if let Ok(existing) = fs::read_to_string(&desktop_path) {
        if !existing.contains(MARKER) {
            return Ok(false); // user-managed launcher: never overwrite
        }
    }
    let mut changed = write_if_changed(&icon_path, icon_png)?;
    changed |= write_if_changed(&desktop_path, desktop_entry(exe, &icon_path).as_bytes())?;
    Ok(changed)
}

/// Best-effort install on GUI start; failures are logged, never fatal.
pub(crate) fn ensure_desktop_integration(icon_png: &[u8]) {
    if std::env::var_os("REPAL_NO_DESKTOP_INTEGRATION").is_some_and(|v| !v.is_empty() && v != "0") {
        return;
    }
    let (Some(home), Ok(exe)) = (data_home(), std::env::current_exe()) else {
        return;
    };
    let exe = exe.canonicalize().unwrap_or(exe);
    // Don't register throwaway dev builds (cargo target dirs) as the system launcher.
    if exe.components().any(|c| c.as_os_str() == "target")
        && exe.to_string_lossy().contains("/release/")
    {
        if std::env::var_os("REPAL_FORCE_DESKTOP_INTEGRATION").is_none() {
            return;
        }
    }
    match install_into(&home, &exe, icon_png) {
        Ok(true) => eprintln!(
            "[repal] installed desktop launcher + icon under {}",
            home.display()
        ),
        Ok(false) => {}
        Err(e) => eprintln!("[repal] desktop integration skipped: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_refreshes_and_respects_user_launchers() {
        let root = std::env::temp_dir().join(format!("repal_xdg_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let exe = PathBuf::from("/opt/repal/repal");
        assert!(install_into(&root, &exe, b"PNG1").unwrap());
        let desk = fs::read_to_string(root.join("applications/repal.desktop")).unwrap();
        assert!(desk.contains("Exec=\"/opt/repal/repal\"") && desk.contains(MARKER));
        assert_eq!(
            fs::read(root.join("icons/hicolor/256x256/apps/repal.png")).unwrap(),
            b"PNG1"
        );
        // Up to date -> no rewrite; moved binary -> Exec refreshed.
        assert!(!install_into(&root, &exe, b"PNG1").unwrap());
        assert!(install_into(&root, Path::new("/home/u/RePal/repal"), b"PNG1").unwrap());
        // User-edited launcher without the marker is never touched.
        fs::write(
            root.join("applications/repal.desktop"),
            "[Desktop Entry]\nName=Mine\n",
        )
        .unwrap();
        assert!(!install_into(&root, &exe, b"PNG2").unwrap());
        assert!(fs::read_to_string(root.join("applications/repal.desktop"))
            .unwrap()
            .contains("Mine"));
        let _ = fs::remove_dir_all(&root);
    }
}
