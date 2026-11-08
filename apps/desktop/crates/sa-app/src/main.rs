// Windows: no console window (the binary is SubtitleAdder.exe; registry + process-name contract, win-ui-main §3).
#![windows_subsystem = "windows"]

fn main() {
    if std::env::args().any(|a| a == "--probe") {
        print!("{}", sa_app::probe_native_stack());
        return;
    }
    if let Err(e) = sa_app::run() {
        eprintln!("Subtitle Adder could not start: {e}");
        std::process::exit(1);
    }
}
