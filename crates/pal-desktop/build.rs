//! Zero-dependency Windows icon resource embedder for `repal.exe`.
//! On non-Windows targets this build script is a no-op.

fn main() {
    println!("cargo:rerun-if-changed=../../site/icons/repal_icon.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico_path = manifest_dir.join("../../site/icons/repal_icon.ico");
    if !ico_path.is_file() {
        return;
    }

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc_path = out_dir.join("repal_icon.rc");
    let res_path = out_dir.join("repal_icon.res");
    let ico_escaped = ico_path.display().to_string().replace('\\', "\\\\");
    if std::fs::write(&rc_path, format!("1 ICON \"{ico_escaped}\"\n")).is_err() {
        return;
    }

    // Try MSVC `rc.exe` first (standard on windows-latest / Visual Studio), then MinGW `windres`.
    let ok = std::process::Command::new("rc")
        .args(["/nologo", "/fo"])
        .arg(&res_path)
        .arg(&rc_path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        || std::process::Command::new("windres")
            .arg(&rc_path)
            .args(["-O", "coff", "-o"])
            .arg(&res_path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

    if ok && res_path.is_file() {
        println!(
            "cargo:rustc-link-arg-bin=repal={}",
            res_path.display()
        );
    }
}
