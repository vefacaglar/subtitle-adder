#!/usr/bin/env bash
# Type-checks the whole workspace for macOS (aarch64-apple-darwin) on a Windows (or Linux) machine without an
# Apple SDK: `cargo check` only, nothing is linked or run. Two native build scripts would otherwise need an Apple
# C toolchain; for a check-only run they are satisfied like this:
#
# * ring (via ureq/rustls: sa-enginekit, sa-providers, sa-viewmodels, sa-app): its build script only compiles C/asm
#   into a static library and prints the link lines, so it is replaced by a build-script override on its `links`
#   key (`target.aarch64-apple-darwin.ring_core_<version>_`, the version read from Cargo.lock). Nothing in ring's
#   Rust code reads build-script output.
# * ffmpeg-sys-next (sa-player `ffmpeg`, sa-app): its build script must run (it generates bindings.rs with bindgen
#   and the `ffmpeg_x_y` cfgs). The feature probe compiles for the host (FFMPEG_DIR = the Windows dev libs, same
#   FFmpeg 8.1 headers); bindgen targets aarch64-apple-darwin and only lacks libc headers, which
#   tools/macos-check/include provides as LP64 Darwin stand-ins (`-nostdinc`, clang's predefined type macros).
#
# Everything else (objc2 + objc2-* framework crates, cpal/coreaudio-rs, winit, Slint with the mac tree and its
# native MenuBar, rfd) is pure Rust for this target and checks as is.
#
# A local convenience: CI builds and tests the desktop app on a real macOS runner (.github/workflows/ci.yml).
#
# Usage (Git Bash, from apps/desktop): scripts/check-macos.sh [extra cargo args, e.g. -p sa-platform]
# Needs: rustup target add aarch64-apple-darwin; libclang and a host C compiler on PATH (or DEVENV = a shell file
# that sets them up, sourced first); FFMPEG_DIR = an FFmpeg 8 SDK (default: engine/.runtime/ffmpeg, which
# engine/scripts/fetch-runtime.ps1 installs).
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
cd "$here"

if [[ -n "${DEVENV:-}" ]]; then
    # shellcheck disable=SC1090
    source "$DEVENV" >/dev/null 2>&1
fi
if [[ -z "${FFMPEG_DIR:-}" ]]; then
    FFMPEG_DIR="$(cd "$here/../.." && pwd)/engine/.runtime/ffmpeg"
    if command -v cygpath >/dev/null 2>&1; then FFMPEG_DIR="$(cygpath -m "$FFMPEG_DIR")"; fi
fi
if [[ ! -d "$FFMPEG_DIR/include" ]]; then
    echo "check-macos: no FFmpeg headers under FFMPEG_DIR=$FFMPEG_DIR" >&2
    exit 1
fi
export FFMPEG_DIR
include="$here/tools/macos-check/include"
if command -v cygpath >/dev/null 2>&1; then include="$(cygpath -m "$include")"; fi
export BINDGEN_EXTRA_CLANG_ARGS_aarch64_apple_darwin="-nostdinc -isystem $include"

# ring's `links` key carries its version: `ring_core_0_17_14_` for 0.17.14.
ring="$(tr -d '\r' < Cargo.lock | awk '/^name = "ring"$/ { getline; gsub(/version = |"/, ""); print; exit }')"
if [[ -z "$ring" ]]; then
    echo "check-macos: ring not found in Cargo.lock" >&2
    exit 1
fi
ring_key="ring_core_${ring//./_}_"

args=("$@")
if [[ ${#args[@]} -eq 0 ]]; then args=(--workspace --all-targets); fi
exec cargo check -j "${JOBS:-4}" --target aarch64-apple-darwin \
    --config "target.aarch64-apple-darwin.${ring_key}.rustc-link-search=[]" \
    "${args[@]}"
