#!/usr/bin/env bash
# Exercise the real wrapper without invoking a compiler or deleting real caches.
set -euo pipefail
if [[ "$(basename "$0")" == cargo ]]; then
  printf '%s\n' "$*" >> "$TEST_ROOT/calls.log"
  if [[ "$1" == metadata ]]; then
    printf '{"target_directory":"%s/target"}\n' "$TEST_ROOT"
  fi
  exit 0
fi
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_ROOT="$(mktemp -d /tmp/joplin-build-cleanup.XXXXXX)"
export TEST_ROOT
trap 'rm -rf "$TEST_ROOT"' EXIT
mkdir "$TEST_ROOT/target"
ln -s "$SCRIPT_DIR/test-cargo-notes.sh" "$TEST_ROOT/cargo"
export PATH="$TEST_ROOT:$PATH"
pgrep() { return 1; }
export -f pgrep
bash "$SCRIPT_DIR/cargo-notes.sh" check --help
awk '
  /^clean / && /--profile dev/ { dev = NR }
  /^clean / && /--release/ { release = NR }
  /^check / { command = NR }
  END {
    if (!(dev && release && command && dev < command && release < command)) {
      print "FAIL: both dev and release project cleanup must precede Cargo command" > "/dev/stderr"
      exit 1
    }
  }
' "$TEST_ROOT/calls.log"
pgrep() { return 0; }
export -f pgrep
if bash "$SCRIPT_DIR/cargo-notes.sh" check --help; then
  echo "FAIL: wrapper must refuse cleanup while another compiler is active" >&2
  exit 1
fi
[[ "$(wc -l < "$TEST_ROOT/calls.log")" -eq 4 ]]
echo "PASS: dev/release cleanup order and active-build guard (no compilation)"

# Run the legacy packaging entry point in a disposable project. Its filesystem
# operations stay real, while Cargo is replaced at the external compiler boundary.
pgrep() { return 1; }
export -f pgrep
mkdir -p "$TEST_ROOT/project/scripts" "$TEST_ROOT/project/resources/macos" "$TEST_ROOT/target/release"
cp "$SCRIPT_DIR/create_macos_app_dist.sh" "$SCRIPT_DIR/cargo-notes.sh" "$TEST_ROOT/project/scripts/"
ln -s /usr/bin/true "$TEST_ROOT/target/release/velotype"
ln -s /dev/null "$TEST_ROOT/project/resources/macos/Info.plist"
ln -s /dev/null "$TEST_ROOT/project/resources/macos/velotype.icns"
bash "$TEST_ROOT/project/scripts/create_macos_app_dist.sh"
awk '
  NR <= 4 { next }
  /^clean / && /--profile dev/ { dev = NR }
  /^clean / && /--release/ { release = NR }
  /^build / { command = NR }
  END {
    if (!(dev && release && command && dev < command && release < command)) {
      print "FAIL: legacy packaging must clean dev/release before building" > "/dev/stderr"
      exit 1
    }
  }
' "$TEST_ROOT/calls.log"
[[ -x "$TEST_ROOT/project/dist/Velotype.app/Contents/MacOS/velotype" ]]
echo "PASS: legacy packaging cleanup and bundle creation (no compilation)"
