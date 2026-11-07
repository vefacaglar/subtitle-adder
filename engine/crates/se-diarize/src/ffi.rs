//! sherpa-onnx C API (`c-api.h`) loaded at run time with `libloading` (no static link). The `#[repr(C)]`
//! structs match sherpa-onnx **2.0.1** (`engine/.venv/Lib/site-packages/sherpa_onnx/include/sherpa-onnx/c-api/c-api.h`;
//! dev copy of the libraries: `engine/.venv/Lib/site-packages/sherpa_onnx/lib`).
//!
//! Library search: the directory of `$ORT_DYLIB_PATH` (when it holds the C API library), next to the
//! executable, `$SUBTITLE_ENGINE_SHERPA_DIR` ([`se_core::fsutil::find_native_library`]) and, in debug builds
//! only, the dev copy in the Python venv. Its onnxruntime must be the one `se-asr` loads: when
//! `ORT_DYLIB_PATH` is set, that library is loaded first so the C API binds to the same module.

use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};

use se_core::{EngineError, Result};

/// Platform file name of the C API library.
pub const SHERPA_LIBRARY_NAME: &str = if cfg!(windows) {
    "sherpa-onnx-c-api.dll"
} else if cfg!(target_os = "macos") {
    "libsherpa-onnx-c-api.dylib"
} else {
    "libsherpa-onnx-c-api.so"
};

/// sherpa-onnx version whose `c-api.h` these structs follow.
pub const SHERPA_VERSION: &str = "2.0.1";

/// `SherpaOnnxOfflineSpeakerSegmentationPyannoteModelConfig`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PyannoteModelConfig {
    pub model: *const c_char,
    /// `(0, 1]`; `<= 0` means the default 0.1.
    pub window_shift_ratio: f32,
}

/// `SherpaOnnxOfflineSpeakerSegmentationModelConfig`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SegmentationModelConfig {
    pub pyannote: PyannoteModelConfig,
    pub num_threads: i32,
    pub debug: i32,
    pub provider: *const c_char,
}

/// `SherpaOnnxSpeakerEmbeddingExtractorConfig`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EmbeddingExtractorConfig {
    pub model: *const c_char,
    pub num_threads: i32,
    pub debug: i32,
    pub provider: *const c_char,
}

/// `SherpaOnnxFastClusteringConfig`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FastClusteringConfig {
    /// `> 0`: known speaker count (threshold ignored); `-1`: automatic.
    pub num_clusters: i32,
    pub threshold: f32,
    pub compute_confidence: i32,
}

/// `SherpaOnnxOfflineSpeakerDiarizationConfig`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DiarizationConfig {
    pub segmentation: SegmentationModelConfig,
    pub embedding: EmbeddingExtractorConfig,
    pub clustering: FastClusteringConfig,
    pub min_duration_on: f32,
    pub min_duration_off: f32,
}

/// `SherpaOnnxOfflineSpeakerDiarizationSegment`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiarizationSegment {
    pub start: f32,
    pub end: f32,
    pub speaker: i32,
    pub confidence: f32,
}

/// Opaque handles.
pub type Diarization = c_void;
pub type DiarizationResult = c_void;
pub type EmbeddingExtractor = c_void;
pub type OnlineStream = c_void;

/// `SherpaOnnxOfflineSpeakerDiarizationProgressCallback`.
pub type ProgressCallback = unsafe extern "C" fn(i32, i32, *mut c_void) -> i32;

/// The loaded C API (function pointers resolved from the library; kept alive with it).
pub struct SherpaApi {
    pub create_diarization: unsafe extern "C" fn(*const DiarizationConfig) -> *const Diarization,
    pub destroy_diarization: unsafe extern "C" fn(*const Diarization),
    pub process_with_callback: unsafe extern "C" fn(
        *const Diarization,
        *const f32,
        i32,
        Option<ProgressCallback>,
        *mut c_void,
    ) -> *const DiarizationResult,
    pub result_num_segments: unsafe extern "C" fn(*const DiarizationResult) -> i32,
    pub result_sort_by_start_time: unsafe extern "C" fn(*const DiarizationResult) -> *const DiarizationSegment,
    pub destroy_segment: unsafe extern "C" fn(*const DiarizationSegment),
    pub destroy_result: unsafe extern "C" fn(*const DiarizationResult),
    pub create_extractor: unsafe extern "C" fn(*const EmbeddingExtractorConfig) -> *const EmbeddingExtractor,
    pub destroy_extractor: unsafe extern "C" fn(*const EmbeddingExtractor),
    pub extractor_dim: unsafe extern "C" fn(*const EmbeddingExtractor) -> i32,
    pub extractor_create_stream: unsafe extern "C" fn(*const EmbeddingExtractor) -> *const OnlineStream,
    pub extractor_is_ready: unsafe extern "C" fn(*const EmbeddingExtractor, *const OnlineStream) -> i32,
    pub extractor_compute: unsafe extern "C" fn(*const EmbeddingExtractor, *const OnlineStream) -> *const f32,
    pub extractor_destroy_embedding: unsafe extern "C" fn(*const f32),
    pub stream_accept_waveform: unsafe extern "C" fn(*const OnlineStream, i32, *const f32, i32),
    pub stream_input_finished: unsafe extern "C" fn(*const OnlineStream),
    pub destroy_stream: unsafe extern "C" fn(*const OnlineStream),
    /// Where the library was loaded from.
    pub path: PathBuf,
    _onnxruntime: Option<libloading::Library>,
    _library: libloading::Library,
}

// The function pointers are plain C entry points and the library handle is never unloaded while in use.
unsafe impl Send for SherpaApi {}
unsafe impl Sync for SherpaApi {}

/// The dev copy of the libraries in the Python venv (debug builds only).
fn dev_library_dir() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(3)?.to_path_buf();
    let dir = if cfg!(windows) {
        root.join("engine").join(".venv").join("Lib").join("site-packages").join("sherpa_onnx").join("lib")
    } else {
        // POSIX venvs: lib/pythonX.Y/site-packages; pick the first match.
        let lib = root.join("engine").join(".venv").join("lib");
        let py = std::fs::read_dir(&lib).ok()?.flatten().map(|e| e.path()).find(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("python"))
        })?;
        py.join("site-packages").join("sherpa_onnx").join("lib")
    };
    dir.is_dir().then_some(dir)
}

/// The C API library to load, or `None` if none is found.
pub fn locate() -> Option<PathBuf> {
    if let Some(ort) = std::env::var_os("ORT_DYLIB_PATH") {
        let candidate = Path::new(&ort).parent().map(|d| d.join(SHERPA_LIBRARY_NAME));
        if let Some(c) = candidate.filter(|c| c.is_file()) {
            return Some(c);
        }
    }
    se_core::fsutil::find_native_library(SHERPA_LIBRARY_NAME)
        .or_else(|| dev_library_dir().map(|d| d.join(SHERPA_LIBRARY_NAME)).filter(|p| p.is_file()))
}

/// Opens a library so that its own directory is searched for its dependencies (onnxruntime).
fn open_library(path: &Path) -> std::result::Result<libloading::Library, libloading::Error> {
    #[cfg(windows)]
    {
        use libloading::os::windows::{LOAD_WITH_ALTERED_SEARCH_PATH, Library};
        // SAFETY: loading a trusted native library; its initializers run here.
        unsafe { Library::load_with_flags(path, LOAD_WITH_ALTERED_SEARCH_PATH).map(Into::into) }
    }
    #[cfg(not(windows))]
    {
        // SAFETY: as above.
        unsafe { libloading::Library::new(path) }
    }
}

fn load_error(detail: impl std::fmt::Display) -> EngineError {
    // stderr is the free-form log; the user sees the localized message.
    eprintln!("[diarize] sherpa-onnx C API: {detail}");
    EngineError::model(se_core::t!("diarize_load_failed"))
}

impl SherpaApi {
    /// Loads the library found by the search order above. Missing library → localized
    /// `diarize_load_failed` (the detail goes to stderr).
    pub fn load() -> Result<Self> {
        match locate() {
            Some(path) => Self::load_from(&path),
            None => Err(load_error(format!(
                "{SHERPA_LIBRARY_NAME} not found next to the executable or in ${}",
                se_core::fsutil::SHERPA_DIR_ENV_VAR
            ))),
        }
    }

    /// Loads a specific library file.
    pub fn load_from(path: &Path) -> Result<Self> {
        let onnxruntime = match std::env::var_os("ORT_DYLIB_PATH") {
            Some(ort) if Path::new(&ort).is_file() => {
                Some(open_library(Path::new(&ort)).map_err(|e| load_error(format!("{}: {e}", Path::new(&ort).display())))?)
            }
            _ => None,
        };
        let library = open_library(path).map_err(|e| load_error(format!("{}: {e}", path.display())))?;
        macro_rules! sym {
            ($name:literal) => {{
                // SAFETY: the symbol has the C signature declared in c-api.h (sherpa-onnx 2.0.1).
                let s = unsafe { library.get(concat!($name, "\0").as_bytes()) }
                    .map_err(|e| load_error(format!("{}: {e}", $name)))?;
                *s
            }};
        }
        Ok(Self {
            create_diarization: sym!("SherpaOnnxCreateOfflineSpeakerDiarization"),
            destroy_diarization: sym!("SherpaOnnxDestroyOfflineSpeakerDiarization"),
            process_with_callback: sym!("SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback"),
            result_num_segments: sym!("SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments"),
            result_sort_by_start_time: sym!("SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime"),
            destroy_segment: sym!("SherpaOnnxOfflineSpeakerDiarizationDestroySegment"),
            destroy_result: sym!("SherpaOnnxOfflineSpeakerDiarizationDestroyResult"),
            create_extractor: sym!("SherpaOnnxCreateSpeakerEmbeddingExtractor"),
            destroy_extractor: sym!("SherpaOnnxDestroySpeakerEmbeddingExtractor"),
            extractor_dim: sym!("SherpaOnnxSpeakerEmbeddingExtractorDim"),
            extractor_create_stream: sym!("SherpaOnnxSpeakerEmbeddingExtractorCreateStream"),
            extractor_is_ready: sym!("SherpaOnnxSpeakerEmbeddingExtractorIsReady"),
            extractor_compute: sym!("SherpaOnnxSpeakerEmbeddingExtractorComputeEmbedding"),
            extractor_destroy_embedding: sym!("SherpaOnnxSpeakerEmbeddingExtractorDestroyEmbedding"),
            stream_accept_waveform: sym!("SherpaOnnxOnlineStreamAcceptWaveform"),
            stream_input_finished: sym!("SherpaOnnxOnlineStreamInputFinished"),
            destroy_stream: sym!("SherpaOnnxDestroyOnlineStream"),
            path: path.to_path_buf(),
            _onnxruntime: onnxruntime,
            _library: library,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn struct_layout_matches_c_api_on_64_bit() {
        if size_of::<usize>() != 8 {
            return;
        }
        assert_eq!(size_of::<PyannoteModelConfig>(), 16);
        assert_eq!(size_of::<SegmentationModelConfig>(), 32);
        assert_eq!(offset_of!(SegmentationModelConfig, provider), 24);
        assert_eq!(size_of::<EmbeddingExtractorConfig>(), 24);
        assert_eq!(size_of::<FastClusteringConfig>(), 12);
        assert_eq!(offset_of!(DiarizationConfig, embedding), 32);
        assert_eq!(offset_of!(DiarizationConfig, clustering), 56);
        assert_eq!(offset_of!(DiarizationConfig, min_duration_on), 68);
        assert_eq!(size_of::<DiarizationConfig>(), 80);
        assert_eq!(align_of::<DiarizationConfig>(), 8);
        assert_eq!(size_of::<DiarizationSegment>(), 16);
    }
}
