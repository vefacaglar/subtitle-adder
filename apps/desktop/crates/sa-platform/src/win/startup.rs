//! The platform steps of the Windows startup sequence (win-ui-main §1.1), in their exact order, so the app's `main`
//! only adds its own steps between them:
//!
//! 1. [`begin`]: install the crash log (step 1), collect the command-line paths (step 2) and run the single-instance
//!    check (step 3). [`Startup::Exit`] means the paths were forwarded to the first instance: exit with code 0 before
//!    any window exists.
//! 2. The app creates the settings store (step 5) and starts the temp cleanup (step 4).
//! 3. [`after_settings`]: file-association self-registration on a worker thread (step 6) and the process UI language
//!    for system dialogs (step 7, `en-US` / `tr-TR`).
//! 4. The app builds its models and the main window, then installs the external-open sink
//!    ([`sa_core::platform::ExternalOpenSource::set_sink`]) and opens [`Primary::initial_paths`] (step 14).

use std::ffi::OsString;
use std::path::PathBuf;

use sa_core::l10n::UiLanguage;
use sa_core::platform::{ExternalOpenSource, InstanceRole};
use sa_core::settings::SettingsStore;

use super::associations::FileAssociationRegistrar;
use super::single_instance::{SingleInstanceCoordinator, command_line_paths};

/// This process owns the UI.
pub struct Primary {
    /// Holds the mutex and runs the pipe server when this is the first instance; dispose it in the shutdown order
    /// (win-ui-main §1.5 step 3).
    pub coordinator: SingleInstanceCoordinator,
    /// Existing files from the command line, as full paths (step 2).
    pub initial_paths: Vec<PathBuf>,
}

pub enum Startup {
    /// The files went to the first instance: exit with code 0 now.
    Exit,
    Primary(Primary),
}

/// Steps 1–3 with the process arguments (`std::env::args_os()`, the program name first).
pub fn begin(args: impl IntoIterator<Item = OsString>) -> Startup {
    super::crash::install();
    begin_with(args, SingleInstanceCoordinator::new())
}

/// Steps 2–3 of [`begin`] with a given coordinator (tests use their own mutex/pipe names; no crash hook).
pub fn begin_with(args: impl IntoIterator<Item = OsString>, mut coordinator: SingleInstanceCoordinator) -> Startup {
    let initial_paths = command_line_paths(args);
    match coordinator.acquire_or_forward(&initial_paths) {
        InstanceRole::Exit => Startup::Exit,
        InstanceRole::Primary => Startup::Primary(Primary { coordinator, initial_paths }),
    }
}

/// Steps 6–7: registers the file associations in the background unless `fileAssociationsDisabled` is true (only
/// for `SubtitleAdder.exe`, errors swallowed), then sets the process UI language of system dialogs.
pub fn after_settings(settings: &dyn SettingsStore, language: UiLanguage) {
    let disabled = settings.get_bool(sa_core::settings::keys::FILE_ASSOCIATIONS_DISABLED);
    FileAssociationRegistrar::register_on_launch_in_background(disabled);
    super::folders::set_process_ui_language(language);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::single_instance::InstanceNames;

    #[test]
    fn a_second_launch_with_files_exits_and_a_plain_one_continues() {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let names = InstanceNames::for_user(&format!("sa startup {}-{n}", std::process::id()));
        let dir = tempfile::tempdir().unwrap();
        let film = dir.path().join("film.mp4");
        std::fs::write(&film, b"x").unwrap();
        let args = |extra: &[&std::path::Path]| {
            std::iter::once(OsString::from("SubtitleAdder.exe")).chain(extra.iter().map(|p| p.as_os_str().to_owned())).collect::<Vec<_>>()
        };

        let first = match begin_with(args(&[&film]), SingleInstanceCoordinator::with_names(names.clone())) {
            Startup::Primary(p) => p,
            Startup::Exit => panic!("the first instance owns the UI"),
        };
        assert!(first.coordinator.is_owner());
        assert_eq!(first.initial_paths, vec![film.clone()]);
        let (tx, rx) = std::sync::mpsc::channel();
        let mut coordinator = first.coordinator;
        coordinator.set_sink(Box::new(move |p| {
            let _ = tx.send(p);
        }));

        let names2 = names.clone();
        let film2 = film.clone();
        let (second_exits, plain_continues) = std::thread::spawn(move || {
            let second = begin_with(args(&[&film2]), SingleInstanceCoordinator::with_names(names2.clone()));
            let plain = begin_with(args(&[]), SingleInstanceCoordinator::with_names(names2));
            (matches!(second, Startup::Exit), matches!(plain, Startup::Primary(ref p) if !p.coordinator.is_owner()))
        })
        .join()
        .unwrap();
        assert!(second_exits);
        assert!(plain_continues);
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), vec![film]);
        coordinator.dispose();
    }
}
