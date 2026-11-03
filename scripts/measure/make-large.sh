#!/usr/bin/env bash
# Reproduce the published 32 KiB-per-county-item resource fixture without statewide buffering.
set -euo pipefail
if [[ $# != 2 ]]; then
  echo 'Usage: scripts/measure/make-large.sh NOVEMBER_SOURCE OUTPUT' >&2
  exit 2
fi
source_hash=f464d3b037a82d594c893aef072042530a84b6b1cb3edb76b1e3873bed2dc3ba
large_hash=4f4ce3c4856a261096848f7b1f033e69ab4e1501f61099411ea771a12a89d2c9
actual=$(sha256sum "$1")
[[ ${actual%% *} == "$source_hash" ]] || { echo 'Input differs from the reviewed November receipt' >&2; exit 1; }
cargo build --locked --release -p civpulse-results-collector --example make_large
target/release/examples/make_large "$1" "$2"
actual=$(sha256sum "$2")
[[ ${actual%% *} == "$large_hash" ]] || { echo 'Generated fixture differs from the reference bytes' >&2; exit 1; }
printf '%s\n' "$actual"
