//! Behavior parity with the C# app (Windows): long pseudo-random operation scripts run through the real C#
//! `PlaybackTimeline` and `PlayerSessionGate` (fixture tool `apps/desktop/tools/fixtures/PlayerCompat`, which compiles
//! the C# app's own `PlaybackTimeline.cs` read-only) and through the Rust port; every step's result and the full state
//! after it must be identical (doubles compared by value, exactly).
//!
//! The one deliberate difference is excluded from the scripts: a non-finite *reported position* in
//! `PlaybackTimeline::observe` keeps the shown position (Swift) instead of jumping to 0 (C#). The FFmpeg backend never
//! reports one (see `timeline.rs`); non-finite seek targets and durations are covered.
//!
//! Opt-in: `SA_DOTNET_FIXTURES=1 cargo test -p sa-player --test csharp_compat` (needs the .NET 10 SDK; the tool is
//! built on first use with `dotnet build -c Release`). Without the variable every test returns early.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sa_player::{PlaybackTimeline, PlayerEventToken, PlayerSessionGate, SourceId};
use serde_json::{Value, json};

fn enabled() -> bool {
    let on = std::env::var("SA_DOTNET_FIXTURES").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set SA_DOTNET_FIXTURES=1 to run the C# compatibility tests");
    }
    on
}

fn tool_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/PlayerCompat")
}

static BUILD: Mutex<()> = Mutex::new(());

fn tool_exe() -> PathBuf {
    let _guard = BUILD.lock().unwrap_or_else(|e| e.into_inner());
    let exe = tool_dir().join("bin/Release/net10.0/PlayerCompat.exe");
    let program = tool_dir().join("Program.cs");
    let stale = match (std::fs::metadata(&exe).and_then(|m| m.modified()), std::fs::metadata(&program).and_then(|m| m.modified())) {
        (Ok(built), Ok(source)) => built < source,
        _ => true,
    };
    if stale {
        let status = Command::new("dotnet")
            .args(["build", "-c", "Release", "-nologo", "-v", "q"])
            .current_dir(tool_dir())
            .status()
            .expect("dotnet is on PATH");
        assert!(status.success(), "building the PlayerCompat fixture tool failed");
    }
    exe
}

fn csharp(script: &str) -> Vec<Value> {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("script.txt");
    std::fs::write(&file, script).expect("script written");
    let out = Command::new(tool_exe()).arg(&file).output().expect("PlayerCompat runs");
    assert!(out.status.success(), "PlayerCompat failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("UTF-8").lines().map(|l| serde_json::from_str(l).expect("a JSON line")).collect()
}

/// xorshift64*: a deterministic generator (no extra dependency).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

/// Script text of a double ("R"-parsable by .NET).
fn d(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v == f64::INFINITY {
        "Infinity".into()
    } else if v == f64::NEG_INFINITY {
        "-Infinity".into()
    } else {
        format!("{v:?}")
    }
}

fn parse(s: &str) -> f64 {
    match s {
        "NaN" => f64::NAN,
        "Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ => s.parse().expect("a double"),
    }
}

fn bits(v: &Value) -> f64 {
    f64::from_bits(v.as_i64().expect("double bits") as u64)
}

fn same(a: f64, b: f64) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

/// A double from interesting edge values and random ones (seconds-like).
fn seconds(rng: &mut Rng, finite_only: bool) -> f64 {
    let edges = [0.0, -0.0, -1.0, 0.25, 0.2499, 0.2501, 30.0, 59.999, 60.0, 3599.999, 3600.0, 86_399.5, 1e15, f64::MAX, 922_337_203_685.9];
    let non_finite = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY];
    match rng.below(10) {
        0 if !finite_only => non_finite[rng.below(3) as usize],
        1 | 2 => edges[rng.below(edges.len() as u64) as usize],
        3 => -((rng.below(100_000) as f64) / 100.0),
        _ => (rng.below(40_000) as f64) / 100.0 + if rng.below(3) == 0 { 0.0 } else { (rng.below(1000) as f64) / 1000.0 },
    }
}

fn timeline_script(rng: &mut Rng, steps: usize) -> String {
    let mut script = String::new();
    let mut ms: i64 = 0;
    let durations = [0.0, 60.0, 300.0, 30.0, -5.0, f64::NAN, f64::INFINITY, 3601.5];
    let mut duration = 300.0;
    for _ in 0..steps {
        // Mostly forward in small steps; sometimes a long gap or a step back.
        ms += match rng.below(12) {
            0 => 2000,
            1 => 1999,
            2 => 2001,
            3 => -(rng.below(500) as i64),
            4 => 3000 + rng.below(5000) as i64,
            _ => rng.below(400) as i64,
        };
        if rng.below(15) == 0 {
            duration = durations[rng.below(durations.len() as u64) as usize];
        }
        let line = match rng.below(20) {
            0 => "t-reset".to_owned(),
            1..=5 => format!("t-seek {} {ms}", d(seconds(rng, false))),
            6..=7 => format!("fmt {} {}", d(seconds(rng, false)), rng.below(2)),
            8 => format!("clamp {} {}", d(seconds(rng, false)), d(durations[rng.below(durations.len() as u64) as usize])),
            9..=11 => {
                // Near the pending target: around the 0.25 s tolerance.
                let base = seconds(rng, true);
                let off = [0.0, 0.25, -0.25, 0.2500001, 0.24, 0.26, 1.0][rng.below(7) as usize];
                format!("t-observe {} {} {ms}", d(base + off), d(duration))
            }
            _ => format!("t-observe {} {} {ms}", d(seconds(rng, true)), d(duration)),
        };
        script.push_str(&line);
        script.push('\n');
    }
    script
}

fn rust_timeline(script: &str) -> Vec<Value> {
    let epoch = Instant::now() + Duration::from_secs(3600);
    let at = |ms: &str| {
        let ms: i64 = ms.parse().expect("ms");
        if ms >= 0 { epoch + Duration::from_millis(ms as u64) } else { epoch - Duration::from_millis((-ms) as u64) }
    };
    let b = |v: f64| json!(v.to_bits() as i64);
    let mut t = PlaybackTimeline::new();
    let mut out = Vec::new();
    for line in script.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        out.push(match f[0] {
            "t-reset" => {
                t.reset();
                json!(["t", b(t.position), b(t.duration)])
            }
            "t-seek" => {
                let r = t.seek(parse(f[1]), at(f[2]));
                json!(["t", b(r), b(t.position), b(t.duration)])
            }
            "t-observe" => {
                t.observe(parse(f[1]), parse(f[2]), at(f[3]));
                json!(["t", b(t.position), b(t.duration)])
            }
            "fmt" => json!(["fmt", PlaybackTimeline::format_time(parse(f[1]), f[2] == "1")]),
            "clamp" => json!(["clamp", b(PlaybackTimeline::clamp(parse(f[1]), parse(f[2])))]),
            other => panic!("unknown op {other}"),
        });
    }
    out
}

fn compare(script: &str, rust: &[Value], cs: &[Value]) {
    let lines: Vec<&str> = script.lines().collect();
    assert_eq!(rust.len(), cs.len(), "step counts differ");
    for (i, (r, c)) in rust.iter().zip(cs).enumerate() {
        let (r, c) = (r.as_array().expect("array"), c.as_array().expect("array"));
        let equal = r.len() == c.len()
            && r.iter().zip(c).enumerate().all(|(k, (a, b))| match (a, b) {
                // Doubles travel as bit patterns: compare by value (0 == -0).
                (Value::Number(_), Value::Number(_)) if (r[0] == "t" || r[0] == "clamp") && k > 0 => same(bits(a), bits(b)),
                _ => a == b,
            });
        assert!(equal, "step {i} `{}`: Rust {r:?} vs C# {c:?}\n(previous: {:?})", lines[i], &lines[i.saturating_sub(3)..i]);
    }
}

#[test]
fn playback_timeline_matches_the_csharp_timeline_step_by_step() {
    if !enabled() {
        return;
    }
    for seed in [0x9E37_79B9_7F4A_7C15u64, 42, 7_777_777, 0xDEAD_BEEF] {
        let mut rng = Rng(seed);
        let script = timeline_script(&mut rng, 4000);
        compare(&script, &rust_timeline(&script), &csharp(&script));
    }
}

fn gate_script(rng: &mut Rng, steps: usize) -> String {
    let gens = ["L", "L", "L", "L-1", "A", "0", "1", "2", "5"];
    let srcs = ["0", "1", "2", "3", "1", "2"];
    let mut script = String::new();
    for _ in 0..steps {
        let g = rng.pick(&gens);
        let s = rng.pick(&srcs);
        let cur = rng.pick(&srcs);
        let flag = rng.below(2);
        let flag2 = rng.below(2);
        let line = match rng.below(14) {
            0 => "g-begin".to_owned(),
            1 => "g-clear".to_owned(),
            2 | 3 => format!("g-activate {g} {s}"),
            4 => format!("g-open {g} {s} {flag} {}", rng.pick(&["-", "boom", "Bilinmeyen"])),
            5 => format!("g-state {g} {s} {flag} {flag2}"),
            6 => "g-capture".to_owned(),
            7 | 8 => format!("g-accept {g} {s} {cur}"),
            9 | 10 => format!("g-opened {g} {s} {cur} {flag}"),
            11 | 12 => format!("g-failed {g} {s} {cur} {flag}"),
            _ => format!("g-current {g}"),
        };
        script.push_str(&line);
        script.push('\n');
    }
    script
}

fn rust_gate(script: &str) -> Vec<Value> {
    let gate = PlayerSessionGate::new();
    let src = |s: &str| -> Option<SourceId> {
        let id: u64 = s.parse().expect("source id");
        (id != 0).then_some(id)
    };
    let gen_of = |s: &str| -> i32 {
        match s {
            "L" => gate.load_generation(),
            "L-1" => gate.load_generation().wrapping_sub(1),
            "A" => gate.active_generation(),
            n => n.parse().expect("generation"),
        }
    };
    let tok = |g: &str, s: &str| -> PlayerEventToken { gate.create_bound_token(gen_of(g), src(s)) };
    let state = |result: Value| -> Value {
        json!([
            "g",
            result,
            gate.load_generation(),
            gate.active_generation(),
            gate.active_source().unwrap_or(0),
            gate.has_active_source_opened(),
            gate.has_active_source_failed(),
            gate.active_source_error()
        ])
    };
    let mut out = Vec::new();
    for line in script.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let one = |s: &str| s == "1";
        let result = match f[0] {
            "g-begin" => json!(gate.begin_load()),
            "g-clear" => {
                gate.clear_active_source();
                Value::Null
            }
            "g-activate" => json!(gate.try_activate_source(gen_of(f[1]), src(f[2]))),
            "g-open" => {
                let err = (f[4] != "-").then(|| f[4].to_owned());
                json!(gate.notify_source_open_completed(tok(f[1], f[2]), one(f[3]), err))
            }
            "g-state" => json!(gate.notify_source_state_changed(tok(f[1], f[2]), one(f[3]), one(f[4]))),
            "g-capture" => {
                let t = gate.capture_event_token();
                json!([t.generation, t.source.unwrap_or(0)])
            }
            "g-accept" => json!(gate.should_accept_event(tok(f[1], f[2]), src(f[3]))),
            "g-opened" => json!(gate.should_accept_media_opened(tok(f[1], f[2]), src(f[3]), one(f[4]))),
            "g-failed" => json!(gate.should_accept_media_failed(tok(f[1], f[2]), src(f[3]), one(f[4]))),
            "g-current" => json!(gate.is_current_load(gen_of(f[1]))),
            other => panic!("unknown op {other}"),
        };
        out.push(state(result));
    }
    out
}

#[test]
fn player_session_gate_matches_the_csharp_gate_step_by_step() {
    if !enabled() {
        return;
    }
    for seed in [1u64, 0x1234_5678_9ABC_DEF0, 99_991, 0xC0FF_EE00] {
        let mut rng = Rng(seed);
        let script = gate_script(&mut rng, 5000);
        compare(&script, &rust_gate(&script), &csharp(&script));
    }
}

/// The scripted C# test case of `LocalProfileTests.PlayerSessionGate_RejectsLateEventFromPreviousSourceAfterUIThreadSwap`
/// (A = 1, B = 2) goes through the C# gate and the Rust gate with the same results.
#[test]
fn the_csharp_late_event_scenario_gives_the_same_answers() {
    if !enabled() {
        return;
    }
    let script = "g-begin\ng-activate L 1\ng-capture\ng-begin\ng-clear\ng-activate L 2\ng-accept L-1 1 2\ng-accept L-1 1 1\n\
                  g-open L-1 1 0 -\ng-open L-1 1 1 -\ng-failed L-1 1 2 0\ng-opened L-1 1 2 1\ng-capture\ng-failed L 2 2 0\n\
                  g-opened L 2 2 0\ng-open L 2 1 -\ng-opened L 2 2 1\n";
    compare(script, &rust_gate(script), &csharp(script));
}
