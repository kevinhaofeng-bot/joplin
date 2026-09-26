#!/usr/bin/env bash
# Idle memory of the notes app on a given library profile.
# Usage: scripts/measure-product-memory.sh <executable> <profile-dir> <output-dir> [runs=3] [settle-seconds=10]
# The profile is opened read-write by the app: pass a COPY, never a personal library.
# Samples RSS (ps) and physical footprint (footprint) after the settle time.
set -euo pipefail
[[ $# -ge 3 ]] || { echo "usage: $0 <executable> <profile-dir> <output-dir> [runs] [settle-seconds]" >&2; exit 2; }
EXE="$1"; PROFILE="$2"; OUT="$3"; RUNS="${4:-3}"; SETTLE="${5:-10}"
[[ -x "$EXE" ]] || { echo "not executable: $EXE" >&2; exit 2; }
[[ "$PROFILE" = /* ]] || { echo "profile must be absolute" >&2; exit 2; }
mkdir -p "$OUT"
RAW="$OUT/samples.tsv"
echo -e "run\tpid\tsettle_s\trss_kib\tfootprint" > "$RAW"
for run in $(seq 1 "$RUNS"); do
  JOPLIN_LITE_PROFILE="$PROFILE" "$EXE" > "$OUT/run-$run.log" 2>&1 &
  pid=$!
  sleep "$SETTLE"
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "app exited early (run $run); see $OUT/run-$run.log" >&2; exit 1
  fi
  rss=$(ps -o rss= -p "$pid" | tr -d ' ')
  fp=$(footprint -p "$pid" 2>/dev/null | sed -n 's/.*[Pp]hys_footprint: *\([0-9.]* [KMG]B\).*/\1/p;s/^.*Footprint: *\([0-9.]* [KMG]B\).*/\1/p' | head -1)
  echo -e "$run\t$pid\t$SETTLE\t$rss\t${fp:-unavailable}" >> "$RAW"
  kill "$pid"; wait "$pid" 2>/dev/null || true
  sleep 2
done
{
  echo "executable_sha256: $(shasum -a 256 "$EXE" | awk '{print $1}')"
  echo "machine: $(sysctl -n machdep.cpu.brand_string) $(($(sysctl -n hw.memsize)/1073741824)) GiB, macOS $(sw_vers -productVersion)"
  echo "profile: $PROFILE ($(du -sh "$PROFILE" | cut -f1))"
  echo "runs: $RUNS, settle: ${SETTLE}s (cold first run, warm after)"
  awk -F'\t' 'NR>1{printf "rss_mib run%s: %.1f\n",$1,$4/1024; if(min==""||$4<min)min=$4; if($4>max)max=$4} END{printf "rss_mib range: %.1f - %.1f\n",min/1024,max/1024}' "$RAW"
} | tee "$OUT/summary.txt"
