#!/usr/bin/env bash
# Stages the Rust engine as a standalone folder on macOS: subtitle-engine plus the dylibs it loads.
#
#   engine/scripts/stage-runtime.sh            # uses $CARGO_TARGET_DIR (or engine/target), profile release
#
# Environment:
#   CARGO_TARGET_DIR  cargo target dir (default engine/target)
#   PROFILE           cargo profile folder (default release)
#   DIST              output folder (default engine/dist, gitignored)
#   FFMPEG_DIR        FFmpeg 8.x shared install (lib/*.dylib); default: `brew --prefix ffmpeg`
#   SHERPA_DIR        folder with libsherpa-onnx-c-api.dylib and libonnxruntime*.dylib
#                     (default: $SUBTITLE_ENGINE_SHERPA_DIR, else engine/.runtime/sherpa-onnx from fetch-runtime.sh)
#
# The FFmpeg dylibs are copied next to the binary and every absolute install name (Homebrew) is rewritten to
# @executable_path/<name>, so the folder runs without DYLD_* variables. The engine finds the sherpa-onnx and
# ONNX Runtime dylibs next to the executable on its own (libloading). Nothing is signed here; scripts/package-macos.sh
# signs inside-out.
# NOTE: written on Windows; not yet run on a Mac.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
engine_rs="$(dirname "$here")"
target="${CARGO_TARGET_DIR:-$engine_rs/target}"
profile="${PROFILE:-release}"
dist="${DIST:-$engine_rs/dist}"
ffmpeg="${FFMPEG_DIR:-$(brew --prefix ffmpeg 2>/dev/null || true)}"
sherpa="${SHERPA_DIR:-${SUBTITLE_ENGINE_SHERPA_DIR:-$engine_rs/.runtime/sherpa-onnx}}"
[[ "$(uname -s)" == Darwin ]] || { echo "stage-runtime.sh supports macOS only (Windows: stage-runtime.ps1)" >&2; exit 1; }

exe="$target/$profile/subtitle-engine"
[[ -x "$exe" ]] || { echo "subtitle-engine not found in $target/$profile (cargo build --release -p se-cli)" >&2; exit 1; }
[[ -d "$ffmpeg/lib" ]] || { echo "FFmpeg not found: set FFMPEG_DIR" >&2; exit 1; }
[[ -f "$sherpa/libsherpa-onnx-c-api.dylib" ]] || {
  echo "sherpa-onnx libs not found in $sherpa: set SHERPA_DIR or run engine/scripts/fetch-runtime.sh" >&2; exit 1; }

mkdir -p "$dist"
echo "Staging into $dist"
cp -f "$exe" "$dist/"
shopt -s nullglob
for lib in "$sherpa"/libsherpa-onnx-c-api.dylib "$sherpa"/libonnxruntime*.dylib; do
  cp -fL "$lib" "$dist/"; echo "  $(basename "$lib")"
done

# Copy every FFmpeg dylib the binary (transitively) references and rewrite the install names.
copy_deps() {
  local file="$1" dep name
  while read -r dep; do
    case "$dep" in
      "$ffmpeg"/*|/opt/homebrew/*|/usr/local/*)
        name="$(basename "$dep")"
        if [[ ! -f "$dist/$name" ]]; then
          cp -fL "$dep" "$dist/$name"; chmod u+w "$dist/$name"
          install_name_tool -id "@executable_path/$name" "$dist/$name"
          echo "  $name"
          copy_deps "$dist/$name"
        fi
        install_name_tool -change "$dep" "@executable_path/$name" "$file"
        ;;
    esac
  done < <(otool -L "$file" | tail -n +2 | awk '{print $1}')
}
copy_deps "$dist/subtitle-engine"

# Smoke test with no DYLD variables.
out="$(env -u DYLD_LIBRARY_PATH -u DYLD_FALLBACK_LIBRARY_PATH "$dist/subtitle-engine" models --json)"
[[ "$out" == *'"event": "models"'* ]] || { echo "smoke test failed: $out" >&2; exit 1; }
echo "Smoke test passed (models --json)."
