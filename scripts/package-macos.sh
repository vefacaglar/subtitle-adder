#!/usr/bin/env bash
# Subtitle Adder.app paketler: SwiftUI uygulaması + PyInstaller (onedir) motoru.
#
#   scripts/package-macos.sh
#
# Ortam değişkenleri:
#   SIGN_IDENTITY   Developer ID kimliği ("Developer ID Application: Ad (TEAMID)").
#                   Yoksa ad-hoc imza (`-`), yalnız bu makinede çalışır.
#   NOTARY_PROFILE  `xcrun notarytool store-credentials` ile kaydedilmiş profil adı;
#                   verilirse (ve SIGN_IDENTITY varsa) notarization + staple yapılır.
#   VERSION         Kısa sürüm (varsayılan: 0.1.0)   BUILD_NUMBER (varsayılan: 1)
#   BUNDLE_ID       varsayılan: app.subtitleadder.mac
#   MAKE_DMG=1      build/Subtitle Adder.dmg de üretir.
#   SKIP_ENGINE_BUILD=1  build/engine-dist zaten hazırsa PyInstaller'ı atlar.
#
# Yalnız derleyen makinenin mimarisini üretir (Apple Silicon'da arm64); universal değil.
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

# notarytool "Invalid" sonucunda da 0 dönebilir; durumu okuyup hatada Apple'ın günlüğünü basar.
notarize() {
    local out id status
    out="$(xcrun notarytool submit "$1" --keychain-profile "$NOTARY_PROFILE" --wait --output-format json)"
    id="$(plutil -extract id raw -o - - <<<"$out")"
    status="$(plutil -extract status raw -o - - <<<"$out")"
    if [ "$status" != "Accepted" ]; then
        echo "Notarization başarısız ($status)." >&2
        xcrun notarytool log "$id" --keychain-profile "$NOTARY_PROFILE" >&2 || true
        exit 1
    fi
}

if ! command -v uv >/dev/null 2>&1; then
    for d in "$HOME/.local/bin" /opt/homebrew/bin /usr/local/bin "$HOME/.cargo/bin"; do
        if [ -x "$d/uv" ]; then PATH="$d:$PATH"; break; fi
    done
fi
# python.org'un framework yapılı Python'ı onedir'e iç içe .framework koyar; uv'nin kendi Python'ı koymaz.
export UV_PYTHON_PREFERENCE=only-managed

log "1/5 Uygulama derleniyor (swift build -c release)"
(cd "$ROOT/apps/macos" && swift build -c release)
APP_BIN="$(cd "$ROOT/apps/macos" && swift build -c release --show-bin-path)/SubtitleAdderApp"

if [ "${SKIP_ENGINE_BUILD:-0}" != "1" ] || [ ! -x "$ENGINE_DIST/subtitle-engine" ]; then
    log "2/5 Motor derleniyor (PyInstaller onedir)"
    if ! command -v uv >/dev/null 2>&1; then
        echo "uv bulunamadı; PATH'e ekleyin (https://docs.astral.sh/uv/)." >&2
        exit 1
    fi
    (cd "$ROOT/engine" && uv run --group build pyinstaller subtitle-engine.spec --noconfirm \
        --distpath "$BUILD/engine-dist" --workpath "$BUILD/engine-work")
else
    log "2/5 Motor derlemesi atlandı"
fi

log "3/5 Bundle kuruluyor"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$APP_BIN" "$CONTENTS/MacOS/SubtitleAdderApp"
# Motor Contents/Resources/subtitle-engine altında durur: codesign, Frameworks/MacOS/Helpers
# gibi konumlarda Mach-O olmayan her dosyayı ("code object is not signed at all") reddeder;
# PyInstaller klasörü ise .py/.json/.onnx gibi çok sayıda veri dosyası içerir. Resources'taki
# Mach-O dosyalar aşağıda tek tek imzalanır (notarization konumdan bağımsız hepsini ister).
# cp -R: PyInstaller'ın sembolik bağlarını olduğu gibi korur.
cp -R "$ENGINE_DIST" "$CONTENTS/Resources/subtitle-engine"
sed -e "s|__BUNDLE_ID__|$BUNDLE_ID|" -e "s|__VERSION__|$VERSION|" -e "s|__BUILD__|$BUILD_NUMBER|" \
    "$ROOT/scripts/macos/Info.plist.in" > "$CONTENTS/Info.plist"
printf 'APPL????' > "$CONTENTS/PkgInfo"
plutil -lint "$CONTENTS/Info.plist" >/dev/null

if [ "$SIGN_IDENTITY" = "-" ]; then SIGN_LABEL="ad-hoc"; else SIGN_LABEL="$SIGN_IDENTITY"; fi
log "4/5 İmzalanıyor ($SIGN_LABEL)"
SIGN_ARGS=(--force --sign "$SIGN_IDENTITY")
if [ "$SIGN_IDENTITY" != "-" ]; then
    SIGN_ARGS+=(--options runtime --timestamp)
fi
# İçten dışa: önce motor içindeki tüm Mach-O dosyaları (dylib/.so/ikili), sonra motor, sonra uygulama.
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
# --deep, Resources altındaki Mach-O'ları kod olarak doğrulamaz; her birini ayrıca doğrula.
while IFS= read -r -d '' f; do
    case "$(file -b "$f")" in
        *Mach-O*) codesign --verify --strict "$f" || { echo "İmzasız ya da bozuk imza: $f" >&2; exit 1; } ;;
    esac
done < <(find "$ENGINE_DIR" -type f -print0)

if [ -n "${NOTARY_PROFILE:-}" ]; then
    if [ "$SIGN_IDENTITY" = "-" ]; then
        echo "NOTARY_PROFILE için SIGN_IDENTITY (Developer ID) gerekir." >&2
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
        echo "Uyarı: ad-hoc imzalı dmg başka bir Mac'te Gatekeeper tarafından engellenir." >&2
    else
        codesign --force --sign "$SIGN_IDENTITY" --timestamp "$DMG"
        if [ -n "${NOTARY_PROFILE:-}" ]; then
            notarize "$DMG"
            xcrun stapler staple "$DMG"
        fi
    fi
fi

log "Hazır: $APP"
du -sh "$APP" "$ENGINE_DIR"
