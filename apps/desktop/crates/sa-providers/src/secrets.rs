//! Encrypted secret store (UNIFIED.md §5.2; win-providers §4; mac-librarykit §13.2).
//!
//! Sidecar `{"keys":{"<entry>":"<b64 nonce‖tag‖ct>"},"version":1}`: AES-256-GCM, **AAD = UTF-8 of the entry key**,
//! fresh nonce per write, atomic write under a lock (Windows named mutex `Local\SubtitleAdder.ProviderKeys.<h16>`;
//! macOS `flock` on `<file>.lock`, 5 s), damaged sidecar set aside (`…corrupt-<yyyyMMdd-HHmmss>[-N].json`).
//! Namespaces: `translation-provider.<UUID>` → `translation-provider-keys.json` (entry = lowercase UUID with dashes);
//! `profile-remember.<lower user>` → `profile-remember-keys.json` (macOS only). Plaintext buffers are zeroized; secrets
//! never appear in `Debug`, logs, broker events or crash reports.
//!
//! Reading never repairs anything: a sidecar that cannot be read or parsed, or an entry that cannot be decrypted, is
//! [`SecretRead::Unavailable`] (never `Absent`), so callers never overwrite or delete on a guess. `h16` of the Windows
//! mutex = the first 16 lowercase hex characters of SHA-256 over the lowercased full sidecar path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aes_gcm::aead::{AeadInOut, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce, Tag};
use base64::Engine as _;
use parking_lot::Mutex;
use sa_core::secrets::{SecretBytes, SecretKey32, SecretRead, SecretStore};
use zeroize::Zeroizing;

use crate::fileio::{self, CrossProcessLock, ReadOutcome};

/// The 32-byte data key.
pub type DataKey = SecretKey32;

/// Why the data key is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtectorError {
    /// The key file/blob exists but cannot be used (wrong owner/mode/size, DPAPI failure). Never "repaired".
    #[error("data key unavailable: {0}")]
    Unavailable(String),
    #[error("I/O error: {0}")]
    Io(String),
}

/// Protects the data key only.
pub trait SecretProtector: Send + Sync {
    /// `Ok(None)` = no key yet; `Err` = unavailable.
    fn load(&self) -> Result<Option<DataKey>, ProtectorError>;
    /// Race-safe create (`EEXIST` → read the winner's key).
    fn create(&self) -> Result<DataKey, ProtectorError>;
}

fn random_key() -> Result<DataKey, ProtectorError> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut bytes[..]).map_err(|e| ProtectorError::Io(e.to_string()))?;
    Ok(SecretKey32::new(*bytes))
}

/// `O_CREAT|O_EXCL` (unix: mode 0600, `O_NOFOLLOW|O_CLOEXEC`) + full write + fsync. A failed write removes the file
/// again so a half-written key never stays behind.
fn create_exclusive(path: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut f = options.open(path)?;
    let written = f.write_all(content).and_then(|()| f.sync_all());
    drop(f);
    if written.is_err() {
        let _ = std::fs::remove_file(path);
    }
    written
}

/// Publishes `content` at `path` only if nothing is there yet: written to a unique temp file next to it, then
/// hard-linked into place (atomic, `EEXIST` when another process won), then the temp is removed. Readers never see a
/// half-written key. On a file system without hard links the key is created in place with `O_CREAT|O_EXCL` instead
/// (mac-librarykit §13.2; a racing reader may then briefly find it incomplete, which reads as unavailable and is
/// never repaired). `Ok(false)` = the path already existed.
fn publish_new_file(path: &Path, content: &[u8]) -> std::io::Result<bool> {
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    if !dir.as_os_str().is_empty() {
        std::fs::create_dir_all(&dir)?;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = dir.join(format!(".{name}.new-{}", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        create_exclusive(&temp, content)?;
        match std::fs::hard_link(&temp, path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            // A symlink (or anything else) already at `path` reports AlreadyExists above; any other failure is a file
            // system that cannot link: create the file directly, still exclusively.
            Err(_) => match create_exclusive(path, content) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(e) => Err(e),
            },
        }
    })();
    let _ = std::fs::remove_file(&temp);
    result
}

/// Windows: `translation-provider-keys.dpapi` = `CryptProtectData(CurrentUser, entropy
/// "SubtitleAdder.TranslationProviderKeys.v1", description "SubtitleAdder provider keys", CRYPTPROTECT_UI_FORBIDDEN)`.
#[cfg(windows)]
pub struct DpapiProtector {
    pub path: PathBuf,
}

#[cfg(windows)]
impl DpapiProtector {
    pub const ENTROPY: &'static str = "SubtitleAdder.TranslationProviderKeys.v1";
    pub const DESCRIPTION: &'static str = "SubtitleAdder provider keys";

    pub fn new(app_data_dir: &Path) -> Self {
        Self { path: app_data_dir.join("translation-provider-keys.dpapi") }
    }

    /// `CryptProtectData` (CurrentUser scope, UI forbidden) with the entropy and description above.
    pub fn protect(data: &[u8]) -> Result<Vec<u8>, ProtectorError> {
        dpapi::run(data, true)
    }

    /// `CryptUnprotectData` with the entropy above; the output buffer is wiped before `LocalFree`.
    pub fn unprotect(blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, ProtectorError> {
        dpapi::run(blob, false).map(Zeroizing::new)
    }
}

#[cfg(windows)]
mod dpapi {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use windows::core::HSTRING;

    use super::{DpapiProtector, ProtectorError};

    pub(super) fn run(data: &[u8], protect: bool) -> Result<Vec<u8>, ProtectorError> {
        let mut input = data.to_vec();
        let mut entropy = DpapiProtector::ENTROPY.as_bytes().to_vec();
        let in_blob = CRYPT_INTEGER_BLOB { cbData: input.len() as u32, pbData: input.as_mut_ptr() };
        let entropy_blob = CRYPT_INTEGER_BLOB { cbData: entropy.len() as u32, pbData: entropy.as_mut_ptr() };
        let mut out = CRYPT_INTEGER_BLOB::default();
        let description = HSTRING::from(DpapiProtector::DESCRIPTION);
        // SAFETY: the input blobs point into live Vecs for the duration of the call; the output blob is allocated
        // by DPAPI and released with LocalFree below.
        let result = unsafe {
            if protect {
                CryptProtectData(&in_blob, &description, Some(&entropy_blob), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            } else {
                CryptUnprotectData(&in_blob, None, Some(&entropy_blob), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            }
        };
        zeroize::Zeroize::zeroize(&mut input);
        let produced = if result.is_ok() && !out.pbData.is_null() {
            // SAFETY: DPAPI returned `cbData` valid bytes at `pbData`.
            let slice = unsafe { std::slice::from_raw_parts_mut(out.pbData, out.cbData as usize) };
            let copy = slice.to_vec();
            slice.fill(0);
            Some(copy)
        } else {
            None
        };
        if !out.pbData.is_null() {
            // SAFETY: the buffer was allocated by DPAPI with LocalAlloc.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(out.pbData as *mut core::ffi::c_void)));
            }
        }
        match (result, produced) {
            (Ok(()), Some(bytes)) => Ok(bytes),
            (Err(e), _) => Err(ProtectorError::Unavailable(format!("DPAPI: {}", e.message()))),
            (Ok(()), None) => Err(ProtectorError::Unavailable("DPAPI returned no data".into())),
        }
    }
}

#[cfg(windows)]
impl SecretProtector for DpapiProtector {
    fn load(&self) -> Result<Option<DataKey>, ProtectorError> {
        let blob = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ProtectorError::Io(e.to_string())),
        };
        let plain = Self::unprotect(&blob)?;
        SecretKey32::from_slice(&plain)
            .map(Some)
            .ok_or_else(|| ProtectorError::Unavailable(format!("data key has {} bytes, expected 32", plain.len())))
    }

    fn create(&self) -> Result<DataKey, ProtectorError> {
        let key = random_key()?;
        let blob = Self::protect(key.expose())?;
        match publish_new_file(&self.path, &blob) {
            Ok(true) => Ok(key),
            Ok(false) => self.load()?.ok_or_else(|| ProtectorError::Unavailable("data key vanished".into())),
            Err(e) => Err(ProtectorError::Io(e.to_string())),
        }
    }
}

/// macOS: `translation-provider-keys.key`, exactly 32 raw bytes, created with
/// `O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC` mode 0600 + fsync; every read checks a regular file owned by
/// `getuid()`, `mode & 0o077 == 0`, size 32. Optional Time Machine exclusion. **Never the Keychain.**
#[cfg(unix)]
pub struct KeyFileProtector {
    pub path: PathBuf,
}

#[cfg(unix)]
impl KeyFileProtector {
    pub fn new(app_data_dir: &Path) -> Self {
        Self { path: app_data_dir.join("translation-provider-keys.key") }
    }
}

#[cfg(unix)]
impl SecretProtector for KeyFileProtector {
    fn load(&self) -> Result<Option<DataKey>, ProtectorError> {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.path);
        let mut file = match file {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                return Err(ProtectorError::Unavailable("key file is a symbolic link".into()));
            }
            Err(e) => return Err(ProtectorError::Io(e.to_string())),
        };
        let meta = file.metadata().map_err(|e| ProtectorError::Io(e.to_string()))?;
        // SAFETY: getuid never fails.
        let uid = unsafe { libc::getuid() };
        if !meta.file_type().is_file() {
            return Err(ProtectorError::Unavailable("key file is not a regular file".into()));
        }
        if meta.uid() != uid {
            return Err(ProtectorError::Unavailable("key file is owned by another user".into()));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(ProtectorError::Unavailable("key file is readable by others".into()));
        }
        if meta.len() != 32 {
            return Err(ProtectorError::Unavailable(format!("key file has {} bytes, expected 32", meta.len())));
        }
        let mut buf = Zeroizing::new([0u8; 32]);
        file.read_exact(&mut buf[..]).map_err(|e| ProtectorError::Io(e.to_string()))?;
        Ok(Some(SecretKey32::new(*buf)))
    }

    fn create(&self) -> Result<DataKey, ProtectorError> {
        let key = random_key()?;
        match publish_new_file(&self.path, key.expose()) {
            Ok(true) => {
                #[cfg(target_os = "macos")]
                exclude_from_time_machine(&self.path);
                Ok(key)
            }
            Ok(false) => self.load()?.ok_or_else(|| ProtectorError::Unavailable("key file vanished".into())),
            Err(e) => Err(ProtectorError::Io(e.to_string())),
        }
    }
}

/// The sticky Time Machine exclusion `tmutil addexclusion` sets: the xattr
/// `com.apple.metadata:com_apple_backup_excludeItem` holding a binary plist of the string `com.apple.backupd`
/// (mac-librarykit §13.2: a backup must not carry the data key next to the ciphertext). Best effort.
#[cfg(any(target_os = "macos", test))]
const TIME_MACHINE_EXCLUDE_XATTR: &str = "com.apple.metadata:com_apple_backup_excludeItem";

/// `bplist00` + ASCII string object (`0x5F`, length 17 as a 1-byte int) + offset table + 32-byte trailer.
#[cfg(any(target_os = "macos", test))]
fn time_machine_exclude_plist() -> Vec<u8> {
    let value = b"com.apple.backupd";
    let mut p = b"bplist00".to_vec();
    p.extend_from_slice(&[0x5F, 0x10, value.len() as u8]);
    p.extend_from_slice(value);
    let offset_table = p.len() as u8;
    p.push(8); // the only object starts right after the header
    p.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 1]); // unused(5), sort version, offset int size, object ref size
    p.extend_from_slice(&1u64.to_be_bytes()); // number of objects
    p.extend_from_slice(&0u64.to_be_bytes()); // top object
    p.extend_from_slice(&(offset_table as u64).to_be_bytes());
    p
}

#[cfg(target_os = "macos")]
fn exclude_from_time_machine(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return };
    let Ok(name) = std::ffi::CString::new(TIME_MACHINE_EXCLUDE_XATTR) else { return };
    let value = time_machine_exclude_plist();
    // SAFETY: valid NUL-terminated strings and a live value buffer; failure is ignored (best effort).
    unsafe {
        libc::setxattr(c_path.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), 0, libc::XATTR_NOFOLLOW);
    }
}

/// The protector of this platform for `app_data_dir`.
pub fn platform_protector(app_data_dir: &Path) -> Arc<dyn SecretProtector> {
    #[cfg(windows)]
    {
        Arc::new(DpapiProtector::new(app_data_dir))
    }
    #[cfg(unix)]
    {
        Arc::new(KeyFileProtector::new(app_data_dir))
    }
}

/// One namespace (= one sidecar file) of encrypted secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretNamespace {
    /// Account prefix, e.g. `translation-provider.`.
    pub account_prefix: &'static str,
    /// Sidecar file name, e.g. `translation-provider-keys.json`.
    pub file_name: &'static str,
    /// Entry key from the account suffix (UUID → lowercase with dashes; username → lowercase).
    pub lowercase_entry: bool,
}

impl SecretNamespace {
    pub const PROVIDER_KEYS: SecretNamespace = SecretNamespace {
        account_prefix: "translation-provider.",
        file_name: "translation-provider-keys.json",
        lowercase_entry: true,
    };
    /// macOS remember-me keys (UNIFIED.md §3.11).
    pub const REMEMBER_KEYS: SecretNamespace = SecretNamespace {
        account_prefix: "profile-remember.",
        file_name: "profile-remember-keys.json",
        lowercase_entry: true,
    };
}

/// Sidecar schema version this build reads and writes.
const SIDECAR_VERSION: i64 = 1;

/// Blob layout sizes.
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// What the sidecar file holds.
enum SidecarRead {
    Missing,
    Unreadable,
    /// Not JSON, not an object, `keys` not an object.
    Damaged,
    /// A newer `version` this build must not rewrite.
    Unsupported,
    Loaded(BTreeMap<String, serde_json::Value>),
}

fn read_sidecar(path: &Path) -> SidecarRead {
    let bytes = match fileio::read_file(path) {
        ReadOutcome::Missing => return SidecarRead::Missing,
        ReadOutcome::Unreadable => return SidecarRead::Unreadable,
        ReadOutcome::Content(b) => b,
    };
    let text = fileio::decode_text(&bytes);
    let Ok(serde_json::Value::Object(root)) = serde_json::from_str::<serde_json::Value>(&text) else {
        return SidecarRead::Damaged;
    };
    match root.get("version") {
        None | Some(serde_json::Value::Null) => {}
        Some(v) => match v.as_i64() {
            Some(SIDECAR_VERSION) => {}
            Some(n) if n > SIDECAR_VERSION => return SidecarRead::Unsupported,
            _ => return SidecarRead::Damaged,
        },
    }
    match root.get("keys") {
        None | Some(serde_json::Value::Null) => SidecarRead::Loaded(BTreeMap::new()),
        Some(serde_json::Value::Object(keys)) => {
            SidecarRead::Loaded(keys.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        }
        Some(_) => SidecarRead::Damaged,
    }
}

fn sidecar_text(entries: &BTreeMap<String, serde_json::Value>) -> String {
    // Compact, sorted keys: `keys` < `version`; the BTreeMap sorts the entries.
    let mut keys = serde_json::Map::new();
    for (k, v) in entries {
        keys.insert(k.clone(), v.clone());
    }
    let mut root = serde_json::Map::new();
    root.insert("keys".into(), serde_json::Value::Object(keys));
    root.insert("version".into(), serde_json::Value::from(SIDECAR_VERSION));
    serde_json::Value::Object(root).to_string()
}

/// Encrypts `plain` for `entry`: base64(nonce ‖ tag ‖ ciphertext), AAD = UTF-8 of `entry`.
pub(crate) fn seal(key: &DataKey, entry: &str, plain: &[u8]) -> Option<String> {
    let cipher = Aes256Gcm::new_from_slice(key.expose()).ok()?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes).ok()?;
    let nonce = Nonce::from(nonce_bytes);
    let mut buf = Zeroizing::new(plain.to_vec());
    let tag = cipher.encrypt_inout_detached(&nonce, entry.as_bytes(), buf.as_mut_slice().into()).ok()?;
    let mut blob = Vec::with_capacity(NONCE_LEN + TAG_LEN + buf.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&tag);
    blob.extend_from_slice(&buf);
    Some(base64::engine::general_purpose::STANDARD.encode(blob))
}

/// Opens a blob written by [`seal`] (or by any implementation with the same layout). `None` on any failure.
pub(crate) fn open(key: &DataKey, entry: &str, blob_b64: &str) -> Option<SecretBytes> {
    let blob = base64::engine::general_purpose::STANDARD.decode(blob_b64.trim()).ok()?;
    if blob.len() < NONCE_LEN + TAG_LEN {
        return None;
    }
    let cipher = Aes256Gcm::new_from_slice(key.expose()).ok()?;
    let nonce = Nonce::try_from(&blob[..NONCE_LEN]).ok()?;
    let tag = Tag::try_from(&blob[NONCE_LEN..NONCE_LEN + TAG_LEN]).ok()?;
    let mut buf = Zeroizing::new(blob[NONCE_LEN + TAG_LEN..].to_vec());
    cipher.decrypt_inout_detached(&nonce, entry.as_bytes(), buf.as_mut_slice().into(), &tag).ok()?;
    Some(SecretBytes::new(std::mem::take(&mut *buf)))
}

/// AES-256-GCM sidecar store over a protector. Accounts outside the configured namespaces → `set` returns false.
pub struct EncryptedSecretStore {
    dir: PathBuf,
    protector: Arc<dyn SecretProtector>,
    namespaces: Vec<SecretNamespace>,
    /// Serializes this process's threads before the cross-process lock is taken.
    local: Mutex<()>,
}

impl std::fmt::Debug for EncryptedSecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedSecretStore").field("dir", &self.dir).field("namespaces", &self.namespaces).finish()
    }
}

impl EncryptedSecretStore {
    pub fn new(app_data_dir: PathBuf, protector: Arc<dyn SecretProtector>, namespaces: Vec<SecretNamespace>) -> Arc<Self> {
        Arc::new(Self { dir: app_data_dir, protector, namespaces, local: Mutex::new(()) })
    }

    /// The platform store for the app data folder: the protector of [`platform_protector`] and, on macOS, the
    /// remember-me namespace too.
    pub fn for_app_data(app_data_dir: &Path) -> Arc<Self> {
        let mut namespaces = vec![SecretNamespace::PROVIDER_KEYS];
        if cfg!(target_os = "macos") {
            namespaces.push(SecretNamespace::REMEMBER_KEYS);
        }
        Self::new(app_data_dir.to_path_buf(), platform_protector(app_data_dir), namespaces)
    }

    /// Full path of a namespace's sidecar.
    pub fn sidecar_path(&self, namespace: &SecretNamespace) -> PathBuf {
        self.dir.join(namespace.file_name)
    }

    /// Removes provider entries whose UUID is not in `keep` (Windows orphan sweep at load; harmless on macOS).
    /// Entries that are not UUIDs are removed too. Touches nothing when the sidecar is missing, unreadable, damaged
    /// or of a newer version.
    pub fn prune_provider_keys(&self, keep: &[uuid::Uuid]) {
        let Some(ns) = self.namespaces.iter().find(|n| n.account_prefix == SecretNamespace::PROVIDER_KEYS.account_prefix)
        else {
            return;
        };
        let path = self.sidecar_path(ns);
        let _local = self.local.lock();
        let Some(_lock) = CrossProcessLock::acquire(&path, fileio::LOCK_TIMEOUT) else { return };
        let SidecarRead::Loaded(mut entries) = read_sidecar(&path) else { return };
        let before = entries.len();
        entries.retain(|k, _| uuid::Uuid::parse_str(k).is_ok_and(|id| keep.contains(&id)));
        if entries.len() != before {
            let _ = fileio::write_atomic(&path, sidecar_text(&entries).as_bytes());
        }
    }

    /// `(namespace, entry key)` of an account; `None` outside the configured namespaces or with an empty suffix.
    fn resolve(&self, account: &str) -> Option<(&SecretNamespace, String)> {
        self.namespaces.iter().find_map(|ns| {
            let suffix = account.strip_prefix(ns.account_prefix)?;
            if suffix.trim().is_empty() {
                return None;
            }
            Some((ns, if ns.lowercase_entry { suffix.to_lowercase() } else { suffix.to_owned() }))
        })
    }
}

impl SecretStore for EncryptedSecretStore {
    fn read(&self, account: &str) -> SecretRead {
        let Some((ns, entry)) = self.resolve(account) else { return SecretRead::Absent };
        let entries = match read_sidecar(&self.sidecar_path(ns)) {
            SidecarRead::Missing => return SecretRead::Absent,
            SidecarRead::Unreadable | SidecarRead::Damaged | SidecarRead::Unsupported => return SecretRead::Unavailable,
            SidecarRead::Loaded(e) => e,
        };
        let Some(value) = entries.get(&entry) else { return SecretRead::Absent };
        let Some(blob) = value.as_str() else { return SecretRead::Unavailable };
        let key = match self.protector.load() {
            Ok(Some(k)) => k,
            Ok(None) | Err(_) => return SecretRead::Unavailable,
        };
        match open(&key, &entry, blob) {
            Some(bytes) => SecretRead::Found(bytes),
            None => SecretRead::Unavailable,
        }
    }

    fn set(&self, bytes: &[u8], account: &str) -> bool {
        let Some((ns, entry)) = self.resolve(account) else { return false };
        let path = self.sidecar_path(ns);
        let _local = self.local.lock();
        let Some(_lock) = CrossProcessLock::acquire(&path, fileio::LOCK_TIMEOUT) else { return false };
        let mut entries = match read_sidecar(&path) {
            SidecarRead::Missing => BTreeMap::new(),
            SidecarRead::Unreadable | SidecarRead::Unsupported => return false,
            SidecarRead::Damaged => {
                if fileio::preserve_damaged(&path, chrono::Utc::now()).is_err() {
                    return false;
                }
                BTreeMap::new()
            }
            SidecarRead::Loaded(e) => e,
        };
        let key = match self.protector.load() {
            Ok(Some(k)) => k,
            Ok(None) => {
                if !entries.is_empty() {
                    // The old entries can no longer be decrypted: keep them aside, start over with a new key.
                    if fileio::preserve_damaged(&path, chrono::Utc::now()).is_err() {
                        return false;
                    }
                    entries.clear();
                }
                match self.protector.create() {
                    Ok(k) => k,
                    Err(_) => return false,
                }
            }
            Err(_) => return false,
        };
        let Some(blob) = seal(&key, &entry, bytes) else { return false };
        entries.insert(entry, serde_json::Value::String(blob));
        fileio::write_atomic(&path, sidecar_text(&entries).as_bytes()).is_ok()
    }

    fn remove(&self, account: &str) {
        let Some((ns, entry)) = self.resolve(account) else { return };
        let path = self.sidecar_path(ns);
        let _local = self.local.lock();
        let Some(_lock) = CrossProcessLock::acquire(&path, fileio::LOCK_TIMEOUT) else { return };
        let SidecarRead::Loaded(mut entries) = read_sidecar(&path) else { return };
        if entries.remove(&entry).is_some() {
            let _ = fileio::write_atomic(&path, sidecar_text(&entries).as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A protector that keeps the key in a plain file (tests only); `broken` simulates an unusable key.
    struct FileProtector {
        path: PathBuf,
        broken: std::sync::atomic::AtomicBool,
    }

    impl SecretProtector for FileProtector {
        fn load(&self) -> Result<Option<DataKey>, ProtectorError> {
            if self.broken.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(ProtectorError::Unavailable("broken".into()));
            }
            match std::fs::read(&self.path) {
                Ok(b) => SecretKey32::from_slice(&b).map(Some).ok_or(ProtectorError::Unavailable("size".into())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(ProtectorError::Io(e.to_string())),
            }
        }
        fn create(&self) -> Result<DataKey, ProtectorError> {
            let key = random_key()?;
            match publish_new_file(&self.path, key.expose()) {
                Ok(true) => Ok(key),
                Ok(false) => self.load()?.ok_or(ProtectorError::Unavailable("gone".into())),
                Err(e) => Err(ProtectorError::Io(e.to_string())),
            }
        }
    }

    fn file_protector(dir: &Path) -> Arc<FileProtector> {
        Arc::new(FileProtector { path: dir.join("test.key"), broken: false.into() })
    }

    fn store(dir: &Path, protector: Arc<dyn SecretProtector>) -> Arc<EncryptedSecretStore> {
        EncryptedSecretStore::new(
            dir.to_path_buf(),
            protector,
            vec![SecretNamespace::PROVIDER_KEYS, SecretNamespace::REMEMBER_KEYS],
        )
    }

    fn found(r: SecretRead) -> Option<Vec<u8>> {
        match r {
            SecretRead::Found(b) => Some(b.expose().to_vec()),
            _ => None,
        }
    }

    const ID: &str = "E621E1F8-C36C-495A-93FC-0C247A3E6E5F";

    fn account() -> String {
        format!("translation-provider.{ID}")
    }

    /// Every file in `dir` as lossy text, concatenated.
    fn all_text(dir: &Path) -> String {
        let mut s = String::new();
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_file() {
                s.push_str(&String::from_utf8_lossy(&std::fs::read(p).unwrap()));
            }
        }
        s
    }

    #[test]
    fn round_trip_across_instances_and_no_plaintext_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let a = store(dir.path(), file_protector(dir.path()));
        assert!(matches!(a.read(&account()), SecretRead::Absent));
        assert!(a.set(b"sk-super-secret-1234", &account()));
        let b = store(dir.path(), file_protector(dir.path()));
        assert_eq!(found(b.read(&account())).unwrap(), b"sk-super-secret-1234");
        assert!(!all_text(dir.path()).contains("sk-super-secret"));
        let text = std::fs::read_to_string(dir.path().join("translation-provider-keys.json")).unwrap();
        assert!(text.starts_with("{\"keys\":{\"e621e1f8-c36c-495a-93fc-0c247a3e6e5f\":\""), "{text}");
        assert!(text.ends_with(",\"version\":1}"));
        b.remove(&account());
        assert!(matches!(a.read(&account()), SecretRead::Absent));
        let text = std::fs::read_to_string(dir.path().join("translation-provider-keys.json")).unwrap();
        assert_eq!(text, "{\"keys\":{},\"version\":1}");
    }

    #[test]
    fn fresh_nonce_per_write() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), file_protector(dir.path()));
        let read_blob = || {
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(dir.path().join("translation-provider-keys.json")).unwrap())
                    .unwrap();
            v["keys"]["e621e1f8-c36c-495a-93fc-0c247a3e6e5f"].as_str().unwrap().to_owned()
        };
        assert!(s.set(b"same", &account()));
        let first = read_blob();
        assert!(s.set(b"same", &account()));
        assert_ne!(first, read_blob());
    }

    #[test]
    fn aad_binds_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), file_protector(dir.path()));
        assert!(s.set(b"k1", &account()));
        let path = dir.path().join("translation-provider-keys.json");
        let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let blob = v["keys"]["e621e1f8-c36c-495a-93fc-0c247a3e6e5f"].clone();
        let other = "11111111-2222-3333-4444-555555555555";
        v["keys"][other] = blob;
        std::fs::write(&path, v.to_string()).unwrap();
        assert!(matches!(s.read(&format!("translation-provider.{}", other.to_uppercase())), SecretRead::Unavailable));
        assert!(found(s.read(&account())).is_some());
    }

    #[test]
    fn tampered_blob_or_bad_base64_is_unavailable_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), file_protector(dir.path()));
        assert!(s.set(b"k1", &account()));
        let path = dir.path().join("translation-provider-keys.json");
        let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let entry = "e621e1f8-c36c-495a-93fc-0c247a3e6e5f";
        let mut raw = base64::engine::general_purpose::STANDARD.decode(v["keys"][entry].as_str().unwrap()).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        v["keys"][entry] = base64::engine::general_purpose::STANDARD.encode(&raw).into();
        std::fs::write(&path, v.to_string()).unwrap();
        assert!(matches!(s.read(&account()), SecretRead::Unavailable));
        v["keys"][entry] = "%%%not base64".into();
        std::fs::write(&path, v.to_string()).unwrap();
        assert!(matches!(s.read(&account()), SecretRead::Unavailable));
        v["keys"][entry] = 42.into();
        std::fs::write(&path, v.to_string()).unwrap();
        assert!(matches!(s.read(&account()), SecretRead::Unavailable));
    }

    #[test]
    fn blob_layout_is_nonce_tag_ciphertext() {
        // Hand-written decoder: nonce = [0..12], tag = [12..28], ct = rest; AES-GCM with AAD = entry.
        let key = SecretKey32::new([7u8; 32]);
        let entry = "e621e1f8-c36c-495a-93fc-0c247a3e6e5f";
        let b64 = seal(&key, entry, b"sk-abc").unwrap();
        let raw = base64::engine::general_purpose::STANDARD.decode(&b64).unwrap();
        assert_eq!(raw.len(), 12 + 16 + 6);
        // Re-assemble in the library's own `ct || tag` order and decrypt with the allocating API.
        use aes_gcm::aead::{Aead, Payload};
        let cipher = Aes256Gcm::new_from_slice(&[7u8; 32]).unwrap();
        let mut ct_tag = raw[28..].to_vec();
        ct_tag.extend_from_slice(&raw[12..28]);
        let nonce = Nonce::try_from(&raw[..12]).unwrap();
        let plain = cipher.decrypt(&nonce, Payload { msg: &ct_tag, aad: entry.as_bytes() }).unwrap();
        assert_eq!(plain, b"sk-abc");
        // And a blob produced the C# way (nonce | tag | ct from the allocating API) opens here.
        let nonce_bytes = [3u8; 12];
        let n = Nonce::from(nonce_bytes);
        let out = cipher.encrypt(&n, Payload { msg: b"from-csharp", aad: entry.as_bytes() }).unwrap();
        let (ct, tag) = out.split_at(out.len() - 16);
        let mut blob = nonce_bytes.to_vec();
        blob.extend_from_slice(tag);
        blob.extend_from_slice(ct);
        let opened = open(&key, entry, &base64::engine::general_purpose::STANDARD.encode(blob)).unwrap();
        assert_eq!(opened.expose(), b"from-csharp");
    }

    #[test]
    fn unknown_accounts_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), file_protector(dir.path()));
        assert!(!s.set(b"x", "something-else.abc"));
        assert!(!s.set(b"x", "translation-provider."));
        assert!(matches!(s.read("something-else.abc"), SecretRead::Absent));
        assert!(!dir.path().join("translation-provider-keys.json").exists());
    }

    #[test]
    fn remember_namespace_has_its_own_file_and_lowercases_users() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), file_protector(dir.path()));
        assert!(s.set(&[9u8; 32], "profile-remember.Ayşe"));
        assert_eq!(found(s.read("profile-remember.ayşe")).unwrap(), vec![9u8; 32]);
        let text = std::fs::read_to_string(dir.path().join("profile-remember-keys.json")).unwrap();
        assert!(text.contains("\"ayşe\""));
        assert!(!dir.path().join("translation-provider-keys.json").exists());
    }

    #[test]
    fn unusable_data_key_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = file_protector(dir.path());
        let s = store(dir.path(), p.clone());
        assert!(s.set(b"k1", &account()));
        let path = dir.path().join("translation-provider-keys.json");
        let before = std::fs::read(&path).unwrap();
        p.broken.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(matches!(s.read(&account()), SecretRead::Unavailable));
        assert!(!s.set(b"k2", &account()));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn missing_key_file_with_entries_sets_the_sidecar_aside_on_the_next_set() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), file_protector(dir.path()));
        assert!(s.set(b"k1", &account()));
        std::fs::remove_file(dir.path().join("test.key")).unwrap();
        assert!(matches!(s.read(&account()), SecretRead::Unavailable));
        assert!(s.set(b"k2", &account()));
        assert_eq!(found(s.read(&account())).unwrap(), b"k2");
        let aside: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("translation-provider-keys.corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1);
    }

    #[test]
    fn damaged_sidecar_reads_unavailable_and_is_set_aside_on_set() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), file_protector(dir.path()));
        let path = dir.path().join("translation-provider-keys.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(s.read(&account()), SecretRead::Unavailable));
        s.remove(&account());
        assert_eq!(std::fs::read(&path).unwrap(), b"{not json", "remove never rewrites a damaged sidecar");
        assert!(s.set(b"k", &account()));
        let names: Vec<String> =
            std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        let aside: Vec<_> = names.iter().filter(|n| n.starts_with("translation-provider-keys.corrupt-")).collect();
        assert_eq!(aside.len(), 1);
        let re = regex_like(aside[0]);
        assert!(re, "{}", aside[0]);
        assert_eq!(std::fs::read(dir.path().join(aside[0])).unwrap(), b"{not json");
    }

    /// `^translation-provider-keys\.corrupt-\d{8}-\d{6}\.json$` without a regex dependency.
    fn regex_like(name: &str) -> bool {
        let Some(rest) = name.strip_prefix("translation-provider-keys.corrupt-") else { return false };
        let Some(ts) = rest.strip_suffix(".json") else { return false };
        let b = ts.as_bytes();
        b.len() == 15 && b[8] == b'-' && b.iter().enumerate().all(|(i, c)| i == 8 || c.is_ascii_digit())
    }

    #[test]
    fn newer_sidecar_version_is_never_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), file_protector(dir.path()));
        let path = dir.path().join("translation-provider-keys.json");
        std::fs::write(&path, b"{\"keys\":{},\"version\":2}").unwrap();
        assert!(matches!(s.read(&account()), SecretRead::Unavailable));
        assert!(!s.set(b"k", &account()));
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"keys\":{},\"version\":2}");
    }

    #[test]
    fn prune_keeps_only_listed_providers() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), file_protector(dir.path()));
        let keep = uuid::Uuid::new_v4();
        let drop_ = uuid::Uuid::new_v4();
        assert!(s.set(b"a", &crate::TranslationProviderProfile::secret_account(keep)));
        assert!(s.set(b"b", &crate::TranslationProviderProfile::secret_account(drop_)));
        s.prune_provider_keys(&[keep]);
        assert!(found(s.read(&crate::TranslationProviderProfile::secret_account(keep))).is_some());
        assert!(matches!(s.read(&crate::TranslationProviderProfile::secret_account(drop_)), SecretRead::Absent));
    }

    #[test]
    fn debug_never_shows_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), file_protector(dir.path()));
        assert!(s.set(b"sk-visible?", &account()));
        let r = s.read(&account());
        assert!(!format!("{r:?}").contains("sk-visible"));
        assert!(!format!("{s:?}").contains("sk-visible"));
        let key = file_protector(dir.path()).load().unwrap().unwrap();
        assert_eq!(format!("{key:?}"), "SecretKey32([redacted])");
    }

    #[test]
    fn concurrent_writers_keep_every_entry() {
        let dir = tempfile::tempdir().unwrap();
        let p = file_protector(dir.path());
        let ids: Vec<uuid::Uuid> = (0..8).map(|_| uuid::Uuid::new_v4()).collect();
        let handles: Vec<_> = ids
            .iter()
            .map(|id| {
                // Separate store instances contend like separate processes.
                let s = store(dir.path(), p.clone());
                let account = crate::TranslationProviderProfile::secret_account(*id);
                std::thread::spawn(move || s.set(account.as_bytes(), &account))
            })
            .collect();
        assert!(handles.into_iter().all(|h| h.join().unwrap()));
        let s = store(dir.path(), p);
        for id in ids {
            let account = crate::TranslationProviderProfile::secret_account(id);
            assert_eq!(found(s.read(&account)).unwrap(), account.as_bytes());
        }
    }

    #[test]
    fn time_machine_exclusion_value_is_the_tmutil_plist() {
        // The value `tmutil addexclusion` writes (`xattr -px com.apple.metadata:com_apple_backup_excludeItem`).
        let mut expected = b"bplist00_\x10\x11com.apple.backupd\x08".to_vec();
        expected.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 1]);
        expected.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        expected.extend_from_slice(&[0; 8]);
        expected.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0x1c]);
        assert_eq!(time_machine_exclude_plist(), expected);
        assert_eq!(TIME_MACHINE_EXCLUDE_XATTR, "com.apple.metadata:com_apple_backup_excludeItem");
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_round_trip_with_entropy() {
        let dir = tempfile::tempdir().unwrap();
        let p = DpapiProtector::new(dir.path());
        assert!(p.load().unwrap().is_none());
        let k = p.create().unwrap();
        assert_eq!(p.load().unwrap().unwrap(), k);
        let blob = std::fs::read(&p.path).unwrap();
        assert!(!blob.windows(32).any(|w| w == k.expose()), "the data key is not stored in the clear");
        // A second create keeps the winner's key.
        assert_eq!(p.create().unwrap(), k);
        // The blob does not open without the entropy.
        let mut input = blob.clone();
        let in_blob = windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB {
            cbData: input.len() as u32,
            pbData: input.as_mut_ptr(),
        };
        let mut out = windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB::default();
        // SAFETY: valid input blob; output intentionally leaked on failure only (none allocated).
        let r = unsafe {
            windows::Win32::Security::Cryptography::CryptUnprotectData(
                &in_blob,
                None,
                None,
                None,
                None,
                windows::Win32::Security::Cryptography::CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        assert!(r.is_err());
        // A corrupted blob is unavailable, never replaced.
        std::fs::write(&p.path, b"garbage").unwrap();
        assert!(matches!(p.load(), Err(ProtectorError::Unavailable(_))));
        assert!(p.create().is_err());
        assert_eq!(std::fs::read(&p.path).unwrap(), b"garbage");
    }

    #[cfg(windows)]
    #[test]
    fn store_over_dpapi() {
        let dir = tempfile::tempdir().unwrap();
        let s = EncryptedSecretStore::for_app_data(dir.path());
        assert!(s.set(b"sk-dpapi-secret", &account()));
        let again = EncryptedSecretStore::for_app_data(dir.path());
        assert_eq!(found(again.read(&account())).unwrap(), b"sk-dpapi-secret");
        assert!(dir.path().join("translation-provider-keys.dpapi").exists());
        assert!(!all_text(dir.path()).contains("sk-dpapi-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_0600_and_wider_modes_or_wrong_sizes_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = Arc::new(KeyFileProtector::new(dir.path()));
        let s = store(dir.path(), p.clone());
        assert!(s.set(b"k", &account()));
        let meta = std::fs::metadata(&p.path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(meta.len(), 32);
        let sidecar = dir.path().join("translation-provider-keys.json");
        assert_eq!(std::fs::metadata(&sidecar).unwrap().permissions().mode() & 0o777, 0o600);
        let before = std::fs::read(&sidecar).unwrap();
        std::fs::set_permissions(&p.path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(s.read(&account()), SecretRead::Unavailable));
        assert!(!s.set(b"k2", &account()));
        assert_eq!(std::fs::read(&sidecar).unwrap(), before);
        std::fs::set_permissions(&p.path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&p.path, [1u8; 31]).unwrap();
        assert!(matches!(p.load(), Err(ProtectorError::Unavailable(_))));
        assert!(!s.set(b"k2", &account()));
    }
}
