#!/usr/bin/env bash
# Packages the Rust desktop app (apps/desktop, Slint + cupertino) as Subtitle Adder.app with the Rust engine folder.
#
#   scripts/package-desktop-macos.sh
#
# Same pipeline and variables as scripts/package-macos.sh (mac-ui §13.4), with the Rust app in place of SwiftPM:
#   SIGN_IDENTITY   Developer ID identity ("Developer ID Application: Name (TEAMID)").
#                   If unset, ad-hoc signing (`-`); only runs on this machine.
#   NOTARY_PROFILE  Profile name saved with `xcrun notarytool store-credentials`;
#                   if set (and SIGN_IDENTITY is set), notarizes and staples.
#   VERSION         Short version (default: 0.1.0)   BUILD_NUMBER (default: 1)
#   BUNDLE_ID       default: app.subtitleadder.mac (the UserDefaults domain and Application Support identity: keep it)
#   MAKE_DMG=1      Also builds build/desktop/Subtitle Adder.dmg.
#   SKIP_ENGINE_BUILD=1  Reuses an already staged engine/dist (skips fetch/cargo/stage).
#   FFMPEG_DIR      FFmpeg 8.x shared install the app builds against and the engine ships (lib/*.dylib; the
#                   engine's `preview` needs a GPL build with libx264); default: `brew --prefix ffmpeg`.
#   FFMPEG_LGPL_DIR FFmpeg 8.x LGPL shared install (same library versions) whose dylibs the player ships in
#                   Contents/Frameworks instead of FFMPEG_DIR's (ARCHITECTURE.md §11). Unset: FFMPEG_DIR's are
#                   bundled and the script warns, since Homebrew's ffmpeg is a GPL build.
#
# Bundle: Contents/MacOS/SubtitleAdderApp (CFBundleExecutable, the Rust `SubtitleAdder` binary renamed),
# Contents/Frameworks/*.dylib (the player's FFmpeg, install names rewritten to @executable_path/../Frameworks),
# Contents/Resources/subtitle-engine/ (copy of engine/dist, symlinks kept), Contents/Resources/AppIcon.icns,
# Contents/Info.plist (scripts/macos/Info.plist.in: document types, UTIs, localizations), Contents/PkgInfo.
# String catalogs are embedded in the binary (no resource bundles). Builds only for the host architecture.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DESKTOP="$ROOT/apps/desktop"
BUILD="$ROOT/build/desktop"
APP="$BUILD/Subtitle Adder.app"
CONTENTS="$APP/Contents"
ENGINE_DIST="$ROOT/engine/dist"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"
VERSION="${VERSION:-0.1.0}"
BUILD_NUMBER="${BUILD_NUMBER:-1}"
BUNDLE_ID="${BUNDLE_ID:-app.subtitleadder.mac}"
ENTITLEMENTS="$ROOT/scripts/macos/engine.entitlements"
EXE_NAME="SubtitleAdderApp"

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

if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found; install Rust (https://rustup.rs/)." >&2
    exit 1
fi
FFMPEG_DIR="${FFMPEG_DIR:-$(brew --prefix ffmpeg 2>/dev/null || true)}"
[ -d "$FFMPEG_DIR/lib" ] || { echo "FFmpeg not found: set FFMPEG_DIR (brew install ffmpeg)." >&2; exit 1; }
export FFMPEG_DIR
# Only the app's copy step reads it: the engine (stage-runtime.sh) keeps FFMPEG_DIR's GPL build.
PLAYER_FFMPEG_LGPL="${FFMPEG_LGPL_DIR:-}"
if [ -n "$PLAYER_FFMPEG_LGPL" ] && [ ! -d "$PLAYER_FFMPEG_LGPL/lib" ]; then
    echo "FFMPEG_LGPL_DIR=$PLAYER_FFMPEG_LGPL has no lib folder." >&2
    exit 1
fi
TARGET="${CARGO_TARGET_DIR:-$DESKTOP/target}"

log "1/5 Building app (cargo build --release -p sa-app)"
# Room in the Mach-O header for the install-name rewrites below (@executable_path/../Frameworks/… and the extra
# rpath can be longer than the Homebrew paths the linker recorded); without it install_name_tool can fail with
# "larger updated load commands do not fit".
(
    cd "$DESKTOP"
    export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C link-arg=-Wl,-headerpad_max_install_names"
    cargo build --release -p sa-app --bin SubtitleAdder
    cargo build --release -p sa-platform --bin sa-make-icon
)
APP_BIN="$TARGET/release/SubtitleAdder"
MAKE_ICON="$TARGET/release/sa-make-icon"
[ -x "$APP_BIN" ] || { echo "cargo produced no $APP_BIN." >&2; exit 1; }

if [ "${SKIP_ENGINE_BUILD:-0}" != "1" ] || [ ! -x "$ENGINE_DIST/subtitle-engine" ]; then
    log "2/5 Building engine (cargo build --release -p se-cli + stage-runtime)"
    bash "$ROOT/engine/scripts/fetch-runtime.sh"
    (cd "$ROOT/engine" && cargo build --release -p se-cli)
    bash "$ROOT/engine/scripts/stage-runtime.sh"
else
    log "2/5 Engine build skipped"
fi

log "3/5 Assembling bundle"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources" "$CONTENTS/Frameworks"
cp "$APP_BIN" "$CONTENTS/MacOS/$EXE_NAME"
chmod u+w "$CONTENTS/MacOS/$EXE_NAME"

# The player's FFmpeg: copy every dylib the app (transitively) references from FFmpeg/Homebrew into Frameworks and
# rewrite the install names (same rule as engine/scripts/stage-runtime.sh, relative to Contents/MacOS).
FRAMEWORKS="$CONTENTS/Frameworks"
# The file bundled for a library the binaries reference: FFMPEG_LGPL_DIR's library of that name when one is set
# (same names and versions, LGPL build), else the referenced file itself.
player_lib() {
    local dep="$1" name
    name="$(basename "$dep")"
    if [ -n "$PLAYER_FFMPEG_LGPL" ] && [ -f "$PLAYER_FFMPEG_LGPL/lib/$name" ]; then
        echo "$PLAYER_FFMPEG_LGPL/lib/$name"
    else
        echo "$dep"
    fi
}
copy_deps() {
    local file="$1" dep name
    while read -r dep; do
        case "$dep" in
            "$FFMPEG_DIR"/*|"${PLAYER_FFMPEG_LGPL:-/nonexistent}"/*|/opt/homebrew/*|/usr/local/*)
                name="$(basename "$dep")"
                if [[ ! -f "$FRAMEWORKS/$name" ]]; then
                    cp -fL "$(player_lib "$dep")" "$FRAMEWORKS/$name"; chmod u+w "$FRAMEWORKS/$name"
                    install_name_tool -id "@executable_path/../Frameworks/$name" "$FRAMEWORKS/$name"
                    echo "  $name"
                    copy_deps "$FRAMEWORKS/$name"
                fi
                install_name_tool -change "$dep" "@executable_path/../Frameworks/$name" "$file"
                ;;
        esac
    done < <(otool -L "$file" | tail -n +2 | awk '{print $1}')
}
copy_deps "$CONTENTS/MacOS/$EXE_NAME"
# An rpath-linked FFmpeg (@rpath/libavformat...) resolves through Frameworks as well.
install_name_tool -add_rpath "@executable_path/../Frameworks" "$CONTENTS/MacOS/$EXE_NAME" 2>/dev/null || true
while read -r dep; do
    name="$(basename "$dep")"
    if [[ ! -f "$FRAMEWORKS/$name" && -f "$FFMPEG_DIR/lib/$name" ]]; then
        cp -fL "$(player_lib "$FFMPEG_DIR/lib/$name")" "$FRAMEWORKS/$name"; chmod u+w "$FRAMEWORKS/$name"
        install_name_tool -id "@rpath/$name" "$FRAMEWORKS/$name"
        copy_deps "$FRAMEWORKS/$name"
        echo "  $name (rpath)"
    fi
done < <(otool -L "$CONTENTS/MacOS/$EXE_NAME" | tail -n +2 | awk '{print $1}' | grep '^@rpath/' || true)
rmdir "$FRAMEWORKS" 2>/dev/null || true
# (L)GPL redistribution: the license files of the FFmpeg build the player ships (Homebrew: LICENSE.md, COPYING.*).
PLAYER_FFMPEG_ROOT="${PLAYER_FFMPEG_LGPL:-$FFMPEG_DIR}"
mkdir -p "$CONTENTS/Resources/FFmpeg"
found_license=0
for f in "$PLAYER_FFMPEG_ROOT"/LICENSE* "$PLAYER_FFMPEG_ROOT"/COPYING*; do
    [ -f "$f" ] || continue
    cp -fL "$f" "$CONTENTS/Resources/FFmpeg/"
    found_license=1
done
if [ "$found_license" = 0 ]; then
    echo "No LICENSE/COPYING files in $PLAYER_FFMPEG_ROOT: the bundled FFmpeg must ship with its license text." >&2
    exit 1
fi

# The engine lives under Contents/Resources/subtitle-engine: codesign rejects any non-Mach-O file
# in places like Frameworks/MacOS/Helpers ("code object is not signed at all"),
# while the engine folder holds the binary plus loose dylibs (FFmpeg, sherpa-onnx, ONNX Runtime). The Mach-O files
# in Resources are signed one by one below (notarization wants all of them, wherever they are).
# cp -R: keeps any symlinks (versioned dylib names) as they are.
cp -R "$ENGINE_DIST" "$CONTENTS/Resources/subtitle-engine"
sed -e "s|__BUNDLE_ID__|$BUNDLE_ID|" -e "s|__VERSION__|$VERSION|" -e "s|__BUILD__|$BUILD_NUMBER|" \
    "$ROOT/scripts/macos/Info.plist.in" > "$CONTENTS/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleExecutable $EXE_NAME" "$CONTENTS/Info.plist"
printf 'APPL????' > "$CONTENTS/PkgInfo"
# Icon: drawn at 1024 px by sa-make-icon (Rust port of make-icon.swift), converted to .icns with iconutil.
ICONSET="$BUILD/AppIcon.iconset"
rm -rf "$ICONSET" && mkdir -p "$ICONSET"
"$MAKE_ICON" "$BUILD/AppIcon-1024.png" 1024
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
# Inside out: the app's dylibs, every Mach-O file inside the engine (dylib/.so/binary), the engine, then the app.
ENGINE_DIR="$CONTENTS/Resources/subtitle-engine"
ENGINE_BIN="$ENGINE_DIR/subtitle-engine"
if [ -d "$FRAMEWORKS" ]; then
    while IFS= read -r -d '' f; do
        case "$(file -b "$f")" in *Mach-O*) codesign "${SIGN_ARGS[@]}" "$f" ;; esac
    done < <(find "$FRAMEWORKS" -type f -print0)
fi
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
VERIFY_DIRS=("$ENGINE_DIR" "$CONTENTS/MacOS")
if [ -d "$FRAMEWORKS" ]; then VERIFY_DIRS+=("$FRAMEWORKS"); fi
while IFS= read -r -d '' f; do
    case "$(file -b "$f")" in
        *Mach-O*) codesign --verify --strict "$f" || { echo "Unsigned or invalid signature: $f" >&2; exit 1; } ;;
    esac
done < <(find "${VERIFY_DIRS[@]}" -type f -print0)
# The player's native stack loads from the bundle alone (no DYLD variables).
probe="$(env -u DYLD_LIBRARY_PATH -u DYLD_FALLBACK_LIBRARY_PATH "$CONTENTS/MacOS/$EXE_NAME" --probe)" \
    || { echo "The app could not load its runtime libraries (--probe failed)." >&2; exit 1; }
echo "$probe"
# A release ships an LGPL FFmpeg with the player (ARCHITECTURE.md §11); Homebrew's ffmpeg is a GPL build.
grep -q '^ffmpeg license: LGPL' <<<"$probe" \
    || echo "warning: the bundled FFmpeg is not an LGPL build (set FFMPEG_LGPL_DIR to an LGPL shared FFmpeg 8; FFMPEG_DIR stays the engine's GPL build)." >&2
echo
out="$(env -u DYLD_LIBRARY_PATH -u DYLD_FALLBACK_LIBRARY_PATH "$ENGINE_BIN" models --json)"
[[ "$out" == *'"event": "models"'* ]] || { echo "Bundled engine smoke test failed: $out" >&2; exit 1; }

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
