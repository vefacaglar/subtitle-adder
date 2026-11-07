//! The `subtitle-engine` command line (port of `cli.py`, `pipeline.py`; spec: `spec/cli.md`).
//!
//! * [`args`]: hand-written argparse-compatible parser (usage/help/error texts verbatim, exit 2).
//! * [`pipeline`]: `pipeline.run` (validation, audio, downloads, transcription, diarization, segmentation,
//!   subtitle outputs).
//! * [`commands`]: the seven command handlers.
//! * [`services`]: dependency injection for tests ([`services::Services`]; production: [`services::Real`]).

pub mod args;
pub mod commands;
pub mod pipeline;
pub mod pyparse;
pub mod services;
mod texts;

use std::ffi::OsString;
use std::io::{Read, Write};

pub use se_core::events::PROTOCOL_VERSION;
pub use services::{Real, Services};

use args::{Command, Parsed};

/// The process streams a command runs with (tests pass buffers).
pub struct Io<'a> {
    /// Raw stdin bytes source (decoded as strict UTF-8 by the handlers that read JSON).
    pub stdin: &'a mut dyn Read,
    /// stdout: UTF-8, LF, one JSON event per line in `--json` mode (flushed per line).
    pub stdout: &'a mut dyn Write,
    /// stderr: free-form log, usage errors, `hata: ...` lines, `… <stage>` lines.
    pub stderr: &'a mut dyn Write,
}

/// `main(argv)` with injected services and streams; returns the exit code (0 ok, 1 handled failure,
/// 2 usage error). `argv` excludes the program name.
///
/// Steps (spec cli.md §3): parse (usage errors exit 2 before anything else) → `i18n::set_language(sub_lang
/// or lang)` → `--offline` sets `SUBTITLE_ENGINE_OFFLINE=1` → go offline unless the command is
/// `transcribe`/`translate` (or always in strict mode) → run the handler; an error becomes an `error` event
/// (with `--json`) or `hata: <message>` on stderr, exit 1.
pub fn main_with(argv: &[OsString], services: &dyn Services, io: &mut Io<'_>) -> i32 {
    let args = match args::parse(argv) {
        Parsed::Run(args) => args,
        Parsed::Exit { code, stdout, stderr } => {
            let _ = io.stdout.write_all(stdout.as_bytes()).and_then(|_| io.stdout.flush());
            let _ = io.stderr.write_all(stderr.as_bytes()).and_then(|_| io.stderr.flush());
            return code;
        }
    };
    se_core::i18n::set_language(args.effective_lang());
    if args.offline {
        se_core::offline::request_offline();
    }
    let may_download = matches!(args.command, Command::Transcribe { .. } | Command::Translate { .. });
    if se_core::offline::offline_requested() || !may_download {
        se_core::offline::go_offline();
    }
    match commands::run_command(&args.command, services, io) {
        Ok(code) => code,
        Err(err) => {
            if args.json() {
                let _ = se_core::events::emit(io.stdout, &se_core::events::error(&err.to_string()));
            } else {
                let _ = writeln!(io.stderr, "{}: {}", se_core::t!("error_prefix"), err).and_then(|_| io.stderr.flush());
            }
            1
        }
    }
}

/// The real process entry: real services, real stdio (stdout/stderr written as raw UTF-8 with LF).
///
/// The handles are not held locked: worker threads of other crates may log to stderr meanwhile.
pub fn main() -> i32 {
    se_models::cuda::set_gpu_probe(se_asr::whisper::cuda_device_count);
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let mut io = Io { stdin: &mut stdin, stdout: &mut stdout, stderr: &mut stderr };
    main_with(&argv, &Real, &mut io)
}
