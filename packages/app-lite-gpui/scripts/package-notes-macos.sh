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

# Evidence274: macOS medium-zone retention increases RSS after repeated PNG
# resizing even after Rust buffers are freed. Scope the verified policy to
# this new LaunchServices bundle, never launchctl or the user's environment.
# Keep an explicit build-time rollback for other OS versions/diagnostics.
MEMORY_POLICY="${JOPLIN_LITE_MACOS_MEMORY_POLICY:-medium-disabled}"
case "$MEMORY_POLICY" in
  medium-disabled|system-default) ;;
  *) echo 'JOPLIN_LITE_MACOS_MEMORY_POLICY must be medium-disabled or system-default' >&2; exit 1 ;;
esac

# Optional acceptance mode (evidence 86): with both variables set the bundle
# gets its own identifier and an Info.plist pin, so any relaunch opens that
# library and nothing else (src/main.rs resolve_startup_profile). Without
# them the package is the ordinary one.
ACCEPTANCE_ID="${JOPLIN_LITE_ACCEPTANCE_ID:-}"
ACCEPTANCE_PROFILE="${JOPLIN_LITE_ACCEPTANCE_PROFILE:-}"
if [[ -n "$ACCEPTANCE_ID" || -n "$ACCEPTANCE_PROFILE" ]]; then
  # ASCII reverse-DNS segments only (letters, digits, hyphen), at least one
  # beyond the formal identifier: nothing that could break the plist XML.
  ( LC_ALL=C; [[ "$ACCEPTANCE_ID" =~ ^com\.arielkevin\.joplinlite(\.[A-Za-z0-9-]+)+$ ]] ) \
    || { echo "JOPLIN_LITE_ACCEPTANCE_ID must be $BUNDLE_ID.<segment>[.<segment>…] (ASCII letters, digits, hyphens)" >&2; exit 1; }
  # Any absolute path; control characters are refused so that BUILD-INFO
  # stays one line per field.
  ( LC_ALL=C; [[ "$ACCEPTANCE_PROFILE" == /?* && ! "$ACCEPTANCE_PROFILE" =~ [[:cntrl:]] ]] ) \
    || { echo "JOPLIN_LITE_ACCEPTANCE_PROFILE must be an absolute path without control characters" >&2; exit 1; }
  BUNDLE_ID="$ACCEPTANCE_ID"
fi

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
bash "$PROJECT_ROOT/scripts/cargo-notes.sh" build --release --bin velotype

# Read Cargo's real target directory (worktrees may share one) instead of
# assuming ./target, so a stale binary is never packaged.
TARGET_DIR="$(cargo metadata --manifest-path "$PROJECT_ROOT/Cargo.toml" --no-deps --format-version 1 \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
[[ -n "$TARGET_DIR" ]] || { echo "cannot read cargo target directory" >&2; exit 1; }
BINARY="$TARGET_DIR/release/velotype"
[[ -x "$BINARY" ]] || { echo "release binary missing: $BINARY" >&2; exit 1; }

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$PROJECT_ROOT/Cargo.toml" | head -1)"
BUILD_NUMBER="$(git -C "$REPO_ROOT" rev-list --count HEAD)"
APP_DIR="$OUT_DIR/$APP_NAME.app"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
cp "$BINARY" "$APP_DIR/Contents/MacOS/$EXECUTABLE"
# The simplified non-green Dock mouse (evidence57); the older large artwork
# stays in the repository unchanged and is not packaged.
ICON_SOURCE="$PROJECT_ROOT/assets/AppIcon-dock-v2.png"
ICON_FILE="$APP_DIR/Contents/Resources/AppIcon.icns"
"$PROJECT_ROOT/scripts/make-app-icon.sh" "$ICON_SOURCE" "$ICON_FILE"
cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>$APP_NAME</string>
<key>CFBundleDisplayName</key><string>$APP_NAME</string>
<key>CFBundleExecutable</key><string>$EXECUTABLE</string>
<key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
<key>CFBundleIconFile</key><string>AppIcon</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$VERSION</string>
<key>CFBundleVersion</key><string>$BUILD_NUMBER</string>
<key>JoplinLiteBuildCommit</key><string>$COMMIT</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
if [[ -n "$ACCEPTANCE_PROFILE" ]]; then
  # The path is its own argument: no command-language parsing of its text.
  /usr/bin/plutil -insert JoplinLiteAcceptanceProfile -string "$ACCEPTANCE_PROFILE" \
    "$APP_DIR/Contents/Info.plist"
  [[ "$(/usr/bin/plutil -extract JoplinLiteAcceptanceProfile raw -o - "$APP_DIR/Contents/Info.plist")" \
    == "$ACCEPTANCE_PROFILE" ]] \
    || { echo "Info.plist acceptance profile does not read back as given" >&2; exit 1; }
fi
# No CFBundleDocumentTypes: installing must not change default file associations.
# Only this new bundle carries the icon: no icon cache reset, no install.
if [[ "$MEMORY_POLICY" == medium-disabled ]]; then
  /usr/bin/plutil -insert LSEnvironment -xml '<dict><key>MallocMediumZone</key><string>0</string></dict>' \
    "$APP_DIR/Contents/Info.plist"
  [[ "$(/usr/bin/plutil -extract LSEnvironment.MallocMediumZone raw -o - "$APP_DIR/Contents/Info.plist")" == 0 ]] \
    || { echo 'App allocator startup policy does not read back as given' >&2; exit 1; }
fi

# Sign the whole bundle (the linker only signed the bare executable, whose
# signature then does not match the bundle). With no identity configured the
# signature is ad-hoc: valid for local use, not notarized.
SIGN_IDENTITY="${JOPLIN_LITE_SIGN_IDENTITY:--}"
# Sign the independent picker first, then seal it into the parent bundle.
bash "$PROJECT_ROOT/scripts/package-picker-helper.sh" "$APP_DIR"
codesign --force --sign "$SIGN_IDENTITY" --identifier "$BUNDLE_ID" \
  --timestamp=none "$APP_DIR"
codesign --verify --deep --strict --verbose=2 "$APP_DIR"
[[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIconFile' "$APP_DIR/Contents/Info.plist")" == AppIcon ]] \
  || { echo "Info.plist does not name AppIcon" >&2; exit 1; }
if [ "$SIGN_IDENTITY" = "-" ]; then SIGNATURE="ad-hoc (not notarized)"; else SIGNATURE="$SIGN_IDENTITY"; fi

{
  echo "app: $APP_NAME ($BUNDLE_ID) $VERSION ($BUILD_NUMBER)"
  echo "commit: $COMMIT"
  echo "worktree_dirty_for_app_sources: $DIRTY"
  echo "binary_sha256: $(shasum -a 256 "$APP_DIR/Contents/MacOS/$EXECUTABLE" | awk '{print $1}')"
  echo "picker_bundle_identifier: $BUNDLE_ID.picker"
  echo "picker_binary_sha256: $(shasum -a 256 "$APP_DIR/Contents/Helpers/Joplin Lite Picker.app/Contents/MacOS/joplin-lite-picker" | awk '{print $1}')"
  echo "signature: $SIGNATURE"
  echo "macos_allocator_policy: $MEMORY_POLICY"
  echo "icon_source_sha256: $(shasum -a 256 "$ICON_SOURCE" | awk '{print $1}')"
  echo "icon_icns_sha256: $(shasum -a 256 "$ICON_FILE" | awk '{print $1}')"
  echo "cargo_lock_sha256: $(shasum -a 256 "$PROJECT_ROOT/Cargo.lock" | awk '{print $1}')"
  echo "rustc: $(rustc --version)"
  echo "built_at_utc: $STAMP"
  if [[ -n "$ACCEPTANCE_PROFILE" ]]; then
    echo "acceptance_profile: $ACCEPTANCE_PROFILE"
  fi
} > "$OUT_DIR/BUILD-INFO.txt"
echo "==> Done: $APP_DIR"
cat "$OUT_DIR/BUILD-INFO.txt"
