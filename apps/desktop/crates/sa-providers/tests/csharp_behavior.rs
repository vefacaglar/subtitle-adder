//! Behavior parity with the C# app's real provider code (win-providers §1–§7): the same inputs go through
//! `tools/fixtures/ProvidersCompat` (which compiles `TranslationProviderManager.cs`, `SubtitleViewModel.cs` and
//! `TranslationModelSelection.cs` from apps/windows) and through this crate, and the outputs must be identical.
//!
//! Skipped (with a note on stderr) when the fixture is not built: `dotnet build -c Release` in that folder.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use sa_core::cancel::CancellationToken;
use sa_enginekit::external_llm::{HttpFetch, HttpRequest};
use sa_enginekit::launch::{EngineLaunchSettings, EngineLaunchState};
use sa_enginekit::presets::TranslationProviderPreset;
use sa_providers::secrets::EncryptedSecretStore;
use sa_providers::win::{self, LegacyLanSettings, ProviderViewState, WinProviderManager};
use sa_providers::{ProviderKeyRead, ProviderStore, TranslationProviderProfile};
use serde_json::{Value, json};
use uuid::Uuid;

/// Tests in this file switch the process-wide UI language.
static LANGUAGE: Mutex<()> = Mutex::new(());

fn fixture() -> Option<PathBuf> {
    let exe = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/fixtures/ProvidersCompat/bin/Release/net10.0/ProvidersCompat.exe");
    if exe.exists() {
        Some(exe)
    } else {
        eprintln!("skipping C# behavior comparison: {} not built", exe.display());
        None
    }
}

fn run_fixture(exe: &Path, args: &[&std::ffi::OsStr]) -> Vec<Value> {
    let out = std::process::Command::new(exe).args(args).output().unwrap();
    assert!(out.status.success(), "fixture failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().map(|s| s.as_str().unwrap().to_owned()).collect()).unwrap_or_default()
}

fn opt_str(v: &Value) -> Option<&str> {
    v.as_str()
}

fn set_language(lang: sa_core::UiLanguage) {
    sa_core::l10n::LanguageState::shared().set_language(lang);
}

const URLS: &[&str] = &[
    "http://127.0.0.1:11434",
    "http://127.0.0.1:11434/",
    "https://openrouter.ai/api/v1",
    "https://openrouter.ai/api/v1/",
    "https://OPENROUTER.AI/api/v1",
    "https://api.openai.com/v1/chat/completions",
    "https://x.test/v1/models",
    "https://x.test/V1/Models",
    "HTTPS://X.test/V1",
    "https://x.test/openai",
    "https://x.test/v1beta/models",
    "https://generativelanguage.googleapis.com/v1beta/openai",
    "https://opencode.ai/zen/go/v1",
    "https://opencode.ai/zen/go/v1/chat/completions",
    "https://opencode.ai/inference/go/v1",
    "https://opencode.ai/inference/go/openai/v1",
    "https://opencode.ai/inference/go/openai/v1/",
    "https://opencode.ai/inference/go/openai/v1/chat/completions",
    "https://OpenCode.ai/inference/go/openai/v1",
    "HTTPS://opencode.ai/zen/go/v1",
    "https://opencode.ai:443/zen/go/v1",
    "https://opencode.ai:8443/zen/go/v1",
    "http://opencode.ai/zen/go/v1",
    "https://opencode.ai/zen/go/v1?x=1",
    "https://opencode.ai/zen/go/v1?",
    "https://opencode.ai/zen/go/v1#top",
    "https://opencode.ai/zen/go/v1#",
    "https://u@opencode.ai/zen/go/v1",
    "https://u:p@opencode.ai/zen/go/v1",
    "https://opencode.ai/ZEN/go/v1",
    "https://opencode.ai/inference/go/v1/chat/completions",
    "https://opencode.ai/inference/go/openai/v1/models",
    "https://opencode.ai//zen/go/v1",
    "https://opencode.ai/zen/go/v1//",
    "https://opencode.ai/zen/go/v1/../v1",
    "https://opencode.ai/zen/%67o/v1",
    "https://opencode.ai./zen/go/v1",
    "https://proxy.example.com/zen/go/v1",
    "not a url",
    "",
    "   ",
    "http://h:1/api/chat",
    "http://h:1/chat/completions/",
    "http://h:1//chat/completions",
    "http://h:1/v1/chat/completions",
    "http://h:1/api/tags",
    "http://h:1/v1/models",
    "http://h:1/v1",
    "http://h:1/v1/v1",
    "HTTP://H:1/V1/CHAT/COMPLETIONS",
    "http://[::1]:8080",
    "mac-studio.local:11434",
    "192.168.1.50:1234/v1/chat/completions",
    "  http://mac-studio.local:11434/api/chat  ",
    "http://",
    "https://",
    "http:",
    "/v1",
    "a b",
    "api.groq.com/openai/v1",
    "https://api.groq.com/openai/v1/models/",
    "openrouter.ai",
    "http://192.168.1.50:11434",
    "https://evil.example/openrouter.ai",
    "https://ŞİRKET.example/v1",
    "http://h:1/chat/completions/chat/completions",
    "http://h:1/models/chat/completions",
    r"https://opencode.ai\zen\go\v1",
    r"https://opencode.ai/zen\go\v1",
    r"https://opencode.ai/zen/go/v1\",
    "https://opencode.ai/zen/go/v1 ",
    "https://opencode.ai/zen/go/v1%20",
    "https://opencode.ai/./zen/go/v1",
    "https://opencode.ai/../zen/go/v1",
    "https://opencode.ai/zen/go/v1/..",
    "https://opencode.ai/zen/go/v1/x/..",
    "https://opencode.ai/zen/go/v1/x/...",
    "https://opencode.ai:65536/zen/go/v1",
    "https://opencode.ai/zen/go/v1/x/.%2e",
    "https://opencode.ai:/zen/go/v1",
    "https://opencode.ai:0443/zen/go/v1",
    "https://opencode.ai/zen/go/v1/.",
    "https://opencode.ai/zen/go/v1/%2E%2E/v1",
    "https://opencode.ai/zen/go%2Fv1",
    "https://opencode.ai/zen/go/v1;x",
    "https://opencode.ai",
    "HTTP://API.OPENAI.COM/V1",
    "http://proxy.test/api.deepsee\u{212a}.com",
    "http://proxy.test/api.deep\u{17f}eek.com",
    "http://proxy.test/OPENCODE.AI",
    "https://\u{212a}.test/v1",
];

#[test]
fn pure_helpers_match_the_csharp_code() {
    let Some(exe) = fixture() else { return };
    let _lock = LANGUAGE.lock();
    set_language(sa_core::UiLanguage::Tr);

    let mut inputs: Vec<Value> = Vec::new();
    for u in URLS {
        for op in ["endpoints", "chat", "opencode", "cloud", "normalize"] {
            inputs.push(json!([op, u]));
        }
    }
    let lists = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<String>>();
    for (d, s, e) in [
        (lists(&["b", "B", " "]), lists(&["c", "b"]), Some(" x ")),
        (lists(&[]), lists(&[]), None),
        (lists(&[" a "]), lists(&["A"]), Some("a")),
        (lists(&["x"]), lists(&[]), Some("  ")),
        (lists(&["İ", "ı", "ſ", "ß", "ǅ", "K"]), lists(&["i", "I", "S", "SS", "Ǆ", "k"]), None),
        (lists(&["m"]), lists(&["n"]), Some("N")),
        (lists(&["µ", "ÿ", "ϐ", "ẞ", "ǆ", "ﬀ", "\u{212a}", "\u{1e9e}"]), lists(&["Μ", "Ÿ", "Β", "ß", "Ǆ", "FF", "k", "ss"]), None),
        (lists(&["\u{a0}a\u{a0}", "\u{2003}b"]), lists(&["a", "b"]), Some("\u{3000}c")),
    ] {
        inputs.push(json!(["merge", d, s, e]));
    }
    let nest = |n: usize| format!("{{\"x\":{}1{},\"data\":[{{\"id\":\"n{n}\"}}]}}", "[".repeat(n), "]".repeat(n));
    let (d62, d63, d64, d65) = (nest(62), nest(63), nest(64), nest(65));
    let deep = format!("{{\"x\":{}1{},\"data\":[{{\"id\":\"deep\"}}]}}", "[".repeat(70), "]".repeat(70));
    for body in [
        r#"{"models":[{"name":"gemma4:e4b"},{"name":"nomic-embed-text:latest"},{"name":"qwen3:14b"}]}"#,
        r#"{"object":"list","data":[{"id":"qwen/qwen3-8b"},{"id":"all-MiniLM-L6"},{"id":"bge-m3"},{"id":" QWEN/qwen3-8b "},{"name":"named"},{"id":""}]}"#,
        "[]",
        "nope",
        "",
        "   ",
        r#"{"data":[{"id":5,"name":"n"}]}"#,
        r#"{"data":[{"id":null,"name":"n"}]}"#,
        r#"{"data":"x","models":[{"name":"m"}]}"#,
        r#"{"data":[1,"s",{"name":" "},null]}"#,
        r#"{"data":[{"id":"EMBED-x"},{"id":"bge-small"},{"id":"bge_small"},{"id":"MiniLM"},{"id":"Embedding"}]}"#,
        r#"{"data":[{"id":"a"}],"models":[{"name":"A"},{"name":"b"}]}"#,
        r#"{"data":[{"id":"a"}],}"#,
        r#"{"data":[{"id":"a"}],"data":[{"id":"b"}]}"#,
        r#"{"data":[{"id":"a","id":"b"}]}"#,
        "\u{feff}{\"data\":[{\"id\":\"a\"}]}",
        r#"{"data":[{"id":"x"}]} trailing"#,
        r#"{"Data":[{"Id":"x"}]}"#,
        &deep,
        &d62,
        &d63,
        &d64,
        &d65,
    ] {
        inputs.push(json!(["parse", body]));
    }
    for key in ["", "   ", "a", "abcd", "abcde", "12345678", "123456789", "sk-1234567890", "  sk-1234567890\n", "ab\tcd ef"] {
        inputs.push(json!(["mask", key]));
    }
    let discovered_sets: Vec<Vec<String>> = vec![
        lists(&[]),
        lists(&["cloud-b", "cloud-a", " cloud-b "]),
        lists(&["gpt-4o-mini", "o3"]),
        lists(&["", "  ", "Z-model", "a-model", "_under", "[bracket", "^caret", "`tick", "a_b", "aab"]),
        lists(&["qwen3:14b", "llama3.2"]),
    ];
    for preset in ["openai", "lan", "LAN", " lan", "custom", "nope", "opencode-go"] {
        for model in ["", " gpt-4o-mini ", "O3", "mine", "zz-top"] {
            for d in &discovered_sets {
                inputs.push(json!(["available", preset, model, d]));
                inputs.push(json!(["picker", preset, model, d]));
            }
        }
    }
    let big = format!("{{\"error\":{{\"message\":\"{}\"}}}}{}", "m".repeat(20), " ".repeat(9000));
    let near = format!("{{\"message\":\"{}😀tail\"}}", "x".repeat(990));
    for (status, body, key) in [
        (400, r#"{"error":{"message":"Global region required (sk-private)"}}"#.to_owned(), Some("sk-private")),
        (402, r#"{"error":"Out of credits"}"#.to_owned(), None),
        (500, r#"{"message":"boom"}"#.to_owned(), Some("")),
        (502, "<html>bad</html>".to_owned(), None),
        (429, "{\"error\":{\"message\":\"Rate limited,\\r\\n slow  down\"}}".to_owned(), None),
        (500, format!("{{\"message\":\"{}\"}}", "x".repeat(3000)), None),
        (500, r#"{"error":null}"#.to_owned(), None),
        (500, r#"{"error":{"message":5}}"#.to_owned(), None),
        (500, r#"{"error":{"message":"  "}}"#.to_owned(), None),
        (500, r#"{"error":{"msg":"x"}}"#.to_owned(), None),
        (500, String::new(), None),
        (500, r#""just a string""#.to_owned(), None),
        (500, big, None),
        (401, r#"{"error":{"message":"bad key sk-secret"}}"#.to_owned(), Some(" sk-secret ")),
        (401, r#"{"error":{"message":"bad key sk-secret"}}"#.to_owned(), Some("sk-secret")),
        (500, near, None),
        (500, "{\"message\":\"line\\u2028sep\\ttab\"}".to_owned(), None),
        (418, r#"{"error":{"message":"a","error":"b"}}"#.to_owned(), None),
        (500, format!("{{\"message\":\"deep\",\"x\":{}1{}}}", "[".repeat(62), "]".repeat(62)), None),
        (500, format!("{{\"message\":\"deep\",\"x\":{}1{}}}", "[".repeat(63), "]".repeat(63)), None),
        (500, format!("{{\"message\":\"deep\",\"x\":{}1{}}}", "[".repeat(64), "]".repeat(64)), None),
    ] {
        inputs.push(json!(["error", status, body, key]));
    }
    for id in [json!("OpenAI"), json!("LAN"), json!(" lan"), json!(""), Value::Null, json!("nope"), json!("OPENCODE-GO")] {
        inputs.push(json!(["preset", id]));
    }

    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("pure.txt");
    std::fs::write(&input, inputs.iter().map(|v| v.to_string() + "\n").collect::<String>()).unwrap();
    let outputs = run_fixture(&exe, &["pure".as_ref(), input.as_os_str()]);
    assert_eq!(outputs.len(), inputs.len());

    let mut mismatches = Vec::new();
    for (input, output) in inputs.iter().zip(&outputs) {
        let a = input.as_array().unwrap();
        let expected = &output[1];
        let actual: Value = match a[0].as_str().unwrap() {
            "endpoints" => json!(win::candidate_model_endpoints(a[1].as_str().unwrap())),
            "chat" => json!(win::chat_completions_endpoint(a[1].as_str().unwrap())),
            "opencode" => json!(win::is_opencode_go_url(a[1].as_str().unwrap())),
            "cloud" => json!(win::is_cloud_provider_url(a[1].as_str().unwrap())),
            "normalize" => json!(win::normalize_external_base_url(a[1].as_str().unwrap())),
            "merge" => json!(win::merge_models(&strings(&a[1]), &strings(&a[2]), opt_str(&a[3]))),
            "parse" => json!(win::parse_suitable_external_models(a[1].as_str().unwrap())),
            "mask" => json!(sa_providers::mask_key(a[1].as_str().unwrap()).unwrap_or_else(|| sa_core::l("Anahtar yok"))),
            "available" => json!(profile(&a[1], &a[2], &a[3]).available_models(false)),
            "picker" => json!(
                sa_providers::selection::provider_models_for(&profile(&a[1], &a[2], &a[3]), false)
                    .into_iter()
                    .map(|c| c.id)
                    .collect::<Vec<_>>()
            ),
            "error" => json!(win::read_provider_error(
                a[1].as_u64().unwrap() as u16,
                a[2].as_str().unwrap(),
                a[3].as_str().unwrap_or("")
            )),
            "preset" => json!(TranslationProviderPreset::preset(a[1].as_str()).id),
            other => panic!("{other}"),
        };
        if &actual != expected {
            mismatches.push(format!("{input}\n    C#:   {expected}\n    Rust: {actual}"));
        }
    }
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
}

/// Files the C# reader and this crate must load the same way (System.Text.Json `PropertyNameCaseInsensitive`).
fn load_cases() -> Vec<(&'static str, Vec<u8>)> {
    const A: &str = "1bed0ce2-2d54-4a5d-a9fa-52fd7ad76929";
    const B: &str = "adf244d7-d14a-4a40-abf1-2da9a43ba655";
    let entry = |id: &str| format!(r#"{{"id":"{id}","name":"n","presetId":"lan","baseUrl":"http://h:1","apiKey":"","model":"m","asyncTranslation":true,"discoveredModels":["a"]}}"#);
    let one = |field: &str| format!(r#"{{"activeProviderId":null,"providers":[{{"id":"{A}",{field}}}]}}"#);
    let deep = |n: usize| format!(r#"{{"x":{}1{},"providers":[{}]}}"#, "[".repeat(n), "]".repeat(n), entry(A));
    let mut utf16 = vec![0xFF, 0xFE];
    for u in format!(r#"{{"providers":[{}]}}"#, entry(A)).encode_utf16() {
        utf16.extend_from_slice(&u.to_le_bytes());
    }
    let doc = format!(r#"{{"providers":[{}]}}"#, entry(A));
    let mut utf16_be = vec![0xFE, 0xFF];
    let mut utf32_le = vec![0xFF, 0xFE, 0, 0];
    let mut utf32_be = vec![0, 0, 0xFE, 0xFF];
    for u in doc.encode_utf16() {
        utf16_be.extend_from_slice(&u.to_be_bytes());
    }
    for c in doc.chars() {
        utf32_le.extend_from_slice(&(c as u32).to_le_bytes());
        utf32_be.extend_from_slice(&(c as u32).to_be_bytes());
    }
    let s = |t: String| t.into_bytes();
    vec![
        ("01-csharp", include_bytes!("../../../tools/fixtures/ProvidersFixture/csharp-translation-providers.json").to_vec()),
        (
            "02-case-null",
            s(format!(
                r#"{{"ACTIVEPROVIDERID":"{A}","Providers":[{{"ID":"{A}","Name":null,"PRESETID":"lan","baseurl":"http://h:1","ApiKey":null,"Model":"m","DiscoveredModels":null,"unknown":{{"x":[1,{{"y":null}}]}}}}]}}"#
            )),
        ),
        ("03-root-null", s("null".into())),
        ("04-empty-object", s("{}".into())),
        ("05-providers-null", s(r#"{"activeProviderId":null,"providers":null}"#.into())),
        ("06-id-braced", s(format!(r#"{{"providers":[{{"id":"{{{A}}}"}}]}}"#))),
        ("07-id-n", s(format!(r#"{{"providers":[{{"id":"{}"}}]}}"#, A.replace('-', "")))),
        ("08-id-upper", s(format!(r#"{{"activeProviderId":"{}","providers":[{{"id":"{}"}}]}}"#, A.to_uppercase(), A.to_uppercase()))),
        ("09-id-spaces", s(format!(r#"{{"providers":[{{"id":" {A} "}}]}}"#))),
        ("10-id-null", s(r#"{"providers":[{"id":null,"name":"x"}]}"#.into())),
        ("11-id-missing", s(r#"{"providers":[{"name":"x"},{"name":"y"}]}"#.into())),
        ("12-id-empty", s(r#"{"providers":[{"id":"","name":"x"}]}"#.into())),
        ("13-active-empty", s(format!(r#"{{"activeProviderId":"","providers":[{}]}}"#, entry(A)))),
        ("14-active-unknown", s(format!(r#"{{"activeProviderId":"{B}","providers":[{}]}}"#, entry(A)))),
        ("15-name-number", s(one(r#""name":5"#))),
        ("16-async-string", s(one(r#""asyncTranslation":"true""#))),
        ("17-async-number", s(one(r#""asyncTranslation":1"#))),
        ("18-async-null", s(one(r#""asyncTranslation":null"#))),
        ("19-async-false", s(one(r#""asyncTranslation":false"#))),
        ("20-models-number", s(one(r#""discoveredModels":["a",5]"#))),
        ("21-models-string", s(one(r#""discoveredModels":"a""#))),
        ("22-dup-keys", s(one(r#""apiKey":"first-key-1234","ApiKey":"second-key-5678","model":"m1","MODEL":"m2""#))),
        ("23-bom", [vec![0xEF, 0xBB, 0xBF], s(format!(r#"{{"providers":[{}]}}"#, entry(A)))].concat()),
        ("24-utf16-bom", utf16),
        ("25-trailing-comma", s(format!(r#"{{"providers":[{}],}}"#, entry(A)))),
        ("26-comment", s(format!("{{\"providers\":[{}] /* c */}}", entry(A)))),
        ("27-array-root", s("[]".into())),
        ("28-providers-object", s(r#"{"providers":{}}"#.into())),
        ("29-depth-63", s(deep(63))),
        ("30-depth-64", s(deep(64))),
        ("31-escapes", s(one(r#""name":"Ağ😀\/","baseUrl":"http:\/\/h:1""#))),
        ("32-plaintext-keys", s(format!(
            r#"{{"activeProviderId":"{B}","providers":[{},{{"id":"{B}","presetId":"OPENAI","apiKey":"  sk-spaced-1234  ","model":" gpt "}}]}}"#,
            entry(A)
        ))),
        ("33-invalid-utf8", [s(one(r#""name":"a"#)), vec![0xFF, 0xFE, b'"', b'}', b']', b'}']].concat()),
        ("34-trailing-garbage", s(format!(r#"{{"providers":[{}]}} x"#, entry(A)))),
        ("35-empty-file", Vec::new()),
        ("36-whitespace", s("  \r\n ".into())),
        ("37-number-id", s(r#"{"providers":[{"id":5}]}"#.into())),
        ("38-providers-string", s(r#"{"providers":"x"}"#.into())),
        ("39-active-number", s(format!(r#"{{"activeProviderId":5,"providers":[{}]}}"#, entry(A)))),
        ("40-utf16-be-bom", utf16_be),
        ("41-utf32-le-bom", utf32_le),
        ("42-utf32-be-bom", utf32_be),
        // Known difference, not compared: an unknown property holding a number beyond f64 (`1e400`) is skipped by
        // System.Text.Json but fails serde_json, so the port treats such a file as damaged (aside copy, no loss).
    ]
}

#[test]
fn files_load_like_the_csharp_reader() {
    let Some(exe) = fixture() else { return };
    let _lock = LANGUAGE.lock();
    set_language(sa_core::UiLanguage::Tr);
    let cases = load_cases();
    let dir = tempfile::tempdir().unwrap();
    let inputs = dir.path().join("files");
    std::fs::create_dir(&inputs).unwrap();
    for (name, bytes) in &cases {
        std::fs::write(inputs.join(name), bytes).unwrap();
    }
    let outputs = run_fixture(&exe, &["load".as_ref(), inputs.as_os_str()]);
    assert_eq!(outputs.len(), cases.len());

    let mut mismatches = Vec::new();
    let mut crashes = Vec::new();
    for ((name, bytes), expected) in cases.iter().zip(&outputs) {
        assert_eq!(expected[0], *name);
        if expected[1] == "crash" {
            // The C# app throws here (win-providers §1.2: null fields); the port loads with defaults instead.
            crashes.push(format!("{name}: {}", expected[2].as_str().unwrap()));
            continue;
        }
        let case_dir = dir.path().join(format!("rust-{name}"));
        std::fs::create_dir(&case_dir).unwrap();
        std::fs::write(case_dir.join("translation-providers.json"), bytes).unwrap();
        let m = WinProviderManager::with_launch_state(
            case_dir.join("translation-providers.json"),
            EncryptedSecretStore::for_app_data(&case_dir),
            None,
            EngineLaunchState::new(EngineLaunchSettings::default()),
        );
        // Only used to tell ids the file contains from fresh ones: every BOM form above, NULs dropped.
        let text = String::from_utf8_lossy(&bytes.iter().copied().filter(|b| *b != 0).collect::<Vec<_>>()).to_lowercase();
        let providers = m.providers();
        let active = m.active_provider_id().map(|id| providers.iter().position(|p| p.id == id).unwrap());
        let rows: Vec<Value> = providers
            .iter()
            .map(|p| {
                let id = p.id.hyphenated().to_string();
                let key = match m.read_key(p.id) {
                    ProviderKeyRead::Found(k) => k.expose().to_owned(),
                    ProviderKeyRead::Absent => String::new(),
                    ProviderKeyRead::Unavailable => "<unavailable>".into(),
                };
                json!([
                    if text.contains(&id) { id } else { "*".into() },
                    p.name,
                    p.preset_id,
                    p.base_url,
                    key,
                    p.model,
                    p.async_translation,
                    p.discovered_models
                ])
            })
            .collect();
        // C# keeps a plaintext key untrimmed in memory (it trims wherever it uses it); the sidecar holds it trimmed.
        let mut expected_rows = expected[2].clone();
        for row in expected_rows.as_array_mut().unwrap() {
            let key = row[4].as_str().unwrap_or("").trim().to_owned();
            row[4] = json!(key);
        }
        let actual = json!([name, active, rows]);
        let expected = json!([name, expected[1], expected_rows]);
        if actual != expected {
            mismatches.push(format!("{name}\n    C#:   {expected}\n    Rust: {actual}"));
        }
    }
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
    assert_eq!(crashes, vec!["02-case-null: NullReferenceException".to_owned()], "only the known C# null crash");
}

fn profile(preset: &Value, model: &Value, discovered: &Value) -> TranslationProviderProfile {
    TranslationProviderProfile {
        id: Uuid::new_v4(),
        name: "x".into(),
        preset_id: preset.as_str().unwrap().to_owned(),
        base_url: "https://x.test/v1".into(),
        model: model.as_str().unwrap().to_owned(),
        async_translation: true,
        discovered_models: strings(discovered),
    }
}

// MARK: scenarios against a real TranslationProviderManager + SubtitleViewModel

/// The C# fallback catalog's translation models (`TranslationModelSelection.GetModels(None)`).
const ON_DEVICE: &[&str] = &["nllb-1.3b", "qwen3.5-4b", "madlad-3b", "gemma-4-e4b", "qwen3.5-9b", "gemma-4-12b"];

/// Scripted HTTP with the fixture's rules: `[method | "*", url suffix (case-insensitive), status (0 = transport
/// error with body as message), body]`, first match wins, else 404.
#[derive(Default)]
struct ScriptFetch {
    rules: Mutex<Vec<(String, String, u16, String)>>,
    seen: Mutex<Vec<HttpRequest>>,
}

impl HttpFetch for ScriptFetch {
    fn fetch(&self, request: HttpRequest, _cancel: &CancellationToken) -> Result<(u16, String), Option<String>> {
        let rules = self.rules.lock().clone();
        let url = request.url.clone();
        let method = request.method;
        self.seen.lock().push(request);
        for (m, suffix, status, body) in rules {
            if (m == "*" || m == method) && url.to_lowercase().ends_with(&suffix.to_lowercase()) {
                return if status == 0 { Err(Some(body)) } else { Ok((status, body)) };
            }
        }
        Ok((404, String::new()))
    }
}

impl ScriptFetch {
    fn report(&self, session: &str) -> Value {
        let seen = std::mem::take(&mut *self.seen.lock());
        json!(
            seen.iter()
                .map(|r| {
                    let oc = r.header("x-opencode-session").map(|v| if v == session { "<session>".to_owned() } else { v.to_owned() });
                    json!([
                        r.method,
                        r.url,
                        r.header("Authorization"),
                        r.header("User-Agent"),
                        r.header("Accept"),
                        oc,
                        r.header("Content-Type"),
                        r.body
                    ])
                })
                .collect::<Vec<_>>()
        )
    }
}

/// "· 123 ms ·" → "· N ms ·" (the fixture does the same with a regex).
fn ms_normalized(message: &str) -> String {
    let mut out = String::new();
    let mut rest = message;
    while let Some(i) = rest.find("\u{b7} ") {
        out.push_str(&rest[..i + "\u{b7} ".len()]);
        rest = &rest[i + "\u{b7} ".len()..];
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && rest[digits..].starts_with(" ms \u{b7}") {
            out.push('N');
            rest = &rest[digits..];
        }
    }
    out.push_str(rest);
    out
}

fn non_blank(s: &str) -> Value {
    let t = s.trim();
    if t.is_empty() { Value::Null } else { json!(t) }
}

/// Replaces every UUID by `U<n>` (order of first appearance), so two files with different random ids compare.
fn mask_uuids(text: &str) -> String {
    let mut seen: Vec<String> = Vec::new();
    let mut out = String::new();
    let mut i = 0;
    while i < text.len() {
        if i + 36 <= text.len() && text.is_char_boundary(i) && text.is_char_boundary(i + 36) {
            let cand = &text[i..i + 36];
            if cand.as_bytes()[8] == b'-' && Uuid::parse_str(cand).is_ok() {
                let lower = cand.to_lowercase();
                let n = match seen.iter().position(|s| *s == lower) {
                    Some(n) => n,
                    None => {
                        seen.push(lower);
                        seen.len() - 1
                    }
                };
                out.push_str(&format!("U{n}"));
                i += 36;
                continue;
            }
        }
        let c = text[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// The C# file keeps keys in plaintext; the port writes `"apiKey": ""` (rule 5).
fn scrub_api_keys(text: &str) -> String {
    text.split("\r\n")
        .map(|line| match line.find("\"apiKey\": \"") {
            Some(i) => {
                let comma = if line.ends_with(',') { "," } else { "" };
                format!("{}\"apiKey\": \"\"{comma}", &line[..i])
            }
            None => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

struct RustScenario {
    _dir: tempfile::TempDir,
    file: PathBuf,
    m: Arc<WinProviderManager>,
    launch: Arc<EngineLaunchState>,
    vm: ProviderViewState,
    translation_model: String,
    known: Vec<Uuid>,
    fetch: ScriptFetch,
}

impl RustScenario {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("translation-providers.json");
        let launch = EngineLaunchState::new(EngineLaunchSettings::default());
        let m = WinProviderManager::with_launch_state(file.clone(), EncryptedSecretStore::for_app_data(dir.path()), None, launch.clone());
        Self {
            _dir: dir,
            file,
            m,
            launch,
            vm: ProviderViewState::default(),
            translation_model: "madlad-3b".into(),
            known: Vec::new(),
            fetch: ScriptFetch::default(),
        }
    }

    fn id(&self, o: &Value) -> Option<Uuid> {
        match o.get("index") {
            None | Some(Value::Null) => None,
            Some(v) => {
                let n = v.as_i64().unwrap();
                Some(if n < 0 { Uuid::new_v4() } else { self.known[n as usize] })
            }
        }
    }

    /// Runs one op; `None` for ops that print nothing.
    fn step(&mut self, number: usize, o: &Value) -> Option<Value> {
        let op = o["op"].as_str().unwrap();
        let s = |k: &str| o[k].as_str().map(str::to_owned);
        *self.fetch.rules.lock() = o
            .get("http")
            .and_then(Value::as_array)
            .map(|rules| {
                rules
                    .iter()
                    .map(|r| {
                        (
                            r[0].as_str().unwrap().to_owned(),
                            r[1].as_str().unwrap().to_owned(),
                            r[2].as_u64().unwrap() as u16,
                            r[3].as_str().unwrap().to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.fetch.seen.lock().clear();
        let cancel = CancellationToken::new();
        let mut result = Value::Null;
        match op {
            "lang" => {
                set_language(if o["value"] == "en" { sa_core::UiLanguage::En } else { sa_core::UiLanguage::Tr });
                return None;
            }
            "add" => {
                let p = TranslationProviderProfile {
                    id: Uuid::new_v4(),
                    name: s("name").unwrap_or_default(),
                    preset_id: s("preset").unwrap(),
                    base_url: s("url").unwrap_or_default(),
                    model: s("model").unwrap_or_default(),
                    async_translation: o["async"].as_bool().unwrap(),
                    discovered_models: Vec::new(),
                };
                let key = s("key").unwrap_or_default();
                self.m.add(p, Some(&key), o["active"].as_bool().unwrap());
            }
            "update" => {
                let p = TranslationProviderProfile {
                    id: self.id(o).unwrap_or(Uuid::nil()),
                    name: s("name").unwrap(),
                    preset_id: String::new(),
                    base_url: s("url").unwrap(),
                    model: s("model").unwrap(),
                    async_translation: o["async"].as_bool().unwrap(),
                    discovered_models: Vec::new(),
                };
                self.m.update(p, Some(&s("key").unwrap()));
            }
            "model" => self.m.update_model(self.id(o).unwrap_or(Uuid::nil()), &s("model").unwrap()),
            "remove" => self.m.remove(self.id(o).unwrap_or(Uuid::nil())),
            "active" => self.m.set_active(self.id(o)),
            "discovered" => {
                self.m.update_provider_discovered_models(self.id(o).unwrap_or(Uuid::nil()), &strings(&o["models"]), o["selected"].as_str())
            }
            "vm" => {
                self.vm.lan = LegacyLanSettings {
                    enabled: o["enabled"].as_bool().unwrap(),
                    url: s("url").unwrap(),
                    model: s("model").unwrap(),
                    async_translation: o["async"].as_bool().unwrap(),
                    models: strings(&o["models"]),
                };
                return None;
            }
            "migrate" => self.m.ensure_migrated_from_legacy(&mut self.vm),
            "sync" => {
                let lan = self.vm.lan.clone();
                self.m.sync_from_legacy_lan_settings(&lan)
            }
            "apply" => self.m.apply_to_view_state(&mut self.vm),
            "select_provider" => {
                let id = self.id(o);
                result = json!(self.m.select_provider(&mut self.vm, id))
            }
            "select_model" => {
                let id = self.id(o);
                let model = s("model").unwrap();
                let ok = self.m.select_model(&mut self.vm, id, &model, ON_DEVICE);
                if ok && id.is_none() {
                    self.translation_model = model;
                }
                result = json!(ok);
            }
            "picker" => {
                result = match self.id(o) {
                    None => json!(ON_DEVICE),
                    Some(id) => json!(
                        self.m
                            .providers()
                            .iter()
                            .find(|p| p.id == id)
                            .map(|p| sa_providers::selection::provider_models_for(p, false).into_iter().map(|c| c.id).collect::<Vec<_>>())
                            .unwrap_or_default()
                    ),
                }
            }
            "test" => {
                let out = self.m.test_and_fetch_models(self.id(o).unwrap_or(Uuid::nil()), &self.fetch, &cancel).unwrap();
                result = json!([out.success, ms_normalized(&out.message), out.models, self.fetch.report(self.m.instance_id())]);
            }
            "draft" => {
                let out = self
                    .m
                    .test_draft(&s("preset").unwrap(), &s("url").unwrap(), &s("key").unwrap(), o["model"].as_str(), &self.fetch, &cancel)
                    .unwrap();
                result = json!([out.success, ms_normalized(&out.message), out.models, self.fetch.report(self.m.instance_id())]);
            }
            "file" => result = std::fs::read_to_string(&self.file).map(|t| json!(mask_uuids(&t))).unwrap_or(Value::Null),
            other => panic!("unknown op {other}"),
        }
        for p in self.m.providers() {
            if !self.known.contains(&p.id) {
                self.known.push(p.id);
            }
        }
        let providers = self.m.providers();
        let rows: Vec<Value> = providers
            .iter()
            .map(|p| {
                let key = match self.m.read_key(p.id) {
                    ProviderKeyRead::Found(k) => k.expose().to_owned(),
                    _ => String::new(),
                };
                json!([
                    self.known.iter().position(|k| *k == p.id),
                    p.name,
                    p.preset_id,
                    p.base_url,
                    key,
                    p.model,
                    p.async_translation,
                    p.discovered_models
                ])
            })
            .collect();
        let engine = match self.launch.get().external_llm {
            Some(l) => json!([non_blank(&l.url), non_blank(&l.model), non_blank(&l.api_key)]),
            None => json!([null, null, null]),
        };
        let vm = &self.vm;
        Some(json!([
            "state",
            number,
            op,
            result,
            rows,
            self.m.active_provider_id().map(|id| self.known.iter().position(|k| *k == id)),
            self.m.is_external_provider_active(),
            engine,
            [
                vm.lan.enabled,
                vm.lan.url,
                vm.lan.model,
                vm.lan.async_translation,
                vm.lan.models,
                vm.active_provider_name,
                vm.active_provider_model,
                vm.active_provider_async,
                self.translation_model
            ]
        ]))
    }
}

fn run_scenario(exe: &Path, script: &[Value]) {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("scenario.txt");
    std::fs::write(&input, script.iter().map(|v| v.to_string() + "\n").collect::<String>()).unwrap();
    let expected = run_fixture(exe, &["scenario".as_ref(), input.as_os_str()]);
    // The fixture and the port both start in Turkish.
    set_language(sa_core::UiLanguage::Tr);
    let mut rust = RustScenario::new();
    let mut actual = Vec::new();
    let mut number = 0;
    for op in script {
        if let Some(state) = rust.step(number, op) {
            actual.push(state);
            number += 1;
        }
    }
    set_language(sa_core::UiLanguage::Tr);
    assert_eq!(expected.len(), actual.len());
    let mut mismatches = Vec::new();
    for (e, a) in expected.iter().zip(&actual) {
        let mut e = e.clone();
        if e[2] == "file"
            && let Some(text) = e[3].as_str()
        {
            e[3] = json!(mask_uuids(&scrub_api_keys(text)));
        }
        if &e != a {
            mismatches.push(format!("op {} {}\n    C#:   {e}\n    Rust: {a}", e[1], e[2]));
        }
    }
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
}

fn add_op(preset: &str, name: Option<&str>, url: Option<&str>, key: Option<&str>, model: Option<&str>, asy: bool, active: bool) -> Value {
    json!({"op": "add", "preset": preset, "name": name, "url": url, "key": key, "model": model, "async": asy, "active": active})
}

fn vm_op(enabled: bool, url: &str, model: &str, asy: bool, models: &[&str]) -> Value {
    json!({"op": "vm", "enabled": enabled, "url": url, "model": model, "async": asy, "models": models})
}

#[test]
fn editing_scenarios_match_the_csharp_manager() {
    let Some(exe) = fixture() else { return };
    let _lock = LANGUAGE.lock();
    run_scenario(&exe, &[
        add_op("opencode-go", None, None, Some("sk-oc-1234567890"), Some("kimi-k2.5"), true, true),
        add_op("openrouter", Some("  My OpenRouter "), None, Some(" sk-or-abcdef9876 "), Some("google/gemma-3-27b-it"), true, false),
        add_op("lan", None, Some("http://192.168.1.50:11434"), Some(""), Some("gemma4:e4b"), false, false),
        add_op(" openai", None, None, None, None, true, false),
        add_op("LAN", Some(" "), Some(" "), None, Some("  "), true, false),
        json!({"op": "active", "index": 1}),
        json!({"op": "active", "index": 2}),
        json!({"op": "active", "index": null}),
        json!({"op": "active", "index": 0}),
        json!({"op": "active", "index": -1}),
        json!({"op": "active", "index": 0}),
        json!({"op": "update", "index": 0, "name": " ", "url": " https://opencode.ai/zen/go/v1 ", "key": " sk-new ", "model": " m ", "async": false}),
        json!({"op": "update", "index": -1, "name": "x", "url": "y", "key": "z", "model": "w", "async": true}),
        json!({"op": "model", "index": 0, "model": " m "}),
        json!({"op": "model", "index": 0, "model": "x"}),
        json!({"op": "model", "index": -1, "model": "x"}),
        json!({"op": "discovered", "index": 0, "models": ["b", " a ", "B", ""], "selected": null}),
        json!({"op": "discovered", "index": 2, "models": [], "selected": " sel "}),
        json!({"op": "discovered", "index": 3, "models": ["first", "second"], "selected": "  "}),
        json!({"op": "discovered", "index": -1, "models": ["q"], "selected": null}),
        json!({"op": "picker", "index": 0}),
        json!({"op": "picker", "index": 1}),
        json!({"op": "picker", "index": 2}),
        json!({"op": "picker", "index": 4}),
        json!({"op": "picker", "index": null}),
        json!({"op": "picker", "index": -1}),
        json!({"op": "select_model", "index": 0, "model": "b"}),
        json!({"op": "select_model", "index": 0, "model": "B"}),
        json!({"op": "select_model", "index": 1, "model": "deepseek/deepseek-chat-v3-0324"}),
        json!({"op": "select_model", "index": null, "model": "madlad-3b"}),
        json!({"op": "select_model", "index": null, "model": "nope"}),
        json!({"op": "select_model", "index": -1, "model": "b"}),
        json!({"op": "select_provider", "index": -1}),
        json!({"op": "select_provider", "index": 2}),
        json!({"op": "select_provider", "index": 4}),
        json!({"op": "apply"}),
        json!({"op": "select_provider", "index": null}),
        json!({"op": "remove", "index": 1}),
        json!({"op": "remove", "index": -1}),
        json!({"op": "active", "index": 3}),
        json!({"op": "remove", "index": 3}),
        json!({"op": "file"}),
    ]);
}

#[test]
fn lan_bridge_scenarios_match_the_csharp_manager() {
    let Some(exe) = fixture() else { return };
    let _lock = LANGUAGE.lock();
    // Startup with the legacy LAN settings on, then cloud providers added and switched (the C# test), then edits in
    // the Settings LAN section.
    run_scenario(&exe, &[
        vm_op(true, "http://mac-studio.local:11434", "gemma4:e4b", true, &[]),
        json!({"op": "migrate"}),
        add_op("opencode-go", None, None, Some("oc_sk_test_123456"), Some("deepseek-v4.1-flash"), true, true),
        add_op("openrouter", None, None, Some("sk-or-test-654321"), Some("google/gemma-3-27b-it"), true, false),
        json!({"op": "apply"}),
        json!({"op": "sync"}),
        json!({"op": "active", "index": 2}),
        json!({"op": "apply"}),
        json!({"op": "sync"}),
        vm_op(false, "http://mac-studio.local:11434", "gemma4:e4b", true, &[]),
        json!({"op": "sync"}),
        json!({"op": "apply"}),
        json!({"op": "active", "index": 0}),
        json!({"op": "apply"}),
        vm_op(false, "http://mac-studio.local:11434", "gemma4:e4b", false, &[]),
        json!({"op": "sync"}),
        json!({"op": "apply"}),
        vm_op(true, "https://openrouter.ai/api/v1", "x", true, &[]),
        json!({"op": "sync"}),
        json!({"op": "migrate"}),
        vm_op(true, "  http://10.0.0.5:1234/v1 ", "", false, &["q1", "q2"]),
        json!({"op": "sync"}),
        json!({"op": "apply"}),
        vm_op(true, "  ", "", false, &[]),
        json!({"op": "sync"}),
        vm_op(false, "", "", false, &[]),
        json!({"op": "sync"}),
        json!({"op": "remove", "index": 0}),
        vm_op(true, "lan-box:11434", "lm", true, &["lm", "other"]),
        json!({"op": "sync"}),
        json!({"op": "apply"}),
        json!({"op": "picker", "index": 3}),
        json!({"op": "select_model", "index": 3, "model": "other"}),
        json!({"op": "file"}),
    ]);
    // Startup variations of EnsureMigratedFromViewModel.
    run_scenario(&exe, &[
        vm_op(false, "http://h:1", "m", true, &["a"]),
        json!({"op": "migrate"}),
        vm_op(true, "http://h:1", "m", true, &["a"]),
        json!({"op": "migrate"}),
        add_op("openai", None, None, Some("sk-1"), None, true, true),
        vm_op(true, "http://h:2", "", false, &["x"]),
        json!({"op": "migrate"}),
        json!({"op": "apply"}),
        json!({"op": "active", "index": null}),
        json!({"op": "migrate"}),
        json!({"op": "file"}),
    ]);
    run_scenario(&exe, &[
        add_op("openai", None, None, Some("sk-1"), None, true, false),
        vm_op(false, "http://h:1", "m", true, &[]),
        json!({"op": "migrate"}),
        vm_op(true, "http://h:1", "m", true, &[]),
        json!({"op": "migrate"}),
        json!({"op": "migrate"}),
        json!({"op": "file"}),
    ]);
}

#[test]
fn connection_test_scenarios_match_the_csharp_manager() {
    let Some(exe) = fixture() else { return };
    let _lock = LANGUAGE.lock();
    let ollama = r#"{"models":[{"name":"qwen3:14b"},{"name":"nomic-embed-text"}]}"#;
    let rejection = r#"{"error":{"message":"Global region required (sk-private)"}}"#;
    let script = |lang: &str| {
        vec![
            json!({"op": "lang", "value": lang}),
            add_op("opencode-go", None, None, Some("sk-opencode-secret"), Some("glm-5"), true, true),
            json!({"op": "test", "index": 0, "http": [["GET", "/models", 404, ""], ["POST", "/chat/completions", 200, "{}"]]}),
            add_op("custom", None, Some("https://API.Example.test/v1"), None, None, true, false),
            json!({"op": "test", "index": 1, "http": [["GET", "/v1/models", 200, r#"{"data":[{"id":"m1"},{"id":"embed-x"},{"id":"m2"}]}"#]]}),
            json!({"op": "test", "index": 1, "http": [["*", "", 401, ""]]}),
            json!({"op": "test", "index": -1}),
            add_op("opencode-go", None, Some("https://opencode.ai/zen/go/v1/chat/completions"), Some("sk-private"), Some("deepseek-v4.1-flash"), true, false),
            json!({"op": "test", "index": 2, "http": [["GET", "", 404, ""], ["POST", "", 400, rejection]]}),
            json!({"op": "draft", "preset": "openai", "url": "   ", "key": "", "model": null}),
            json!({"op": "draft", "preset": "custom", "url": "nowhere.test", "key": "", "model": "mine", "http": [["*", "", 0, "No such host is known."]]}),
            json!({"op": "draft", "preset": "lan", "url": "127.0.0.1:11434", "key": "", "model": "gemma4:e4b", "http": [["GET", "/v1/models", 200, ollama]]}),
            json!({"op": "draft", "preset": "lan", "url": "http://127.0.0.1:11434/", "key": "", "model": null, "http": [["GET", "/v1/models", 200, r#"{"data":[]}"#], ["GET", "/models", 200, r#"{"models":[{"name":"q"}]}"#]]}),
            json!({"op": "draft", "preset": "custom", "url": "https://x.test/v1", "key": "k", "model": null, "http": [["POST", "", 403, ""]]}),
            json!({"op": "draft", "preset": "custom", "url": "http://[::1]:8080", "key": "", "model": "", "http": [["GET", "/v1/models", 200, r#"{"data":[{"id":"v6"}]}"#]]}),
            json!({"op": "draft", "preset": "custom", "url": "HTTPS://API.OpenAI.com/v1", "key": "", "model": null, "http": [["GET", "", 200, r#"{"data":[]}"#]]}),
            json!({"op": "draft", "preset": "openai", "url": "https://api.openai.com/v1", "key": " sk ", "model": " gpt-x ", "http": [["GET", "", 200, "not json"]]}),
            json!({"op": "draft", "preset": "groq", "url": "https://api.groq.com/openai/v1", "key": "gsk", "model": null, "http": [["GET", "", 500, r#"{"error":{"message":"down"}}"#], ["POST", "", 502, r#"{"error":{"message":"gateway gsk"}}"#]]}),
            json!({"op": "draft", "preset": "custom", "url": "https://x.test", "key": "", "model": null, "http": [["GET", "/v1/models", 0, "reset"], ["GET", "/models", 500, ""], ["GET", "/api/tags", 0, "refused"], ["POST", "", 0, "closed"]]}),
            json!({"op": "draft", "preset": "custom", "url": "https://x.test/api/chat", "key": "", "model": null, "http": [["POST", "/api/chat", 200, "{}"]]}),
            json!({"op": "draft", "preset": "nope", "url": "https://x.test/models", "key": "", "model": null, "http": [["GET", "/models", 200, r#"{"data":[{"id":"only"}]}"#]]}),
            json!({"op": "file"}),
        ]
    };
    run_scenario(&exe, &script("tr"));
    run_scenario(&exe, &script("en"));
    // Address validation of the draft test (.NET `Uri.TryCreate` + non-empty host) and the raw request URLs built
    // from what was typed. No key (a key over plain http to a public host is refused on purpose, UNIFIED.md §3.7) and
    // no `.local` names (they would resolve over the real network).
    let odd = [
        "http://", "https://", "http:", "a b", "http://a b", "http://[::1", "http://[::1]", "http://h:99999", "http://h:65535",
        "http://h:0", "http://h:", "http://h:x", "http://-h", "http://h_1", "http://ü.test", "http://ŞİRKET.test/v1",
        "http://h:1/a b", "http://user@h:1", "http://user:pw@h:1/v1", "ftp://h", "h:abc", "http://.", "http://..",
        "http://h..x", "http://1.2.3.4.5", "http://999.1.1.1", "http://%41", r"\\server\share", "file:///c:/x",
        "http://h#frag", "http://h?q=1", "mailto:x", "http://h/%zz", "http://h:1//", "HTTP://H:1/V1/", "//h:1",
        "http:/h", "http:\\\\h", "https://h:443", "http://h:80/v1", "http://127.1", "http://0x7f.1", "http://[fe80::1%25eth0]",
    ];
    let hosts = [
        "-h", "h-", "_h", "1.2.3.4.5", "999.1.1.1", "0177.0.0.1", "1", "4294967295", "4294967296", "0", "1.2.3",
        "01.02.03.04", "0x7fffffff", "256.1.1.1", "1.2.3.256", "h.", ".h", "İ", "ı", "ß.de", "XN--BCHER-KVA.example",
        "h!", "h$", "h&", "h'", "h(", "h*", "h+", "h,", "h;", "h=", "h~", "h\"", "h<", "h{", "h|", "h^", "h`",
        "Ⅻ.test", "ｈｅｌｌｏ.test", "h\u{a0}x", "h\u{200b}x", "[::1", "[0:0:0:0:0:0:0:1]", "[fe80::1%eth0]",
        "[::ffff:1.2.3.4]", "[v1.x]", "[1::2::3]", "[ABCD::1]", "[::1]x", "::1", "LOCALHOST", "123", "1.2.3.4.",
        "0x1.0x2.0x3.0x4", "1.2.3.0x4", "08.1.1.1", "1e1.1.1.1", "a.1", "1.a", "x.-h", "Ab-Cd.EF", "tr.İstanbul",
        "ΑΒΓ.test", "Αx", "ŞA", "Aİ", "ÉCOLE.fr", "École", "A_B.ü", "A..ü", "ü.", "ü..", "a!ü", "日本.jp", "h\u{301}",
        "h:65536", "h:00080", "h:+1", "h:-1", "@h", "a@b@h", "u@", ":1", "[]", "[::1]:99999", "[::1]:", "h:1:2",
        "h::1", "h%2ex", "h/%zz", "h/a<b", "h/a\\b", "h\\x", "h?q=a b", "h#a b", "x.local.",
    ];
    let mut script = vec![json!({"op": "lang", "value": "tr"})];
    for url in odd.iter().map(|u| (*u).to_owned()).chain(hosts.iter().map(|h| format!("http://{h}"))) {
        script.push(json!({"op": "draft", "preset": "custom", "url": url, "key": "", "model": null,
            "http": [["GET", "/v1/models", 200, r#"{"data":[{"id":"ok"}]}"#]]}));
    }
    run_scenario(&exe, &script);
    // Default names in English.
    run_scenario(&exe, &[
        json!({"op": "lang", "value": "en"}),
        add_op("lan", None, None, None, None, true, true),
        add_op("custom", Some(""), Some("https://x.test/v1"), None, None, true, false),
        json!({"op": "update", "index": 1, "name": "  ", "url": "https://x.test/v2", "key": "", "model": "", "async": true}),
        vm_op(true, "http://h:9", "m", false, &[]),
        json!({"op": "sync"}),
        json!({"op": "remove", "index": 0}),
        json!({"op": "sync"}),
        json!({"op": "file"}),
    ]);
}
