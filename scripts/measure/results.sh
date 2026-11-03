#!/usr/bin/env bash
# Offline only: the example connects solely to a loopback RPC double and local disk.
set -euo pipefail
if [[ $# -lt 3 ]]; then
  echo 'Usage: scripts/measure/results.sh INPUT DATE OUTPUT_PREFIX [--pipeline] [JURISDICTIONS ITEMS]' >&2
  exit 2
fi
input=$1
date=$2
output=$3
shift 3
CIVPULSE_MEASURE_CODE_REVISION=$(git rev-parse HEAD) cargo build --locked --release -p civpulse-results-collector --example measure
/usr/bin/time -v -o "${output}.time.txt" target/release/examples/measure "$input" "$date" "$@" > "${output}.json"
cat "${output}.json"
