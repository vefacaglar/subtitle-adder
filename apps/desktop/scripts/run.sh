#!/usr/bin/env bash
# Run mode for macOS: builds the Rust engine into engine/dist when it is missing or older than its sources, builds the
# Rust desktop app (release) and starts it. The dev app finds the engine in the repo's engine/dist.
# Used by the VS Code entry "Rust: Run (macOS)".
#
#   apps/desktop/scripts/run.sh [app arguments...]
#
# Environment: CARGO_TARGET_DIR (default apps/desktop/target), FFMPEG_DIR (default: `brew --prefix ffmpeg`).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
DESKTOP="$ROOT/apps/desktop"
ENGINE="$ROOT/engine"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export FFMPEG_DIR="${FFMPEG_DIR:-$(brew --prefix ffmpeg 2>/dev/null || true)}"
TARGET="${CARGO_TARGET_DIR:-$DESKTOP/target}"

# Engine: rebuild only when engine/dist is missing or any engine source is newer.
engine_exe="$ENGINE/dist/subtitle-engine"
if [ ! -x "$engine_exe" ] || [ -n "$(find "$ENGINE/crates" "$ENGINE/Cargo.toml" "$ENGINE/Cargo.lock" -newer "$engine_exe" -type f \( -name '*.rs' -o -name '*.toml' -o -name 'Cargo.lock' \) 2>/dev/null | head -1)" ]; then
  echo "==> Building the engine (engine/dist)"
  bash "$ENGINE/scripts/fetch-runtime.sh"
  (cd "$ENGINE" && cargo build --release -p se-cli)
  bash "$ENGINE/scripts/stage-runtime.sh"
fi

echo "==> Building the desktop app (release)"
(cd "$DESKTOP" && CARGO_TARGET_DIR="$TARGET" cargo build --release -p sa-app --bin SubtitleAdder)

echo "==> Starting $TARGET/release/SubtitleAdder"
exec "$TARGET/release/SubtitleAdder" "$@"
