//! `sa-fake-engine <script file> <engine args…>`: a scripted stand-in for `subtitle-engine` used by the process tests
//! (port of the C# `FakeEngine`, win-enginekit §11). The script runs line by line; `${NAME}` is replaced by the
//! environment variable `NAME` (missing → empty) and `${ARGn}` by the n-th engine argument (1-based). Commands:
//!
//! * `out <text>` – a stdout line (flushed); `out` alone writes an empty line
//! * `outraw <text>` – text with `\n`, `\r` and `\\` escapes, no newline added (flushed)
//! * `outbig <bytes>` – one stdout line of that many `a` characters
//! * `err <text>` – a stderr line
//! * `args <file>` – the engine arguments joined by single spaces
//! * `stdin <file>` – all of stdin into the file; `cat` – stdin to stdout; `drain` – read and discard stdin
//! * `sleep <ms>`; `pid <file>` – own PID; `child <file>` – spawn `--sleep-child` (30 s) and write its PID
//! * `exit <code>`
//!
//! Anything else writes `unknown fake engine command: X` to stderr and exits with 99.

use std::io::{Read, Write};

fn write_file(path: &str, text: &str) {
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, text).expect("write");
    std::fs::rename(&tmp, path).expect("rename");
}

fn substitute(line: &str, args: &[String]) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                if let Some(n) = name.strip_prefix("ARG").and_then(|n| n.parse::<usize>().ok()) {
                    out.push_str(args.get(n.wrapping_sub(1)).map_or("", String::as_str));
                } else {
                    out.push_str(&std::env::var(name).unwrap_or_default());
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("--sleep-child") {
        std::thread::sleep(std::time::Duration::from_secs(30));
        return;
    }
    let script_path = argv.get(1).expect("script path");
    let engine_args: Vec<String> = argv[2..].to_vec();
    let script = std::fs::read_to_string(script_path).expect("script");
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    for raw in script.split('\n') {
        let line = substitute(raw.strip_suffix('\r').unwrap_or(raw), &engine_args);
        if line.is_empty() {
            continue;
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        match cmd {
            "out" => {
                let mut o = stdout.lock();
                let _ = writeln!(o, "{rest}");
                let _ = o.flush();
            }
            "outraw" => {
                let mut o = stdout.lock();
                let _ = o.write_all(unescape(rest).as_bytes());
                let _ = o.flush();
            }
            "outbig" => {
                let n: usize = rest.trim().parse().expect("size");
                let mut o = stdout.lock();
                let _ = o.write_all(&vec![b'a'; n]);
                let _ = o.write_all(b"\n");
                let _ = o.flush();
            }
            "err" => {
                let mut e = stderr.lock();
                let _ = writeln!(e, "{rest}");
                let _ = e.flush();
            }
            "args" => write_file(rest, &engine_args.join(" ")),
            "stdin" => {
                let mut data = Vec::new();
                let _ = std::io::stdin().read_to_end(&mut data);
                std::fs::write(rest, data).expect("stdin file");
            }
            "cat" => {
                let mut data = Vec::new();
                let _ = std::io::stdin().read_to_end(&mut data);
                let mut o = stdout.lock();
                let _ = o.write_all(&data);
                let _ = o.flush();
            }
            "drain" => {
                let mut data = Vec::new();
                let _ = std::io::stdin().read_to_end(&mut data);
            }
            "sleep" => std::thread::sleep(std::time::Duration::from_millis(rest.trim().parse().expect("ms"))),
            "pid" => write_file(rest, &std::process::id().to_string()),
            "child" => {
                let me = std::env::current_exe().expect("exe");
                let child = std::process::Command::new(me)
                    .arg("--sleep-child")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .expect("child");
                write_file(rest, &child.id().to_string());
            }
            "exit" => std::process::exit(rest.trim().parse().expect("code")),
            other => {
                let _ = writeln!(stderr.lock(), "unknown fake engine command: {other}");
                std::process::exit(99);
            }
        }
    }
}
