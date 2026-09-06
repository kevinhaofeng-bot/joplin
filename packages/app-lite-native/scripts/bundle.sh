#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
DIST_DIR="$PACKAGE_DIR/dist"
STAGING_DIR=""

cleanup() {
  if [[ -n "$STAGING_DIR" && -d "$STAGING_DIR" ]]; then
    rm -rf -- "$STAGING_DIR"
  fi
}
trap cleanup EXIT

cargo build --manifest-path "$PACKAGE_DIR/Cargo.toml" --release

mkdir -p "$DIST_DIR"
STAGING_DIR="$(mktemp -d "$DIST_DIR/.bundle-staging.XXXXXX")"
BUNDLE_PATH="$STAGING_DIR/Joplin Lite Native.app"
CONTENTS_PATH="$BUNDLE_PATH/Contents"
mkdir -p "$CONTENTS_PATH/MacOS" "$CONTENTS_PATH/Resources"
cp "$PACKAGE_DIR/target/release/joplin-lite-native" "$CONTENTS_PATH/MacOS/joplin-lite-native"
cp "$PACKAGE_DIR/Info.plist" "$CONTENTS_PATH/Info.plist"
chmod 755 "$CONTENTS_PATH/MacOS/joplin-lite-native"

codesign --force --deep --sign - "$BUNDLE_PATH"
codesign --verify --deep --strict --verbose=2 "$BUNDLE_PATH"

FINAL_PATH="$DIST_DIR/Joplin Lite Native.app"
if [[ -e "$FINAL_PATH" ]]; then
  rm -rf -- "$FINAL_PATH"
fi
mv -- "$BUNDLE_PATH" "$FINAL_PATH"
printf 'Created signed app: %s\n' "$FINAL_PATH"
