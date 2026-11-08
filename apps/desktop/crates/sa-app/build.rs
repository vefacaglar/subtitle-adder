//! Compiles the Slint UI tree of the target platform with its style (ARCHITECTURE.md §6):
//! Windows → `ui/win/app.slint` with `fluent`; macOS → `ui/mac/app.slint` with `cupertino`.
//!
//! Overrides (look checks on a machine of the other platform, e.g. `sa-shot`):
//! * `SA_UI_PLATFORM=win|mac` picks the tree;
//! * `SA_SLINT_STYLE=<style>` picks the style (default follows the tree).
//!
//! Emits `cfg(sa_ui = "win" | "mac")` so Rust glue can follow the compiled tree.

fn main() {
    println!("cargo::rerun-if-env-changed=SA_UI_PLATFORM");
    println!("cargo::rerun-if-env-changed=SA_SLINT_STYLE");
    println!("cargo::rustc-check-cfg=cfg(sa_ui, values(\"win\", \"mac\"))");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let ui = match std::env::var("SA_UI_PLATFORM").ok().as_deref() {
        Some("mac") => "mac",
        Some("win") => "win",
        _ if target_os == "macos" => "mac",
        _ => "win",
    };
    let style = std::env::var("SA_SLINT_STYLE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| if ui == "mac" { "cupertino".into() } else { "fluent".into() });

    println!("cargo::rustc-cfg=sa_ui=\"{ui}\"");
    println!("cargo::rustc-env=SA_UI_TREE={ui}");
    println!("cargo::rustc-env=SA_UI_STYLE={style}");

    // The mac tree built on another OS compiles its preview entry (no native MenuBar).
    let entry = if ui == "mac" && target_os != "macos" { "ui/mac/preview.slint".to_owned() } else { format!("ui/{ui}/app.slint") };
    let config = slint_build::CompilerConfiguration::new().with_style(style);
    slint_build::compile_with_config(entry, config).expect("Slint UI compiles");
}
