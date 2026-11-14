//! Windows MSVC: link CTranslate2 and oneDNN against Intel OpenMP (`libiomp5md`), as the Python ctranslate2 wheel
//! does (it is built with MSVC `/openmp` and ships `libiomp5md.dll`).
//!
//! Why: oneDNN (ct2rs feature `dnnl`, needed for int8 on AMD CPUs and for the same GEMM backend as the wheel) is
//! compiled with `/openmp`. Without an OpenMP-enabled CTranslate2 its own thread pool calls into oneDNN from several
//! threads, every call forks a vcomp team, and decoding becomes ~100x slower. CTranslate2 refuses
//! `OPENMP_RUNTIME=COMP` together with MKL on Windows, so the runtime is Intel's, whose import library also provides
//! the `_vcomp_*` entry points of the MSVC-compiled code.
//!
//! ct2rs (feature `openmp-runtime-intel`) emits `rustc-link-lib=iomp5`, but the MKL prebuild (nuget packages
//! `intelopenmp.devel.win` / `intelopenmp.redist.win`) names the files `libiomp5md.lib` / `libiomp5md.dll`. This
//! script copies the import library to `$OUT_DIR/iomp5.lib` (propagated as a link search path) and the DLL next to
//! the binaries of the current profile (`target/<profile>`, `deps`, `examples`) so tests and examples run.
//! Packaging must ship `libiomp5md.dll` next to `subtitle-engine.exe`.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !windows_msvc {
        return;
    }
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    // Cargo < 1.100: OUT_DIR = <target>/<profile>/build/se-asr-<hash>/out
    // Cargo >= 1.100: OUT_DIR = <target>/<profile>/build/se-asr/<hash>/out
    let Some(build_dir) = out_dir.ancestors().skip(2).find(|p| p.file_name().is_some_and(|n| n == "build")) else {
        return;
    };
    let Some(profile_dir) = build_dir.parent() else { return };

    let lib = find_in_prebuild(build_dir, &["intelopenmp.devel.win", "build", "native", "win-x64", "libiomp5md.lib"]);
    let dll = find_in_prebuild(build_dir, &["intelopenmp.redist.win", "runtimes", "win-x64", "native", "libiomp5md.dll"]);
    let (Some(lib), Some(dll)) = (lib, dll) else {
        println!("cargo:warning=libiomp5md not found in the intel-onemkl-prebuild output; linking iomp5 may fail");
        return;
    };
    std::fs::copy(&lib, out_dir.join("iomp5.lib")).expect("copy libiomp5md.lib");
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    for dir in [profile_dir.to_path_buf(), profile_dir.join("deps"), profile_dir.join("examples")] {
        let _ = std::fs::create_dir_all(&dir);
        let dest = dir.join("libiomp5md.dll");
        let fresh = std::fs::metadata(&dest).map(|m| m.len()).ok() == std::fs::metadata(&dll).map(|m| m.len()).ok();
        if !fresh {
            let _ = std::fs::copy(&dll, &dest);
        }
    }
}

/// `<build>/intel-onemkl-prebuild-*/out/<parts...>` (Cargo < 1.100) or
/// `<build>/intel-onemkl-prebuild/*/out/<parts...>` (Cargo >= 1.100), newest first.
fn find_in_prebuild(build_dir: &Path, parts: &[&str]) -> Option<PathBuf> {
    let mut out_dirs = Vec::new();
    for entry in std::fs::read_dir(build_dir).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("intel-onemkl-prebuild-") {
            out_dirs.push(entry.path().join("out"));
        } else if name == "intel-onemkl-prebuild" {
            if let Ok(hashes) = std::fs::read_dir(entry.path()) {
                for hash in hashes.flatten() {
                    out_dirs.push(hash.path().join("out"));
                }
            }
        }
    }
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = out_dirs
        .into_iter()
        .map(|out| parts.iter().fold(out, |p, part| p.join(part)))
        .filter(|p| p.is_file())
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
        .collect();
    found.sort();
    found.pop().map(|(_, p)| p)
}
