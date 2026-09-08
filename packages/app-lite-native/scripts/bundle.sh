#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
DIST_DIR="$PACKAGE_DIR/dist"
STAGING_DIR=""
ICON_MAIN_SOURCE="$PACKAGE_DIR/assets/AppIcon-source.png"
ICON_SMALL_SOURCE="$PACKAGE_DIR/assets/AppIcon-small-source.png"

bash "$SCRIPT_DIR/check-icon-contract.sh"

cleanup() {
  if [[ -n "$STAGING_DIR" && -d "$STAGING_DIR" ]]; then
    rm -rf -- "$STAGING_DIR"
  fi
}
trap cleanup EXIT

cargo build --manifest-path "$PACKAGE_DIR/Cargo.toml" --release

mkdir -p "$DIST_DIR"
STAGING_DIR="$(mktemp -d "$DIST_DIR/.bundle-staging.XXXXXX")"
BUNDLE_PATH="$STAGING_DIR/Joplin Lite Native.app"
CONTENTS_PATH="$BUNDLE_PATH/Contents"
mkdir -p "$CONTENTS_PATH/MacOS" "$CONTENTS_PATH/Resources"
cp "$PACKAGE_DIR/target/release/joplin-lite-native" "$CONTENTS_PATH/MacOS/joplin-lite-native"
cp "$PACKAGE_DIR/Info.plist" "$CONTENTS_PATH/Info.plist"
chmod 755 "$CONTENTS_PATH/MacOS/joplin-lite-native"

ICONSET_PATH="$STAGING_DIR/AppIcon.iconset"
mkdir -p "$ICONSET_PATH"
MASK_TOOL="$STAGING_DIR/mask-icon"
swiftc "$SCRIPT_DIR/mask-icon.swift" -o "$MASK_TOOL" \
  -framework CoreGraphics -framework ImageIO
for spec in \
  "icon_16x16.png:16:small" "icon_16x16@2x.png:32:small" \
  "icon_32x32.png:32:small" "icon_32x32@2x.png:64:small" \
  "icon_128x128.png:128:small" "icon_128x128@2x.png:256:main" \
  "icon_256x256.png:256:main" "icon_256x256@2x.png:512:main" \
  "icon_512x512.png:512:main" "icon_512x512@2x.png:1024:main"; do
  IFS=: read -r filename size source_kind <<< "$spec"
  if [[ "$source_kind" == small ]]; then
    source="$ICON_SMALL_SOURCE"
  else
    source="$ICON_MAIN_SOURCE"
  fi
  "$MASK_TOOL" "$source" "$ICONSET_PATH/$filename" "$size"
done
"$SCRIPT_DIR/check-icon-contract.sh" --iconset "$ICONSET_PATH"
iconutil --convert icns --output "$CONTENTS_PATH/Resources/AppIcon.icns" "$ICONSET_PATH"

codesign --force --deep --sign - "$BUNDLE_PATH"
codesign --verify --deep --strict --verbose=2 "$BUNDLE_PATH"
bash "$SCRIPT_DIR/check-icon-contract.sh" --bundle "$BUNDLE_PATH"

FINAL_PATH="$DIST_DIR/Joplin Lite Native.app"
if [[ -e "$FINAL_PATH" ]]; then
  rm -rf -- "$FINAL_PATH"
fi
mv -- "$BUNDLE_PATH" "$FINAL_PATH"
bash "$SCRIPT_DIR/check-attachment-contract.sh" "$FINAL_PATH"
printf 'Created signed app: %s\n' "$FINAL_PATH"
