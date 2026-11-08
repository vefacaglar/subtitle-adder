//! `sa-shot --window settings`: opens the real Settings window over in-memory fakes with representative content
//! (`--page`, `--scene`), so both layouts can be checked as PNGs without the user's data.
//!
//! Scenes: `default`; `providers` (three saved providers, one active cloud provider, test notices); `edit` (Windows:
//! the form editing a provider; macOS: the editor sheet); `confirm` (a model delete confirmation); `downloaded`
//! (models on disk, one in use, one being deleted, a delete error).

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use sa_core::cancel::CancellationToken;
use sa_core::dispatch::{QueueDispatcher, UiDispatcher};
use sa_core::engine_types::{EngineError, EngineModel};
use sa_core::models::{ModelCatalog, ModelCatalogBackend, ModelCatalogOptions, fallback_models};
use sa_core::settings::SettingsStore;
use sa_providers::{ProviderStore, TranslationProviderProfile};
use uuid::Uuid;

use super::host::SettingsWindowHost;
use super::model::Flavor;
use super::testing::{Fakes, fake_deps_with};

/// Keeps the window, its model and the fakes alive while sa-shot renders.
pub struct Shot {
    pub host: Rc<SettingsWindowHost>,
    pub fakes: Fakes,
    pub ui: Arc<QueueDispatcher>,
}

struct DownloadedBackend;

impl ModelCatalogBackend for DownloadedBackend {
    fn models(&self, _cancel: &CancellationToken) -> Result<Vec<EngineModel>, EngineError> {
        let on_disk = ["small", "large-v3", "speaker-diarization", "madlad-3b", "qwen3.5-4b", "nllb-1.3b"];
        Ok(fallback_models()
            .into_iter()
            .map(|mut m| {
                if on_disk.contains(&m.id.as_str()) {
                    m.downloaded = true;
                    m.disk_mb = Some(m.size_mb);
                }
                m
            })
            .collect())
    }
    fn delete_model(&self, id: &str, _cancel: &CancellationToken) -> Result<i64, EngineError> {
        if id == "nllb-1.3b" {
            return Err(EngineError::Failed("Model klasörü kullanımda: silinemedi.".into()));
        }
        // Stays "deleting" for the screenshot.
        std::thread::sleep(Duration::from_secs(3600));
        Err(EngineError::Cancelled)
    }
}

fn profile(name: &str, preset: &str, url: &str, model: &str, discovered: &[&str]) -> TranslationProviderProfile {
    TranslationProviderProfile {
        id: Uuid::new_v4(),
        name: name.into(),
        preset_id: preset.into(),
        base_url: url.into(),
        model: model.into(),
        async_translation: preset == "lan",
        discovered_models: discovered.iter().map(|s| s.to_string()).collect(),
    }
}

impl Shot {
    pub fn pump(&self) {
        self.ui.run_until(Duration::from_millis(150), || false);
        self.host.push_now();
    }
}

/// Opens the Settings window of the compiled tree on `page` (`general`, `translation`, `appearance`, `models`).
pub fn open(page: &str, scene: Option<&str>, ui_language: &str, dark: bool) -> Shot {
    let ui = QueueDispatcher::new();
    let dispatcher: Arc<dyn UiDispatcher> = ui.clone();
    let fonts = ["Arial", "Avenir Next", "Georgia", "Helvetica Neue", "Menlo", "Segoe UI", "Verdana"].map(String::from).to_vec();
    let flavor = Flavor::native();
    let scene = scene.unwrap_or("default");
    let options = if flavor == Flavor::Mac { ModelCatalogOptions::MAC } else { ModelCatalogOptions::WINDOWS };
    let catalog = Rc::new(ModelCatalog::with_options(Some(Arc::new(DownloadedBackend)), dispatcher.clone(), options));
    let fakes = fake_deps_with(dispatcher.clone(), fonts, ui_language, catalog.clone());
    fakes.settings.set_string("appTheme", if dark { "dark" } else { "light" });
    catalog.load();
    ui.run_until(Duration::from_secs(2), || catalog.has_loaded());
    catalog.set_active_models(HashSet::from(["large-v3".to_owned()]));
    let p = &fakes.providers;
    if matches!(scene, "providers" | "edit") {
        *fakes.vm.offline.borrow_mut() = false;
        let a = p.add(profile("OpenCode Go", "opencode-go", "https://opencode.ai/inference/go/openai/v1", "glm-5.3-flash", &[]), Some("sk-oc-1234567890"), false);
        let b = p.add(profile("Ofis hesabı", "openai", "https://api.openai.com/v1", "gpt-4o-mini", &["gpt-4o-mini", "gpt-4.1-mini"]), None, false);
        let c = p.add(profile("Ev sunucusu", "lan", "http://192.168.1.50:11434", "gemma4:e4b", &["gemma4:e4b", "qwen3.5:4b"]), None, false);
        p.set_active(Some(a.id));
        if flavor == Flavor::Mac {
            *fakes.vm.cloud_name.borrow_mut() = Some("OpenCode Go".into());
        }
        let _ = (b, c);
    }
    if flavor == Flavor::Windows && matches!(scene, "providers" | "edit") {
        fakes.settings.set_string("externalLlmUrl", "192.168.1.50:11434");
        fakes.settings.set_string("externalLlmModel", "gemma4:e4b");
    }

    let host = SettingsWindowHost::new(fakes.deps.clone());
    host.show(Some(page)).expect("settings window");
    let shot = Shot { host, fakes, ui };
    shot.pump();
    let model = shot.host.model().expect("model");
    match scene {
        "providers" if flavor == Flavor::Mac => {
            let ids: Vec<Uuid> = shot.fakes.providers.providers().iter().map(|p| p.id).collect();
            *shot.fakes.tester.answer.lock() = Some(super::deps::TestOutcome {
                success: true,
                message: sa_core::l!("Bağlantı başarılı (%@ · %lld ms · %lld uygun model bulundu)", "192.168.1.50", 12i64, 2i64),
                models: vec!["gemma4:e4b".into(), "qwen3.5:4b".into()],
            });
            model.test_provider(ids[2]);
            shot.ui.run_until(Duration::from_secs(2), || !model.view().provider_rows[2].notice_text.is_empty());
        }
        "providers" => {
            *shot.fakes.tester.answer.lock() = Some(super::deps::TestOutcome {
                success: true,
                message: sa_core::l!("Bağlantı başarılı (%@ · %lld ms · %lld uygun model bulundu)", "opencode.ai", 184i64, 5i64),
                models: vec![],
            });
            let id = shot.fakes.providers.providers()[0].id;
            model.test_provider(id);
            shot.ui.run_until(Duration::from_secs(2), || model.view().provider_status_open);
        }
        "edit" => {
            let id = shot.fakes.providers.providers()[1].id;
            if flavor == Flavor::Mac {
                model.add_provider();
                model.form_select_preset(0);
            } else {
                model.edit_provider(id);
            }
        }
        "confirm" | "downloaded" if page == "models" => {
            if scene == "confirm" {
                model.request_model_delete("small");
            } else {
                shot.fakes.deps.catalog.delete("nllb-1.3b", Box::new(|_| {}));
                let c = shot.fakes.deps.catalog.clone();
                shot.ui.run_until(Duration::from_secs(2), || !c.delete_errors().is_empty());
                shot.fakes.deps.catalog.delete("madlad-3b", Box::new(|_| {}));
            }
        }
        _ => {}
    }
    shot.pump();
    shot
}
