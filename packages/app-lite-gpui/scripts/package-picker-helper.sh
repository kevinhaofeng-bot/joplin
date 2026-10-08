#!/usr/bin/env bash
# Embed a short-lived native picker with an identity distinct from its parent.
set -euo pipefail
APP="${1:?usage: package-picker-helper.sh /absolute/path/Parent.app}"
[[ "$APP" == /*.app && -f "$APP/Contents/Info.plist" && -x "$APP/Contents/MacOS/joplin-lite" ]] \
  || { echo 'picker helper requires an existing absolute notes app bundle' >&2; exit 1; }
PARENT_PLIST="$APP/Contents/Info.plist"
PARENT_ID="$(/usr/bin/plutil -extract CFBundleIdentifier raw "$PARENT_PLIST")"
( LC_ALL=C; [[ "$PARENT_ID" =~ ^[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+$ ]] ) \
  || { echo 'invalid parent bundle identifier' >&2; exit 1; }
VERSION="$(/usr/bin/plutil -extract CFBundleShortVersionString raw "$PARENT_PLIST")"
BUILD_NUMBER="$(/usr/bin/plutil -extract CFBundleVersion raw "$PARENT_PLIST")"
HELPER="$APP/Contents/Helpers/Joplin Lite Picker.app"
[[ ! -e "$HELPER" && ! -L "$HELPER" ]] \
  || { echo 'refusing to overwrite existing picker helper' >&2; exit 1; }
mkdir -p "$HELPER/Contents/MacOS"
# A private copy: signing a hard link would corrupt the parent's signature.
cp "$APP/Contents/MacOS/joplin-lite" "$HELPER/Contents/MacOS/joplin-lite-picker"
PLIST="$HELPER/Contents/Info.plist"
/usr/bin/plutil -create xml1 "$PLIST"
/usr/bin/plutil -insert CFBundleName -string 'Joplin Lite Picker' "$PLIST"
/usr/bin/plutil -insert CFBundleDisplayName -string 'Joplin Lite Picker' "$PLIST"
/usr/bin/plutil -insert CFBundleExecutable -string joplin-lite-picker "$PLIST"
/usr/bin/plutil -insert CFBundleIdentifier -string "$PARENT_ID.picker" "$PLIST"
/usr/bin/plutil -insert CFBundlePackageType -string APPL "$PLIST"
/usr/bin/plutil -insert CFBundleShortVersionString -string "$VERSION" "$PLIST"
/usr/bin/plutil -insert CFBundleVersion -string "$BUILD_NUMBER" "$PLIST"
/usr/bin/plutil -insert LSMinimumSystemVersion -string 13.0 "$PLIST"
/usr/bin/plutil -insert LSUIElement -bool true "$PLIST"
/usr/bin/plutil -insert NSHighResolutionCapable -bool true "$PLIST"
# No profile pin, document types, or icon: this helper only displays one panel.
codesign --force --sign "${JOPLIN_LITE_SIGN_IDENTITY:--}" --identifier "$PARENT_ID.picker" \
  --timestamp=none "$HELPER"
codesign --verify --deep --strict "$HELPER"
echo "$HELPER"
