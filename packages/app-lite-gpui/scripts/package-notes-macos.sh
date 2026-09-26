#!/usr/bin/env bash
# Package the Joplin Lite notes app as a macOS .app into a NEW output
# directory. Never touches the donor Velotype dist/ flow.
# Usage: scripts/package-notes-macos.sh [output-parent]   (default: ./dist-notes)
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(git -C "$PROJECT_ROOT" rev-parse --show-toplevel)"
OUTPUT_PARENT="${1:-$PROJECT_ROOT/dist-notes}"
APP_NAME="Joplin Lite"
EXECUTABLE="joplin-lite"
BUNDLE_ID="com.arielkevin.joplinlite"

COMMIT="$(git -C "$REPO_ROOT" rev-parse HEAD)"
SHORT="$(git -C "$REPO_ROOT" rev-parse --short=9 HEAD)"
SOURCE_STATUS="$(git -C "$REPO_ROOT" status --porcelain -- "$PROJECT_ROOT" "$REPO_ROOT/packages/app-lite-core")"
DIRTY=no
if [[ -n "$SOURCE_STATUS" ]]; then DIRTY=yes; fi
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="$OUTPUT_PARENT/$STAMP-$SHORT"
if [[ -e "$OUT_DIR" ]]; then
  echo "refusing to overwrite existing output: $OUT_DIR" >&2
  exit 1
fi

echo "==> Build Release binary (locked)."
cargo build --manifest-path "$PROJECT_ROOT/Cargo.toml" --release --locked --bin velotype

# Read Cargo's real target directory (worktrees may share one) instead of
# assuming ./target, so a stale binary is never packaged.
TARGET_DIR="$(cargo metadata --manifest-path "$PROJECT_ROOT/Cargo.toml" --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
BINARY="$TARGET_DIR/release/velotype"
[[ -x "$BINARY" ]] || { echo "release binary missing: $BINARY" >&2; exit 1; }

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$PROJECT_ROOT/Cargo.toml" | head -1)"
BUILD_NUMBER="$(git -C "$REPO_ROOT" rev-list --count HEAD)"
APP_DIR="$OUT_DIR/$APP_NAME.app"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
cp "$BINARY" "$APP_DIR/Contents/MacOS/$EXECUTABLE"
cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>$APP_NAME</string>
<key>CFBundleDisplayName</key><string>$APP_NAME</string>
<key>CFBundleExecutable</key><string>$EXECUTABLE</string>
<key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$VERSION</string>
<key>CFBundleVersion</key><string>$BUILD_NUMBER</string>
<key>JoplinLiteBuildCommit</key><string>$COMMIT</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
# No CFBundleDocumentTypes: installing must not change default file associations.
# No icon: only the donor Velotype icon exists and a product icon has not been chosen.

{
  echo "app: $APP_NAME ($BUNDLE_ID) $VERSION ($BUILD_NUMBER)"
  echo "commit: $COMMIT"
  echo "worktree_dirty_for_app_sources: $DIRTY"
  echo "binary_sha256: $(shasum -a 256 "$APP_DIR/Contents/MacOS/$EXECUTABLE" | awk '{print $1}')"
  echo "cargo_lock_sha256: $(shasum -a 256 "$PROJECT_ROOT/Cargo.lock" | awk '{print $1}')"
  echo "rustc: $(rustc --version)"
  echo "built_at_utc: $STAMP"
} > "$OUT_DIR/BUILD-INFO.txt"
echo "==> Done: $APP_DIR"
cat "$OUT_DIR/BUILD-INFO.txt"
