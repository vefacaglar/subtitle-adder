#!/usr/bin/env bash
# Release mode for macOS: packages the Rust desktop app (scripts/package-desktop-macos.sh, release build, signed
# inside-out; ad-hoc unless SIGN_IDENTITY is set) into build/desktop/Subtitle Adder.app and opens it.
# Used by the VS Code entry "Rust: Release (macOS)". MAKE_DMG=1 also builds the .dmg; NO_LAUNCH=1 skips opening.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
bash "$ROOT/scripts/package-desktop-macos.sh"
if [ "${NO_LAUNCH:-0}" != "1" ]; then
  open "$ROOT/build/desktop/Subtitle Adder.app"
fi
