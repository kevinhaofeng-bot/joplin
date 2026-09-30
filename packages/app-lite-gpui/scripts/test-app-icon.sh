#!/usr/bin/env bash
# Checks the packaged icon: every iconset representation is present at its
# pixel size, its corners are transparent, and none of it is green (the
# user's Dock icon requirement, evidence57). A green or non-square source
# must be refused by the same checks.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
source_png="${1:-$script_dir/../assets/AppIcon-dock-v2.png}"
python=/Users/kevinhao/miniconda3/bin/python3
test_root=$(mktemp -d "${TMPDIR:-/tmp}/joplin-lite-icon-test.XXXXXX")
trap 'rm -rf "$test_root"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

check_iconset() {
  "$python" - "$1" <<'PY'
import pathlib
import sys
from PIL import Image

iconset = pathlib.Path(sys.argv[1])
expected = {}
for points in (16, 32, 128, 256, 512):
    expected[f"icon_{points}x{points}.png"] = points
    expected[f"icon_{points}x{points}@2x.png"] = points * 2
found = sorted(path.name for path in iconset.glob("*.png"))
assert found == sorted(expected), found
for name, pixels in sorted(expected.items(), key=lambda item: item[1]):
    image = Image.open(iconset / name).convert("RGBA")
    assert image.size == (pixels, pixels), (name, image.size)
    corners = [(0, 0), (pixels - 1, 0), (0, pixels - 1), (pixels - 1, pixels - 1)]
    alpha = [image.getpixel(corner)[3] for corner in corners]
    assert max(alpha) == 0, (name, "corners not transparent", alpha)
    visible = green = 0
    for red, g, blue, a in image.getdata():
        if a < 32:
            continue
        visible += 1
        if g > red + 24 and g > blue + 24:
            green += 1
    assert visible > pixels * pixels // 4, (name, "icon nearly empty", visible)
    assert green <= visible // 1000, (name, "green pixels", green, visible)
    print(f"ok {name} {pixels}px visible={visible} green={green} corner_alpha={alpha}")
PY
}

"$script_dir/make-app-icon.sh" "$source_png" "$test_root/AppIcon.icns"
iconutil --convert iconset --output "$test_root/roundtrip.iconset" "$test_root/AppIcon.icns"
check_iconset "$test_root/roundtrip.iconset"
echo "icns_sha256: $(shasum -a 256 "$test_root/AppIcon.icns" | awk '{print $1}')"

# Negative controls: a green icon fails the colour check, a non-square
# source is refused before any icns is written.
"$python" - "$test_root/green.png" "$test_root/wide.png" <<'PY'
import sys
from PIL import Image, ImageDraw

green = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
ImageDraw.Draw(green).rounded_rectangle((80, 80, 944, 944), 180, fill=(0, 168, 45, 255))
green.save(sys.argv[1])
Image.new("RGBA", (1200, 1024), (60, 60, 60, 255)).save(sys.argv[2])
PY
"$script_dir/make-app-icon.sh" "$test_root/green.png" "$test_root/green.icns"
iconutil --convert iconset --output "$test_root/green.iconset" "$test_root/green.icns"
if check_iconset "$test_root/green.iconset" >/dev/null 2>&1; then
  fail "a green icon passed the colour check"
fi
if "$script_dir/make-app-icon.sh" "$test_root/wide.png" "$test_root/wide.icns" 2>/dev/null; then
  fail "a non-square source was accepted"
fi
[[ ! -e "$test_root/wide.icns" ]] || fail "a refused source still wrote an icns"
echo "PASS: app icon"
