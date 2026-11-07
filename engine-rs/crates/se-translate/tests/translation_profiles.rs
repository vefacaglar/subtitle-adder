//! Pure parts of `tests/test_translation_profiles.py` (profile detection, messages, generation options,
//! HY-MT templates). The `LlamaTranslator`-driven tests of that file are ported with the LLM translator.

use se_core::json::dumps;
use se_translate::profiles::{TranslationProfile, build_hy_mt_prompt, profile_for_model};

#[test]
fn test_profile_detection_including_tags_quantizations_and_distinct_variants() {
    for (model, family) in [
        ("gemma3:27b", "gemma-legacy"),
        ("google/gemma-3-12b-it", "gemma-legacy"),
        ("bartowski/gemma-2-9b-it-GGUF:Q4_K_M", "gemma-legacy"),
        ("gemma3n:e4b", "gemma-legacy"),
        ("gemma4:12b", "gemma4"),
        ("unsloth/gemma-4-E4B-it-Q4_K_M.gguf", "gemma4"),
        ("Qwen/Qwen3-4B", "qwen3"),
        ("qwen3:8b", "qwen3"),
        ("Qwen/Qwen3.5-4B", "qwen3.5"),
        ("qwen3.5:9b", "qwen3.5"),
        ("translategemma:4b", "translategemma"),
        ("google/translate_gemma-4b-it", "translategemma"),
        ("hf.co/tencent/HY-MT1.5-1.8B-GGUF:Q4_K_M", "hy-mt1.5"),
        ("tencent/Hunyuan-MT-7B", "hunyuan-mt"),
        ("Hunyuan-MT-Chimera-7B", "generic"),
        ("Qwen3-4B-Instruct-2507", "generic"),
        ("Qwen3-30B-A3B-Thinking-2507", "generic"),
        ("Qwen3-VL-8B", "generic"),
        ("Qwen2.5:7b", "generic"),
        ("qwen3.6:27b", "generic"),
        ("gemma40:4b", "generic"),
        ("custom-model", "generic"),
        // Catalog ids.
        ("gemma-4-e4b", "gemma4"),
        ("gemma-4-12b", "gemma4"),
        ("qwen3.5-4b", "qwen3.5"),
        ("qwen3.5-9b", "qwen3.5"),
        ("madlad-3b", "generic"),
    ] {
        assert_eq!(profile_for_model(model).name, family, "{model}");
    }
}

/// Parity with the Python engine over generated names (fixture from `fixtures/gen_profile_names.py`).
#[test]
fn test_profile_detection_matches_python_on_generated_names() {
    let text = include_str!("fixtures/profile_names.json");
    let rows = se_core::json::parse(text).unwrap();
    let rows = rows.as_array().unwrap();
    assert!(rows.len() > 1000);
    let mut mismatches = Vec::new();
    for row in rows {
        let row = row.as_array().unwrap();
        let (name, family) = (row[0].as_str().unwrap(), row[1].as_str().unwrap());
        let got = profile_for_model(name).name;
        if got != family {
            mismatches.push(format!("{name:?}: python {family}, rust {got}"));
        }
    }
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches[..mismatches.len().min(20)].join("\n"));
}

#[test]
fn test_profile_repr_is_the_python_dataclass_repr() {
    let cases = [
        ("custom", "TranslationProfile(name='generic', single_user=False, disable_thinking=False, soft_no_think=False, temperature=0.2, top_p=0.9, top_k=None, repetition_penalty=None)"),
        ("gemma4:12b", "TranslationProfile(name='gemma4', single_user=False, disable_thinking=True, soft_no_think=False, temperature=0.2, top_p=0.9, top_k=None, repetition_penalty=None)"),
        ("qwen3.5:4b", "TranslationProfile(name='qwen3.5', single_user=False, disable_thinking=True, soft_no_think=False, temperature=0.2, top_p=0.9, top_k=None, repetition_penalty=None)"),
        ("qwen3:8b", "TranslationProfile(name='qwen3', single_user=False, disable_thinking=True, soft_no_think=True, temperature=0.2, top_p=0.9, top_k=None, repetition_penalty=None)"),
        ("gemma3:27b", "TranslationProfile(name='gemma-legacy', single_user=True, disable_thinking=False, soft_no_think=False, temperature=0.2, top_p=0.9, top_k=None, repetition_penalty=None)"),
        ("translategemma:4b", "TranslationProfile(name='translategemma', single_user=True, disable_thinking=False, soft_no_think=False, temperature=0.2, top_p=0.9, top_k=None, repetition_penalty=None)"),
        ("hy-mt1.5:7b", "TranslationProfile(name='hy-mt1.5', single_user=True, disable_thinking=False, soft_no_think=False, temperature=0.7, top_p=0.6, top_k=20, repetition_penalty=1.05)"),
        ("tencent/Hunyuan-MT-7B", "TranslationProfile(name='hunyuan-mt', single_user=True, disable_thinking=False, soft_no_think=False, temperature=0.7, top_p=0.6, top_k=20, repetition_penalty=1.05)"),
    ];
    for (model, repr) in cases {
        assert_eq!(profile_for_model(model).py_repr(), repr, "{model}");
    }
    assert_eq!(TranslationProfile::default(), profile_for_model("x"));
}

#[test]
fn test_generation_options_golden() {
    let opts = |model: &str, ollama: bool| dumps(&profile_for_model(model).generation_options(ollama), false);
    for model in ["custom-model", "gemma3:27b", "translategemma:4b"] {
        assert_eq!(opts(model, false), r#"{"temperature": 0.2, "top_p": 0.9, "max_tokens": 1536}"#);
        assert_eq!(opts(model, true), r#"{"options": {"temperature": 0.2, "top_p": 0.9, "num_predict": 1536}}"#);
    }
    for model in ["gemma4:12b", "qwen3.5:4b", "qwen3:8b"] {
        assert_eq!(
            opts(model, false),
            r#"{"temperature": 0.2, "top_p": 0.9, "max_tokens": 1536, "chat_template_kwargs": {"enable_thinking": false}}"#
        );
        assert_eq!(opts(model, true), r#"{"options": {"temperature": 0.2, "top_p": 0.9, "num_predict": 1536}, "think": false}"#);
    }
    for model in ["hy-mt1.5:7b", "tencent/Hunyuan-MT-7B"] {
        assert_eq!(opts(model, false), r#"{"temperature": 0.7, "top_p": 0.6, "max_tokens": 1536}"#);
        assert_eq!(
            opts(model, true),
            r#"{"options": {"temperature": 0.7, "top_p": 0.6, "top_k": 20, "repeat_penalty": 1.05, "num_predict": 1536}}"#
        );
    }
}

#[test]
fn test_chat_messages_roles_and_soft_switch() {
    for (model, roles, soft_switch) in [
        ("gemma3:27b", vec!["user"], false),
        ("gemma-4-e4b", vec!["system", "user"], false),
        ("qwen3:8b", vec!["system", "user"], true),
        ("qwen3.5:4b", vec!["system", "user"], false),
        ("custom-model", vec!["system", "user"], false),
    ] {
        let messages = profile_for_model(model).chat_messages("SYSTEM", "[1] Hello.");
        let got: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(got, roles, "{model}");
        let last = &messages.last().unwrap().content;
        assert!(last.contains("[1] Hello."));
        assert_eq!(last.ends_with("/no_think"), soft_switch, "{model}");
        assert!(messages[0].content.contains("SYSTEM"));
    }
    let single = profile_for_model("gemma3:27b").chat_messages("S", "U");
    assert_eq!(single[0].content, "S\n\nU");
    let qwen = profile_for_model("qwen3:8b").chat_messages("S", "U");
    assert_eq!(qwen[1].content, "U\n/no_think");
}

#[test]
fn test_hy_mt_chinese_pair_uses_version_specific_template() {
    let lines = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(build_hy_mt_prompt(&lines(&["Hello"]), "zh", Some("en"), None, false).starts_with("将以下文本翻译为中文"));
    assert!(build_hy_mt_prompt(&lines(&["你好"]), "tr", Some("zh"), None, true).starts_with("把下面的文本翻译成土耳其语"));
}

#[test]
fn test_hy_mt_prompt_templates_verbatim() {
    let lines: Vec<String> = vec!["Hello.".into(), "Welcome.".into()];
    assert_eq!(
        build_hy_mt_prompt(&lines, "tr", Some("en"), None, true),
        "Translate the following segment into Turkish, without additional explanation.\n\n[1] Hello.\n[2] Welcome."
    );
    assert_eq!(
        build_hy_mt_prompt(&lines[..1], "zh", None, None, true),
        "把下面的文本翻译成中文，不要额外解释。\n\n[1] Hello."
    );
    assert_eq!(
        build_hy_mt_prompt(&lines[..1], "zh", None, None, false),
        "将以下文本翻译为中文，注意只需要输出翻译后的结果，不要额外解释：\n\n[1] Hello."
    );
    let context: Vec<String> = vec![
        "The director has arrived.".into(),
        "Wait for her.".into(),
        "Previous translations (terminology reference only):".into(),
        "Müdür Hanım geldi.".into(),
        "Onu bekleyin.".into(),
    ];
    assert_eq!(
        build_hy_mt_prompt(&["Please come in, director.".to_string()], "tr", Some("en"), Some(&context), false),
        "The director has arrived.\nWait for her.\nPrevious translations (terminology reference only):\nMüdür Hanım geldi.\nOnu bekleyin.\n参考上面的信息，把下面的文本翻译成土耳其语，注意不需要翻译上文，也不要额外解释：\n[1] Please come in, director."
    );
    // An empty context list is falsy in Python.
    assert_eq!(
        build_hy_mt_prompt(&lines[..1], "tr", None, Some(&[]), false),
        "Translate the following segment into Turkish, without additional explanation.\n\n[1] Hello."
    );
}
