//! Developer tool for parity checks against the Python engine:
//!
//! ```text
//! cargo run -p se-audio --example media_tool -- extract <video> <out.wav>
//! cargo run -p se-audio --example media_tool -- preview <source|-> <out.mp4>
//! cargo run -p se-audio --example media_tool -- tone <out.mp4>
//! ```

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("extract") => {
            let mut cb = |f: f64| eprintln!("{}", se_core::pyfmt::float_repr(f));
            se_audio::extract_audio(&PathBuf::from(&args[1]), &PathBuf::from(&args[2]), Some(&mut cb)).map(|_| ()).map_err(|e| e.to_string())
        }
        Some("preview") => {
            let source = if args[1] == "-" { se_audio::PreviewSource::Stdin } else { se_audio::PreviewSource::Path(args[1].clone().into()) };
            se_audio::make_preview(source, &PathBuf::from(&args[2])).map_err(|e| e.to_string())
        }
        Some("tone") => se_audio::testgen::tone_video(&PathBuf::from(&args[1])),
        _ => Err("usage: media_tool extract|preview|tone ...".to_string()),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
