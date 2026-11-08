//! Data compatibility with the C# app's `translation-providers.json` (win-providers §2, §4.2).
//!
//! `tools/fixtures/ProvidersFixture/csharp-translation-providers.json` was written by System.Text.Json with the C#
//! app's options (`ProvidersFixture write`). When the fixture is built (`dotnet build -c Release` in that folder) the
//! file the Rust app writes is also read back by the same C# deserializer.

#![cfg(windows)]

use std::path::PathBuf;
use std::sync::Arc;

use sa_enginekit::launch::{EngineLaunchSettings, EngineLaunchState};
use sa_providers::secrets::EncryptedSecretStore;
use sa_providers::win::WinProviderManager;
use sa_providers::{ProviderKeyRead, ProviderStore};
use uuid::Uuid;

const FIXTURE: &str = include_str!("../../../tools/fixtures/ProvidersFixture/csharp-translation-providers.json");
const KEY: &str = "oc_csharp_plaintext_0123456789";

fn fixture_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/ProvidersFixture/bin/Release/net10.0/ProvidersFixture.exe")
}

fn open(dir: &std::path::Path) -> Arc<WinProviderManager> {
    WinProviderManager::with_launch_state(
        dir.join("translation-providers.json"),
        EncryptedSecretStore::for_app_data(dir),
        None,
        EngineLaunchState::new(EngineLaunchSettings::default()),
    )
}

#[test]
fn a_csharp_written_file_is_read_migrated_and_rewritten_in_the_same_shape() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("translation-providers.json");
    std::fs::write(&file, FIXTURE).unwrap();
    let m = open(dir.path());

    let providers = m.providers();
    assert_eq!(providers.len(), 2);
    let lan = &providers[0];
    assert_eq!(lan.id, Uuid::parse_str("1bed0ce2-2d54-4a5d-a9fa-52fd7ad76929").unwrap());
    assert_eq!(lan.name, "Yerel Ağ (Ollama / LM Studio)");
    assert_eq!(lan.discovered_models, vec!["gemma4:e2b", "qwen3.5:4b"]);
    let oc = &providers[1];
    assert_eq!(oc.name, "Şirket <test> & 'quote' + `tick` \"dq\" \\ 😀");
    assert!(!oc.async_translation);
    assert_eq!(m.active_provider_id(), Some(oc.id));
    match m.read_key(oc.id) {
        ProviderKeyRead::Found(k) => assert_eq!(k.expose(), KEY),
        other => panic!("{other:?}"),
    }
    assert_eq!(m.active_external_llm().unwrap().llm.api_key, KEY);

    // Scrubbed rewrite, byte-identical to System.Text.Json apart from the key.
    let rewritten = std::fs::read_to_string(&file).unwrap();
    assert_eq!(rewritten, FIXTURE.replace(KEY, ""));
    for e in std::fs::read_dir(dir.path()).unwrap() {
        let p = e.unwrap().path();
        assert!(!String::from_utf8_lossy(&std::fs::read(&p).unwrap()).contains(KEY), "{}", p.display());
    }

    // The C# deserializer reads what Rust wrote (empty key; everything else intact).
    let exe = fixture_exe();
    if !exe.exists() {
        eprintln!("skipping C# read-back: {} not built", exe.display());
        return;
    }
    let out = std::process::Command::new(&exe).arg("read").arg(&file).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines, vec![
        "active=adf244d7-d14a-4a40-abf1-2da9a43ba655",
        "1bed0ce2-2d54-4a5d-a9fa-52fd7ad76929|Yerel Ağ (Ollama / LM Studio)|lan|http://192.168.68.58:11434||gemma4:12b|True|gemma4:e2b,qwen3.5:4b",
        "adf244d7-d14a-4a40-abf1-2da9a43ba655|Şirket <test> & 'quote' + `tick` \"dq\" \\ 😀|opencode-go|https://opencode.ai/inference/go/openai/v1||glm-5.3-flash|False|",
    ]);
}

#[test]
fn a_provider_added_by_rust_is_readable_by_csharp() {
    let exe = fixture_exe();
    if !exe.exists() {
        eprintln!("skipping: {} not built", exe.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path());
    let mut p = sa_providers::TranslationProviderProfile::from_preset(
        sa_enginekit::presets::TranslationProviderPreset::preset(Some("groq")),
        true,
    );
    p.name = "Groq ğüşiöç".into();
    let stored = m.add(p, Some("gsk-never-in-json"), true);
    let file = dir.path().join("translation-providers.json");
    let out = std::process::Command::new(&exe).arg("read").arg(&file).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        vec![
            format!("active={}", stored.id.hyphenated()),
            format!("{}|Groq ğüşiöç|groq|https://api.groq.com/openai/v1||llama-3.3-70b-versatile|True|", stored.id.hyphenated()),
        ]
    );
}
