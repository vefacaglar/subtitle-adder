//! `sa-make-icon <output.png> [size]` (default 1024) or `sa-make-icon --ico <output.ico>`: renders the app icon
//! (Rust port of `scripts/macos/make-icon.swift`; used by `scripts/package-desktop-macos.sh`).

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let usage = || {
        eprintln!("usage: sa-make-icon <output.png> [size]\n       sa-make-icon --ico <output.ico>");
        ExitCode::from(1)
    };
    let (bytes, out) = match args.get(1).map(String::as_str) {
        None => return usage(),
        Some("--ico") => match args.get(2) {
            Some(out) => (sa_platform::icon::ico(&[16, 20, 24, 32, 40, 48, 64, 128, 256]), out.clone()),
            None => return usage(),
        },
        Some(out) => {
            let size = args.get(2).and_then(|s| s.parse::<f64>().ok()).map(|s| s as usize).filter(|s| *s > 0).unwrap_or(1024);
            (sa_platform::icon::png(size), out.to_owned())
        }
    };
    match std::fs::write(&out, bytes) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{out}: {e}");
            ExitCode::from(1)
        }
    }
}
