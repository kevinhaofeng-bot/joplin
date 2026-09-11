#!/usr/bin/env bash
# Create a macOS .app for Velotype
# Usage: ./scripts/create_app_dist.sh
set -euo pipefail

BINARY_NAME="velotype"
APP_NAME="Velotype"

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$PROJECT_ROOT/dist"
MACOS_RESOURCES_DIR="$PROJECT_ROOT/resources/macos"

echo "==> Clean old dists."
rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

echo "==> Build Release binary."
cargo build --manifest-path "$PROJECT_ROOT/Cargo.toml" --release

# Worktrees share Cargo's target directory through the repository-level
# `.cargo/config.toml`. Do not assume `$PROJECT_ROOT/target`: when an old local
# target directory happens to exist, copying it would silently package a stale
# release binary instead of the one Cargo just built.
TARGET_DIR="$(cargo metadata --manifest-path "$PROJECT_ROOT/Cargo.toml" --no-deps --format-version 1 | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
RELEASE_BINARY="$TARGET_DIR/release/$BINARY_NAME"
if [[ -z "$TARGET_DIR" || ! -x "$RELEASE_BINARY" ]]; then
  echo "Unable to locate the fresh Cargo release binary: $RELEASE_BINARY" >&2
  exit 1
fi

echo "==> Create App Bundle struct."
APP_DIR="$DIST_DIR/$APP_NAME.app"
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"
cp "$RELEASE_BINARY" "$APP_DIR/Contents/MacOS/"
cp "$MACOS_RESOURCES_DIR/Info.plist" "$APP_DIR/Contents/"
cp "$MACOS_RESOURCES_DIR/$BINARY_NAME.icns" "$APP_DIR/Contents/Resources/$BINARY_NAME.icns"

echo "==> Copy resources files"
[ -f "$PROJECT_ROOT/README.md" ] && cp "$PROJECT_ROOT/README.md" "$APP_DIR/Contents/Resources/"

echo "==> ✅ Done"
echo "    Output: $APP_DIR"
echo "    Binary: $RELEASE_BINARY"
echo "       Use: open '$APP_DIR'"
