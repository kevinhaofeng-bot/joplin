#!/usr/bin/env bash
# Real plist/filesystem/codesign boundary, no Cargo build and no GUI launch.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIXTURE="$(mktemp -d "${TMPDIR:-/tmp}/joplin-picker-package.XXXXXX")"
trap 'rm -rf "$FIXTURE"' EXIT
APP="$FIXTURE/中文 有空格/Joplin Lite.app"
mkdir -p "$APP/Contents/MacOS"
cp /usr/bin/true "$APP/Contents/MacOS/joplin-lite"
PLIST="$APP/Contents/Info.plist"
plutil -create xml1 "$PLIST"
plutil -insert CFBundleIdentifier -string com.arielkevin.joplinlite.acceptance.packagetest "$PLIST"
plutil -insert CFBundleShortVersionString -string 0.7.2 "$PLIST"
plutil -insert CFBundleVersion -string 16409 "$PLIST"
plutil -insert JoplinLiteAcceptanceProfile -string "$FIXTURE/must-not-open" "$PLIST"
bash "$SCRIPT_DIR/package-picker-helper.sh" "$APP"
HELPER="$APP/Contents/Helpers/Joplin Lite Picker.app"
[[ -x "$HELPER/Contents/MacOS/joplin-lite-picker" ]] || { echo 'FAIL: packaged helper executable absent' >&2; exit 1; }
HELPER_PLIST="$HELPER/Contents/Info.plist"
[[ "$(plutil -extract CFBundleIdentifier raw "$HELPER_PLIST")" == com.arielkevin.joplinlite.acceptance.packagetest.picker ]]
[[ "$(plutil -extract CFBundleExecutable raw "$HELPER_PLIST")" == joplin-lite-picker ]]
[[ "$(plutil -extract LSUIElement raw "$HELPER_PLIST")" == true ]]
if plutil -extract JoplinLiteAcceptanceProfile raw "$HELPER_PLIST" >/dev/null 2>&1; then
  echo 'FAIL: helper inherited notes profile metadata' >&2; exit 1
fi
codesign --verify --deep --strict "$HELPER"
if bash "$SCRIPT_DIR/package-picker-helper.sh" "$APP" >/dev/null 2>&1; then
  echo 'FAIL: existing helper was overwritten' >&2; exit 1
fi
MISSING="$FIXTURE/Missing.app"
mkdir -p "$MISSING/Contents"
cp "$PLIST" "$MISSING/Contents/Info.plist"
if bash "$SCRIPT_DIR/package-picker-helper.sh" "$MISSING" >/dev/null 2>&1; then
  echo 'FAIL: missing parent executable accepted' >&2; exit 1
fi
[[ ! -e "$MISSING/Contents/Helpers" ]]
echo 'PASS: helper executable, distinct identity, no Dock/profile pin, signature, no overwrite, missing-parent rejection'
