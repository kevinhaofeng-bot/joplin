#!/usr/bin/env bash
set -euo pipefail

filter_otool_dependency_rows() {
  awk '/^[[:space:]]/{print}'
}

if [[ "${1:-}" == "--filter-otool-dependencies" ]]; then
  filter_otool_dependency_rows
  exit 0
fi

APP_PATH="${1:?app path required}"
CONTENTS_PATH="$APP_PATH/Contents"
PLIST="$CONTENTS_PATH/Info.plist"

test -d "$APP_PATH"
test -f "$PLIST"
test -f "$CONTENTS_PATH/Resources/AppIcon.icns"
test "$(plutil -extract CFBundleIconFile raw -o - "$PLIST")" = "AppIcon"

EXECUTABLE_NAME="$(plutil -extract CFBundleExecutable raw -o - "$PLIST")"
test -n "$EXECUTABLE_NAME"
EXECUTABLE_PATH="$CONTENTS_PATH/MacOS/$EXECUTABLE_NAME"
test -f "$EXECUTABLE_PATH"
if [[ -L "$EXECUTABLE_PATH" ]]; then
  echo "bundle executable must not be a symlink" >&2
  exit 1
fi
EXECUTABLE_KIND="$(file -b "$EXECUTABLE_PATH")"
if [[ "$EXECUTABLE_KIND" != *Mach-O* ]]; then
  echo "bundle executable is not Mach-O: $EXECUTABLE_PATH" >&2
  exit 1
fi

if find "$APP_PATH" -type l -print -quit | grep -q .; then
  echo "symlinks inside app bundle are forbidden" >&2
  exit 1
fi
if find "$APP_PATH" \( \
  -iname 'Electron Framework.framework' -o \
  -iname 'WebKit.framework' -o \
  -iname 'JavaScriptCore.framework' -o \
  -iname '*libnode*' -o \
  -iname 'node' -o \
  -iname 'WebKit' -o \
  -iname 'JavaScriptCore' \
\) -print -quit | grep -q .; then
  echo "WebKit, JavaScriptCore, Electron, and Node payloads are forbidden" >&2
  exit 1
fi

MACHO_LIST="$(mktemp "${TMPDIR:-/tmp}/joplin-lite-macho.XXXXXX")"
cleanup() { rm -f -- "$MACHO_LIST"; }
trap cleanup EXIT
find "$APP_PATH" -type f -print >"$MACHO_LIST"
while IFS= read -r candidate; do
  [[ -n "$candidate" ]] || continue
  kind="$(file -b "$candidate")"
  if [[ "$kind" != *Mach-O* ]]; then
    continue
  fi
  # otool prints one unindented path/architecture header per architecture and
  # indented install-name rows below it. Scan only those dependency rows so a
  # clean bundle under a directory named WebKit (or JavaScriptCore/libnode)
  # is not rejected by its absolute path, including for universal binaries.
  dependencies="$(otool -L "$candidate" | filter_otool_dependency_rows)"
  if grep -Eiq 'WebKit|JavaScriptCore|libnode' <<<"$dependencies"; then
    echo "forbidden runtime dependency in $candidate" >&2
    exit 1
  fi
done <"$MACHO_LIST"

codesign --verify --deep --strict "$APP_PATH"
