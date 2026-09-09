#!/usr/bin/env bash
# Deliberately separate binary identity for stale run-set provenance testing.
exec "$(dirname "$0")/fake-task7-binary.sh" "$@"
