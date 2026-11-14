#!/usr/bin/env bash
# Bumps the project version across VERSION, engine/Cargo.toml, apps/desktop/Cargo.toml, and both Cargo.lock files.
#
#   scripts/bump-version.sh [patch | minor | major | X.Y.Z] [--dry-run]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION_FILE="$ROOT/VERSION"
ENGINE_TOML="$ROOT/engine/Cargo.toml"
DESKTOP_TOML="$ROOT/apps/desktop/Cargo.toml"

BUMP="patch"
DRY_RUN=0

for arg in "$@"; do
    case "$arg" in
        --dry-run|-n) DRY_RUN=1 ;;
        patch|minor|major) BUMP="$arg" ;;
        *)
            if [[ "$arg" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
                BUMP="$arg"
            else
                echo "Usage: scripts/bump-version.sh [patch|minor|major|X.Y.Z] [--dry-run]" >&2
                exit 1
            fi
            ;;
    esac
done

if [ ! -f "$VERSION_FILE" ]; then
    echo "VERSION file not found at $VERSION_FILE" >&2
    exit 1
fi

CURRENT="$(tr -d '[:space:]' < "$VERSION_FILE")"
if [[ ! "$CURRENT" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    echo "Invalid semver in VERSION: '$CURRENT' (expected X.Y.Z)" >&2
    exit 1
fi
MAJOR="${BASH_REMATCH[1]}"
MINOR="${BASH_REMATCH[2]}"
PATCH="${BASH_REMATCH[3]}"

case "$BUMP" in
    patch) NEXT="$MAJOR.$MINOR.$((PATCH + 1))" ;;
    minor) NEXT="$MAJOR.$((MINOR + 1)).0" ;;
    major) NEXT="$((MAJOR + 1)).0.0" ;;
    *) NEXT="$BUMP" ;;
esac

if [ "$DRY_RUN" = "1" ]; then
    echo "[dry-run] Version: $CURRENT -> $NEXT"
    echo "[dry-run] Would update: VERSION, engine/Cargo.toml, apps/desktop/Cargo.toml, engine/Cargo.lock, apps/desktop/Cargo.lock"
    exit 0
fi

printf '%s\n' "$NEXT" > "$VERSION_FILE"

update_workspace_version() {
    local toml_path="$1"
    local new_ver="$2"
    local tmp_file
    tmp_file="$(mktemp)"
    awk -v ver="$new_ver" '
        BEGIN { in_ws = 0; replaced = 0 }
        /^\[workspace\.package\]/ { in_ws = 1; print; next }
        /^\[/ { in_ws = 0; print; next }
        in_ws && !replaced && /^version[[:space:]]*=/ {
            sub(/"[^"]*"/, "\"" ver "\"")
            replaced = 1
        }
        { print }
        END { if (!replaced) exit 2 }
    ' "$toml_path" > "$tmp_file"
    mv "$tmp_file" "$toml_path"
}

update_workspace_version "$ENGINE_TOML" "$NEXT"
update_workspace_version "$DESKTOP_TOML" "$NEXT"

if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi

if command -v cargo >/dev/null 2>&1; then
    (cd "$ROOT/engine" && cargo update --workspace --offline >/dev/null)
    (cd "$ROOT/apps/desktop" && cargo update --workspace --offline >/dev/null)
else
    echo "Warning: cargo not found; skipped Cargo.lock workspace sync." >&2
fi

echo "Bumped version: $CURRENT -> $NEXT"
echo "Updated: VERSION, engine/Cargo.toml, apps/desktop/Cargo.toml, engine/Cargo.lock, apps/desktop/Cargo.lock"
