//! Windows (MSVC): compiles a resource script (the exe icon from the WinUI app's `Assets/AppIcon.ico` + the
//! application manifest `windows/SubtitleAdder.exe.manifest`: PerMonitorV2, UTF-8 code page, Common Controls v6 for
//! rfd's TaskDialog) with `rc.exe` and hands the `.res` to the final link of every binary that uses this crate
//! (`static:-bundle,+verbatim`: link.exe takes `.res` files as inputs). Only one crate of the graph may embed these
//! resources, otherwise the link fails with duplicate resources: this is that crate.
//!
//! `rc.exe` is taken from `RC`, `PATH` or the newest Windows 10/11 SDK; without it (or without the icon) the build
//! continues with a warning and no resources, as a plain `cargo build` on a machine without the SDK should.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=windows/SubtitleAdder.exe.manifest");
    println!("cargo::rerun-if-env-changed=RC");
    println!("cargo::rerun-if-env-changed=SA_NO_WIN_RESOURCES");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" || std::env::var_os("SA_NO_WIN_RESOURCES").is_some() {
        return;
    }
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let manifest = manifest_dir.join("windows").join("SubtitleAdder.exe.manifest");
    let icon = manifest_dir.join("../../../windows/src/SubtitleAdder/Assets/AppIcon.ico");
    println!("cargo::rerun-if-changed={}", icon.display());

    let Some(rc) = find_rc() else {
        println!("cargo::warning=rc.exe not found (set RC or install the Windows SDK): SubtitleAdder.exe gets no icon/manifest");
        return;
    };

    // The script with absolute paths (rc resolves relative names against the current directory). No
    // `#include <winuser.h>` (it needs the SDK include path): RT_MANIFEST = 24, CREATEPROCESS_MANIFEST_RESOURCE_ID = 1.
    let mut script = String::new();
    if icon.is_file() {
        script.push_str(&format!("1 ICON \"{}\"\n", rc_path(&icon)));
    } else {
        println!("cargo::warning=no {}: SubtitleAdder.exe gets no icon", icon.display());
    }
    script.push_str(&format!("1 24 \"{}\"\n", rc_path(&manifest)));
    let rc_file = out_dir.join("sa-platform-app.rc");
    std::fs::write(&rc_file, script).expect("write the resource script");
    let res_name = "sa-platform-app.res";
    let res = out_dir.join(res_name);
    let status = Command::new(&rc).arg("/nologo").arg("/fo").arg(&res).arg(&rc_file).status();
    match status {
        Ok(s) if s.success() && res.is_file() => {
            println!("cargo::rustc-link-search=native={}", out_dir.display());
            println!("cargo::rustc-link-lib=static:-bundle,+verbatim={res_name}");
        }
        other => println!("cargo::warning=rc.exe failed ({other:?}): SubtitleAdder.exe gets no icon/manifest"),
    }
}

/// A path inside an rc string literal (backslashes doubled).
fn rc_path(p: &Path) -> String {
    let full = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = full.to_string_lossy().into_owned();
    let s = s.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(s);
    s.replace('\\', "\\\\")
}

fn find_rc() -> Option<PathBuf> {
    if let Some(rc) = std::env::var_os("RC").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(rc);
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    // The newest SDK under Windows Kits\10\bin\<version>\x64.
    let roots = [std::env::var_os("ProgramFiles(x86)"), std::env::var_os("ProgramFiles")];
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for root in roots.into_iter().flatten() {
        let bin = PathBuf::from(root).join("Windows Kits").join("10").join("bin");
        let Ok(entries) = std::fs::read_dir(&bin) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(version) = name.split('.').map(|p| p.parse::<u32>().ok()).collect::<Option<Vec<u32>>>() else { continue };
            let candidate = e.path().join("x64").join("rc.exe");
            if candidate.is_file() && best.as_ref().is_none_or(|(v, _)| version > *v) {
                best = Some((version, candidate));
            }
        }
    }
    best.map(|(_, p)| p)
}
