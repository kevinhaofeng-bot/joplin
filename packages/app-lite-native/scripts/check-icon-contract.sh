#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
SOURCE_PATH="$PACKAGE_DIR/assets/AppIcon-source.png"
SMALL_SOURCE_PATH="$PACKAGE_DIR/assets/AppIcon-small-source.png"

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

if [[ "${1:-}" == "--bundle" ]]; then
  BUNDLE_PATH="${2:?bundle path required}"
  ICON_PATH="$BUNDLE_PATH/Contents/Resources/AppIcon.icns"
  [[ -f "$ICON_PATH" ]] || { echo "missing bundled icon: $ICON_PATH" >&2; exit 1; }
  ICON_FILE="$(plutil -extract CFBundleIconFile raw -o - "$BUNDLE_PATH/Contents/Info.plist")"
  [[ "$ICON_FILE" == "AppIcon" ]] || {
    echo "CFBundleIconFile must be AppIcon (got $ICON_FILE)" >&2
    exit 1
  }
fi
