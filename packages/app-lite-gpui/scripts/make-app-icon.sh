#!/usr/bin/env bash
# Build the Joplin Lite .icns from one square PNG: every representation the
# macOS iconset format defines (16-512 pt at 1x and 2x), each downscaled from
# the same simplified non-green Dock artwork, so Finder, Dock, Launchpad and
# Retina displays all show it whichever size they pick.
# Usage: scripts/make-app-icon.sh <source.png> <output.icns>
set -euo pipefail

SOURCE="${1:?source PNG}"
OUTPUT="${2:?output .icns}"
[[ -f "$SOURCE" ]] || { echo "icon source missing: $SOURCE" >&2; exit 1; }
[[ ! -e "$OUTPUT" ]] || { echo "refusing to overwrite: $OUTPUT" >&2; exit 1; }

read -r WIDTH HEIGHT < <(sips -g pixelWidth -g pixelHeight "$SOURCE" \
  | awk '/pixelWidth/{w=$2} /pixelHeight/{h=$2} END{print w, h}')
if [[ "$WIDTH" != "$HEIGHT" || "$WIDTH" -lt 1024 ]]; then
  echo "icon source must be square and at least 1024px: ${WIDTH}x${HEIGHT}" >&2
  exit 1
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/joplin-lite-icon.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
ICONSET="$WORK/AppIcon.iconset"
mkdir "$ICONSET"
for POINTS in 16 32 128 256 512; do
  sips -s format png -z "$POINTS" "$POINTS" "$SOURCE" \
    --out "$ICONSET/icon_${POINTS}x${POINTS}.png" >/dev/null
  PIXELS=$((POINTS * 2))
  sips -s format png -z "$PIXELS" "$PIXELS" "$SOURCE" \
    --out "$ICONSET/icon_${POINTS}x${POINTS}@2x.png" >/dev/null
done
iconutil --convert icns --output "$OUTPUT" "$ICONSET"
