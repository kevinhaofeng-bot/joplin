#!/usr/bin/env bash
set -euo pipefail
APP_PATH="${1:?app path required}"
PLIST="$APP_PATH/Contents/Info.plist"
test -f "$APP_PATH/Contents/Resources/AppIcon.icns"
test -f "$PLIST"
test "$(plutil -extract CFBundleIconFile raw -o - "$PLIST")" = "AppIcon"

if find "$APP_PATH" \( \
  -iname 'Electron Framework.framework' -o \
  -iname 'WebKit.framework' -o \
  -iname 'JavaScriptCore.framework' -o \
  -type f -name node -o \
  -type f -name 'libnode*' \
\) -print -quit | grep -q .; then
  echo "WebKit, JavaScriptCore, Electron, and Node payloads are forbidden" >&2
  exit 1
fi

codesign --verify --deep --strict "$APP_PATH"
