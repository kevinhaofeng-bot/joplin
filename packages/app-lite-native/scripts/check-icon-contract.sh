#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
SOURCE_PATH="$PACKAGE_DIR/assets/AppIcon-source.png"
SMALL_SOURCE_PATH="$PACKAGE_DIR/assets/AppIcon-small-source.png"
TEMP_DIRS=()
ALPHA_TOOL=""
NEW_TEMP_DIR=""

cleanup() {
  for temp_dir in "${TEMP_DIRS[@]-}"; do
    [[ -z "$temp_dir" ]] || rm -rf -- "$temp_dir"
  done
}
trap cleanup EXIT

new_temp_dir() {
  NEW_TEMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/joplin-icon-contract.XXXXXX")"
  TEMP_DIRS+=("$NEW_TEMP_DIR")
}

ensure_alpha_tool() {
  if [[ -z "$ALPHA_TOOL" ]]; then
    local tool_dir
    new_temp_dir
    tool_dir="$NEW_TEMP_DIR"
    ALPHA_TOOL="$tool_dir/check-icon-alpha"
    swiftc "$SCRIPT_DIR/check-icon-alpha.swift" -o "$ALPHA_TOOL" \
      -framework CoreGraphics -framework ImageIO
  fi
}

check_iconset_alpha() {
  local iconset_path="$1"
  local filename
  ensure_alpha_tool
  for filename in \
    icon_16x16.png icon_16x16@2x.png \
    icon_32x32.png icon_32x32@2x.png \
    icon_128x128.png icon_128x128@2x.png \
    icon_256x256.png icon_256x256@2x.png \
    icon_512x512.png icon_512x512@2x.png; do
    if [[ ! -f "$iconset_path/$filename" ]]; then
      echo "missing iconset representation: $iconset_path/$filename" >&2
      exit 1
    fi
    "$ALPHA_TOOL" "$iconset_path/$filename"
  done
}

for source_path in "$SOURCE_PATH" "$SMALL_SOURCE_PATH"; do
  if [[ ! -f "$source_path" ]]; then
    echo "missing icon source: $source_path" >&2
    exit 1
  fi

  read -r WIDTH HEIGHT < <(
    sips -g pixelWidth -g pixelHeight "$source_path" 2>/dev/null \
      | awk '/pixelWidth:/ { width=$2 } /pixelHeight:/ { height=$2 } END { print width, height }'
  )
  if [[ "${WIDTH:-}" -lt 512 || "${HEIGHT:-}" -lt 512 ]]; then
    echo "icon source must be at least 512x512 (got ${WIDTH:-unknown}x${HEIGHT:-unknown}: $source_path)" >&2
    exit 1
  fi
  if [[ "$WIDTH" != "$HEIGHT" ]]; then
    echo "icon source must be square (got ${WIDTH}x${HEIGHT}: $source_path)" >&2
    exit 1
  fi
done

if [[ "${1:-}" == "--iconset" ]]; then
  check_iconset_alpha "${2:?iconset path required}"
  exit 0
fi

if [[ "${1:-}" == "--bundle" ]]; then
  BUNDLE_PATH="${2:?bundle path required}"
  ICON_PATH="$BUNDLE_PATH/Contents/Resources/AppIcon.icns"
  [[ -f "$ICON_PATH" ]] || { echo "missing bundled icon: $ICON_PATH" >&2; exit 1; }
  new_temp_dir
  EXTRACT_PARENT="$NEW_TEMP_DIR"
  EXTRACTED_ICONSET="$EXTRACT_PARENT/AppIcon.iconset"
  iconutil --convert iconset --output "$EXTRACTED_ICONSET" "$ICON_PATH"
  check_iconset_alpha "$EXTRACTED_ICONSET"
  ICON_FILE="$(plutil -extract CFBundleIconFile raw -o - "$BUNDLE_PATH/Contents/Info.plist")"
  [[ "$ICON_FILE" == "AppIcon" ]] || {
    echo "CFBundleIconFile must be AppIcon (got $ICON_FILE)" >&2
    exit 1
  }
fi
