//! `sa-media`: media library index (`media-index.json`, two backends with the same schema; UNIFIED.md §3.16,
//! §5.3), the Windows encrypted video chunk cache and buffered stream (win-media §2–§3, current C# v3 layout), and
//! the macOS folder lister rules.
//!
//! Privacy contract (rule 6): `search` and `all_files` are **never called while logged out** by the hub; the explorer
//! may list the current folder logged out but never runs index-wide search then. The index itself stays plaintext
//! (listings, not history). On Windows it records no scan or update time, and a folder that was only the folder of an
//! opened video is listed transiently ([`win::WinMediaIndex::scan_transient`]) so it never becomes a record.

use std::path::Path;

use sa_core::cancel::CancellationToken;
use sa_core::subscription::Subscription;

pub mod dates;
pub mod schema;
pub mod text;

#[cfg(windows)]
pub mod win;

#[cfg(any(target_os = "macos", test))]
pub mod mac;

pub use schema::{DateStyle, DirectorySnapshot, IndexedMediaFile, MediaIndexDocument};

/// Outcome of scanning one folder.
#[derive(Debug, Clone, PartialEq)]
pub enum ScanResult {
    Listed(DirectorySnapshot),
    Missing,
    /// The folder exists but could not be listed; the cached entry (if any) was kept (`cached_snapshot`).
    Unreadable,
    Cancelled,
}

impl ScanResult {
    /// The listing, if any.
    pub fn snapshot(&self) -> Option<&DirectorySnapshot> {
        match self {
            ScanResult::Listed(s) => Some(s),
            _ => None,
        }
    }
}

/// The media index contract.
pub trait MediaIndex: Send + Sync {
    /// Lists one folder, updates the index and persists it (atomic; Windows only when the listing changed).
    fn scan(&self, dir: &Path, cancel: &CancellationToken) -> ScanResult;
    /// Background crawl below `root` (default depth 2, at most `max_folders` folders); returns folders scanned
    /// (Windows) or listed (macOS).
    fn crawl(&self, root: &Path, max_depth: u8, max_folders: usize, cancel: &CancellationToken, progress: &dyn Fn(usize)) -> usize;
    /// The cached listing (first paint), no file access.
    fn cached_snapshot(&self, dir: &Path) -> Option<DirectorySnapshot>;
    fn total_indexed_files(&self) -> usize;
    /// Index-wide search. **Never called logged out** (hub, explorer).
    fn search(&self, query: &str, limit: usize) -> Vec<IndexedMediaFile>;
    /// Every indexed file, newest first (macOS hub Keşfet; Windows `search("", limit)`). **Never called logged out.**
    fn all_files(&self, limit: usize) -> Vec<IndexedMediaFile>;
    /// Change notifications, called on the worker thread that changed the index (after the write on macOS).
    fn subscribe_changes(&self, handler: Box<dyn Fn() + Send + Sync>) -> Subscription;
    /// A subtitle was saved next to a video: refresh its sidecar badge (only for a folder already indexed; the
    /// Windows refresh stays in memory, so opening a video never rewrites the index file).
    fn invalidate_sidecar(&self, video_path: &Path);
    /// Lists a folder without touching the index, in memory or on disk (Windows `ScanDirectoryTransientAsync`: the
    /// folder of a video that was merely opened). Default: [`MediaIndex::scan`] (macOS has no such mode).
    fn scan_transient(&self, dir: &Path, cancel: &CancellationToken) -> ScanResult {
        self.scan(dir, cancel)
    }
    /// Rescans a folder and updates only the in-memory index, never the file (Windows
    /// `RefreshDirectoryInMemoryAsync`). Default: [`MediaIndex::scan`].
    fn refresh_in_memory(&self, dir: &Path, cancel: &CancellationToken) -> ScanResult {
        self.scan(dir, cancel)
    }
    /// Whether the folder is already recorded in the index (Windows `IsDirectoryIndexed`). Default: a cached
    /// listing exists.
    fn is_directory_indexed(&self, dir: &Path) -> bool {
        self.cached_snapshot(dir).is_some()
    }
}

/// `formatted_size` of index rows (win-media §1.6, .NET `0.#` / `0.##` invariant formats; macOS uses
/// `ByteCountFormatter` via `PlatformPolicy`).
pub fn format_file_size(bytes: i64) -> String {
    let bytes = bytes.max(0);
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        return format!("{} KB", format_dotnet_decimals(kb, 1));
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format!("{} MB", format_dotnet_decimals(mb, 1));
    }
    format!("{} GB", format_dotnet_decimals(mb / 1024.0, 2))
}

/// .NET custom numeric format `0.#…` (`decimals` optional digits, invariant culture): the double is first rounded to
/// 15 significant digits, then half away from zero to `decimals`, trailing zeros dropped.
pub fn format_dotnet_decimals(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return "0".into();
    }
    let negative = value < 0.0;
    // 15 significant digits: value = mantissa × 10^(exp - 14).
    let sci = format!("{:.14e}", value.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i64 = exp.parse().unwrap_or(0);
    let n: u128 = mantissa.bytes().filter(u8::is_ascii_digit).fold(0u128, |acc, d| acc * 10 + u128::from(d - b'0'));
    // Units of 10^-decimals, rounded half away from zero.
    let shift = exp - 14 + decimals as i64;
    let units: u128 = if shift >= 0 {
        n.saturating_mul(10u128.saturating_pow(shift.min(38) as u32))
    } else if -shift > 38 {
        0
    } else {
        let div = 10u128.pow((-shift) as u32);
        let (q, r) = (n / div, n % div);
        if r * 2 >= div { q + 1 } else { q }
    };
    let scale = 10u128.pow(decimals as u32);
    let int_part = units / scale;
    let frac = format!("{:0width$}", units % scale, width = decimals);
    let frac = frac.trim_end_matches('0');
    let body = if decimals == 0 || frac.is_empty() { int_part.to_string() } else { format!("{int_part}.{frac}") };
    if negative && units != 0 { format!("-{body}") } else { body }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_file_size_vectors() {
        let vectors = [
            (0, "0 B"),
            (-5, "0 B"),
            (1023, "1023 B"),
            (1024, "1 KB"),
            (1100, "1.1 KB"),
            (1536, "1.5 KB"),
            (10239, "10 KB"),
            (1_048_575, "1024 KB"),
            (1_048_576, "1 MB"),
            (148_897_792, "142 MB"),
            (1_073_741_824, "1 GB"),
            (1_503_238_553, "1.4 GB"),
            (5_368_709_120, "5 GB"),
            (1_099_511_627_776, "1024 GB"),
        ];
        for (bytes, expected) in vectors {
            assert_eq!(format_file_size(bytes), expected, "{bytes}");
        }
    }

    #[test]
    fn dotnet_rounding_is_half_away_from_zero_on_15_digits() {
        assert_eq!(format_dotnet_decimals(1.25, 1), "1.3");
        assert_eq!(format_dotnet_decimals(1.35, 1), "1.4");
        assert_eq!(format_dotnet_decimals(2.675, 2), "2.68");
        assert_eq!(format_dotnet_decimals(9.96, 1), "10");
        assert_eq!(format_dotnet_decimals(0.04, 1), "0");
        assert_eq!(format_dotnet_decimals(0.05, 1), "0.1");
        assert_eq!(format_dotnet_decimals(0.004, 2), "0");
        assert_eq!(format_dotnet_decimals(999.96, 1), "1000");
    }
}
