//! Read-latency measurement of the player's read path on Windows: `BufferedVideoStream` over a 1 GiB file, read
//! in 256 KiB calls (the demuxer's AVIO buffer), without and with an encrypted SSD cache session
//! (`VideoCacheStore`, what a logged-in profile gets). Ignored by default: it writes a 1 GiB source file plus up
//! to about 1 GiB of encrypted cache chunks to a temp folder, which it removes at the end (failing loudly, with
//! the path, when it cannot):
//!
//! ```text
//! cargo test -p sa-media --release --test stream_measure -- --ignored --nocapture --test-threads=1
//! ```
//!
//! A 4K remux needs about 15 MB/s sustained, and no single `read` should block for more than about 50 ms. Each
//! scenario runs unpaced (as fast as the stream allows) and paced at 30 MB/s (twice a 4K remux, the way a player
//! pulls), sequentially and with a random seek every 4 MiB. The source file was just written, so it is mostly in
//! the OS file cache: the numbers measure the stream and the cache store, not the source disk.

#![cfg(windows)]

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sa_core::secrets::SecretKey32;
use sa_media::win::{BufferedVideoStream, VaultKeyProvider, VideoCacheStore};

const FILE_LEN: u64 = 1 << 30;
const READ: usize = 256 * 1024;
const SEEK_EVERY: u64 = 4 * 1024 * 1024;
const PACED_BYTES_PER_SEC: f64 = 30.0 * 1024.0 * 1024.0;

/// Cheap pseudo-random bytes (xorshift64*), so no chunk compresses or deduplicates.
fn write_source(path: &Path) {
    let mut f = std::io::BufWriter::with_capacity(8 << 20, std::fs::File::create(path).unwrap());
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut block = vec![0u8; 8 << 20];
    let mut written = 0u64;
    while written < FILE_LEN {
        for word in block.chunks_exact_mut(8) {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            word.copy_from_slice(&x.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes());
        }
        f.write_all(&block).unwrap();
        written += block.len() as u64;
    }
    f.flush().unwrap();
}

struct Stats {
    label: String,
    bytes: u64,
    elapsed: Duration,
    reads: Vec<Duration>,
}

impl Stats {
    fn print(&mut self) {
        self.reads.sort();
        let pick = |q: f64| self.reads[((self.reads.len() - 1) as f64 * q) as usize].as_secs_f64() * 1000.0;
        let over = self.reads.iter().filter(|d| **d > Duration::from_millis(50)).count();
        println!(
            "{:<44} {:>8.1} MB/s  reads {:>5}  p50 {:>6.2} ms  p99 {:>7.2} ms  max {:>7.2} ms  >50ms {}",
            self.label,
            self.bytes as f64 / (1024.0 * 1024.0) / self.elapsed.as_secs_f64(),
            self.reads.len(),
            pick(0.5),
            pick(0.99),
            pick(1.0),
            over
        );
    }
}

/// Reads `budget` bytes in 256 KiB calls; `seeks` jumps to a pseudo-random position every 4 MiB; `paced` holds the
/// average rate at 30 MB/s.
fn run(label: &str, stream: &mut BufferedVideoStream, budget: u64, seeks: bool, paced: bool) -> Stats {
    let mut buf = vec![0u8; READ];
    let mut reads = Vec::new();
    let mut total = 0u64;
    let mut since_seek = 0u64;
    let mut rng: u64 = 0xD1B5_4A32_D192_ED03;
    stream.seek(SeekFrom::Start(0)).unwrap();
    let start = Instant::now();
    while total < budget {
        if seeks && since_seek >= SEEK_EVERY {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let target = (rng % (FILE_LEN - SEEK_EVERY)) & !0xFFF;
            stream.seek(SeekFrom::Start(target)).unwrap();
            since_seek = 0;
        }
        let t = Instant::now();
        let n = stream.read(&mut buf).unwrap();
        reads.push(t.elapsed());
        if n == 0 {
            stream.seek(SeekFrom::Start(0)).unwrap();
            continue;
        }
        total += n as u64;
        since_seek += n as u64;
        if paced {
            let due = Duration::from_secs_f64(total as f64 / PACED_BYTES_PER_SEC);
            if let Some(wait) = due.checked_sub(start.elapsed()) {
                std::thread::sleep(wait);
            }
        }
    }
    Stats { label: label.to_owned(), bytes: total, elapsed: start.elapsed(), reads }
}

fn open(path: &Path, store: Option<Arc<VideoCacheStore>>) -> BufferedVideoStream {
    BufferedVideoStream::open_file(
        path,
        BufferedVideoStream::DEFAULT_CHUNK,
        BufferedVideoStream::DEFAULT_MAX_CACHED_CHUNKS,
        BufferedVideoStream::DEFAULT_READ_AHEAD_CHUNKS,
        store,
    )
    .unwrap()
}

#[test]
#[ignore = "writes up to ~2 GiB of temp files; run with --release --ignored --nocapture"]
fn buffered_stream_read_latency() {
    let dir = tempfile::Builder::new().prefix("sa-stream-measure-").tempdir().unwrap();
    let source = dir.path().join("source.bin");
    let t = Instant::now();
    write_source(&source);
    println!("wrote {} MiB in {:.1} s", FILE_LEN >> 20, t.elapsed().as_secs_f64());

    let key: [u8; 32] = std::array::from_fn(|i| i as u8 ^ 0x5A);
    let keys: VaultKeyProvider = Arc::new(move || Some(("bench".to_owned(), SecretKey32::new(key))));
    let store = VideoCacheStore::new(dir.path().join("VideoCache"), keys, Some(4 << 30));
    store.purge_legacy_layout();

    let paced_budget = 256 << 20; // ~8.5 s at 30 MB/s
    let mut results = Vec::new();
    for (label, cached) in [("no store", false), ("cache session", true)] {
        let s = || if cached { Some(store.clone()) } else { None };
        // A fresh stream per scenario, so RAM chunks of the previous one do not help.
        results.push(run(&format!("{label}: sequential, unpaced (1st pass)"), &mut open(&source, s()), FILE_LEN, false, false));
        results.push(run(&format!("{label}: sequential, unpaced (2nd pass)"), &mut open(&source, s()), FILE_LEN, false, false));
        results.push(run(&format!("{label}: sequential, 30 MB/s"), &mut open(&source, s()), paced_budget, false, true));
        results.push(run(&format!("{label}: seek every 4 MiB, unpaced"), &mut open(&source, s()), FILE_LEN / 2, true, false));
        results.push(run(&format!("{label}: seek every 4 MiB, 30 MB/s"), &mut open(&source, s()), paced_budget, true, true));
    }
    println!("cache store holds {} MiB", store.total_used_bytes() >> 20);
    for r in &mut results {
        r.print();
    }
    // Every stream was dropped at the end of its statement (which drains its SSD writes); dropping the store ends
    // its session, so no file in the folder is open any more and it can be removed. A silent `TempDir` drop would
    // leave gigabytes behind on Windows if a file were still open.
    drop(store);
    let path = dir.path().to_path_buf();
    let mut result = dir.close();
    // The session watcher thread may still hold the store for one more tick (250 ms); retry briefly.
    for _ in 0..20 {
        if result.is_ok() || !path.exists() {
            result = Ok(());
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        result = std::fs::remove_dir_all(&path);
    }
    if let Err(e) = result {
        panic!("could not remove the measurement folder {}: {e}; delete it by hand", path.display());
    }
}
