//! Card pictures (win-ui-hub-settings §5.6, mac-ui §9.6 `CardImageCache`): stored thumbnail bytes (JPEG/PNG/BMP from
//! the profile's encrypted vault, supplied by the view model) are decoded off the UI thread and cached per path.
//!
//! Privacy: every entry belongs to one session generation. `reset()` (logout, profile switch, delete) bumps the
//! generation and drops every decoded picture at once; a decode that finishes for an older generation is dropped.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{Sender, channel};

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// Decoded pictures kept at most (macOS `CardImageCache` count limit).
pub const CACHE_LIMIT: usize = 400;

/// A decoded picture ready for `Image::from_rgba8` on the UI thread.
pub type Decoded = SharedPixelBuffer<Rgba8Pixel>;

/// Decodes `bytes` and scales it to `target_width` px (keeping the aspect; never upscales).
pub fn decode(bytes: &[u8], target_width: u32) -> Option<Decoded> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let img = if w > target_width.max(1) {
        let nh = ((h as f64) * (target_width as f64) / (w as f64)).round().max(1.0) as u32;
        image::imageops::resize(&img, target_width, nh, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let (w, h) = img.dimensions();
    Some(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(img.as_raw(), w, h))
}

struct Job {
    generation: u64,
    key: String,
    bytes: Vec<u8>,
    width: u32,
}

/// One background decode thread; results go to `sink` (which posts them to the UI thread).
pub struct DecodeWorker {
    tx: Sender<Job>,
}

impl DecodeWorker {
    pub fn spawn(sink: impl Fn(u64, String, Option<Decoded>) + Send + 'static) -> Self {
        let (tx, rx) = channel::<Job>();
        let _ = std::thread::Builder::new().name("sa-hub-thumbs".into()).spawn(move || {
            while let Ok(job) = rx.recv() {
                let out = decode(&job.bytes, job.width);
                sink(job.generation, job.key, out);
            }
        });
        Self { tx }
    }

    fn submit(&self, job: Job) -> bool {
        self.tx.send(job).is_ok()
    }
}

/// The UI-thread cache of decoded card pictures.
#[derive(Default)]
pub struct CardImageCache {
    generation: u64,
    images: HashMap<String, Image>,
    order: VecDeque<String>,
    in_flight: HashSet<String>,
}

impl CardImageCache {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&Image> {
        self.images.get(key)
    }

    pub fn is_in_flight(&self, key: &str) -> bool {
        self.in_flight.contains(key)
    }

    /// Drops every picture and makes pending decodes stale (session change).
    pub fn reset(&mut self) {
        self.generation += 1;
        self.images.clear();
        self.order.clear();
        self.in_flight.clear();
    }

    /// Queues a decode unless cached or already in flight. Returns whether a job was queued.
    pub fn request(&mut self, worker: &DecodeWorker, key: &str, bytes: Vec<u8>, width: u32) -> bool {
        if self.images.contains_key(key) || self.in_flight.contains(key) || bytes.is_empty() {
            return false;
        }
        self.in_flight.insert(key.to_owned());
        let queued = worker.submit(Job { generation: self.generation, key: key.to_owned(), bytes, width });
        if !queued {
            self.in_flight.remove(key);
        }
        queued
    }

    /// A finished decode. Returns the image when it belongs to the current generation.
    pub fn finish(&mut self, generation: u64, key: String, decoded: Option<Decoded>) -> Option<Image> {
        if generation != self.generation {
            return None;
        }
        self.in_flight.remove(&key);
        let image = Image::from_rgba8(decoded?);
        self.insert(key, image.clone());
        Some(image)
    }

    pub fn insert(&mut self, key: String, image: Image) {
        if !self.images.contains_key(&key) {
            self.order.push_back(key.clone());
        }
        self.images.insert(key, image);
        while self.images.len() > CACHE_LIMIT {
            match self.order.pop_front() {
                Some(old) => {
                    self.images.remove(&old);
                }
                None => break,
            }
        }
    }
}
