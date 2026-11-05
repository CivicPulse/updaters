#!/usr/bin/env bash
# Offline only: the example connects solely to a loopback RPC double and local disk.
set -euo pipefail
exec python3 "$(dirname -- "${BASH_SOURCE[0]}")/results.py" "$@"
