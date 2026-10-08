#!/usr/bin/env bash
# One entry point for notes build/test/check: prune this project's dev/release
# artifacts before compiling; retain third-party dependencies and packaged Apps.
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"
COMMAND="${1:-}"
case "$COMMAND" in build|test|check) shift ;; *) echo "usage: bash scripts/cargo-notes.sh build|test|check [cargo arguments]" >&2; exit 2 ;; esac
if pgrep -x cargo >/dev/null || pgrep -x rustc >/dev/null; then
  echo "another Rust build is running; refusing concurrent cache cleanup" >&2
  exit 1
fi
TARGET_DIR="$(cargo metadata --manifest-path "$PROJECT_ROOT/Cargo.toml" --no-deps --offline --locked --format-version 1 \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
case "$TARGET_DIR" in ""|/|"$HOME"|"$PROJECT_ROOT") echo "unsafe cargo target directory" >&2; exit 1 ;; /*) ;; *) echo "cargo target directory must be absolute" >&2; exit 1 ;; esac
DEBUG_KIB=0
if [[ -d "$TARGET_DIR/debug" ]]; then DEBUG_KIB="$(du -sk "$TARGET_DIR/debug" | awk '{print $1}')"; fi
if (( DEBUG_KIB > 26214400 )); then
  # 25 GiB high-water mark: all of this profile is reproducible compiler output.
  cargo clean --manifest-path "$PROJECT_ROOT/Cargo.toml" --target-dir "$TARGET_DIR" --profile dev --offline --locked
else
  cargo clean --manifest-path "$PROJECT_ROOT/Cargo.toml" --target-dir "$TARGET_DIR" --profile dev --offline --locked -p velotype -p app-lite-core
fi
cargo clean --manifest-path "$PROJECT_ROOT/Cargo.toml" --target-dir "$TARGET_DIR" --release --offline --locked -p velotype -p app-lite-core
export CARGO_INCREMENTAL=0
exec cargo "$COMMAND" --manifest-path "$PROJECT_ROOT/Cargo.toml" --locked "$@"
