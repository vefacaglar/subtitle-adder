#!/usr/bin/env bash
# Packages Subtitle Adder.app: the SwiftUI app + the Rust engine folder (binary + dylibs).
#
#   scripts/package-macos.sh
#
# Environment variables:
#   SIGN_IDENTITY   Developer ID identity ("Developer ID Application: Name (TEAMID)").
#                   If unset, ad-hoc signing (`-`); only runs on this machine.
#   NOTARY_PROFILE  Profile name saved with `xcrun notarytool store-credentials`;
#                   if set (and SIGN_IDENTITY is set), notarizes and staples.
#   NOTARY_KEY_PATH / NOTARY_KEY_ID / NOTARY_ISSUER
#                   App Store Connect API Key credentials for `xcrun notarytool` (CI alternative to NOTARY_PROFILE).
#   NOTARY_ALLOW_FAILURE=1
#                   Warn and continue with an unnotarized package if notarization/stapling fails.
#   NOTARY_STATUS_FILE
#                   Optional file path to record signing/notarization state (adhoc | signed-unnotarized | signed-notarized).
#   VERSION         Short version (default: 0.1.0)   BUILD_NUMBER (default: 1)
#   BUNDLE_ID       default: app.subtitleadder.mac
#   MAKE_DMG=1      Also builds build/Subtitle Adder.dmg.
#   SKIP_ENGINE_BUILD=1  Reuses an already staged engine/dist (skips fetch/cargo/stage).
#
# Builds only for the architecture of the build machine (arm64 on Apple Silicon); not universal.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$ROOT/build"
APP="$BUILD/Subtitle Adder.app"
CONTENTS="$APP/Contents"
ENGINE_DIST="$ROOT/engine/dist"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"
VERSION="${VERSION:-0.1.0}"
BUILD_NUMBER="${BUILD_NUMBER:-1}"
BUNDLE_ID="${BUNDLE_ID:-app.subtitleadder.mac}"
ENTITLEMENTS="$ROOT/scripts/macos/engine.entitlements"

log() { printf '\n==> %s\n' "$*"; }

# notarytool can return 0 even on an "Invalid" result; this reads the status and prints Apple's log on failure.
notarize() {
    local target="$1"
    local err_file out id status
    err_file="$(mktemp)"
    if ! out="$(xcrun notarytool submit "$target" "${NOTARY_AUTH_ARGS[@]}" --wait --output-format json 2>"$err_file")"; then
        echo "Notarization submit failed for $target:" >&2
        cat "$err_file" >&2 || true
        echo "$out" >&2
        rm -f "$err_file"
        return 1
    fi
    rm -f "$err_file"
    id="$(plutil -extract id raw -o - - <<<"$out" 2>/dev/null || true)"
    status="$(plutil -extract status raw -o - - <<<"$out" 2>/dev/null || true)"
    if [ "$status" != "Accepted" ]; then
        echo "Notarization failed (${status:-unknown}) for $target." >&2
        if [ -n "$id" ]; then
            xcrun notarytool log "$id" "${NOTARY_AUTH_ARGS[@]}" >&2 || true
        fi
        return 1
    fi
    return 0
}

if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi

log "1/5 Building app (swift build -c release)"
(cd "$ROOT/apps/macos" && swift build -c release)
APP_BIN_DIR="$(cd "$ROOT/apps/macos" && swift build -c release --show-bin-path)"
APP_BIN="$APP_BIN_DIR/SubtitleAdderApp"

if [ "${SKIP_ENGINE_BUILD:-0}" != "1" ] || [ ! -x "$ENGINE_DIST/subtitle-engine" ]; then
    log "2/5 Building engine (cargo build --release -p se-cli + stage-runtime)"
    if ! command -v cargo >/dev/null 2>&1; then
        echo "cargo not found; install Rust (https://rustup.rs/)." >&2
        exit 1
    fi
    bash "$ROOT/engine/scripts/fetch-runtime.sh"
    (cd "$ROOT/engine" && cargo build --release -p se-cli)
    bash "$ROOT/engine/scripts/stage-runtime.sh"
else
    log "2/5 Engine build skipped"
fi

log "3/5 Assembling bundle"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$APP_BIN" "$CONTENTS/MacOS/SubtitleAdderApp"
# SwiftPM resource bundles (String Catalogs: SubtitleAdder_SubtitleAdderApp.bundle, SubtitleAdder_EngineKit.bundle).
# Bundle.module looks for them in Contents/Resources; without them the UI falls back to Turkish keys.
RESOURCE_BUNDLES=()
for name in SubtitleAdder_SubtitleAdderApp.bundle SubtitleAdder_EngineKit.bundle; do
    if [ ! -d "$APP_BIN_DIR/$name" ]; then
        echo "Missing SwiftPM resource bundle $APP_BIN_DIR/$name (localization would be missing)." >&2
        exit 1
    fi
    cp -R "$APP_BIN_DIR/$name" "$CONTENTS/Resources/"
    RESOURCE_BUNDLES+=("$CONTENTS/Resources/$name")
done
# The engine lives under Contents/Resources/subtitle-engine: codesign rejects any non-Mach-O file
# in places like Frameworks/MacOS/Helpers ("code object is not signed at all"),
# while the engine folder holds the binary plus loose dylibs (FFmpeg, sherpa-onnx, ONNX Runtime). The Mach-O files
# in Resources are signed one by one below (notarization wants all of them, wherever they are).
# cp -R: keeps any symlinks (versioned dylib names) as they are.
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
for b in "${RESOURCE_BUNDLES[@]}"; do codesign "${SIGN_ARGS[@]}" "$b"; done
codesign "${SIGN_ARGS[@]}" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
# --deep doesn't verify Mach-O files under Resources as code; verify each one separately.
while IFS= read -r -d '' f; do
    case "$(file -b "$f")" in
        *Mach-O*) codesign --verify --strict "$f" || { echo "Unsigned or invalid signature: $f" >&2; exit 1; } ;;
    esac
done < <(find "$ENGINE_DIR" -type f -print0)

NOTARY_AUTH_ARGS=()
if [ -n "${NOTARY_KEY_PATH:-}" ] || [ -n "${NOTARY_KEY_ID:-}" ] || [ -n "${NOTARY_ISSUER:-}" ]; then
    if [ -z "${NOTARY_KEY_PATH:-}" ] || [ -z "${NOTARY_KEY_ID:-}" ] || [ -z "${NOTARY_ISSUER:-}" ]; then
        echo "Notarization with API key requires NOTARY_KEY_PATH, NOTARY_KEY_ID, and NOTARY_ISSUER." >&2
        exit 1
    fi
    if [ ! -f "$NOTARY_KEY_PATH" ]; then
        echo "NOTARY_KEY_PATH not found: $NOTARY_KEY_PATH" >&2
        exit 1
    fi
    NOTARY_AUTH_ARGS=(--key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
elif [ -n "${NOTARY_PROFILE:-}" ]; then
    NOTARY_AUTH_ARGS=(--keychain-profile "$NOTARY_PROFILE")
fi

if [ "$SIGN_IDENTITY" = "-" ]; then
    SIGN_STATUS="adhoc"
else
    SIGN_STATUS="signed-unnotarized"
fi

if [ "${#NOTARY_AUTH_ARGS[@]}" -gt 0 ]; then
    if [ "$SIGN_IDENTITY" = "-" ]; then
        if [ "${NOTARY_ALLOW_FAILURE:-0}" = "1" ]; then
            echo "Warning: notarization skipped because SIGN_IDENTITY is ad-hoc (-)." >&2
        else
            echo "Notarization requires SIGN_IDENTITY (Developer ID)." >&2
            exit 1
        fi
    else
        log "Notarization"
        ZIP="$BUILD/notarize.zip"
        ditto -c -k --keepParent "$APP" "$ZIP"
        if notarize "$ZIP" && xcrun stapler staple "$APP"; then
            SIGN_STATUS="signed-notarized"
        else
            if [ "${NOTARY_ALLOW_FAILURE:-0}" = "1" ]; then
                echo "Warning: app notarization/stapling failed; continuing with signed unnotarized build." >&2
                SIGN_STATUS="signed-unnotarized"
            else
                rm -f "$ZIP"
                exit 1
            fi
        fi
        rm -f "$ZIP"
    fi
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
        if [ "${#NOTARY_AUTH_ARGS[@]}" -gt 0 ] && [ "$SIGN_STATUS" = "signed-notarized" ]; then
            if ! (notarize "$DMG" && xcrun stapler staple "$DMG"); then
                if [ "${NOTARY_ALLOW_FAILURE:-0}" = "1" ]; then
                    echo "Warning: DMG notarization/stapling failed; continuing with unnotarized DMG." >&2
                    SIGN_STATUS="signed-unnotarized"
                else
                    exit 1
                fi
            fi
        fi
    fi
fi

if [ -n "${NOTARY_STATUS_FILE:-}" ]; then
    mkdir -p "$(dirname "$NOTARY_STATUS_FILE")"
    printf '%s\n' "$SIGN_STATUS" > "$NOTARY_STATUS_FILE"
fi

log "Done: $APP ($SIGN_STATUS)"
du -sh "$APP" "$ENGINE_DIR"
