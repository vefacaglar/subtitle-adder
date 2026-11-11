//! Windows-only composition: the C#-compatible stores in `%LOCALAPPDATA%\SubtitleAdder` (profiles.json v3 +
//! profiles.db, media-index.json, translation-providers.json with DPAPI-protected keys, VideoCache), the settings
//! broker hook, the temp-folder cleanup (win-viewmodels §17) and RefreshInitialExternalModels (win-ui-main §1.2).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use sa_core::cancel::CancellationToken;
use sa_core::platform::{FileAssociations, PlatformShell};
use sa_core::settings::{JsonFileSettingsStore, SettingsStore, SettingsSyncHook, defaults, keys};
use sa_core::subscription::Subscription;
use sa_enginekit::external_llm::{HttpFetch, UreqFetch};
use sa_media::win::{BufferedVideoStream, VideoCacheStore, WinMediaIndex};
use parking_lot::Mutex;
use sa_player::ffmpeg_backend::{BufferProbe, FfmpegPlayer};
use sa_player::{FailureKind, LoadGeneration, MediaPlayerBackend, MediaSource, PlayerEvent, ReadSeek, VideoFrame};
use sa_profiles::ProfileStore;
use sa_profiles::win::WinProfileStore;
use sa_providers::ProviderStore;
use sa_providers::win::WinProviderManager;
use sa_sync::wire::{EventKind, StateSyncEvent};
use sa_sync::{NamedPipeBus, SyncBus, Topic, TopicFilter};

use sa_core::l10n::LocalizedMessage;
use sa_viewmodels::providers_ui::{ExternalConnectionNotice, ProviderConnectionTester, TestReport};

use crate::settings_window::deps::{ProviderTester, WinOps, WinSettingsOps, WinTester};

/// The `settings` broker topic (win-core §1.4): publish `SettingsUpdated` after each write, reload on others' events.
pub struct BusSettingsHook {
    pub bus: Arc<dyn SyncBus>,
}

impl SettingsSyncHook for BusSettingsHook {
    fn published(&self, instance_id: &str, path: &Path) {
        let event = StateSyncEvent::new(Topic::Settings, EventKind::SettingsUpdated, instance_id, &path.to_string_lossy());
        self.bus.publish(Topic::Settings, event);
    }
    fn on_remote_change(&self, _reload: Box<dyn Fn() + Send + Sync>) -> Subscription {
        // Not used: `on_remote_event` lets the store apply the C# sender/scope filter itself.
        Subscription::empty()
    }
    fn on_remote_event(&self, handler: Box<dyn Fn(&str, &str) + Send + Sync>) -> Option<Subscription> {
        Some(self.bus.subscribe(
            TopicFilter::Only(Topic::Settings),
            Box::new(move |e: &StateSyncEvent| handler(&e.sender_instance_id, &e.scope_path)),
        ))
    }
}

pub struct WinParts {
    pub json_settings: Arc<JsonFileSettingsStore>,
    pub pipe_bus: Arc<NamedPipeBus>,
    pub profiles: Arc<WinProfileStore>,
    pub media_index: Arc<WinMediaIndex>,
    pub providers: Arc<WinProviderManager>,
    pub cache: Option<Arc<VideoCacheStore>>,
    pub fetch: Arc<dyn HttpFetch>,
    pub associations: Arc<dyn FileAssociations>,
}

impl WinParts {
    pub fn build(
        data_dir: &Path,
        pipe_bus: &Arc<NamedPipeBus>,
        bus: Arc<dyn SyncBus>,
        settings: &dyn SettingsStore,
        json_settings: Arc<JsonFileSettingsStore>,
    ) -> Self {
        let broker: Arc<dyn sa_sync::ProfileWriteBroker> = pipe_bus.clone();
        let profiles = WinProfileStore::new(data_dir.join("profiles.json"), broker);
        let media_index = WinMediaIndex::new(Some(WinMediaIndex::default_path(data_dir)), Some(bus.clone()));
        // DPAPI-protected sidecar (reads the C# app's plaintext keys once and moves them into it), orphan sweep.
        let providers = WinProviderManager::open(data_dir, Some(bus));
        // The encrypted per-profile chunk cache; logged out nothing is cached (no vault key, rule 6).
        let cache = {
            let store = video_cache_store(data_dir.join("VideoCache"), &profiles);
            let gb = settings.get_int(keys::VIDEO_CACHE_MAX_SIZE_GB).unwrap_or(defaults::VIDEO_CACHE_MAX_SIZE_GB).clamp(1, 500);
            if gb != store.max_cache_size_gb() {
                store.set_max_cache_size_gb(gb);
            }
            Some(store)
        };
        Self {
            json_settings,
            pipe_bus: pipe_bus.clone(),
            profiles,
            media_index,
            providers,
            cache,
            fetch: Arc::new(UreqFetch),
            associations: Arc::new(sa_platform::win::associations::FileAssociationRegistrar::new()),
        }
    }

    pub fn tester(&self) -> Arc<dyn ProviderTester> {
        Arc::new(WinTester { providers: self.providers.clone(), fetch: self.fetch.clone() })
    }

    /// The toolbar picker's "Yenile" (C# `TestAndFetchModelsAsync(providerId)` without the storing, which the
    /// picker's `ProvidersModel` does the same way: discovered models, an empty model takes the first).
    pub fn picker_tester(&self) -> Arc<dyn ProviderConnectionTester> {
        Arc::new(WinPickerTester { providers: self.providers.clone(), fetch: self.fetch.clone() })
    }

    pub fn settings_ops(&self) -> Arc<dyn WinSettingsOps> {
        Arc::new(WinOps {
            providers: self.providers.clone(),
            fetch: self.fetch.clone(),
            cache: self.cache.clone(),
            associations: Some(self.associations.clone()),
        })
    }

    /// The window's player: FFmpeg reading existing files through `BufferedVideoStream` over the encrypted SSD
    /// cache (C# `PlayerController`), with the stream's buffered ranges as the transport's buffer bar.
    pub fn player(&self) -> Box<dyn MediaPlayerBackend> {
        Box::new(CachedPlayer { inner: FfmpegPlayer::new(), cache: self.cache.clone() })
    }

    /// win-ui-main §1.2: at launch the active provider's models are fetched once in the background (errors ignored).
    pub fn refresh_initial_external_models(&self) {
        let providers = self.providers.clone();
        let fetch = self.fetch.clone();
        let Some(active) = providers.active_provider() else { return };
        let _ = std::thread::Builder::new().name("sa-initial-models".into()).spawn(move || {
            let _ = providers.test_and_fetch_models(active.id, fetch.as_ref(), &CancellationToken::new());
        });
    }

    /// win-ui-main §1.5 step 3: flush profile writes, dispose the broker, flush and dispose the settings store.
    pub fn shutdown(&self, bus: &dyn SyncBus) {
        let _ = self.profiles.flush_pending_writes(Duration::from_secs(5));
        bus.dispose();
        self.json_settings.flush();
        self.json_settings.dispose();
    }
}

/// The encrypted chunk cache over the profile store. The vault key is read (under the profile store lock) only when
/// a playback stream opens, together with its session generation; every chunk read and write of the stream then
/// compares the store's lock-free generation, so the demux thread never waits for a profile save and a logout or user
/// switch ends the stream's cache session (its key copy zeroized) at the next chunk, already while the change is
/// still in progress.
fn video_cache_store(dir: PathBuf, profiles: &Arc<WinProfileStore>) -> Arc<VideoCacheStore> {
    let (key_profiles, generation_profiles) = (profiles.clone(), profiles.clone());
    VideoCacheStore::with_session_keys(
        dir,
        Arc::new(move || key_profiles.active_vault_key_in_session()),
        Arc::new(move || generation_profiles.settled_session_generation()),
        None,
    )
}

/// Saved-provider tests with the C# messages: `WinProviderManager::test_draft` (raises `IsTesting`), the message shown
/// verbatim for success and failure alike; a cancelled test reports nothing.
struct WinPickerTester {
    providers: Arc<WinProviderManager>,
    fetch: Arc<dyn HttpFetch>,
}

impl ProviderConnectionTester for WinPickerTester {
    fn test(&self, preset_id: &str, base_url: &str, api_key: &str, model: &str, cancel: &CancellationToken) -> TestReport {
        match self.providers.test_draft(preset_id, base_url, api_key, Some(model), self.fetch.as_ref(), cancel) {
            Ok(o) => {
                let message = LocalizedMessage::verbatim(o.message);
                TestReport {
                    notice: Some(if o.success { ExternalConnectionNotice::success(message) } else { ExternalConnectionNotice::error(message) }),
                    models: if o.success { o.models } else { Vec::new() },
                }
            }
            Err(_) => TestReport::default(),
        }
    }

    /// C# `_manager.IsTesting`: the manager counts every test, the Settings window's included.
    fn is_busy(&self) -> bool {
        self.providers.is_testing()
    }
}

/// `FfmpegPlayer` whose existing-file loads go through one shared `BufferedVideoStream` (C# `new
/// BufferedVideoStream(path)` + `MediaSource.CreateFromStream`), so cached playback, the SSD chunk cache and the
/// buffer bar work; any other source (or a file that cannot be opened) goes to FFmpeg's own open, like the C#
/// `CreateFromUri` fallback.
struct CachedPlayer {
    inner: FfmpegPlayer,
    cache: Option<Arc<VideoCacheStore>>,
}

/// The stream shared by the demuxer (reads) and the buffer-bar probe (UI thread, never blocks).
struct SharedStream(Arc<Mutex<BufferedVideoStream>>);

impl std::io::Read for SharedStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.lock().read(buf)
    }
}

impl std::io::Seek for SharedStream {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.lock().seek(pos)
    }
}

impl MediaPlayerBackend for CachedPlayer {
    fn load(&mut self, source: Option<MediaSource>, auto_play: bool) -> LoadGeneration {
        let Some(MediaSource::File(path)) = &source else { return self.inner.load(source, auto_play) };
        if !path.is_file() {
            return self.inner.load(source, auto_play);
        }
        let stream = BufferedVideoStream::open_file(
            path,
            BufferedVideoStream::DEFAULT_CHUNK,
            BufferedVideoStream::DEFAULT_MAX_CACHED_CHUNKS,
            BufferedVideoStream::DEFAULT_READ_AHEAD_CHUNKS,
            self.cache.clone(),
        );
        let Ok(stream) = stream else { return self.inner.load(source, auto_play) };
        let shared = Arc::new(Mutex::new(stream));
        let probe_stream = shared.clone();
        // The last answer is reused while a read holds the stream (a chunk load must not stall the UI thread).
        let last = Arc::new(std::sync::atomic::AtomicU64::new(0f64.to_bits()));
        let probe: BufferProbe = Arc::new(move |ratio| match probe_stream.try_lock() {
            Some(s) => {
                let r = s.buffered_ratio(ratio);
                last.store(r.to_bits(), std::sync::atomic::Ordering::Relaxed);
                r
            }
            None => f64::from_bits(last.load(std::sync::atomic::Ordering::Relaxed)),
        });
        let reader = SharedStream(shared);
        let source = MediaSource::Reader { path: path.clone(), open: Box::new(move || Ok(Box::new(reader) as Box<dyn ReadSeek>)) };
        self.inner.load_with_probe(Some(source), auto_play, Some(probe))
    }
    fn play(&mut self) {
        self.inner.play()
    }
    fn pause(&mut self) {
        self.inner.pause()
    }
    fn seek(&mut self, seconds: f64) {
        self.inner.seek(seconds)
    }
    fn set_volume(&mut self, volume: f64) {
        self.inner.set_volume(volume)
    }
    fn set_muted(&mut self, muted: bool) {
        self.inner.set_muted(muted)
    }
    fn position(&self) -> f64 {
        self.inner.position()
    }
    fn duration(&self) -> f64 {
        self.inner.duration()
    }
    fn is_playing(&self) -> bool {
        self.inner.is_playing()
    }
    fn natural_size(&self) -> Option<(u32, u32)> {
        self.inner.natural_size()
    }
    fn buffered_ratio(&self) -> f64 {
        self.inner.buffered_ratio()
    }
    fn latest_frame(&mut self) -> Option<VideoFrame> {
        self.inner.latest_frame()
    }
    fn set_event_sink(&mut self, sink: Box<dyn Fn(PlayerEvent) + Send + Sync>) {
        self.inner.set_event_sink(sink)
    }
    fn dispose(&mut self) {
        self.inner.dispose()
    }
    fn failure_kind(&self) -> Option<FailureKind> {
        self.inner.failure_kind()
    }
    fn set_planar_output(&mut self, planar: bool) {
        self.inner.set_planar_output(planar)
    }
    fn diagnostics(&self) -> Option<String> {
        self.inner.diagnostics()
    }
}

/// `CleanupTempDirectory()` on a worker at startup (win-viewmodels §17).
pub fn cleanup_temp_directory_in_background(shell: Arc<dyn PlatformShell>, bus: Arc<dyn SyncBus>) {
    let _ = std::thread::Builder::new().name("sa-temp-cleanup".into()).spawn(move || {
        let another = bus.has_other_active_instances() || shell.other_app_instances_running();
        let brokers = sa_sync::total_active_in_process_brokers() > 0;
        cleanup_temp_directory(&shell.engine_work_dir(), another, brokers, SystemTime::now());
    });
}

/// Deletes every file under `dir` (recursively) except broker files while a broker or another instance is alive and,
/// when another instance runs, files changed in the last 2 hours; then removes `dir` itself if empty and no other
/// instance runs. Every error is swallowed (locked files survive).
pub fn cleanup_temp_directory(dir: &Path, another_instance: bool, broker_active: bool, now: SystemTime) {
    if !dir.is_dir() {
        return;
    }
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                stack.push(path);
                continue;
            }
            let lower = path.to_string_lossy().to_lowercase();
            if lower.contains("broker-") && (broker_active || another_instance) {
                continue;
            }
            if another_instance {
                let recent = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| now.duration_since(t).ok())
                    .is_none_or(|age| age < Duration::from_secs(2 * 3600));
                if recent {
                    continue;
                }
            }
            let _ = std::fs::remove_file(&path);
        }
    }
    if !another_instance {
        // Non-recursive, like `Directory.Delete(dir)`: only an empty folder goes.
        let _ = std::fs::remove_dir(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app's wiring of the chunk cache over the real profile store: an open stream yields the profile's cached
    /// plaintext only while the session it opened under lasts; a logout or a switch stops it at the next chunk.
    #[test]
    fn the_video_cache_follows_the_profile_session_of_an_open_stream() {
        struct Unreadable(u64, u64);
        impl std::io::Read for Unreadable {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("source unavailable"))
            }
        }
        impl std::io::Seek for Unreadable {
            fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
                self.1 = match pos {
                    std::io::SeekFrom::Start(p) => p,
                    std::io::SeekFrom::End(d) => (self.0 as i64 + d) as u64,
                    std::io::SeekFrom::Current(d) => (self.1 as i64 + d) as u64,
                };
                Ok(self.1)
            }
        }
        const CHUNK: i32 = 64 * 1024;
        let dir = tempfile::tempdir().unwrap();
        let profiles = WinProfileStore::open(dir.path().join("profiles.json"), sa_sync::InProcessBus::with_scope(&sa_core::text::guid_n()));
        let cache = video_cache_store(dir.path().join("VideoCache"), &profiles);
        let raw: Vec<u8> = (0..3 * CHUNK as usize).map(|i| (i * 31 % 251) as u8).collect();
        let path = dir.path().join("film.mp4").to_string_lossy().into_owned();
        let open = |source: Box<dyn sa_media::win::ReadSeek>| {
            BufferedVideoStream::from_stream(source, false, CHUNK, 2, 0, Some(cache.clone()), None, Some(path.clone())).unwrap()
        };
        let cache_only = || open(Box::new(Unreadable(raw.len() as u64, 0)));
        let c = CHUNK as usize;

        for change in ["logout", "switch"] {
            profiles.register_user(&format!("ali-{change}"), "pw-1234", false).unwrap();
            // First playback fills the profile's cache from the source.
            let mut buf = vec![0u8; raw.len()];
            std::io::Read::read_exact(&mut open(Box::new(std::io::Cursor::new(raw.clone()))), &mut buf).unwrap();
            assert_eq!(buf, raw);

            let mut stream = cache_only();
            let mut first = vec![0u8; c];
            std::io::Read::read_exact(&mut stream, &mut first).unwrap();
            assert_eq!(first, raw[..c]);
            match change {
                "logout" => profiles.logout_user(),
                _ => profiles.register_user(&format!("veli-{change}"), "pw-5678", false).unwrap(),
            }
            let mut next = vec![0u8; c];
            assert!(std::io::Read::read(&mut stream, &mut next).is_err(), "{change}: no cached plaintext of the previous session");
            assert_eq!(stream.encrypted_cache_read_count(), 1);
            if change == "switch" {
                profiles.logout_user();
            }
        }
    }

    #[test]
    fn cleanup_keeps_broker_files_while_a_broker_runs_and_recent_files_of_other_instances() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("SubtitleAdder");
        std::fs::create_dir_all(dir.join("broker-abc")).unwrap();
        std::fs::create_dir_all(dir.join("translations")).unwrap();
        std::fs::write(dir.join("a.wav"), b"x").unwrap();
        std::fs::write(dir.join("broker-abc").join("slot-1"), b"x").unwrap();
        std::fs::write(dir.join("translations").join("chunk-1.json"), b"{}").unwrap();

        // Another instance runs: recent files survive, broker files too.
        cleanup_temp_directory(&dir, true, false, SystemTime::now());
        assert!(dir.join("a.wav").exists());
        assert!(dir.join("broker-abc").join("slot-1").exists());

        // Two hours later the other instance's old files go, the broker's stay.
        cleanup_temp_directory(&dir, true, false, SystemTime::now() + Duration::from_secs(3 * 3600));
        assert!(!dir.join("a.wav").exists());
        assert!(!dir.join("translations").join("chunk-1.json").exists());
        assert!(dir.join("broker-abc").join("slot-1").exists());

        // Alone with an in-process broker: broker files stay, so the folder stays.
        cleanup_temp_directory(&dir, false, true, SystemTime::now());
        assert!(dir.join("broker-abc").join("slot-1").exists());
        assert!(dir.exists());

        // Alone, no broker: everything goes; the folder only once it is empty (sub-folders keep it).
        cleanup_temp_directory(&dir, false, false, SystemTime::now());
        assert!(!dir.join("broker-abc").join("slot-1").exists());
    }
}
