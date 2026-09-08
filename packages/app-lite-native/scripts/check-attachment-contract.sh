#!/usr/bin/env bash
set -euo pipefail

filter_otool_dependency_rows() {
  awk '/^[[:space:]]/{print}'
}

if [[ "${1:-}" == "--filter-otool-dependencies" ]]; then
  filter_otool_dependency_rows
  exit 0
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
EXPECTED_VERSION="0.7.3"
SOURCE_SCAN="$(mktemp "${TMPDIR:-/tmp}/joplin-lite-source.XXXXXX")"
MACHO_LIST=""
cleanup() {
  [[ -z "$MACHO_LIST" ]] || rm -f -- "$MACHO_LIST"
  rm -f -- "$SOURCE_SCAN"
}
trap cleanup EXIT
while IFS= read -r source; do
  # Keep the contract focused on production code. Test modules are appended
  # after the production implementation in each source file.
  awk '/^mod tests[[:space:]]*\{/{exit} {print}' \
    "$source" >>"$SOURCE_SCAN"
done < <(find "$PACKAGE_DIR/src" -type f -name '*.rs' -print | sort)

for production_marker in 'define_class!' 'ThumbnailRuntime' 'enum EditorAction'; do
  if ! rg -q --fixed-strings "$production_marker" "$SOURCE_SCAN"; then
    echo "production source scan is incomplete; missing marker: $production_marker" >&2
    exit 1
  fi
done

if rg -n 'RTFFromRange|RtfLoadDecision|RtfSavePlan|sanitized_rtf_from_editor|rtf_save_plan|rtf_load_decision|rtf_text_matches_body|editor_save_projection|editor_segments_with_ranges' "$SOURCE_SCAN"; then
  echo "obsolete normal-runtime RTF save/load path is present" >&2
  exit 1
fi
if rg -n -i 'tauri|electron|node\.js|wkwebview|webkit|javascriptcore|nsstackview|note_rows|note_buttons|list_stack|tag.?index|selectNote:' "$SOURCE_SCAN"; then
  echo "forbidden runtime, eager note-row, or legacy tag-index path is present" >&2
  exit 1
fi
if rg -n '"[^"\n]*(AI|分享|协作|字体族|任意颜色|font family|font-family|color picker)[^"\n]*"' "$SOURCE_SCAN"; then
  echo "unsupported toolbar action label is present" >&2
  exit 1
fi
for label in '插入图片' '撤销' '重做' '正文/标题' '粗体' '斜体' '下划线' '高亮' \
  '项目符号' '编号列表' '清单' '链接' '左对齐' '居中' '右对齐' \
  '增加缩进' '减少缩进' '删除线' '清除格式' '更多'; do
  if ! rg -q --fixed-strings "label: \"$label\"" "$SOURCE_SCAN" \
    && ! rg -q --fixed-strings "\"$label\"" "$SOURCE_SCAN"; then
    echo "toolbar catalogue is missing label: $label" >&2
    exit 1
  fi
done
if rg -n 'body_rtf[[:space:]]*=' "$SOURCE_SCAN" | rg -v "body_rtf[[:space:]]*=[[:space:]]*X''"; then
  echo "normal runtime must only write an empty legacy body_rtf column" >&2
  exit 1
fi
LEGACY_DECODER_COUNT="$(rg -o 'NSAttributedString::initWithRTF_documentAttributes' "$PACKAGE_DIR/src/app.rs" | wc -l | tr -d ' ')"
test "$LEGACY_DECODER_COUNT" = "1"
rg -q 'fn decode_legacy_rtf_for_html_migration' "$PACKAGE_DIR/src/app.rs"

APP_PATH="${1:?app path required}"
CONTENTS_PATH="$APP_PATH/Contents"
PLIST="$CONTENTS_PATH/Info.plist"

test -d "$APP_PATH"
test -f "$PLIST"
test -f "$CONTENTS_PATH/Resources/AppIcon.icns"
test -s "$CONTENTS_PATH/Resources/AppIcon.icns"
PACKAGE_VERSION="$(awk -F'"' '$1 ~ /^[[:space:]]*version[[:space:]]*=/ { print $2; exit }' "$PACKAGE_DIR/Cargo.toml")"
test "$PACKAGE_VERSION" = "$EXPECTED_VERSION"
test "$(plutil -extract CFBundleIconFile raw -o - "$PLIST")" = "AppIcon"
test "$(plutil -extract CFBundleShortVersionString raw -o - "$PLIST")" = "$EXPECTED_VERSION"
test "$(plutil -extract CFBundleVersion raw -o - "$PLIST")" = "$EXPECTED_VERSION"
BUNDLE_IDENTIFIER="$(plutil -extract CFBundleIdentifier raw -o - "$PLIST")"
test -n "$BUNDLE_IDENTIFIER"

EXECUTABLE_NAME="$(plutil -extract CFBundleExecutable raw -o - "$PLIST")"
test -n "$EXECUTABLE_NAME"
EXECUTABLE_PATH="$CONTENTS_PATH/MacOS/$EXECUTABLE_NAME"
test -f "$EXECUTABLE_PATH"
if [[ -L "$EXECUTABLE_PATH" ]]; then
  echo "bundle executable must not be a symlink" >&2
  exit 1
fi
if [[ -d "$CONTENTS_PATH/Helpers" ]]; then
  echo "child helper executables are forbidden" >&2
  exit 1
fi
if find "$CONTENTS_PATH" \( -type d -name '*.framework' -o -path "$CONTENTS_PATH/Frameworks" \) -print -quit | grep -q .; then
  echo "embedded frameworks are forbidden; use system frameworks only" >&2
  exit 1
fi
if find "$CONTENTS_PATH/MacOS" -type f ! -name "$EXECUTABLE_NAME" -print -quit | grep -q .; then
  echo "bundle must contain only its main executable" >&2
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
SIGNED_IDENTIFIER="$(codesign -dv --verbose=4 "$APP_PATH" 2>&1 | sed -n 's/^Identifier=//p' | head -n 1)"
test "$SIGNED_IDENTIFIER" = "$BUNDLE_IDENTIFIER"
