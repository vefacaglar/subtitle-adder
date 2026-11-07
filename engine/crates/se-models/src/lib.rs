//! Model catalog, Hugging Face cache, downloads, `delete-model`, CUDA 12 libraries and the llama.cpp runtime
//! (port of `models.py`, `cuda.py`, `llama.py`; spec: `spec/models.md`).
//!
//! The HF cache layout must stay compatible with huggingface_hub (`models--org--name/{blobs,snapshots/<rev>,
//! refs/main}`), so models downloaded by the Python engine are reused as is.

pub mod cache;
pub mod catalog;
pub mod cuda;
pub mod delete;
pub mod download;
pub mod fnmatch;
pub mod fsx;
pub mod http;
pub mod hub;
pub mod llama;
pub mod scan;

pub use cache::{cached_disk_mb, delete_model, disk_mb, is_downloaded, model_dir, model_file};
pub use catalog::{Backend, KATALOG, ModelInfo, ModelKind, find, support_error};
pub use download::{ensure_model, needs_download, require_download};
pub use se_core::i18n::{display_name, language_name};
pub use se_core::offline::{go_offline, offline_requested};
