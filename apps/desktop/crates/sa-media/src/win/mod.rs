//! Windows backend (win-media, ported from the current C# sources): [`WinMediaIndex`] (`media-index.json` v2,
//! OrdinalIgnoreCase keys, broker `MediaIndexed` freshness), [`VideoCacheStore`]
//! (`%LOCALAPPDATA%\SubtitleAdder\VideoCache\v3`, keyed file names, per-profile AES-GCM chunks padded to the chunk
//! size, fixed file times) and [`BufferedVideoStream`] (RAM read-ahead + SSD chunk cache) used by the FFmpeg player
//! through a custom AVIO context.
//!
//! Rule 6: while no profile is logged in the cache neither writes nor lists chunks (RAM read-ahead only); a stream's
//! cache session ends at logout or a profile switch; the legacy layouts (including the C# app's old anonymous
//! folder `06b631aa99fabdf205dd7684223ae0d6`) are purged at startup.

pub mod cache;
mod dict;
pub mod fsio;
pub mod index;
pub mod paths;
pub mod stream;

#[cfg(test)]
mod tests;

pub use cache::{CacheSession, SessionGenerationProvider, SessionKeyProvider, VaultKeyProvider, VideoCacheStore};
pub use index::WinMediaIndex;
pub use stream::{BufferedVideoStream, ReadSeek, mime_type};
