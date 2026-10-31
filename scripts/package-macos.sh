#!/usr/bin/env bash
# Packages Subtitle Adder.app: the SwiftUI app + the PyInstaller (onedir) engine.
#
#   scripts/package-macos.sh
#
# Environment variables:
#   SIGN_IDENTITY   Developer ID identity ("Developer ID Application: Name (TEAMID)").
#                   If unset, ad-hoc signing (`-`); only runs on this machine.
#   NOTARY_PROFILE  Profile name saved with `xcrun notarytool store-credentials`;
#                   if set (and SIGN_IDENTITY is set), notarizes and staples.
#   VERSION         Short version (default: 0.1.0)   BUILD_NUMBER (default: 1)
#   BUNDLE_ID       default: app.subtitleadder.mac
#   MAKE_DMG=1      Also builds build/Subtitle Adder.dmg.
#   SKIP_ENGINE_BUILD=1  Skips PyInstaller if build/engine-dist is already built.
#
# Builds only for the architecture of the build machine (arm64 on Apple Silicon); not universal.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$ROOT/build"
APP="$BUILD/Subtitle Adder.app"
CONTENTS="$APP/Contents"
ENGINE_DIST="$BUILD/engine-dist/subtitle-engine"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"
VERSION="${VERSION:-0.1.0}"
BUILD_NUMBER="${BUILD_NUMBER:-1}"
BUNDLE_ID="${BUNDLE_ID:-app.subtitleadder.mac}"
ENTITLEMENTS="$ROOT/scripts/macos/engine.entitlements"

log() { printf '\n==> %s\n' "$*"; }

# notarytool can return 0 even on an "Invalid" result; this reads the status and prints Apple's log on failure.
notarize() {
    local out id status
    out="$(xcrun notarytool submit "$1" --keychain-profile "$NOTARY_PROFILE" --wait --output-format json)"
    id="$(plutil -extract id raw -o - - <<<"$out")"
    status="$(plutil -extract status raw -o - - <<<"$out")"
    if [ "$status" != "Accepted" ]; then
        echo "Notarization failed ($status)." >&2
        xcrun notarytool log "$id" --keychain-profile "$NOTARY_PROFILE" >&2 || true
        exit 1
    fi
}

if ! command -v uv >/dev/null 2>&1; then
    for d in "$HOME/.local/bin" /opt/homebrew/bin /usr/local/bin "$HOME/.cargo/bin"; do
        if [ -x "$d/uv" ]; then PATH="$d:$PATH"; break; fi
    done
fi
# python.org's framework-style Python puts a nested .framework in the onedir; uv's own Python doesn't.
export UV_PYTHON_PREFERENCE=only-managed

log "1/5 Building app (swift build -c release)"
(cd "$ROOT/apps/macos" && swift build -c release)
APP_BIN="$(cd "$ROOT/apps/macos" && swift build -c release --show-bin-path)/SubtitleAdderApp"

if [ "${SKIP_ENGINE_BUILD:-0}" != "1" ] || [ ! -x "$ENGINE_DIST/subtitle-engine" ]; then
    log "2/5 Building engine (PyInstaller onedir)"
    if ! command -v uv >/dev/null 2>&1; then
        echo "uv not found; add it to PATH (https://docs.astral.sh/uv/)." >&2
        exit 1
    fi
    (cd "$ROOT/engine" && uv run --group build pyinstaller subtitle-engine.spec --noconfirm \
        --distpath "$BUILD/engine-dist" --workpath "$BUILD/engine-work")
else
    log "2/5 Engine build skipped"
fi

log "3/5 Assembling bundle"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$APP_BIN" "$CONTENTS/MacOS/SubtitleAdderApp"
# The engine lives under Contents/Resources/subtitle-engine: codesign rejects any non-Mach-O file
# in places like Frameworks/MacOS/Helpers ("code object is not signed at all"),
# while the PyInstaller folder contains many data files such as .py/.json/.onnx. The Mach-O files
# in Resources are signed one by one below (notarization wants all of them, wherever they are).
# cp -R: keeps PyInstaller's symlinks as they are.
cp -R "$ENGINE_DIST" "$CONTENTS/Resources/subtitle-engine"
sed -e "s|__BUNDLE_ID__|$BUNDLE_ID|" -e "s|__VERSION__|$VERSION|" -e "s|__BUILD__|$BUILD_NUMBER|" \
    "$ROOT/scripts/macos/Info.plist.in" > "$CONTENTS/Info.plist"
printf 'APPL????' > "$CONTENTS/PkgInfo"
# Icon: drawn at 1024 px by scripts/macos/make-icon.swift, converted to .icns with iconutil.
ICONSET="$BUILD/AppIcon.iconset"
rm -rf "$ICONSET" && mkdir -p "$ICONSET"
swift "$ROOT/scripts/macos/make-icon.swift" "$BUILD/AppIcon-1024.png" 1024
for px in 16 32 128 256 512; do
    sips -z "$px" "$px" "$BUILD/AppIcon-1024.png" --out "$ICONSET/icon_${px}x${px}.png" >/dev/null
    sips -z $((px * 2)) $((px * 2)) "$BUILD/AppIcon-1024.png" --out "$ICONSET/icon_${px}x${px}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$CONTENTS/Resources/AppIcon.icns"
plutil -lint "$CONTENTS/Info.plist" >/dev/null

if [ "$SIGN_IDENTITY" = "-" ]; then SIGN_LABEL="ad-hoc"; else SIGN_LABEL="$SIGN_IDENTITY"; fi
log "4/5 Signing ($SIGN_LABEL)"
SIGN_ARGS=(--force --sign "$SIGN_IDENTITY")
if [ "$SIGN_IDENTITY" != "-" ]; then
    SIGN_ARGS+=(--options runtime --timestamp)
fi
# Inside out: first every Mach-O file inside the engine (dylib/.so/binary), then the engine, then the app.
ENGINE_DIR="$CONTENTS/Resources/subtitle-engine"
ENGINE_BIN="$ENGINE_DIR/subtitle-engine"
while IFS= read -r -d '' f; do
    [ "$f" = "$ENGINE_BIN" ] && continue
    case "$(file -b "$f")" in *Mach-O*) codesign "${SIGN_ARGS[@]}" "$f" ;; esac
done < <(find "$ENGINE_DIR" -type f -print0)
ENGINE_ARGS=("${SIGN_ARGS[@]}")
if [ "$SIGN_IDENTITY" != "-" ]; then ENGINE_ARGS+=(--entitlements "$ENTITLEMENTS"); fi
codesign "${ENGINE_ARGS[@]}" --identifier "$BUNDLE_ID.engine" "$ENGINE_BIN"
codesign "${SIGN_ARGS[@]}" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
# --deep doesn't verify Mach-O files under Resources as code; verify each one separately.
while IFS= read -r -d '' f; do
    case "$(file -b "$f")" in
        *Mach-O*) codesign --verify --strict "$f" || { echo "Unsigned or invalid signature: $f" >&2; exit 1; } ;;
    esac
done < <(find "$ENGINE_DIR" -type f -print0)

if [ -n "${NOTARY_PROFILE:-}" ]; then
    if [ "$SIGN_IDENTITY" = "-" ]; then
        echo "NOTARY_PROFILE requires SIGN_IDENTITY (Developer ID)." >&2
        exit 1
    fi
    log "Notarization"
    ZIP="$BUILD/notarize.zip"
    ditto -c -k --keepParent "$APP" "$ZIP"
    notarize "$ZIP"
    xcrun stapler staple "$APP"
    rm -f "$ZIP"
fi

if [ "${MAKE_DMG:-0}" = "1" ]; then
    log "5/5 DMG"
    DMG="$BUILD/Subtitle Adder.dmg"
    STAGE="$BUILD/dmg-stage"
    rm -rf "$STAGE" "$DMG"
    mkdir -p "$STAGE"
    cp -R "$APP" "$STAGE/"
    ln -s /Applications "$STAGE/Applications"
    hdiutil create -volname "Subtitle Adder" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
    rm -rf "$STAGE"
    if [ "$SIGN_IDENTITY" = "-" ]; then
        echo "Warning: an ad-hoc signed dmg is blocked by Gatekeeper on another Mac." >&2
    else
        codesign --force --sign "$SIGN_IDENTITY" --timestamp "$DMG"
        if [ -n "${NOTARY_PROFILE:-}" ]; then
            notarize "$DMG"
            xcrun stapler staple "$DMG"
        fi
    fi
fi

log "Done: $APP"
du -sh "$APP" "$ENGINE_DIR"
