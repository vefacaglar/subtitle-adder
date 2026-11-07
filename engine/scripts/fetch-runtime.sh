#!/usr/bin/env bash
# Downloads the pinned prebuilt native runtimes the Rust engine needs on macOS/Linux into engine/.runtime (gitignored).
#
#   engine/scripts/fetch-runtime.sh [--only sherpa|ffmpeg] [--force]
#
#   engine/.runtime/sherpa-onnx   sherpa-onnx 2.0.1 C API (libsherpa-onnx-c-api) and its libonnxruntime, from the
#                                 k2-fsa/sherpa-onnx release "*-shared-lib" assets (osx-arm64 / osx-x64 /
#                                 linux-x64 / linux-aarch64). se-diarize's FFI structs follow this exact version.
#   engine/.runtime/ffmpeg        Linux only: FFmpeg 8.1 shared SDK (lib, include, relocatable lib/pkgconfig) from a
#                                 dated BtbN autobuild (month-end autobuilds stay published long term), since distro
#                                 packages lag behind FFmpeg 8. On macOS use Homebrew's ffmpeg (8.x).
#
# Every archive is checked against a pinned SHA-256 before it is extracted. A component whose folder already holds
# the stamp of the pinned hash is skipped (--force downloads again).
#
# The engine's debug builds find engine/.runtime/sherpa-onnx on their own; stage-runtime.sh falls back to it when
# SHERPA_DIR / SUBTITLE_ENGINE_SHERPA_DIR are unset. For cargo on Linux, export the printed FFMPEG_DIR and add
# $FFMPEG_DIR/lib to LD_LIBRARY_PATH (tests load the shared FFmpeg libraries). On GitHub Actions this script appends
# FFMPEG_DIR, PKG_CONFIG_PATH, LD_LIBRARY_PATH and SUBTITLE_ENGINE_SHERPA_DIR to $GITHUB_ENV.
set -euo pipefail

only=all
force=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --only) only="${2:?--only needs sherpa or ffmpeg}"; shift 2 ;;
    --force) force=1; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$only" in all|sherpa|ffmpeg) ;; *) echo "--only must be sherpa or ffmpeg" >&2; exit 2 ;; esac

here="$(cd "$(dirname "$0")" && pwd)"
engine="$(dirname "$here")"
runtime="$engine/.runtime"

SHERPA_BASE=https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1
FFMPEG_BASE=https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-31-12-59
FFMPEG_BUILD=ffmpeg-n8.1.3-27-g8dca1586f4

os="$(uname -s)"
arch="$(uname -m)"
sherpa_asset="" sherpa_sha="" ffmpeg_asset="" ffmpeg_sha=""
case "$os/$arch" in
  Darwin/arm64)
    sherpa_asset=sherpa-onnx-v2.0.1-osx-arm64-shared-lib
    sherpa_sha=e2d57ad9c83803d6c48f259caa66e44879bd4c4724c61f0d51a277b411bffb1d ;;
  Darwin/x86_64)
    sherpa_asset=sherpa-onnx-v2.0.1-osx-x64-shared-lib
    sherpa_sha=be9042971c5d1b14da76a960534d2a8cdb6f0e5e02f5e1dc8bddff8576359bdd ;;
  Linux/x86_64)
    sherpa_asset=sherpa-onnx-v2.0.1-linux-x64-shared-lib
    sherpa_sha=532c45bbd41036d83cd70606797529aa8086c2053395a58967736c727625b120
    ffmpeg_asset=$FFMPEG_BUILD-linux64-gpl-shared-8.1
    ffmpeg_sha=202fe67679a0860b155817a0907ffcc5c1ea82e186acaa21cb87d307f54fd59a ;;
  Linux/aarch64|Linux/arm64)
    sherpa_asset=sherpa-onnx-v2.0.1-linux-aarch64-shared-cpu-lib
    sherpa_sha=15bd737eae467da32ec97e3740a329a535fd21e33caca4f327b0081451d11533
    ffmpeg_asset=$FFMPEG_BUILD-linuxarm64-gpl-shared-8.1
    ffmpeg_sha=9d3161962e57f0f621b87c1fe4287ad14b4fd12c74c243436143319e4e14a0a0 ;;
  *) echo "unsupported platform: $os/$arch (Windows: use fetch-runtime.ps1)" >&2; exit 1 ;;
esac
if [[ "$os" == Darwin ]]; then lib_ext=dylib; else lib_ext=so; fi

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'; else shasum -a 256 "$1" | awk '{print $1}'; fi
}

# install <name> <dest dir> <url> <sha256> <path inside the archive> <required files...>
install_component() {
  local name="$1" dest="$2" url="$3" sha="$4" inner="$5"; shift 5
  local f missing=0
  if [[ -f "$dest/.sha256" && "$(cat "$dest/.sha256")" == "$sha" ]]; then
    for f in "$@"; do [[ -e "$dest/$f" ]] || missing=1; done
    if [[ $missing == 0 && $force == 0 ]]; then echo "$name: up to date ($dest)"; return; fi
  fi

  local work="$runtime/.download-$name"
  rm -rf "$work"; mkdir -p "$work/x"
  local archive="$work/${url##*/}"
  echo "$name: downloading $url"
  curl -fsSL --retry 3 -o "$archive" "$url"
  local actual; actual="$(sha256 "$archive")"
  if [[ "$actual" != "$sha" ]]; then
    echo "$name: SHA-256 mismatch for $archive" >&2
    echo "  expected $sha" >&2
    echo "  actual   $actual" >&2
    rm -rf "$work"; exit 1
  fi
  tar -xf "$archive" -C "$work/x"
  for f in "$@"; do
    [[ -e "$work/x/$inner/$f" ]] || { echo "$name: $f missing from $url" >&2; rm -rf "$work"; exit 1; }
  done
  rm -rf "$dest"
  mv "$work/x/$inner" "$dest"
  printf '%s' "$sha" > "$dest/.sha256"
  rm -rf "$work"
  echo "$name: installed into $dest"
}

mkdir -p "$runtime"
sherpa_dir="" ffmpeg_dir=""
if [[ "$only" == all || "$only" == sherpa ]]; then
  sherpa_dir="$runtime/sherpa-onnx"
  install_component sherpa "$sherpa_dir" "$SHERPA_BASE/$sherpa_asset.tar.bz2" "$sherpa_sha" "$sherpa_asset/lib" \
    "libsherpa-onnx-c-api.$lib_ext" "libonnxruntime.$lib_ext"
  echo "SUBTITLE_ENGINE_SHERPA_DIR=$sherpa_dir"
fi
if [[ "$only" == all || "$only" == ffmpeg ]]; then
  if [[ -n "$ffmpeg_asset" ]]; then
    ffmpeg_dir="$runtime/ffmpeg"
    install_component ffmpeg "$ffmpeg_dir" "$FFMPEG_BASE/$ffmpeg_asset.tar.xz" "$ffmpeg_sha" "$ffmpeg_asset" \
      lib/libavcodec.so lib/libavformat.so lib/pkgconfig/libavcodec.pc include/libavcodec/avcodec.h
    echo "FFMPEG_DIR=$ffmpeg_dir"
  elif [[ "$only" == ffmpeg ]]; then
    echo "ffmpeg: no pinned build for $os (macOS: brew install ffmpeg)" >&2; exit 1
  fi
fi

# GitHub Actions: export the folders to later steps.
if [[ -n "${GITHUB_ENV:-}" ]]; then
  [[ -n "$sherpa_dir" ]] && echo "SUBTITLE_ENGINE_SHERPA_DIR=$sherpa_dir" >> "$GITHUB_ENV"
  if [[ -n "$ffmpeg_dir" ]]; then
    {
      echo "FFMPEG_DIR=$ffmpeg_dir"
      echo "PKG_CONFIG_PATH=$ffmpeg_dir/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
      echo "LD_LIBRARY_PATH=$ffmpeg_dir/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    } >> "$GITHUB_ENV"
  fi
fi
exit 0
