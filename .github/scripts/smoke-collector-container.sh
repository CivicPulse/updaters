#!/usr/bin/env bash
# Exercise the actual daemon image under deployment constraints, without credentials.
set -euo pipefail
collector_image=${1:?Usage: smoke-collector-container.sh IMAGE}
collector_scratch=$(mktemp -d)
collector_name="civpulse-disabled-smoke-${RANDOM}-${RANDOM}"
cleanup() {
  docker rm --force "$collector_name" >/dev/null 2>&1 || true
  rm -rf "$collector_scratch"
}
trap cleanup EXIT
# A bind mount is disk backed, matching the chart's emptyDir medium. World-write
# is limited to this otherwise empty, isolated test directory and is removed below.
chmod 0777 "$collector_scratch"
collector_user=$(docker inspect --format '{{.Config.User}}' "$collector_image")
if [[ "$collector_user" != '65532:65532' ]]; then
  printf 'Collector image must declare the non-root numeric runtime user\n' >&2
  exit 1
fi
docker run --detach --name "$collector_name" --network none \
  --read-only --cap-drop=ALL --security-opt=no-new-privileges \
  --memory=1g --cpus=1 --pids-limit=128 \
  --mount "type=bind,source=$collector_scratch,target=/tmp" \
  --env CIVPULSE_ENV=test --env SOS_COLLECTOR_ENABLED=false \
  --env SOS_COLLECTOR_HEARTBEAT=/tmp/civpulse-results-heartbeat \
  --env R2_BUCKET=civpulse-files-test \
  --env R2_PUBLIC_ORIGIN=https://files-test.civpulse.org \
  --env SUPABASE_URL=http://127.0.0.1:9 \
  --env SUPABASE_SERVICE_ROLE_KEY=deliberately-unusable-local-smoke-fixture \
  "$collector_image" daemon >/dev/null
for ((attempt=0; attempt<50; attempt++)); do
  if [[ -s "$collector_scratch/civpulse-results-heartbeat" ]]; then
    break
  fi
  if [[ "$(docker inspect --format '{{.State.Running}}' "$collector_name")" != true ]]; then
    printf 'Disabled collector exited before writing its heartbeat\n' >&2
    exit 1
  fi
  sleep 0.2
done
if [[ ! -s "$collector_scratch/civpulse-results-heartbeat" ]]; then
  printf 'Disabled collector could not write to its disk scratch mount\n' >&2
  exit 1
fi
# Probe the exact same executable/heartbeat path used by Kubernetes.
docker exec "$collector_name" /usr/local/bin/civpulse-tool liveness
# The auxiliary binary runs inside the exact distroless image, with stdin and
# explicit disk scratch: no shell, stat executable, credentials or external network.
docker exec -i "$collector_name" /usr/local/bin/civpulse-measure \
  --scratch-root /tmp - 2026-11-03 --pipeline 2 2 \
  < crates/civpulse-results-contract/tests/fixtures/base.json > "$collector_scratch/measure.json"
python3 - "$collector_scratch/measure.json" <<'PYTHON'
import json, sys
receipt = json.load(open(sys.argv[1]))
assert receipt['jurisdictions'] == 2 and receipt['staged_items'] == 2
assert receipt['production_acceptance'] is False
assert receipt['scratch_filesystem'] not in ('tmpfs', 'ramfs')
assert receipt['semantic_sha256'] and receipt['staged_batches']
PYTHON
stop_started=$SECONDS
docker kill --signal=TERM "$collector_name" >/dev/null
collector_exit=$(timeout 15s docker wait "$collector_name")
if [[ "$collector_exit" != 0 || $((SECONDS-stop_started)) -gt 15 ]]; then
  printf 'Disabled daemon did not exit cleanly within its SIGTERM bound\n' >&2
  exit 1
fi
docker logs "$collector_name" > "$collector_scratch/logs.jsonl" 2>&1
python3 - "$collector_scratch/logs.jsonl" <<'PY'
import json, sys
from pathlib import Path
records = [json.loads(line) for line in Path(sys.argv[1]).read_text().splitlines() if line.strip()]
if any(record.get("level") == "ERROR" for record in records):
    raise SystemExit("Disabled daemon attempted failing work; inspect sanitized logs")
PY
printf 'Disabled collector daemon passed scratch, heartbeat, no-network and bounded SIGTERM checks\n'
